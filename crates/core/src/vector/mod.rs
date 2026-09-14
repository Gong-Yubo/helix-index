//! 向量存储与近邻检索：`VectorIndex` trait + HNSW / 暴力实现。
//!
//! # 边界（架构文档 4.2）
//!
//! 本模块**不认识文本，只认 `Vec<f32>`**；入库前必须 L2 归一化。
//! `NormalizedVector` 的构造函数已强制归一化，因此这里不会再出现未归一化向量。
//!
//! # 图持久化（V2 Step 2 / ADR-A）
//!
//! HNSW 图以 sidecar 派生缓存落盘（`persist` 模块），**不进快照正文**。
//! 「哪些后端可持久化」由 [`VectorIndex::as_graph_persist`] 在类型层表达：
//! 默认 `None`（Brute），`HnswRsIndex` 覆盖为 `Some(self)`。

mod brute;
mod hnsw_rs_index;
mod persist;
mod point;

pub use brute::BruteForceIndex;
pub use hnsw_rs_index::{HnswRsIndex, BRUTE_FALLBACK_MAX_ALLOWED};
pub use persist::{load_graph_checked, validate_graph_description, GraphStats, VectorGraphPersist};
pub use point::NormalizedVector;
// R19 panic 边界（V2 Step 3 / D-S3-04）：门面层写图链路必经；pub(crate) 不进公开面
pub(crate) use persist::dump_graph_caught;

use crate::error::Result;
use crate::predicate::CandidateFilter;
use crate::types::ChunkId;

/// 向量路本次实际走的路径（V2 Step 5 / D-S5-07）。
///
/// 由后端经 [`VectorIndex::prefers_exact`] 给出**策略**，编排层据此**分派**并把
/// 结果记进 [`crate::query::Metrics::vector_route`]——「兜底到底生效了没有」由此
/// 从延迟反推变成直接可读（V2.1 的 prefilter 判据必须连看它，见 `vector_shortfall`
/// 的语义变更）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VectorRoute {
    /// 未走向量路（`SearchMode::Bm25`，以及索引为空 / 过滤排空等早退路径）。
    ///
    /// ⚠️ 本变体**不是**分派函数的返回值——"这次检索有没有向量路"是编排层的信息，
    /// 不该由后端编码；后端的策略只回答 `Ann` / `Exact`。
    #[default]
    None,
    /// 走了 ANN（近似；低选择度下可能有召回缺口）。
    Ann,
    /// 走了精确扫描（保证 `min(k, allowed)` 条、无召回缺口）。
    Exact,
}

/// 向量索引抽象。
pub trait VectorIndex: Send + Sync {
    /// 增量插入一条向量。当前实现（`HnswRsIndex` / `BruteForceIndex`）均支持。
    fn add(&mut self, id: ChunkId, vec: NormalizedVector) -> Result<()>;

    /// 批量插入（D-S2-05 / S2-11）。默认实现为逐条 `add`（串行）；
    /// `HnswRsIndex` 可覆盖为 `parallel_insert_slice`（并行建图开关，
    /// 由门面层按阈值决定是否走批量路径）。
    ///
    /// ⚠️ **默认值已于 T7-21 / D-J11 翻转为「开」**（实测数据已出，见 issue #24；
    /// 实测 12K 真实语料 11.676s → 2.182s = **5.35×**，oracle 重合率无差异）。
    /// 代价是并行插入顺序不确定 ⇒ 拓扑不可复现（C8），但 **NFR-06 的口径是
    /// 「同快照两次**加载**」、不约束建库过程** ⇒ 该代价被接受。
    /// ⚠️ **生效范围有限**：只对「单次全量 `add_batch`」（`rebuild_vector_index`）有效，
    /// 增量 `flush`（默认 `batch_size = 64` < 阈值 1000）仍串行 —— 详见
    /// `SearchIndexBuilder::parallel_build` 的文档。
    fn add_batch(&mut self, items: &[(ChunkId, NormalizedVector)]) -> Result<()> {
        for (id, v) in items {
            self.add(*id, v.clone())?;
        }
        Ok(())
    }

