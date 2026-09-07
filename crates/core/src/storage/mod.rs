//! 快照持久化：bincode 编解码、版本与 CRC 校验。
//!
//! # 硬约束（架构文档 4.2）
//!
//! **不得静默读错**——magic / 版本不匹配或 CRC 失败必须报错，
//! 绝不带着可疑数据继续跑。
//!
//! # 快照策略（p4-design.md D1，V2 Step 2 起补充）
//!
//! 快照存**原始数据**（含原始向量）；加载时由调用方重建索引结构。
//! HNSW 图**不进快照正文**（`FORMAT_VERSION` 保持 2），而是以
//! **sidecar 派生缓存**的形式落盘（`graph` 模块 / ADR-A 方案 C）：
//! 图可随时丢弃，丢弃后降级重建，功能不丢。

mod codec;
mod graph;
mod snapshot;

pub use codec::{FORMAT_VERSION, MAGIC};
pub use graph::{
    file_crc32_len, graph_basename, graph_paths, read_manifest, remove_sidecars, require_file_name,
    write_manifest_atomic, GraphManifest, GraphPaths, DIST_ID, MANIFEST_VERSION,
    PLATFORM_FINGERPRINT, PLATFORM_LE64, PLATFORM_OTHER,
};
pub use snapshot::{load, load_with_crc, save, save_with_crc, ConfigFingerprint};
