//! 文档与分片模型。
//!
//! `Document` 是摄入的原始单位，`Chunk` 是检索的最小单位。
//! `Chunk.char_start` / `char_end` 是**原文偏移**，用于精确引用（FR-12），
//! 分块实现必须保证 `&doc.text[start..end] == chunk.text`。

use serde::{Deserialize, Serialize};

use crate::types::{ChunkId, DocId};

/// 摄入的文档
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// 内部 ID，由索引分配
    pub doc_id: DocId,
    /// 出处：文件路径 / URL / 标题 —— **溯源用**（FR-12）
    pub source: String,
    /// 业务自定义元数据 —— 过滤用（FR-14）
    pub metadata: serde_json::Value,
    /// 内容哈希 —— 幂等 upsert 用（FR-15）
    pub content_hash: u64,
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
