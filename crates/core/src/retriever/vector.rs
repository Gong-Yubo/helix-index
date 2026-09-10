//! 向量单路召回。
//!
//! 流程：`embed_query`（**加前缀**）→ 向量索引检索 → 距离转相似度 → Top-K。
//!
//! # 边界（架构文档 4.3 硬约束）
//!
//! 本文件**不得** import `super::bm25`，反之亦然——两条 lane 互不引用。
//!
//! # V2 Step 5：向量路的两条形态
//!
//! `Retriever::search_filtered` 仍是 **ANN 路径**（语义一字未改）；
//! 新增的 inherent [`VectorRetriever::search_exact_filtered`] 是**精确路径**，
//! [`VectorRetriever::plan`] 回答"该谓词下走哪条"。
//! **分派与记账归编排层**（`query::searcher`）——它才是"这次检索有没有向量路"
//! 的信息持有者，也是 `Metrics::vector_route` 的写入者。

use std::cmp::Ordering;

use crate::embed::Embedder;
use crate::error::Result;
use crate::predicate::CandidateFilter;
use crate::types::ChunkId;
use crate::vector::{NormalizedVector, VectorIndex, VectorRoute};

use super::{Retriever, Scored};

/// 向量检索器。持有 embedder 与向量索引的引用，不拥有它们。
pub struct VectorRetriever<'a> {
    embedder: &'a dyn Embedder,
    index: &'a dyn VectorIndex,
}

impl<'a> VectorRetriever<'a> {
    /// 构造向量检索器：embedder 做查询侧向量化，index 做近邻检索。
    pub fn new(embedder: &'a dyn Embedder, index: &'a dyn VectorIndex) -> Self {
        Self { embedder, index }
    }

    /// 该谓词下向量路将走哪条（供编排层**分派**与**记账**，D-S5-01 / D-S5-07）。
    ///
    /// **策略归后端**：`prefers_exact` 由 `VectorIndex` 实现自己回答（它最清楚
    /// 这个谓词下 ANN 会不会退化成整图遍历），编排层不复制阈值规则。
    /// `O(1)`，可在并行 `rayon::join` 之前先算好。
    ///
    /// `filter = None`（无用户过滤）恒为 [`VectorRoute::Ann`]——热路径不走精确扫描
    /// （D-S5-02）。`VectorRoute::None`（未走向量路）**不由本函数产生**：那是
    /// 编排层在 `SearchMode::Bm25` 下写入的取值。
    pub fn plan(&self, filter: Option<&dyn CandidateFilter>) -> VectorRoute {
        match filter {
            Some(f) if self.index.prefers_exact(f) => VectorRoute::Exact,
            _ => VectorRoute::Ann,
        }
    }

    /// **精确路径**：与 `Retriever::search_filtered` 同转换（`score = 1 − d²/2`）、
    /// 同排序，只有底层调用的方法不同（`search_exact_filtered` vs `search_filtered`）。
    ///
    /// 转换与排序由私有的 `VectorRetriever::to_scored` **单点实现**，两条路径共用——
    /// 否则"两条路径的输出口径一致"会变成两份代码的巧合。
    ///
    /// ⚠️ 代价不保证优于 ANN（精确扫描是 `O(N)`）⇒ **调用方必须先问 [`Self::plan`]**。
    pub fn search_exact_filtered(
        &self,
        query: &str,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<Scored>> {
        // 早退条件与 ANN 路径逐条对齐（空查询 / 空索引 / k=0），
        // 否则两条路径对同一输入会给出不同的空语义
        if k == 0 || query.trim().is_empty() || self.index.is_empty() {
            return Ok(Vec::new());
        }
        let q = NormalizedVector::new(self.embedder.embed_query(query)?);
        let raw = self.index.search_exact_filtered(&q, k, filter)?;
        Ok(Self::to_scored(raw))
    }

    /// `(chunk_id, 平方欧氏距离)` → `Scored`（相似度降序、同分按 `chunk_id` 升序）。
    ///
    /// 两条路径的**唯一**输出转换点（`score = 1 - d²/2 = cos`，单位向量）。
    fn to_scored(raw: Vec<(ChunkId, f32)>) -> Vec<Scored> {
        let mut out: Vec<Scored> = raw
            .into_iter()
            .map(|(chunk_id, d)| Scored {
                chunk_id,
                score: 1.0 - d / 2.0,
            })
            .collect();

        // 相似度降序；同分按 chunk_id 升序（确定性 NFR-06）
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        out
    }
}

impl Retriever for VectorRetriever<'_> {
    /// 向量召回（**ANN 路径**）：谓词**直接下推**到 `VectorIndex`（而非召回后再过滤）。
    ///
    /// 下推是必须的：已软删除的 chunk 其向量仍留在 HNSW 图里（hnsw_rs 无 remove API），
    /// 后置过滤会浪费 Top-K 名额、且低选择度下几乎召不回东西。
    ///
    /// ⚠️ `filter = None` 语义同 [`Retriever::search_filtered`]：不过滤，**含已软删除条目**。
    ///
    /// ⚠️ 本方法**不自己分派**精确路径：低选择度下的兜底由编排层按 [`VectorRetriever::plan`]
    /// 决定并记账（`Metrics::vector_route`）。在这里偷偷分派会让路径变成不可观测的副作用。
    fn search_filtered(
        &self,
        query: &str,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<Scored>> {
        if k == 0 || query.trim().is_empty() || self.index.is_empty() {
            return Ok(Vec::new());
        }

        let q = NormalizedVector::new(self.embedder.embed_query(query)?);
        let raw = self.index.search_filtered(&q, k, filter)?;
        Ok(Self::to_scored(raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::BruteForceIndex;

    /// 测试用假 Embedder：把文本映射到固定向量（不依赖模型）。
    struct FakeEmbedder;
    impl Embedder for FakeEmbedder {
        fn dim(&self) -> usize {
            2
        }
        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| vec![t.len() as f32, 1.0]).collect())
        }
        fn embed_query(&self, _text: &str) -> Result<Vec<f32>> {
            // 查询向量恒为 (1,0) → 最近的是 (1,1) 归一化后的向量
            Ok(vec![1.0, 0.0])
        }
    }

    #[test]
    fn topk按相似度降序() {
        let mut idx = BruteForceIndex::new();
        // 三个向量：0≈(1,0) 1≈(0,1) 2≈(1,1)/√2
        idx.add(0, NormalizedVector::new(vec![1.0, 0.0])).unwrap();
        idx.add(1, NormalizedVector::new(vec![0.0, 1.0])).unwrap();
        idx.add(2, NormalizedVector::new(vec![1.0, 1.0])).unwrap();

        let e = FakeEmbedder;
        let r = VectorRetriever::new(&e, &idx);

        // 查询向量 (1,0)：最近 0（cos=1），其次 2（cos≈0.707），最远 1（cos=0）
        let got = r.search("任意", 10).unwrap();
        let ids: Vec<u32> = got.iter().map(|s| s.chunk_id).collect();
        assert_eq!(ids, vec![0, 2, 1]);
        // 分数 = cos，递减
        assert!(got[0].score > got[1].score && got[1].score > got[2].score);
        assert!((got[0].score - 1.0).abs() < 1e-5);
        assert!(got[1].score < 0.75);
    }

    #[test]
    fn 空查询空索引() {
        let e = FakeEmbedder;
        let idx = BruteForceIndex::new();
        let r = VectorRetriever::new(&e, &idx);
        assert!(r.search("", 10).unwrap().is_empty());
        assert!(r.search("x", 10).unwrap().is_empty());
    }
}
