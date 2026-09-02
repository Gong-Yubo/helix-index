//! 文本向量化：`Embedder` trait + 本地 fastembed / 远程 HTTP 两个实现。
//!
//! # 边界（架构文档 4.2）
//!
//! 本模块**不知道索引的存在**。`embed_query` 与 `embed_documents` **必须分开**——
//! BGE 查询侧需要 instruction 前缀，入库侧不能加（风险 R2）。

#[cfg(feature = "local-embed")]
mod local;
mod remote;

#[cfg(feature = "local-embed")]
pub use local::LocalEmbedder;
#[cfg(feature = "remote-embed")]
pub use remote::RemoteEmbedder;

use crate::error::Result;

/// 文本向量化抽象。
///
/// 两个方法分开是刻意的：查询侧与入库侧在 BGE 上语义不同。
pub trait Embedder: Send + Sync {
    /// 向量维度
    fn dim(&self) -> usize;

    /// 入库侧：**不加** instruction 前缀。
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// 查询侧：**加** instruction 前缀（BGE 要求，风险 R2）。
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;

    /// 输出是否已 L2 归一化。本地 fastembed 为 `true`。
    fn is_normalized(&self) -> bool {
        false
    }
}
