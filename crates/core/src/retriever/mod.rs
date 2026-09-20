//! 单路召回：BM25 路 与 向量路。
//!
//! # 边界（架构文档 4.3 硬约束）
//!
//! 两条 lane 之间**禁止互相引用**——BM25 与向量的实现文件不得 import 对方。
//! 每路只依赖 `index` / `analyze`（BM25 路）或 `vector` / `embed`（向量路）。

mod bm25;
mod vector;

pub use bm25::{Bm25Params, Bm25Retriever, SegmentRef, SegmentSet, SegmentedBm25Retriever};
pub use vector::VectorRetriever;

use crate::error::Result;
use crate::predicate::CandidateFilter;
use crate::types::{ChunkId, Score};

/// 一条召回结果
#[derive(Debug, Clone, Copy, PartialEq)]
/// 单路召回的一条结果（不含正文，回捞由上层负责）。
pub struct Scored {
    /// 命中的分片 ID
    pub chunk_id: ChunkId,
    /// 该路的原始分数（BM25 无上界 / 向量为余弦相似度）
    pub score: Score,
}

/// 单路召回抽象。
///
/// 输入原始查询文本，输出按分数降序的结果（不含正文，回捞由上层负责）。
pub trait Retriever: Send + Sync {
    /// 单路召回 Top-K 且**只保留通过 `filter` 的候选**，按分数降序。**不得与其他 lane 交互**。
    ///
    /// # ⚠️ `filter = None` 的语义
    ///
    /// **`None` 表示"不过滤"，包含已软删除的条目**。唯一正确的调用方式是经过编排层
    /// （由它注入存活谓词）；直接调 [`Self::search`] 会绕过存活过滤——
    /// 这是逃生舱路径的已知契约，不是 bug（详见 `predicate` 模块文档）。
    fn search_filtered(
        &self,
        query: &str,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<Scored>>;

    /// 不带过滤的召回（默认转发到 [`Self::search_filtered`]，`None` 语义见其上）。
    fn search(&self, query: &str, k: usize) -> Result<Vec<Scored>> {
        self.search_filtered(query, k, None)
    }
}
