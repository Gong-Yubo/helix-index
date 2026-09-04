//! # helix-core
//!
//! **HelixIndex** —— 面向 Agent 场景的**通用检索引擎内核**。
//!
//! crate 名 `helix-core`，CLI 二进制为 `helix`。
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
//! **P0~P5 全部完成**（2026-09-03），当前版本 `0.1.0`。
//! 下一阶段 P6（v2：Rerank / MMR / 自研索引）不在当前版本范围。
//!
//! 评测结论见 `docs/devel/eval-report.md`；
//! 任务清单与进度见 `docs/devel/plan.md`。

// rustdoc 守门（V1-02）：公有 API 必须有文档注释。
// 用 `deny` 而非 `warn`——`warn` 不打断构建，不构成守门。
// 注意：只加在 **库** crate；CLI（crates/cli）是二进制 crate、无 pub API，
// 加它是 no-op（评审意见 v1.1 第 4 条）。
#![deny(missing_docs)]

pub mod analyze;
pub mod bench;
pub mod bitmap;
pub mod chunk;
pub mod document;
pub mod embed;
pub mod error;
pub mod fusion;
pub mod index;
pub mod predicate;
pub mod query;
pub mod rerank;
pub mod retriever;
pub mod schema;
pub mod search;
pub mod storage;
pub mod types;
pub mod vector;

/// 常用类型与错误的统一导入路径
pub mod prelude {
    pub use crate::document::{Chunk, DocRecord, Document};
    pub use crate::error::{Error, Result};
    pub use crate::query::{EmptyReason, Hit, SearchMode, SearchResponse};
    pub use crate::schema::Filter;
    pub use crate::search::{SearchIndex, SearchIndexBuilder, Searcher, VectorBackend};
    pub use crate::types::{ChunkId, DocId, Score, TermId};
}
