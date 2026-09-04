//! 文档与分片模型。
//!
//! `Document` 是**摄入的输入 DTO**（用户给什么），`DocRecord` 是**存储记录**
//! （索引存什么），`Chunk` 是检索的最小单位。
//! `Chunk.char_start` / `char_end` 是**原文偏移**，用于精确引用（FR-12），
//! 分块实现必须保证 `&doc.text[start..end] == chunk.text`。

use serde::{Deserialize, Serialize};

use crate::types::{ChunkId, DocId};

/// 摄入文档的输入 DTO（P6 / I-02）。
///
/// - `doc_id` 由索引分配，不出现在输入里
/// - `content_hash` 由库内计算（`xxh64(dedup_key.unwrap_or(text))`，FR-15），
///   调用方不再手算
/// - `dedup_key`：自定义幂等键。`None` 时以 `text` 哈希为准；
///   同一逻辑文档应始终使用同一 `dedup_key`，否则视为不同文档（p6-design 6.1）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// 原文正文
    pub text: String,
    /// 出处：文件路径 / URL / 标题 —— **溯源用**（FR-12）
    pub source: String,
    /// 业务自定义元数据 —— 过滤用（FR-14）
    pub metadata: serde_json::Value,
    /// 自定义幂等键（可选）；`None` 时用 `content_hash(text)`
    pub dedup_key: Option<String>,
}

impl Document {
    /// 从纯文本构造（source 留空，metadata 空，无 dedup_key）。
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            source: String::new(),
            metadata: serde_json::json!({}),
            dedup_key: None,
        }
    }

    /// 设置出处（溯源用，FR-12）。
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// 设置业务元数据（过滤用，FR-14）。
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    /// 设置自定义幂等键（FR-15；语义见类型文档）。
    pub fn with_dedup_key(mut self, key: impl Into<String>) -> Self {
        self.dedup_key = Some(key.into());
        self
    }
}

impl From<&str> for Document {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for Document {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

/// 存储记录（原 `Document` 的存储语义，P6 / I-02 拆出）。
///
/// 索引内部与快照承载的文档记录：`doc_id` 由索引分配，
/// `content_hash` 由库内计算（入库时从 `Document` 投影而来）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocRecord {
    /// 内部 ID，由索引分配
    pub doc_id: DocId,
    /// 出处：文件路径 / URL / 标题 —— **溯源用**（FR-12）
    pub source: String,
    /// 业务自定义元数据 —— 过滤用（FR-14）
    pub metadata: serde_json::Value,
    /// 内容哈希 —— 幂等 upsert 用（FR-15）
    pub content_hash: u64,
}

/// 计算文档内容的哈希（幂等 upsert 用，FR-15）。
///
/// 用 xxh64：64-bit 稳定哈希，无随机种子，同内容跨进程结果一致。
/// 相比 crc32（32-bit）碰撞概率低一个量级。
pub fn content_hash(text: &str) -> u64 {
    xxhash_rust::xxh64::xxh64(text.as_bytes(), 0)
}

/// 检索的最小单位
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    /// 分片 ID
    pub chunk_id: ChunkId,
    /// 所属文档
    pub doc_id: DocId,
    /// 在文档内的序号，用于相邻分片合并（FR-24，v2）
    pub ordinal: u32,
    /// 分片正文
    pub text: String,
    /// 原文起始偏移（含）
    pub char_start: usize,
    /// 原文结束偏移（不含）
    pub char_end: usize,
}