    /// 检索最近的 k 条**且通过 `filter` 的**候选，返回 `(chunk_id, distance)`，按距离**升序**。
    ///
    /// `distance` 是平方欧氏距离，越小越近；转相似度由上层负责。
    ///
    /// # 契约
    ///
    /// - `filter = Some(f)`：返回的**每一条都必须满足 `f.contains(id)`**。
    ///   允许返回少于 k 条（过滤后不足），**不允许**用未通过过滤的候选凑数。
    /// - `filter = None`：**不过滤，结果可能包含已软删除的 chunk**。
    ///   这是逃生舱路径的已知契约——`hnsw_rs` 无法从图中物理摘除向量，
    ///   存活过滤必须由编排层注入谓词完成（详见 `predicate` 模块文档与 T17）。
    fn search_filtered(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<(ChunkId, f32)>>;

    /// 不带过滤的检索（默认转发到 [`Self::search_filtered`]，`None` 语义见其上）。
    fn search(&self, query: &NormalizedVector, k: usize) -> Result<Vec<(ChunkId, f32)>> {
        self.search_filtered(query, k, None)
    }

    /// **精确**（无近似）的过滤检索：返回**全部**满足 `filter` 的最近 `min(k, 命中数)` 条。
    ///
    /// 返回值与 [`Self::search_filtered`] 同形（`(chunk_id, 平方欧氏距离)`，距离升序、
    /// 同距离按 `chunk_id` 升序），与它的**唯一语义差异是精确性**：
    ///
    /// - 返回条数恒为 `min(k, 命中的候选数)`，**不允许少返回**——这正是它在低选择度
    ///   场景的价值（ANN 会在那里静默少召回）。
    /// - `filter = None` 时不过滤（含已软删除条目），契约同 [`Self::search_filtered`]。
    ///
    /// # ⚠️ 这是"必选方法"而非带默认实现
    ///
    /// 默认实现只能给出"某种"行为，而后端**是否真精确**是它的实现事实。设为必选
    /// ⇒ 新增后端必须显式回答"我的精确路径是什么"（同 [`Self::as_graph_persist`]
    /// 用 `None` 把"Brute 无图"写成类型事实的手法）。
    ///
    /// # 代价
    ///
    /// 不保证优于 [`Self::search_filtered`]——实现可能是 `O(N)`（逐点谓词判定）。
    /// ⇒ **调用方必须先问 [`Self::prefers_exact`]**，不要无条件改用它。
    fn search_exact_filtered(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<(ChunkId, f32)>>;

    /// 本后端在**该谓词**下是否应走精确路径。
    ///
    /// **策略归后端、分派归编排层**：后端最清楚"这个谓词下我的 ANN 会不会退化成
    /// 整图遍历"，而编排层只需据此分派并记账（`Metrics.vector_route`）——阈值规则
    /// 只有一份实现，也不必把 `raw_vectors` 之类的内部状态穿过 `&dyn VectorIndex`。
    ///
    /// 默认 `false` ⇒ **不改变任何既有后端的行为**（零回归）。
    fn prefers_exact(&self, _filter: &dyn CandidateFilter) -> bool {
        false
    }

    /// 已入库向量条数（**含已软删除的**；与存活数无关）。
    fn len(&self) -> usize;

    /// 是否为空索引。
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 若该后端支持图持久化，返回其 [`VectorGraphPersist`] 视图；否则 `None`
    /// （P0-4：门面层从 `Box<dyn VectorIndex>` 触达持久化能力的**唯一**通道。
    /// 默认 `None` ⇒ 「Brute 无图」是类型事实。对象安全、零破坏。）
    fn as_graph_persist(&self) -> Option<&dyn VectorGraphPersist> {
        None
    }
}
