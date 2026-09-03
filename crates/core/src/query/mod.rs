//! 编排：Searcher / 请求解析 / 可解释性输出。
//!
//! # 边界（架构文档 4.2）
//!
//! 本模块**不自己实现算法**，只调用 `retriever` / `fusion` / `rerank`。

pub mod explain;
pub mod filter;
pub mod metrics;
pub mod response;
pub mod searcher;

pub use response::{EmptyReason, Explain, Hit, SearchResponse};
pub use searcher::{SearchMode, Searcher};
