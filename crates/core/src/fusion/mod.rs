//! 多路结果融合：RRF（默认）与加权归一（备选）。
//!
//! # 边界（架构文档 4.3 硬约束）
//!
//! 本模块**不回捞正文**——融合只操作 `(chunk_id, score)`，正文回捞统一在
//! 融合与精排之后由 `query` 层对 Top-K 做一次。一旦 fusion 开始回捞正文，
//! 换融合策略就再也不能独立测试。

mod rrf;
mod weighted;

pub use rrf::RrfFusion;
pub use weighted::WeightedFusion;

use crate::types::{ChunkId, Score};

/// 单路结果：已经按分数排好序的 `(chunk_id, score)`。
pub type LaneResults = Vec<(ChunkId, Score)>;

/// 融合策略抽象。
pub trait FusionStrategy: Send + Sync {
    /// 策略名（用于 explain / 日志）。
    fn name(&self) -> &'static str;

    /// 融合多路结果，返回按 fused_score 降序的 `(chunk_id, fused_score)`。
    ///
    /// 约定：结果必须确定性排序（fused_score 降序，同分按 chunk_id 升序）。
    fn fuse(&self, lanes: &[LaneResults], k: usize) -> Vec<(ChunkId, Score)>;
}
