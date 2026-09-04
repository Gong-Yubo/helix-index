//! 编排：检索器 / 请求解析 / 可解释性输出。
//!
//! # 边界（架构文档 4.2）
//!
//! 本模块**不自己实现算法**，只调用 `retriever` / `fusion` / `rerank`。
//!
//! # 结构（P6 / I-01）
//!
//! 编排逻辑的唯一实现在 [`searcher::search_parts`]（自由函数）。
//! [`searcher::QueryExecutor`]（旧名 `Searcher`）与门面层 owned `Searcher`（I-05）
//! 都是它的薄壳。

pub mod explain;
pub mod filter;
pub mod metrics;
pub mod response;
pub mod searcher;

pub use response::{EmptyReason, Explain, Hit, SearchResponse};
pub use searcher::{QueryExecutor, SearchMode, SearchParts};
