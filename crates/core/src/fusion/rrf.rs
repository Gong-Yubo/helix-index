//! 倒数排名融合（RRF，ADR-004 默认策略）。
//!
//! `score(d) = Σ w_i / (k + rank_i(d))`，rank 从 1 起，某 lane 未命中该文档则贡献 0。
//! 只用排名、免疫量纲（BM25 无上界 vs 余弦集中在 [0.6, 0.95]）。

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::types::{ChunkId, Score};

use super::{FusionStrategy, LaneResults};

/// RRF 融合。`k` 默认 60（论文推荐值）；
/// `weights` 默认 (bm25, vector) = (1.0, 1.5)——P5 weights 诊断定稿
/// （T2Ranking 320 query：等权 (1,1) NDCG=0.5119 → (1,1.5)=0.5221，
/// Recall@10/MRR@10 三路最高；(1,2) 以上回落。BM25 为弱路，加权后融合
/// 被 NDCG 主导的 vector 单路追平。详见 docs/devel/p5-design.md 8.5 与 eval-report）。
#[derive(Debug, Clone)]
pub struct RrfFusion {
    k: f32,
    weights: Vec<f32>,
}

impl RrfFusion {
    pub fn new(k: f32, weights: Vec<f32>) -> Self {
        assert!(k > 0.0, "RRF 的 k 必须 > 0");
        Self { k, weights }
    }

    /// RRF 的 k（默认 60，论文推荐值）。
    pub fn k(&self) -> f32 {
        self.k
    }

    /// 路权重（默认 (bm25, vector) = (1.0, 1.5)）。
    ///
    /// CLI 的 `--rrf-k` / `--rrf-weights` 默认值从此派生，避免"内核定稿值"与
    /// "CLI 硬编码默认值"两处漂移（V1-14：修 bench 默认 `1,1` 与 search `1,1.5` 分裂）。
    pub fn weights(&self) -> &[f32] {
        &self.weights
    }
}

impl Default for RrfFusion {
    fn default() -> Self {
        Self::new(60.0, vec![1.0, 1.5])
    }
}

impl FusionStrategy for RrfFusion {
    fn name(&self) -> &'static str {
        "rrf"
    }

    fn fuse(&self, lanes: &[LaneResults], k: usize) -> Vec<(ChunkId, Score)> {
        // chunk_id → 累计分
        let mut acc: HashMap<ChunkId, f32> = HashMap::new();

        for (lane_idx, lane) in lanes.iter().enumerate() {
            let w = self.weights.get(lane_idx).copied().unwrap_or(1.0);
            for (rank, (chunk_id, _)) in lane.iter().enumerate() {
                // rank 从 1 起
                let contribution = w / (self.k + (rank as f32 + 1.0));
                *acc.entry(*chunk_id).or_insert(0.0) += contribution;
            }
        }

        let mut out: Vec<(ChunkId, Score)> = acc.into_iter().collect();
        // fused_score 降序；同分按 chunk_id 升序（确定性 NFR-06）
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
    fn rrf手算值比对() {
        // p3-design.md 附录 A（等权手算锚点——显式构造，不随 Default 定稿值漂移）：
        // bm25 路: A(1) B(2) C(3)；vector 路: B(1) D(2) A(3)
        let bm25: LaneResults = vec![(0, 10.0), (1, 8.0), (2, 5.0)]; // A=0, B=1, C=2
        let vec: LaneResults = vec![(1, 0.9), (3, 0.8), (0, 0.7)]; // B=1, D=3, A=0
        let fusion = RrfFusion::new(60.0, vec![1.0, 1.0]);

        let out = fusion.fuse(&[bm25, vec], 10);
        let ids: Vec<ChunkId> = out.iter().map(|(id, _)| *id).collect();
        // 期望 B(1) > A(0) > D(3) > C(2)
        assert_eq!(ids, vec![1, 0, 3, 2]);

        // 校验分数精确值（B=0.032522, A=0.032266, D=0.016129, C=0.015873）
        let score_of = |id: ChunkId| out.iter().find(|(i, _)| *i == id).map(|(_, s)| *s).unwrap();
        assert!((score_of(1) - 0.032522).abs() < 1e-4);
        assert!((score_of(0) - 0.032266).abs() < 1e-4);
        assert!((score_of(3) - 0.016129).abs() < 1e-4);
        assert!((score_of(2) - 0.015873).abs() < 1e-4);
    }

    #[test]
    fn 未命中不贡献() {
        // 单路：只有 chunk 5 命中 rank1
        let lane: LaneResults = vec![(5, 1.0)];
        let fusion = RrfFusion::new(60.0, vec![1.0]);
        let out = fusion.fuse(&[lane], 10);
        assert_eq!(out, vec![(5, 1.0 / 61.0)]);
    }

    #[test]
    fn 空输入返回空() {
        let fusion = RrfFusion::default();
        assert!(fusion.fuse(&[], 10).is_empty());
        assert!(fusion.fuse(&[vec![]], 10).is_empty());
    }

    #[test]
    fn 同分按chunk_id升序() {
        // 两个 chunk 只在各自 lane 的相同 rank 出现 → 同分
        let a: LaneResults = vec![(2, 1.0)];
        let b: LaneResults = vec![(1, 0.5)];
        let fusion = RrfFusion::default();
        let out = fusion.fuse(&[a, b], 10);
        // 二者分数相同（1/61），chunk_id 升序 → 1 在 2 前
        let ids: Vec<ChunkId> = out.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![1, 2]);
    }
}
