//! 精排：`Reranker` trait。v1 只有 `NoOpReranker`，仅把调用链留出来（FR-18）。
//!
//! # 边界（架构文档 4.2）
//!
//! 第一版不接模型。`NoOpReranker` 原样返回，保证调用链完整但零副作用。

mod noop;

pub use noop::NoOpReranker;

use crate::error::Result;
use crate::query::response::Hit;

/// 精排抽象：在粗召回 + 融合之后，对少量候选重新排序。
pub trait Reranker: Send + Sync {
    fn rerank(&self, query: &str, hits: Vec<Hit>, top_n: usize) -> Result<Vec<Hit>>;
}
