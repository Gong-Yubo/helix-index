//! 门面层：`SearchIndex`（写端）+ `Searcher`（读端）。
//!
//! # 定位（架构文档第 10 章 / p6-design 5.1）
//!
//! 这是内核对外唯一的**默认装配层**。它只做装配与生命周期管理，**不实现任何算法**：
//! `add` 内部依次调用 `Chunker::chunk` → `Index::add` → `Embedder::embed_documents` →
//! `VectorIndex::add`，自己不碰 postings、BM25 公式或 HNSW 图。
//!
//! # 逃生舱
//!
//! 门面只是"默认装配"，不是"唯一路径"。需要逐 lane 自定义组装时，
//! 底层 [`crate::query::QueryExecutor`] 与六个 trait 永远可达（p6-design 7.3）。

mod config;
mod index;
mod searcher;
mod view;

pub use config::{required_local_embedder, GraphPersistMode, SearchIndexBuilder, VectorBackend};
pub use index::{
    AddOutcome, CompactionReport, GraphStatus, MergeReport, SearchIndex, SizeBytes, TombstoneStats,
    VectorMergeStrategy,
};
pub use searcher::{SearchRequest, Searcher};
