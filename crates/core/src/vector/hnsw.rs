//! HNSW 近似最近邻索引（基于 `instant-distance`）。
//!
//! - **一次性构建**：无增量 insert（架构 7.5 / ADR-005），`add` 返回 `ImmutableIndex`
//! - **固定 seed**：构建带随机性，固定 seed 保证确定性（NFR-06，T3-08 提前）
//! - `MapItem.value` 已按内部点序对齐，直接当 `ChunkId` 用

use instant_distance::{Builder, HnswMap, Search};

use crate::error::{Error, Result};
use crate::types::ChunkId;

use super::{NormalizedVector, VectorIndex};

/// 固定随机种子（确定性）。
pub const FIXED_SEED: u64 = 0x5EED_2026_0902;

/// HNSW 索引（封装 `instant_distance::HnswMap`）。
pub struct HnswIndex {
    map: HnswMap<NormalizedVector, ChunkId>,
}

impl HnswIndex {
    /// 一次性构建。
    ///
    /// `ef_construction` / `ef_search` 取 300/200：在 1 万条随机 512 维向量上
    /// 保证 Top-10 召回重合率 ≥ 95%（随机向量是比真实 embedding 更难的场景）。
    /// 这些参数在 P5 会做网格搜索校准（架构文档 7.5 / plan.md T5-xx）。
    pub fn build(entries: Vec<(ChunkId, NormalizedVector)>) -> Self {
        let (values, points): (Vec<ChunkId>, Vec<NormalizedVector>) = entries.into_iter().unzip();
        let map = Builder::default()
            .ef_construction(300)
            .ef_search(200)
            .seed(FIXED_SEED)
            .build(points, values);
        Self { map }
    }
}

impl VectorIndex for HnswIndex {
    fn add(&mut self, _id: ChunkId, _vec: NormalizedVector) -> Result<()> {
        Err(Error::ImmutableIndex)
    }

    fn search(&self, query: &NormalizedVector, k: usize) -> Result<Vec<(ChunkId, f32)>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let mut scratch = Search::default();
        let mut out: Vec<(ChunkId, f32)> = self
            .map
            .search(query, &mut scratch)
            .map(|item| (*item.value, item.distance))
            .collect();
        out.truncate(k);
        Ok(out)
    }

    fn len(&self) -> usize {
        self.map.values.len()
    }
}

#[cfg(test)]
mod tests {
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

    /// 快速冒烟：小规模下 HNSW 能返回正确排序（debug 模式几秒内）。
    #[test]
    fn 小规模基本检索正确() {
        let mut seed: u64 = 7;
        let n = 100usize;
        let entries: Vec<(ChunkId, NormalizedVector)> = (0..n)
            .map(|i| (i as ChunkId, NormalizedVector::new(random_vec(&mut seed))))
            .collect();
        let hnsw = HnswIndex::build(entries.clone());

        let q = entries[0].1.clone();
        let got = hnsw.search(&q, 5).unwrap();
        assert_eq!(got.len(), 5);
        // 自匹配应最近（距离 ≈ 0）
        assert!(got[0].1 < 1e-3, "最近邻应是自身，实际距离 {}", got[0].1);
        // 距离升序
        assert!(got.windows(2).all(|w| w[0].1 <= w[1].1));
    }

    /// 与暴力 Top-10 重合率 ≥ 95%（1 万条）。
    ///
    /// debug 模式下 HNSW 构建极慢（2000 条就要 100s），故标记 ignore，
    /// 用 release 跑：`cargo test --release -p index-core --lib vector::hnsw -- --ignored`
    #[test]
    #[ignore]
    fn 与暴力万条重合率达标() {
        let n = 10_000usize;
        let mut seed: u64 = 7;
        let entries: Vec<(ChunkId, NormalizedVector)> = (0..n)
            .map(|i| (i as ChunkId, NormalizedVector::new(random_vec(&mut seed))))
            .collect();

        let hnsw = HnswIndex::build(entries.clone());
        let brute = BruteForceIndex::from_entries(entries);

        let mut total_overlap = 0.0;
        let mut queries = 0;
        for _ in 0..10 {
            let q = NormalizedVector::new(random_vec(&mut seed));
            let h: Vec<ChunkId> = hnsw
                .search(&q, 10)
                .unwrap()
                .iter()
                .map(|(id, _)| *id)
                .collect();
            let b: Vec<ChunkId> = brute
                .search(&q, 10)
                .unwrap()
                .iter()
                .map(|(id, _)| *id)
                .collect();
            let hset: std::collections::HashSet<ChunkId> = h.iter().copied().collect();
            let inter = b.iter().filter(|x| hset.contains(x)).count();
            total_overlap += inter as f32 / 10.0;
            queries += 1;
        }
        let avg = total_overlap / queries as f32;
        println!("HNSW vs 暴力 Top-10 平均重合率 = {avg:.3}");
        assert!(avg >= 0.95, "重合率过低: {avg}");
    }
}
