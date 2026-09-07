//! 错误类型。
//!
//! 设计原则：**配置错误要在最早时刻暴露**，而不是让错误数据静默进入索引。
//! `NoEmbedder` 与 `DimensionMismatch` 就是为此存在的——前者表现为"跑不起来"，
//! 后者若不拦截则表现为"检索效果莫名其妙地差"，排查成本极高。

use thiserror::Error;

/// index-core 的统一错误类型
#[derive(Debug, Error)]
pub enum Error {
    /// 快照文件读写失败
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    /// 快照编码失败
    #[error("序列化错误: {0}")]
    Codec(#[from] bincode::error::EncodeError),

    /// 快照解码失败（版本不符或内容损坏会先被前两级拦下）
    #[error("反序列化错误: {0}")]
    Decode(#[from] bincode::error::DecodeError),

    /// 快照由更高/更低版本的内核写出，无法安全读取
    #[error("快照版本不兼容: 文件版本 {found}, 支持版本 {expected}")]
    SnapshotVersionMismatch {
        /// 文件中的版本号
        found: u32,
        /// 当前内核支持的版本号
        expected: u32,
    },

    /// 快照 CRC 校验失败，文件已损坏
    #[error("快照校验失败，文件可能损坏")]
    SnapshotCorrupted,

    /// 入库向量与索引既有向量维度不一致
    #[error("向量维度不匹配: 期望 {expected}, 实际 {found}")]
    DimensionMismatch {
        /// 索引期望的维度
        expected: usize,
        /// 实际传入的维度
        found: usize,
    },

    /// 嵌入模型初始化或推理失败
    #[error("嵌入模型错误: {0}")]
    Embedding(String),

    /// 未启用任何 Embedder 实现，请开启 local-embed 或 remote-embed feature
    #[error("未启用任何 Embedder 实现，请开启 local-embed 或 remote-embed feature")]
    NoEmbedder,

    /// 分词器内部错误
    #[error("分词错误: {0}")]
    Analyze(String),

    /// 引用的分片 ID 不存在（已删除或越界）
    #[error("分片不存在: {0}")]
    ChunkNotFound(u32),

    /// 调用方传入的参数不合法（如 judgments 格式错误、权重数量不对）
    #[error("非法输入: {0}")]
    InvalidInput(String),

    /// 快照加载时装配与建库时不一致（p6-design 8.2，修 B1/B2）
    #[error("配置不匹配: 快照期望 {expected}，当前装配 {actual}")]
    ConfigMismatch {
        /// 快照记录的配置指纹（建库时）
        expected: String,
        /// 当前装配的配置指纹
        actual: String,
    },

    /// 向量图 sidecar 读写失败（hnsw_rs 落盘/加载、manifest 编解码）
    #[error("向量图读写失败: {0}")]
    VectorGraph(String),

    /// 图 sidecar 与快照不匹配（缺失/损坏/版本不符）。图是派生缓存，
    /// 默认降级重建（警告输出）；仅 strict 模式（D-S2-04）作为 Err 返回。
    #[error("向量图不可用（{reason}），已降级为加载后重建")]
    GraphStale {
        /// 降级原因（可观测，NFR-07：降级不能静默）
        reason: String,
    },
}

/// 统一结果类型
pub type Result<T> = std::result::Result<T, Error>;
