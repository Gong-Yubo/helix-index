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
//! ## 嵌入宿主约束（V2 Step 3 / R19，文档级防线）
//!
//! **不得以 `panic = "abort"` 编译嵌入 helix-core 的宿主二进制。**
//! `vector::persist` 的 `dump_graph_caught` 依赖 `catch_unwind` 把 `hnsw_rs`
//! 写路径的 `panic_any`（R19 TOCTOU 残余）降级为 `Err`；abort 构建下该防线
//! **静默失效**（图 dump panic 直接杀进程）。库自身的 profile 对宿主构建不生效，
//! 此约束**无编译期检测手段**，只能文档级声明（本节 + 架构文档 §14.1 R19 残余表；
//! 本 workspace 的 CLI / 测试构建由根 Cargo.toml 注释钉死）。
//!
//! ## 阶段进度
//!
//! **V1 已完成**（`P0~P6`，2026-09-04；`docs/devel/plan.md` 已冻结为 V1 计划）；
//! **V2 已交付**（2026-10-16）—— `V2.0`（`Step 1~7`：质量与性能夯实 + 精排）与
//! `V2.1`（`Step 8~10`：读写并发 + 相关性深化 + 场景机制）**全部合并**，当前版本 `0.3.0`。
//!
//! 评测结论见 `docs/devel/eval-report.md`；
//! 任务清单与进度见 `docs/devel/plan-v2.md`（V2）与 `docs/devel/plan.md`（V1，已冻结）。

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
pub mod post;
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
