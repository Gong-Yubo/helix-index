//! 基础类型定义。
//!
//! 所有 ID 均为 `u32`（万级规模足够，且比 `u64` 省一半内存）。
//! 若未来规模突破 40 亿再统一改为 `u64` —— 因为它们是类型别名，改一处即可。

/// 文档 ID（内部自增，对外不暴露）
pub type DocId = u32;

/// 分片 ID
pub type ChunkId = u32;

/// 词项 ID（term dictionary 中的编号）
pub type TermId = u32;

/// 分数值（BM25 无上界；向量相似度为余弦值）
pub type Score = f32;
