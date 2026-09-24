//! 倒数排名融合（RRF，ADR-004 默认策略）。
//!
//! `score(d) = Σ w_i / (k + rank_i(d))`，rank 从 1 起，某 lane 未命中该文档则贡献 0。
//! 只用排名、免疫量纲（BM25 无上界 vs 余弦集中在 [0.6, 0.95]）。

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::types::{ChunkId, Score};

use super::adaptive::{AdaptiveRule, FusionCtx};
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
    /// 自适应规则（V2 Step 9 / T7-14；`None` = 不自适应 = 今天的行为）。
    ///
    /// ⚠️ 规则**只有**在编排层同时打开 `adaptive_fusion` 开关
    /// （`SearchParts::adaptive_fusion`，门面经 `Config::adaptive_fusion`）时才被
    /// 消费——开关关 ⇒ 编排层根本不调 `fuse_adaptive` ⇒ 逐位一致（I9-3）。
    adaptive: Option<AdaptiveRule>,
}

impl RrfFusion {
    /// 构造 RRF 融合。默认请直接用 `RrfFusion::default()`
    /// （k=60、weights=[1.0, 1.5]，均为 P5 实测定稿值）。
    pub fn new(k: f32, weights: Vec<f32>) -> Self {
        assert!(k > 0.0, "RRF 的 k 必须 > 0");
        Self {
            k,
            weights,
            adaptive: None,
        }
    }

