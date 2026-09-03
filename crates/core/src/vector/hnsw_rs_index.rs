//! `hnsw_rs` 向量索引（T4-06a 的 A/B 候选）。
//!
//! 与 `instant-distance` 的关键差异：
//!
//! | 维度 | instant-distance | hnsw_rs |
//! | -- | ---------------- | ------- |
//! | 增量 insert | ❌（一次性 build） | ✅ 原生 `insert(&self)` |
//! | 距离 | 自定义 `Point`（平方欧氏） | `DistDot` = `1 − cos`（要求归一化） |
//!
//! # 距离换算
//!
//! `DistDot` 返回 `1 − cos`；本实现的 `search` 输出**统一换算为平方欧氏距离**
//! `d² = 2 − 2·cos = 2·(1 − cos)`，与 `VectorIndex` trait 的约定（d²）对齐，
//! 这样 `VectorRetriever` 的 `score = 1 − d²/2 = cos` 在两种实现下都成立。

use hnsw_rs::hnsw::Hnsw;
use hnsw_rs::prelude::Distance;

use crate::error::Result;
use crate::types::ChunkId;

use super::{NormalizedVector, VectorIndex};

/// 对齐 `HnswIndex`（instant-distance）的参数（见 p2-design.md）。
const MAX_NB_CONNECTION: usize = 32; // M
const MAX_LAYER: usize = 16;
const EF_CONSTRUCTION: usize = 300;
const EF_SEARCH: usize = 200;

/// 自定义点积距离：`1 − cos`（要求向量已 L2 归一化）。
///
/// **不用库里的 `DistDot`**：它在 aarch64 的标量实现里有 `assert!(dot <= 1.)`，
/// 自匹配等边界场景会因浮点舍入（dot = 1.0000002）直接 panic。
/// 我们把点积 clamp 到 [−1, 1] 再相减，语义相同但数值安全。
#[derive(Default, Copy, Clone)]
struct DistDotClamped;

impl Distance<f32> for DistDotClamped {
    fn eval(&self, va: &[f32], vb: &[f32]) -> f32 {
        let dot: f32 = va.iter().zip(vb.iter()).map(|(a, b)| a * b).sum();
        1.0 - dot.clamp(-1.0, 1.0)
    }
}

/// hnsw_rs 实现的向量索引：支持原生增量插入。
pub struct HnswRsIndex {
    hnsw: Hnsw<'static, f32, DistDotClamped>,
}

impl HnswRsIndex {
    /// `max_capacity` 为容量提示（分配优化，非硬上限）。
    pub fn with_capacity(max_capacity: usize) -> Self {
        let hnsw = Hnsw::new(
            MAX_NB_CONNECTION,
            max_capacity,
            MAX_LAYER,
            EF_CONSTRUCTION,
            DistDotClamped,
        );
        Self { hnsw }
    }
}

impl Default for HnswRsIndex {
    fn default() -> Self {
        Self::with_capacity(1024)
    }
}

impl VectorIndex for HnswRsIndex {
    fn add(&mut self, id: ChunkId, vec: NormalizedVector) -> Result<()> {
        // hnsw_rs 原生增量插入（insert 只需 &self）
        self.hnsw.insert((vec.as_slice(), id as usize));
        Ok(())
    }

    fn search(&self, query: &NormalizedVector, k: usize) -> Result<Vec<(ChunkId, f32)>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let neighbours = self.hnsw.search(query.as_slice(), k, EF_SEARCH);
        let out: Vec<(ChunkId, f32)> = neighbours
            .into_iter()
            .map(|n| {
                // DistDot = 1 - cos；换算为 d² = 2(1-cos)，对齐 trait 约定
                (n.d_id as ChunkId, 2.0 * n.distance)
            })
            .collect();
        Ok(out)
    }

    fn len(&self) -> usize {
        self.hnsw.get_nb_point()
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;
    use crate::vector::BruteForceIndex;

    fn random_vec(seed: &mut u64) -> Vec<f32> {
        let mut next = || {
            *seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((*seed >> 33) as u32 as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        (0..512).map(|_| next()).collect()
    }

    #[test]
    fn 增量插入立即可检索() {
        let mut idx = HnswRsIndex::with_capacity(100);
        let mut seed = 7u64;

        // 先插 50 条
        let entries: Vec<(u32, NormalizedVector)> = (0..50)
            .map(|i| (i as u32, NormalizedVector::new(random_vec(&mut seed))))
            .collect();
        for (id, v) in &entries {
            idx.add(*id, v.clone()).unwrap();
        }
        assert_eq!(idx.len(), 50);

        // 自匹配应为最近邻（距离 ≈ 0）
        let q = entries[0].1.clone();
        let got = idx.search(&q, 5).unwrap();
        assert!(!got.is_empty());
        assert!(got[0].1 < 1e-3, "最近邻应是自身，实际 d²={}", got[0].1);
    }

    /// 与暴力 Top-10 重合率（1 万条）。debug 下 hnsw_rs 构建慢，标 ignore 用 release 跑。
    #[test]
    #[ignore]
    fn 与暴力万条重合率达标() {
        let n = 10_000usize;
        let mut seed = 7u64;
        let entries: Vec<(u32, NormalizedVector)> = (0..n)
            .map(|i| (i as u32, NormalizedVector::new(random_vec(&mut seed))))
            .collect();

        // hnsw_rs：逐条增量插入（这正是它的卖点）
        let mut h = HnswRsIndex::with_capacity(n);
        for (id, v) in &entries {
            h.add(*id, v.clone()).unwrap();
        }
        let brute = BruteForceIndex::from_entries(entries);

        let mut total = 0.0;
        let mut queries = 0;
        for _ in 0..10 {
            let q = NormalizedVector::new(random_vec(&mut seed));
            let hv: Vec<u32> = h.search(&q, 10).unwrap().iter().map(|(i, _)| *i).collect();
            let bv: Vec<u32> = brute
                .search(&q, 10)
                .unwrap()
                .iter()
                .map(|(i, _)| *i)
                .collect();
            let hs: std::collections::HashSet<u32> = hv.iter().copied().collect();
            total += bv.iter().filter(|x| hs.contains(x)).count() as f32 / 10.0;
            queries += 1;
        }
        let avg = total / queries as f32;
        println!("hnsw_rs vs 暴力 Top-10 平均重合率 = {avg:.3}");
        assert!(avg >= 0.95, "重合率过低: {avg}");
    }
}
