//! 错误类型。
//!
//! 设计原则：**配置错误要在最早时刻暴露**，而不是让错误数据静默进入索引。
//! `NoEmbedder` 与 `DimensionMismatch` 就是为此存在的——前者表现为"跑不起来"，
//! 后者若不拦截则表现为"检索效果莫名其妙地差"，排查成本极高。

use thiserror::Error;

/// index-core 的统一错误类型
#[derive(Debug, Error)]
pub enum Error {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("序列化错误: {0}")]
    Codec(#[from] bincode::error::EncodeError),

    #[error("反序列化错误: {0}")]
    Decode(#[from] bincode::error::DecodeError),

    #[error("快照版本不兼容: 文件版本 {found}, 支持版本 {expected}")]
    SnapshotVersionMismatch { found: u32, expected: u32 },

    #[error("快照校验失败，文件可能损坏")]
    SnapshotCorrupted,

    #[error("向量维度不匹配: 期望 {expected}, 实际 {found}")]
    DimensionMismatch { expected: usize, found: usize },

    #[error("嵌入模型错误: {0}")]
    Embedding(String),

    #[error("未启用任何 Embedder 实现，请开启 local-embed 或 remote-embed feature")]
    NoEmbedder,

    #[error("该向量索引不支持增量插入（HNSW 需一次性构建）")]
    ImmutableIndex,

    #[error("分词错误: {0}")]
    Analyze(String),

    #[error("分片不存在: {0}")]
    ChunkNotFound(u32),
}

/// 统一结果类型
pub type Result<T> = std::result::Result<T, Error>;
