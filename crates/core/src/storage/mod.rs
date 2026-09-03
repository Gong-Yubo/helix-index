//! 快照持久化：bincode 编解码、版本与 CRC 校验。
//!
//! # 硬约束（架构文档 4.2）
//!
//! **不得静默读错**——magic / 版本不匹配或 CRC 失败必须报错，
//! 绝不带着可疑数据继续跑。
//!
//! # 快照策略（p4-design.md D1）
//!
//! 快照存**原始数据**（含原始向量），加载时由调用方重建索引结构
//! （含向量索引），不序列化 HNSW 图。

mod codec;
mod snapshot;

pub use codec::{FORMAT_VERSION, MAGIC};
pub use snapshot::{load, save};
