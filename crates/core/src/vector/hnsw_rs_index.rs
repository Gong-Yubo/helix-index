//! `hnsw_rs` 向量索引（P4 A/B 定案：原生增量 insert 胜出，生产路径）。
//!
//! # 距离换算
//!
//! `DistDot` 返回 `1 − cos`；本实现的 `search` 输出**统一换算为平方欧氏距离**
//! `d² = 2 − 2·cos = 2·(1 − cos)`，与 `VectorIndex` trait 的约定（d²）对齐，
//! 这样 `VectorRetriever` 的 `score = 1 − d²/2 = cos` 在两种实现下都成立。

use hnsw_rs::hnsw::Hnsw;
use hnsw_rs::prelude::Distance;

use crate::error::Result;
use crate::predicate::{CandidateFilter, FilterKind};
use crate::types::ChunkId;

use super::{NormalizedVector, VectorIndex};

/// HNSW 图构建/检索参数（P4 定稿；P5 在真实语料上校准 ef_search，见 p5-design 8.6）。
const MAX_NB_CONNECTION: usize = 32; // M
const MAX_LAYER: usize = 16;
const EF_CONSTRUCTION: usize = 300;
const EF_SEARCH: usize = 200;

/// 带过滤时的过采样倍率（`ef = k * FACTOR`，纯搜索宽度，见 §5.7 路径 B）。
///
/// ⚠️ 这不是"目标收集数"——`hnsw_rs` 的 `search_filter` 在结果堆**未满**时会
/// 关闭距离剪枝（`hnsw.rs:1019`），`ef` 只是"堆满即开剪枝"的阈值。
const EF_FILTER_FACTOR: usize = 4;

/// `ef` 的硬上限，防止低选择度下延迟失控（低选择度时本来就是整图遍历）。
const EF_FILTER_MAX: usize = 256;

/// 路径 A 的候选数上限（防止重度删除场景过采样爆炸）。
const PHYSICAL_OVERSAMPLE_CAP: usize = 1024;

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
    /// 查询时动态候选列表宽度（P5 前硬编码 200 从未校准；8.6 最小校准注入口）
    ef_search: usize,
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
        Self {
            hnsw,
            ef_search: EF_SEARCH,
        }
    }

    /// 覆盖 ef_search（8.6 向量路诊断：ef_search ∈ {100, 200, 400} 最小校准）。
    pub fn with_ef_search(mut self, ef_search: usize) -> Self {
        self.ef_search = ef_search;
        self
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

    /// 向量近邻检索（过滤下推，见设计文档 §5.7 的两条路径）。
    ///
    /// # 路径 A：`None` 或 `FilterKind::Alive`（热路径）
    ///
    /// 走**普通 `search()`**，保留 fast-return（`hnsw.rs:983-984`），成本结构与
    /// 无过滤检索完全一致——这是所有无过滤查询的必经之路，不能为了过滤而拖慢它。
    /// 已软删除的向量无法从图中摘除，因此按存活比例**过采样 `knbn`**，返回后再过滤。
    ///
    /// # 路径 B：`FilterKind::Filtered`
    ///
    /// 走 `search_filter`。代价是 **fast-return 被禁用**（`hnsw.rs:985-992`：堆满后
    /// 只 `retain` 不 return），且堆未满时距离剪枝全程关闭 ⇒ 低选择度下退化为整图遍历。
    /// 这是 `hnsw_rs` 的结构性行为，调参治不了；我们用 `EF_FILTER_MAX` 限制最坏延迟，
    /// 召回缺口由 `Metrics::vector_shortfall` 暴露。
    fn search_filtered(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        if k == 0 {
            return Ok(Vec::new());
        }

        let is_filtered = filter.is_some_and(|f| f.kind() == FilterKind::Filtered);
        let mut out: Vec<(ChunkId, f32)> = if is_filtered {
            self.search_path_b(query, k, filter)
        } else {
            self.search_path_a(query, k, filter)
        };

        // 统一稳定排序（距离升序 → chunk_id 升序），保 NFR-06 确定性
        out.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        Ok(out)
    }

    fn len(&self) -> usize {
        self.hnsw.get_nb_point()
    }
}

