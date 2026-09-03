//! 单路召回：BM25 路 与 向量路。
//!
//! # 边界（架构文档 4.3 硬约束）
//!
//! 两条 lane 之间**禁止互相引用**——BM25 与向量的实现文件不得 import 对方。
//! 每路只依赖 `index` / `analyze`（BM25 路）或 `vector` / `embed`（向量路）。

mod bm25;
mod vector;

pub use bm25::{Bm25Params, Bm25Retriever};
pub use vector::VectorRetriever;

use crate::error::Result;
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
    /// 单路召回 Top-K，按分数降序。**不得与其他 lane 交互**。
    fn search(&self, query: &str, k: usize) -> Result<Vec<Scored>>;
}
