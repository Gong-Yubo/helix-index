//! 加权归一融合（备选，FR-11）。
//!
//! 每路 score 除以该路 **Top-1 分数**（而非 min-max，见架构 7.4），
//! 单候选（或空）时退化为 1.0，避免除零。
//!
//! 说明：这仍然是"量纲可比"的近似——它假设各路分数都约在 [0,1] 上均匀分布，
//! 对 BM25（无上界）其实仍不稳。因此它是备选，默认仍用 RRF。

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::types::{ChunkId, Score};

use super::{FusionStrategy, LaneResults};

/// 加权归一融合。
#[derive(Debug, Clone, Default)]
pub struct WeightedFusion {
    weights: Vec<f32>,
}

impl WeightedFusion {
    /// 按给定路权重构造（权重与 lane 顺序一一对应）。
    pub fn new(weights: Vec<f32>) -> Self {
        Self { weights }
    }
}

impl FusionStrategy for WeightedFusion {
    fn name(&self) -> &'static str {
        "weighted"
    }

    fn fuse(&self, lanes: &[LaneResults], k: usize) -> Vec<(ChunkId, Score)> {
        let mut acc: HashMap<ChunkId, f32> = HashMap::new();

        for (lane_idx, lane) in lanes.iter().enumerate() {
            if lane.is_empty() {
                continue;
            }
            let w = self.weights.get(lane_idx).copied().unwrap_or(1.0);
            // Top-1 分数作归一化分母；单候选时 Top-1 即该分数，归一化后 = 1.0
            let top = lane[0].1;
            let denom = if top.abs() < f32::EPSILON { 1.0 } else { top };

            for (chunk_id, score) in lane {
                let normalized = score / denom;
                *acc.entry(*chunk_id).or_insert(0.0) += w * normalized;
            }
        }

        let mut out: Vec<(ChunkId, Score)> = acc.into_iter().collect();
        out.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        out.truncate(k);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 单候选不除零() {
        let lane: LaneResults = vec![(7, 42.0)]; // 只有一条，Top-1 就是它自己
        let fusion = WeightedFusion::default();
        let out = fusion.fuse(&[lane], 10);
        // 归一化后 = 1.0
        assert_eq!(out.len(), 1);
        assert!((out[0].1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn 除以top1归一化() {
        // 某路 Top-1 = 100，第二条 = 50 → 归一化后 1.0 与 0.5
        let lane: LaneResults = vec![(0, 100.0), (1, 50.0)];
        let fusion = WeightedFusion::default();
        let out = fusion.fuse(&[lane], 10);
        let score_of = |id| out.iter().find(|(i, _)| *i == id).map(|(_, s)| *s).unwrap();
        assert!((score_of(0) - 1.0).abs() < 1e-6);
        assert!((score_of(1) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn 零分top1退化为1() {
        let lane: LaneResults = vec![(0, 0.0), (1, 0.0)];
        let fusion = WeightedFusion::default();
        let out = fusion.fuse(&[lane], 10);
        // top = 0 → 分母退化为 1.0，score = 0/1 = 0
        assert!(out.iter().all(|(_, s)| s.abs() < 1e-6));
    }
}
