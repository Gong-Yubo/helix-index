//! 向量存储与近邻检索：`VectorIndex` trait + HNSW / 暴力实现。
//!
//! # 边界（架构文档 4.2）
//!
//! 本模块**不认识文本，只认 `Vec<f32>`**；入库前必须 L2 归一化。
//! `NormalizedVector` 的构造函数已强制归一化，因此这里不会再出现未归一化向量。

mod brute;
mod hnsw_rs_index;
mod point;

pub use brute::BruteForceIndex;
pub use hnsw_rs_index::HnswRsIndex;
pub use point::NormalizedVector;

use crate::error::Result;
use crate::types::ChunkId;

/// 向量索引抽象。
pub trait VectorIndex: Send + Sync {
    /// 增量插入一条向量。当前实现（`HnswRsIndex` / `BruteForceIndex`）均支持。
    fn add(&mut self, id: ChunkId, vec: NormalizedVector) -> Result<()>;

    /// 检索最近的 k 条，返回 `(chunk_id, distance)`，按距离**升序**。
    /// `distance` 是平方欧氏距离，越小越近；转相似度由调用方负责。
    fn search(&self, query: &NormalizedVector, k: usize) -> Result<Vec<(ChunkId, f32)>>;

    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
