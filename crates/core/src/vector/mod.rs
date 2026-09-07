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
pub use hnsw_rs_index::HnswRsIndex;
pub use persist::{load_graph_checked, validate_graph_description, GraphStats, VectorGraphPersist};
pub use point::NormalizedVector;

use crate::error::Result;
use crate::predicate::CandidateFilter;
use crate::types::ChunkId;

/// 向量索引抽象。
pub trait VectorIndex: Send + Sync {
    /// 增量插入一条向量。当前实现（`HnswRsIndex` / `BruteForceIndex`）均支持。
    fn add(&mut self, id: ChunkId, vec: NormalizedVector) -> Result<()>;

    /// 批量插入（D-S2-05 / S2-11）。默认实现为逐条 `add`（串行）；
    /// `HnswRsIndex` 可覆盖为 `parallel_insert_slice`（并行建图开关，
    /// 由门面层按阈值决定是否走批量路径——**默认串行**，保住与 P5 基线的
    /// 可比性：并行插入顺序不确定 ⇒ 拓扑不可复现，先出实测再定默认值）。
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
