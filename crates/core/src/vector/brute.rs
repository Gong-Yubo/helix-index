//! 暴力线性扫描索引：精确、增量、确定性。P2 的 CLI 默认用它（30 篇规模足够），
//! 同时作为 HNSW 的对照 oracle（T2-05 重合率单测）。
//!
//! V2 Step 5：它**天然满足** [`VectorIndex::search_exact_filtered`] 的精确契约，
//! 因此该方法是转发、[`VectorIndex::prefers_exact`] 恒 `true`——"Brute 的结果
//! 本来就精确"由类型与实现表达，编排层不必为它特判。

use crate::error::Result;
use crate::predicate::CandidateFilter;
use crate::types::ChunkId;

use super::{NormalizedVector, VectorIndex};

/// 暴力索引
#[derive(Debug, Default)]
pub struct BruteForceIndex {
    entries: Vec<(ChunkId, NormalizedVector)>,
}

impl BruteForceIndex {
    /// 创建空索引。
    pub fn new() -> Self {
        Self::default()
    }

    /// 由既有向量批量构造（诊断/基线对照用，见 p5-design 8.6）。
    pub fn from_entries(entries: Vec<(ChunkId, NormalizedVector)>) -> Self {
        Self { entries }
    }
}

impl VectorIndex for BruteForceIndex {
    fn add(&mut self, id: ChunkId, vec: NormalizedVector) -> Result<()> {
        self.entries.push((id, vec));
        Ok(())
    }

    /// 线性扫描时判定谓词：**精确、零召回损失**，作为 HNSW 的对照 oracle。
    fn search_filtered(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let mut scored: Vec<(ChunkId, f32)> = self
            .entries
            .iter()
            .filter(|(id, _)| filter.is_none_or(|f| f.contains(*id)))
            .map(|(id, v)| (*id, query.distance_sq(v)))
            .collect();
        // 距离升序；同距离按 chunk_id 升序（确定性 NFR-06）。
        // ⚠️ `total_cmp` 而非 `partial_cmp(..).unwrap_or(Equal)`：与
        // `HnswRsIndex::search_exact_filtered` 的排序**同源**，让 I4 的"逐位一致"
        // 由结构保证（V2 Step 5 / S5-01，§4.2.2）。唯一行为差异在 NaN 输入
        // （旧：视作相等 ⇒ 顺序取决于 sort 的不稳定行为；新：确定性全序），
        // 而 NaN 属 embedder 契约外输入，精确路径已用 `debug_assert!` 强制。
        scored.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        scored.truncate(k);
        Ok(scored)
    }

    /// Brute 的线性扫描本就是精确的 ⇒ 直接转发 [`Self::search_filtered`]。
    ///
    /// 保留独立方法（而非让编排层对 Brute 特判）是为了让"精确路径"在所有后端上
    /// 都只有**一个**调用点——策略由 `prefers_exact` 表达，分派逻辑不必认识后端类型。
    fn search_exact_filtered(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        self.search_filtered(query, k, filter)
    }