    /// 构造带自适应规则的 RRF（V2 Step 9 / T7-14 / FR-35）。
    ///
    /// ⚠️ 还需编排层打开 `adaptive_fusion` 开关才会生效（见 `adaptive` 字段的
    /// 文档）；只配规则不开开关 ⇒ `fuse` 照旧（逐位一致，S9-T1 钉住）。
    pub fn new_adaptive(k: f32, weights: Vec<f32>, rule: AdaptiveRule) -> Self {
        assert!(k > 0.0, "RRF 的 k 必须 > 0");
        assert!(
            (0.0..=1.0).contains(&rule.theta),
            "自适应阈值 θ 必须 ∈ [0, 1]（预注册网格见设计 §4.5），收到 {}",
            rule.theta
        );
        Self {
            k,
            weights,
            adaptive: Some(rule),
        }
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

    /// 自适应规则（`None` = 未配置）。
    pub fn adaptive_rule(&self) -> Option<AdaptiveRule> {
        self.adaptive
    }

    /// RRF 本体（**带权重的唯一实现**）：`fuse` 与 `fuse_adaptive` 共用同一段
    /// 数学 ⇒ 不会出现两份 RRF（设计 §4.1 对 P3-1 的处置要求）。
    fn fuse_weighted(
        &self,
        lanes: &[LaneResults],
        k: usize,
        weights: &[f32],
    ) -> Vec<(ChunkId, Score)> {
        // chunk_id → 累计分
        let mut acc: HashMap<ChunkId, f32> = HashMap::new();

        for (lane_idx, lane) in lanes.iter().enumerate() {
            let w = weights.get(lane_idx).copied().unwrap_or(1.0);
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
        self.fuse_weighted(lanes, k, &self.weights)
    }

    /// 决策：tier 1（空 lane 置 0，零参数）+ tier 2（`s < θ` ⇒ BM25 路降到 0，单阈值）。
    ///
    /// ⚠️ 未配置规则 ⇒ `None`（沿用自身权重）；配置了则**恒返回 `Some`**——
    /// 哪怕两级都没改动权重，也把「本次实际生效的权重」报出去
    /// （`Metrics.fusion_weights` 的可观测语义，S9-T7 ① 的判据面）。
    fn weights_override(&self, ctx: &FusionCtx<'_>) -> Option<Vec<f32>> {
        let rule = self.adaptive?;
        // 与 lanes 一一对应（含空 lane 槽位；超出的 lane 沿用 fuse 的 1.0 兜底口径）
        let mut w: Vec<f32> = (0..ctx.lanes.len())
            .map(|i| self.weights.get(i).copied().unwrap_or(1.0))
            .collect();
        // tier 1：某 lane 为空 ⇒ 该路权重置 0（🔴 不删槽——权重按 lane 序号取）
        for (i, lane) in ctx.lanes.iter().enumerate() {
            if lane.len == 0 {
                w[i] = 0.0;
            }
        }
        // tier 2：信号 s < θ ⇒ BM25 路（lane 0）权重降到 0（信号无定义 ⇒ 不触发）
        if let Some(s) = rule.signal.value(ctx) {
            if s < rule.theta && !w.is_empty() {
                w[0] = 0.0;
            }
        }
        Some(w)
    }

    fn fusion_signal(&self, ctx: &FusionCtx<'_>) -> Option<f32> {
        self.adaptive.and_then(|rule| rule.signal.value(ctx))
    }

    fn fuse_adaptive(
        &self,
        lanes: &[LaneResults],
        k: usize,
        ctx: &FusionCtx<'_>,
    ) -> Vec<(ChunkId, Score)> {
        match self.weights_override(ctx) {
            Some(w) => self.fuse_weighted(lanes, k, &w),
            None => self.fuse(lanes, k),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)] // S9_T 前缀用例名（中文 + 判据编号）
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

    // ---- V2 Step 9 / T7-14：自适应融合（tier 规则 + 两通道等价性） ----

    use crate::fusion::adaptive::{AdaptiveSignal, LaneStats};

    /// 两路 `LaneStats`（ctx 由各用例按字段构造——字段白名单由 `adaptive.rs`
    /// 的 `S9_T3` 钉住，这里只服务行为判据）。
    fn s9_lanes<'a>(a: &'a LaneResults, b: &'a LaneResults) -> Vec<LaneStats<'a>> {
        vec![LaneStats::new(a), LaneStats::new(b)]
    }

    /// 中性 ctx（`query_df` / `has_ascii` 可覆盖触发条件；`top_k` 取 10）。
    fn s9_ctx<'a>(lanes: &'a [LaneStats<'a>], query_df: f32, has_ascii: bool) -> FusionCtx<'a> {
        FusionCtx {
            lanes,
            query_terms: 3,
            query_chars: 12,
            has_ascii,
            query_df,
            top_k: 10,
        }
    }

    /// 逐位比较（`f32` 用 `to_bits`，与 `searcher.rs` 既有判据同口径）。
    fn s9_bits(out: &[(ChunkId, Score)]) -> Vec<(ChunkId, u32)> {
        out.iter().map(|(id, s)| (*id, s.to_bits())).collect()
    }

    /// **S9-T6（等价性，两半）**：① 未覆写的第三方实现——`weights_override`/
    /// `fusion_signal` 恒 `None`、`fuse_adaptive` 与 `fuse` 直调**逐位一致**
    /// （「既有实现一行不改」的判据面）；② `RrfFusion` 未配规则——自适应通道
    /// 退回 `fuse`（`None` ⇒ 不改权重）。
    #[test]
    fn S9_T6_默认实现两通道与fuse直调逐位一致() {
        // ① 只实现 name/fuse 的「下游自定义策略」形态
        struct BareConcat;
        impl FusionStrategy for BareConcat {
            fn name(&self) -> &'static str {
                "bare"
            }
            fn fuse(&self, lanes: &[LaneResults], k: usize) -> Vec<(ChunkId, Score)> {
                let mut out: Vec<(ChunkId, Score)> = lanes.iter().flatten().cloned().collect();
                out.sort_by(|a, b| a.0.cmp(&b.0));
                out.truncate(k);
                out
            }
        }
        let a: LaneResults = vec![(3, 1.0), (1, 0.9)];
        let b: LaneResults = vec![(2, 0.5)];
        let lanes = s9_lanes(&a, &b);
        let ctx = s9_ctx(&lanes, 0.5, false);
        let s = BareConcat;
        assert_eq!(
            s.weights_override(&ctx),
            None,
            "未覆写 ⇒ None（沿用自身权重）"
        );
        assert_eq!(s.fusion_signal(&ctx), None);
        assert_eq!(
            s9_bits(&s.fuse_adaptive(&[a.clone(), b.clone()], 10, &ctx)),
            s9_bits(&s.fuse(&[a.clone(), b.clone()], 10)),
            "fuse_adaptive 默认必须逐字转发 fuse"
        );

        // ② RrfFusion 未配规则
        let r = RrfFusion::default();
        assert_eq!(r.weights_override(&ctx), None);
        assert_eq!(r.fusion_signal(&ctx), None);
        assert_eq!(
            s9_bits(&r.fuse_adaptive(&[a.clone(), b.clone()], 10, &ctx)),
            s9_bits(&r.fuse(&[a, b], 10))
        );
    }

    /// **S9-T1（融合层的一半）**：配了触发态规则、但走 `fuse`（= 开关关时编排层
    /// 的唯一调用路径）⇒ 与「不配规则」逐位一致。开关本身的判据在
    /// `query::searcher` 的用例（编排层不调 `fuse_adaptive` 是结构性的）。
    #[test]
    fn S9_T1_配了规则但走fuse时逐位一致() {
        let a: LaneResults = vec![(3, 1.0), (1, 0.9)];
        let b: LaneResults = vec![(2, 0.5), (4, 0.4)];
        // 规则处于「必触发」形态（s3=0.5 < θ=0.6）
        let armed = RrfFusion::new_adaptive(
            60.0,
            vec![1.0, 1.5],
            AdaptiveRule {
                signal: AdaptiveSignal::DfCoverage,
                theta: 0.6,
            },
        );
        let plain = RrfFusion::new(60.0, vec![1.0, 1.5]);
        assert_eq!(
            s9_bits(&armed.fuse(&[a.clone(), b.clone()], 10)),
            s9_bits(&plain.fuse(&[a.clone(), b.clone()], 10)),
            "🔴 规则配置不得影响 fuse 本身（开关关 ⇒ 逐位一致）"
        );
    }

    /// **S9-T2（I9-2）**：空 lane 的**槽位仍在**、权重仍对齐——只能置 0、不得删槽。
    ///
    /// 🔑 夹具要害：权重取 `[2.0, 1.5]`（两路**可区分**）——若实现错误地删掉空
    /// lane，剩余 lane 会变成 lane 0、权重串到 `2.0` ⇒ 分数全变 ⇒ 断言红。
    /// 期望值用「单路、权重 1.5」现算（不硬编码数字）。
    #[test]
    fn S9_T2_空lane槽位仍在且权重不串路() {
        let empty: LaneResults = vec![];
        let b: LaneResults = vec![(2, 0.5), (4, 0.4)];
        let f = RrfFusion::new_adaptive(
            60.0,
            vec![2.0, 1.5],
            AdaptiveRule {
                signal: AdaptiveSignal::DfCoverage,
                theta: 0.0, // tier 2 永不触发（s ≥ 0 恒不小于 0）⇒ 纯 tier 1
            },
        );
        let lanes = s9_lanes(&empty, &b);
        let ctx = s9_ctx(&lanes, 0.5, false);

        // ① 可观测面：置 0 且槽位数 == lanes 数（w[1] 原样 1.5）
        assert_eq!(f.weights_override(&ctx), Some(vec![0.0, 1.5]));

        // ② 行为面：结果 == 「只把 lane 1 当唯一路、用权重 1.5 融合」
        let expect = RrfFusion::new(60.0, vec![1.5]).fuse(std::slice::from_ref(&b), 10);
        assert_eq!(
            s9_bits(&f.fuse_adaptive(&[empty.clone(), b.clone()], 10, &ctx)),
            s9_bits(&expect),
            "🔴 空路权重疑似串到另一路（实现删了槽而不是置 0）"
        );

        // ③ 反向臂：BM25 非空 / vector 空 —— 置 0 的是 lane 1
        let a: LaneResults = vec![(7, 3.0)];
        let lanes = s9_lanes(&a, &empty);
        let ctx = s9_ctx(&lanes, 0.5, false);
        assert_eq!(f.weights_override(&ctx), Some(vec![2.0, 0.0]));
        let expect = RrfFusion::new(60.0, vec![2.0]).fuse(std::slice::from_ref(&a), 10);
        assert_eq!(
            s9_bits(&f.fuse_adaptive(&[a.clone(), vec![]], 10, &ctx)),
            s9_bits(&expect)
        );
    }

    /// **S9-T7（tier 1）**：① 可观测面 `Some([0.0, w])`（这条才区分得了 tier 1
    /// 是否生效——P4-4）；② 结果与不开 tier 1 时相同（RRF 只用名次 ⇒ 空 lane
    /// 本来就零贡献，② 独立于 tier 1，单靠 ② 无效——如实两记）。
    #[test]
    fn S9_T7_tier1可观测面与行为空转() {
        let empty: LaneResults = vec![];
        let b: LaneResults = vec![(2, 0.5), (4, 0.4)];
        let f = RrfFusion::new_adaptive(
            60.0,
            vec![1.0, 1.5],
            AdaptiveRule {
                signal: AdaptiveSignal::DfCoverage,
                theta: 0.0,
            },
        );
        let lanes = s9_lanes(&empty, &b);
        let ctx = s9_ctx(&lanes, 0.5, false);

        // ① 可观测面（判据本体）
        assert_eq!(
            f.weights_override(&ctx),
            Some(vec![0.0, 1.5]),
            "tier 1 生效的可观测证据"
        );
        // ② 行为空转（P4-4：空 lane 本来就零贡献 ⇒ 与 fuse 相同）
        assert_eq!(
            s9_bits(&f.fuse_adaptive(&[vec![], b.clone()], 10, &ctx)),
            s9_bits(&f.fuse(&[vec![], b], 10)),
            "tier 1 单独不改变排序（设计 §4.2 点破的行为空转）"
        );
    }

    /// **tier 2 触发**：`s < θ` ⇒ BM25 路权重降到 0 ⇒ 名次等价于「只剩 vector 路」
    /// （vector 路 ≥ k 条时**逐位**等价；BM25 条目只剩 0 分、被截断）。
    /// 这是 S9-1b「上界结构性可达」的实现面证据。
    #[test]
    fn tier2触发时等价于弃BM25路() {
        let a: LaneResults = vec![(3, 1.0), (1, 0.9), (5, 0.8)];
        let b: LaneResults = vec![(2, 0.5), (4, 0.4), (6, 0.3)];
        let f = RrfFusion::new_adaptive(
            60.0,
            vec![1.0, 1.5],
            AdaptiveRule {
                signal: AdaptiveSignal::DfCoverage,
                theta: 0.6, // s3 = 0.5 < 0.6 ⇒ 触发
            },
        );
        let lanes = s9_lanes(&a, &b);
        let ctx = s9_ctx(&lanes, 0.5, false);

        assert_eq!(f.weights_override(&ctx), Some(vec![0.0, 1.5]));
        assert_eq!(f.fusion_signal(&ctx), Some(0.5), "信号值原样上报");

        // 期望值现算：只把 vector 路当唯一路、用权重 1.5 融合（不硬编码数字）
        let expect = RrfFusion::new(60.0, vec![1.5]).fuse(std::slice::from_ref(&b), 3);
        assert_eq!(
            s9_bits(&f.fuse_adaptive(&[a.clone(), b.clone()], 3, &ctx)),
            s9_bits(&expect),
            "🔴 触发后应等价于弃 BM25 路（BM25 条目 0 分、被 k 截断）"
        );

        // 未触发（s ≥ θ）⇒ 权重原样、与 fuse 逐位一致
        let f_hi = RrfFusion::new_adaptive(
            60.0,
            vec![1.0, 1.5],
            AdaptiveRule {
                signal: AdaptiveSignal::DfCoverage,
                theta: 0.4, // 0.5 ≥ 0.4 ⇒ 不触发
            },
        );
        let a: LaneResults = vec![(3, 1.0), (1, 0.9), (5, 0.8)];
        let b: LaneResults = vec![(2, 0.5), (4, 0.4), (6, 0.3)];
        let lanes = s9_lanes(&a, &b);
        let ctx = s9_ctx(&lanes, 0.5, false);
        assert_eq!(f_hi.weights_override(&ctx), Some(vec![1.0, 1.5]));
        assert_eq!(
            s9_bits(&f_hi.fuse_adaptive(&[a.clone(), b.clone()], 3, &ctx)),
            s9_bits(&f_hi.fuse(&[a.clone(), b.clone()], 3))
        );
    }

    /// tier 1 + tier 2 叠加：两臂都命中时**不重复打折**（w[0] 已是 0）。
    #[test]
    fn tier1与tier2叠加不重复打折() {
        let a: LaneResults = vec![];
        let b: LaneResults = vec![(2, 0.5)];
        let f = RrfFusion::new_adaptive(
            60.0,
            vec![1.0, 1.5],
            AdaptiveRule {
                signal: AdaptiveSignal::DfCoverage,
                theta: 0.9, // 0.5 < 0.9 ⇒ tier 2 也触发
            },
        );
        let lanes = s9_lanes(&a, &b);
        let ctx = s9_ctx(&lanes, 0.5, false);
        assert_eq!(
            f.weights_override(&ctx),
            Some(vec![0.0, 1.5]),
            "两臂叠加后 w[0] 仍是 0（幂等），w[1] 不受牵连"
        );
    }

    /// **S9-T8（确定性）**：同输入两次调用同结果（含「同分按 chunk_id 升序」——
    /// 触发态下 w_bm25 = 0 ⇒ BM25 路条目全 0 分 ⇒ 同分 tie-break 可达）。
    #[test]
    fn S9_T8_自适应路径确定性同输入两次同结果() {
        let a: LaneResults = vec![(5, 1.0), (9, 0.9)];
        let b: LaneResults = vec![(6, 0.5), (8, 0.4)];
        let f = RrfFusion::new_adaptive(
            60.0,
            vec![1.0, 1.0],
            AdaptiveRule {
                signal: AdaptiveSignal::DfCoverage,
                theta: 0.6,
            },
        );
        let lanes = s9_lanes(&a, &b);
        let ctx = s9_ctx(&lanes, 0.5, false);
        let out1 = f.fuse_adaptive(&[a.clone(), b.clone()], 10, &ctx);
        let out2 = f.fuse_adaptive(&[a.clone(), b.clone()], 10, &ctx);
        assert_eq!(s9_bits(&out1), s9_bits(&out2));
        // 触发态（s=0.5 < 0.6）⇒ a 路条目全 0 分 ⇒ 5/9 同分按 id 升序
        assert_eq!(out1.len(), 4);
        let zero_scored: Vec<ChunkId> = out1
            .iter()
            .filter(|(_, s)| *s == 0.0)
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(zero_scored, vec![5, 9], "0 分条目按 chunk_id 升序");
    }
}
