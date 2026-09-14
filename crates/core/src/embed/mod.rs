//! 文本向量化：`Embedder` trait + 本地 fastembed / 远程 HTTP 两个实现。
//!
//! # 边界（架构文档 4.2）
//!
//! 本模块**不知道索引的存在**。`embed_query` 与 `embed_documents` **必须分开**——
//! BGE 查询侧需要 instruction 前缀，入库侧不能加（风险 R2）。

mod cached;
#[cfg(feature = "local-embed")]
mod local;
mod remote;

pub use cached::CachedEmbedder;
#[cfg(feature = "local-embed")]
pub use local::LocalEmbedder;
// 只服务 `rerank::local`（设计 §4.4.1 的「复用同一缓存根」）⇒ 与它**同 gate**，
// 否则默认构建下这是一条无人使用的再导出。
#[cfg(feature = "local-rerank")]
pub(crate) use local::default_cache_dir;
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

    /// 模型身份标识（配置指纹用，p6-design 8.2）。
    ///
    /// 默认 `"custom"`；`LocalEmbedder` 显式返回 `"bge-small-zh-v1.5"`。
    /// 快照 load 时用它校验"当前装配的模型 == 建库时模型"（R2 / 维度一致性）。
    fn id(&self) -> &'static str {
        "custom"
    }
}