    /// 恒 `true`：本后端的**每一次**检索都是精确的，于是它的
    /// `Metrics.vector_route` 恒为 `Exact`——那不是"兜底被触发"，而是在陈述事实。
    fn prefers_exact(&self, _filter: &dyn CandidateFilter) -> bool {
        true
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)] // 中文测试名
    use super::*;

    #[test]
    fn 暴力检索按距离升序() {
        let mut idx = BruteForceIndex::new();
        // 三个单位向量：0=(1,0) 1=(0,1) 2=(0.6,0.8)
        idx.add(0, NormalizedVector::new(vec![1.0, 0.0])).unwrap();
        idx.add(1, NormalizedVector::new(vec![0.0, 1.0])).unwrap();
        idx.add(2, NormalizedVector::new(vec![0.6, 0.8])).unwrap();

        // 查询 (1,0)：最近的是 0，其次 2（cos 0.6），最远 1（cos 0）
        let q = NormalizedVector::new(vec![1.0, 0.0]);
        let got = idx.search(&q, 10).unwrap();
        let ids: Vec<ChunkId> = got.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![0, 2, 1]);
    }

    /// **T17**：`filter = None` 的契约——**不过滤，含已软删除条目**。
    ///
    /// 这条测试存在的意义是防止有人误以为"Q-C1 已在向量层修好"。
    /// 向量层**做不到**（hnsw_rs 无 remove API），存活过滤只能靠编排层注入谓词。
    #[test]
    fn None谓词会召回已软删除的chunk() {
        use crate::bitmap::Bitmap;
        use crate::predicate::AliveOnly;

        let mut idx = BruteForceIndex::new();
        for id in 0..4u32 {
            idx.add(id, NormalizedVector::new(vec![id as f32, 1.0]))
                .unwrap();
        }
        // 模拟：chunk 1、3 已被软删除
        let mut alive = Bitmap::new();
        alive.set(0);
        alive.set(2);
        let predicate = AliveOnly::new(&alive);

        let q = NormalizedVector::new(vec![1.0, 0.0]);

        // None → 幽灵候选照常出现（这是契约，不是 bug）
        let all: Vec<ChunkId> = idx
            .search(&q, 10)
            .unwrap()
            .iter()
            .map(|(i, _)| *i)
            .collect();
        assert_eq!(all.len(), 4, "None 应不过滤，返回全部 4 条");
        assert!(
            all.contains(&1) && all.contains(&3),
            "已软删除的 chunk 仍会被召回"
        );

        // Some(AliveOnly) → 幽灵候选被挡掉（按距离升序：2 比 0 更近）
        let filtered: Vec<ChunkId> = idx
            .search_filtered(&q, 10, Some(&predicate))
            .unwrap()
            .iter()
            .map(|(i, _)| *i)
            .collect();
        assert_eq!(filtered, vec![2, 0], "AliveOnly 必须挡掉已软删除的 chunk");
        assert!(!filtered.contains(&1) && !filtered.contains(&3));
    }

    #[test]
    fn 过滤后不足k条时只返回通过的部分() {
        use crate::bitmap::Bitmap;
        use crate::predicate::AliveOnly;

        let mut idx = BruteForceIndex::new();
        for id in 0..5u32 {
            idx.add(id, NormalizedVector::new(vec![1.0, 0.0])).unwrap();
        }
        let mut alive = Bitmap::new();
        alive.set(4);
        let predicate = AliveOnly::new(&alive);

        let q = NormalizedVector::new(vec![1.0, 0.0]);
        let got = idx.search_filtered(&q, 3, Some(&predicate)).unwrap();
        assert_eq!(
            got.len(),
            1,
            "只允许 1 条通过时，不能用未通过的候选把 k 凑满"
        );
        assert_eq!(got[0].0, 4);
    }

    // ---- V2 Step 5 / S5-01：`search_exact_filtered` 的 brute 侧契约 ----

    /// 只允许偶数 id 通过的谓词（S5-T2 / S5-T5 用）。
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
        fn kind(&self) -> crate::predicate::FilterKind {
            crate::predicate::FilterKind::Filtered
        }
    }

    /// **S5-T2（brute 侧）**：精确路径 soundness（每条满足谓词）+ 完整性
    /// （条数 `== min(k, allowed)`，不允许少返回）+ `k=0` 边界。
    #[test]
    fn 精确路径满足谓词且条数为min_k_allowed() {
        let mut idx = BruteForceIndex::new();
        for id in 0..20u32 {
            idx.add(id, NormalizedVector::new(vec![id as f32, 1.0]))
                .unwrap();
        }
        let even = EvenChunks { count: 10 };
        let q = NormalizedVector::new(vec![1.0, 0.0]);

        // k < allowed：恰好 k 条
        let got = idx.search_exact_filtered(&q, 4, Some(&even)).unwrap();
        assert_eq!(got.len(), 4);
        for (id, _) in &got {
            assert!(even.contains(*id), "精确路径返回了未通过谓词的 chunk {id}");
        }
        // k > allowed：返回全部 allowed 条（**不允许少返回**，这是与 ANN 的唯一差异）
        let all = idx.search_exact_filtered(&q, 999, Some(&even)).unwrap();
        assert_eq!(all.len(), 10, "应返回全部 10 条 allowed");
        // 距离升序
        for w in all.windows(2) {
            assert!(w[0].1 <= w[1].1, "距离必须升序");
        }
        // k = 0 边界
        assert!(idx
            .search_exact_filtered(&q, 0, Some(&even))
            .unwrap()
            .is_empty());
    }

    /// **S5-T5**：精确路径对 `None` 谓词的语义与 [`VectorIndex::search_filtered`] 一致
    /// ——**不过滤、含已软删除条目**（`None` 的契约不能因走新路径而漂移）。
    #[test]
    fn 精确路径None谓词含幽灵候选() {
        use crate::bitmap::Bitmap;
        use crate::predicate::AliveOnly;

        let mut idx = BruteForceIndex::new();
        for id in 0..4u32 {
            idx.add(id, NormalizedVector::new(vec![id as f32, 1.0]))
                .unwrap();
        }
        let mut alive = Bitmap::new();
        alive.set(0);
        alive.set(2);
        let predicate = AliveOnly::new(&alive);
        let q = NormalizedVector::new(vec![1.0, 0.0]);

        let none_ids: Vec<ChunkId> = idx
            .search_exact_filtered(&q, 10, None)
            .unwrap()
            .iter()
            .map(|(i, _)| *i)
            .collect();
        assert_eq!(none_ids.len(), 4, "None 应不过滤，返回全部 4 条");
        assert!(none_ids.contains(&1) && none_ids.contains(&3));

        let alive_ids: Vec<ChunkId> = idx
            .search_exact_filtered(&q, 10, Some(&predicate))
            .unwrap()
            .iter()
            .map(|(i, _)| *i)
            .collect();
        assert_eq!(alive_ids, vec![2, 0], "AliveOnly 必须挡掉已软删除的 chunk");
    }

    /// **S5-T6（brute 侧）**：同距离按 `chunk_id` 升序，且**不依赖插入序**；
    /// 连续 **100** 次结果一致（NFR-06 确定性；与设计 I5 / S5-T6 的次数对齐）。
    #[test]
    fn 精确路径同距离按chunk_id升序且确定() {
        let mut idx = BruteForceIndex::new();
        // 插入顺序与 id 顺序刻意相反
        idx.add(9, NormalizedVector::new(vec![0.0, 1.0])).unwrap(); // d²=2
        idx.add(3, NormalizedVector::new(vec![0.0, -1.0])).unwrap(); // d²=2
        idx.add(7, NormalizedVector::new(vec![1.0, 0.0])).unwrap(); // d²=0
        let q = NormalizedVector::new(vec![1.0, 0.0]);

        let expect: Vec<ChunkId> = vec![7, 3, 9];
        for round in 0..100 {
            let ids: Vec<ChunkId> = idx
                .search_exact_filtered(&q, 10, None)
                .unwrap()
                .iter()
                .map(|(i, _)| *i)
                .collect();
            assert_eq!(ids, expect, "第 {round} 次结果与预期不一致");
        }
    }

    /// **S5-T1（brute 侧）**：Brute 恒精确 ⇒ `prefers_exact` 恒 `true`。
    #[test]
    fn brute恒走精确路径() {
        let idx = BruteForceIndex::new();
        let even = EvenChunks { count: 0 };
        assert!(idx.prefers_exact(&even), "Brute 的结果本来就精确");
    }
}