/// 两条检索路径的实现细节（放在 inherent impl，避免污染 trait 契约）。
impl HnswRsIndex {
    /// 路径 A：普通 `search()` + 按存活比例过采样 + 后置过滤（保 fast-return）。
    fn search_path_a(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Vec<(ChunkId, f32)> {
        let alive_ratio = match filter {
            None => 1.0,
            Some(f) => f.allowed_count() as f64 / self.len().max(1) as f64,
        };
        // 期望需要 k/ratio 个候选才能凑出 k 个存活项，再加 k 的余量吸收方差
        let knbn = ((k as f64 / alive_ratio.max(0.01)).ceil() as usize + k)
            .min(self.len())
            .min(PHYSICAL_OVERSAMPLE_CAP)
            .max(k.min(self.len()));

        let neighbours = self.hnsw.search(query.as_slice(), knbn, self.ef_search);
        let mut out: Vec<(ChunkId, f32)> = neighbours
            .into_iter()
            .map(|n| (n.d_id as ChunkId, 2.0 * n.distance))
            .collect();

        if let Some(f) = filter {
            out.retain(|(id, _)| f.contains(*id));
        }
        out.truncate(k);
        out
    }

    /// 路径 B：`search_filter` + `FilterT` 适配器（真下推，但无 fast-return）。
    fn search_path_b(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Vec<(ChunkId, f32)> {
        // knbn 是输出条数旋钮，ef 只是搜索宽度（§3.3 结论 4）
        let ef = (k * EF_FILTER_FACTOR).max(k).min(EF_FILTER_MAX);
        let neighbours = match filter {
            Some(f) => {
                // `FilterT` 对 `Fn(&usize) -> bool` 有 blanket impl，闭包即可适配
                let adapt = |id: &usize| f.contains(*id as ChunkId);
                self.hnsw
                    .search_filter(query.as_slice(), k, ef, Some(&adapt))
            }
            None => self.hnsw.search_filter(query.as_slice(), k, ef, None),
        };
        neighbours
            .into_iter()
            .map(|n| (n.d_id as ChunkId, 2.0 * n.distance))
            .collect()
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

    /// 只允许偶数 chunk_id 通过的谓词（路径 B 测试用）。
    struct EvenChunks {
        count: usize,
    }

    impl CandidateFilter for EvenChunks {
        fn contains(&self, id: ChunkId) -> bool {
            id.is_multiple_of(2)
        }
        fn allowed_count(&self) -> usize {
            self.count
        }
        fn kind(&self) -> FilterKind {
            FilterKind::Filtered
        }
    }

    /// 建一个 `n` 条的图，返回 (index, entries)。
    fn build_index(n: usize, seed: &mut u64) -> (HnswRsIndex, Vec<(u32, NormalizedVector)>) {
        let mut h = HnswRsIndex::with_capacity(n);
        let entries: Vec<(u32, NormalizedVector)> = (0..n)
            .map(|i| (i as u32, NormalizedVector::new(random_vec(seed))))
            .collect();
        for (id, v) in &entries {
            h.add(*id, v.clone()).unwrap();
        }
        (h, entries)
    }

    /// **路径 A**：`AliveOnly` 必须挡掉已软删除的候选（Q-C1 的修复点）。
    #[test]
    fn 路径A存活过滤挡掉幽灵候选() {
        use crate::bitmap::Bitmap;
        use crate::predicate::AliveOnly;

        let mut seed = 11u64;
        let (idx, entries) = build_index(200, &mut seed);

        // 只留偶数 id 存活
        let mut alive = Bitmap::new();
        for i in 0..200u32 {
            if i % 2 == 0 {
                alive.set(i);
            }
        }
        let predicate = AliveOnly::new(&alive);

        let q = entries[7].1.clone();
        let got = idx.search_filtered(&q, 10, Some(&predicate)).unwrap();

        assert!(!got.is_empty(), "过采样后应能凑出足够的存活候选");
        assert_eq!(got.len(), 10, "存活比例 50% 时应能凑满 k=10");
        for (id, _) in &got {
            assert!(
                predicate.contains(*id),
                "路径 A 返回了已软删除的 chunk {id}"
            );
            assert_eq!(id % 2, 0);
        }
        // 自匹配：chunk 7 是死的，最近的存活候选不应是它
        assert!(!got.iter().any(|(id, _)| *id == 7));
    }

    /// **路径 B**：`search_filter` 的返回值**必须全部通过谓词**（soundness）。
    #[test]
    fn 路径B结果全部通过谓词() {
        let mut seed = 23u64;
        let (idx, entries) = build_index(200, &mut seed);
        let predicate = EvenChunks { count: 100 };

        for probe in [3usize, 17, 99, 150] {
            let q = entries[probe].1.clone();
            let got = idx.search_filtered(&q, 5, Some(&predicate)).unwrap();
            assert!(!got.is_empty(), "probe={probe} 应至少召回一条");
            for (id, _) in &got {
                assert!(
                    predicate.contains(*id),
                    "路径 B 返回了未通过谓词的 chunk {id}（probe={probe}）"
                );
            }
            // 距离升序（统一稳定排序的契约）
            for w in got.windows(2) {
                assert!(w[0].1 <= w[1].1 || (w[0].1 - w[1].1).abs() < 1e-6);
            }
        }
    }

    /// 路径 A 与路径 B 的选择：谓词种类决定走哪条（行为等价性由 T12 量化）。
    #[test]
    fn 无过滤时走路径A且与现状一致() {
        let mut seed = 31u64;
        let (idx, entries) = build_index(200, &mut seed);
        let q = entries[5].1.clone();

        let via_trait = idx.search_filtered(&q, 10, None).unwrap();
        let via_search = idx.search(&q, 10).unwrap();
        assert_eq!(via_trait, via_search, "None 谓词应等价于无过滤检索");
        assert_eq!(via_trait[0].0, 5, "自匹配应是最近邻（d²≈0）");
    }

    /// **T12**：「选择度 × 延迟 × 召回」三元数据（V2.1 是否引入 prefilter 结构的依据）。
    ///
    /// ⚠️ 必须 `--release` 运行（debug 下 hnsw_rs 建图极慢）。
    /// 本测试**不做硬断言**（低选择度 + ANN 的延迟与召回无法两全，是 hnsw_rs 的
    /// 结构性行为），只打印三元曲线供人工判读。
    #[test]
    #[ignore]
    fn T12_选择度与延迟与召回三元数据() {
        let n = 5_000usize;
        let mut seed = 7u64;
        let (idx, entries) = build_index(n, &mut seed);
        let brute = BruteForceIndex::from_entries(entries.clone());

        // 选择度档位：100% / 10% / 1% / 0.1%
        for (name, modulus) in [("100%", 1u32), ("10%", 10), ("1%", 100), ("0.1%", 1000)] {
            let mut alive = crate::bitmap::Bitmap::new();
            for i in 0..n as u32 {
                if i % modulus == 0 {
                    alive.set(i);
                }
            }
            let allowed = alive.count_ones();
            let predicate = crate::predicate::AliveOnly::new(&alive);
            let predicate_b = EvenChunks { count: allowed };

            let mut lat_us = Vec::new();
            let mut recall = Vec::new();
            let mut shortfall = Vec::new();

            for probe in (0..20).map(|i| i * 37 % n) {
                let q = entries[probe].1.clone();

                let t0 = std::time::Instant::now();
                let got = idx.search_filtered(&q, 10, Some(&predicate)).unwrap();
                lat_us.push(t0.elapsed().as_micros());
                shortfall.push(10usize.saturating_sub(got.len()));

                // 召回基线：暴力 + 同谓词（精确 oracle）
                let oracle: Vec<u32> = brute
                    .search_filtered(&q, 10, Some(&predicate_b))
                    .unwrap_or_default()
                    .iter()
                    .map(|(i, _)| *i)
                    .collect();
                let hit = std::collections::HashSet::<u32>::from_iter(got.iter().map(|(i, _)| *i));
                let inter = oracle.iter().filter(|x| hit.contains(x)).count();
                recall.push(inter as f32 / oracle.len().max(1) as f32);
            }

            let avg_lat = lat_us.iter().sum::<u128>() as f32 / lat_us.len() as f32;
            let p99_lat = {
                let mut s = lat_us.clone();
                s.sort_unstable();
                s[((s.len() as f32 * 0.99) as usize).min(s.len() - 1)]
            };
            let avg_recall = recall.iter().sum::<f32>() / recall.len() as f32;
            let avg_short = shortfall.iter().sum::<usize>() as f32 / shortfall.len() as f32;

            println!(
                "选择度 {name:>5} (allowed={allowed:>5}) | 平均 {avg_lat:>8.1}µs | \
                 P99 {p99_lat:>8}µs | 召回 {avg_recall:.3} | 平均缺口 {avg_short:.2}"
            );
        }
    }
}
