//! # index-core
//!
//! 面向 Agent 场景的**通用检索引擎内核**。
//!
//! ## 模块边界（硬性约束，见架构文档 4.2）
//!
//! | 模块          | 职责            | 禁止                  |
//! | ----------- | ------------- | ------------------- |
//! | `analyze`   | 文本 → Token 序列 | 不知道索引的存在            |
//! | `index`     | 倒排 / 正排 / 统计量 | 不知道融合与排序            |
//! | `vector`    | 向量存储与近邻检索     | 不知道文本，只认 `Vec<f32>` |
//! | `embed`     | 文本 → 向量       | 不知道索引的存在            |
//! | `retriever` | 单路召回          | 不与其他 lane 交互        |
//! | `fusion`    | 多路合并          | 不回捞正文               |
//! | `query`     | 编排以上全部        | 不自己实现算法             |
//!
//! 违反边界**不会导致编译失败**，只能靠代码评审守住。一旦打破，trait 抽象就失去意义：
//! 例如 `fusion` 一旦开始回捞正文，换融合策略就再也不能独立测试。
//!
//! ## 阶段进度
//!
//! P0（当前）：工程骨架，无业务逻辑。

pub mod analyze;
pub mod bench;
pub mod chunk;
pub mod document;
pub mod embed;
pub mod error;
pub mod fusion;
pub mod index;
pub mod query;
pub mod rerank;
pub mod retriever;
pub mod schema;
pub mod storage;
pub mod types;
pub mod vector;

/// 常用类型与错误的统一导入路径
pub mod prelude {
    pub use crate::document::{Chunk, Document};
    pub use crate::error::{Error, Result};
    pub use crate::schema::Filter;
    pub use crate::types::{ChunkId, DocId, Score, TermId};
}
