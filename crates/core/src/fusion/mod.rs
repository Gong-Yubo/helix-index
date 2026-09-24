//! 多路结果融合：RRF（默认）与加权归一（备选）。
//!
//! # 边界（架构文档 4.3 硬约束）
//!
//! 本模块**不回捞正文**——融合只操作 `(chunk_id, score)`，正文回捞统一在
//! 融合与精排之后由 `query` 层对 Top-K 做一次。一旦 fusion 开始回捞正文，
//! 换融合策略就再也不能独立测试。

mod adaptive;
mod rrf;
mod weighted;

pub use adaptive::{AdaptiveRule, AdaptiveSignal, FusionCtx, LaneStats};
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

    /// 【V2 Step 9 / FR-35 新增，**provided**】按 query 侧信号**覆盖本次的权重**
    /// （**决策**钩子）。默认实现返回 `None` ⇒ 沿用策略自身的权重 ⇒
    /// **既有实现一行不改**、**逐位一致**。
    ///
    /// ⚠️ 返回 `Some(w)` 时，`w` 必须与召回结果**一一对应**（含**空 lane** 的槽位：
    /// 权重按 lane 序号取，删槽会让权重串到另一路，I9-2——只能**置 0**、不得删槽）。
    ///
    /// ⚠️ 上下文 [`FusionCtx`] **只含检索自身可观测的量**（I9-1 / S9-3）：实现
    /// 不得从任何渠道读入相关性标注；本 crate 的字段白名单由 `S9_T3` 钉住。
    fn weights_override(&self, _ctx: &FusionCtx<'_>) -> Option<Vec<f32>> {
        None
    }

    /// 【V2 Step 9 / FR-35 新增，**provided**】本次决策所依据的**信号值** ∈ [0, 1]
    /// （可观测上报通道，`Metrics.fusion_signal` 的数据源）。默认 `None`。
    ///
    /// 与 [`Self::weights_override`] 一样是 `&self` 的**纯函数**（无内部状态）⇒
    /// 编排层「再调一次填 `Metrics`」必然得到与决策时相同的值（设计 §4.1 规则 3）。
    /// 信号无定义（如 s1 需要 exactly 两路）时返回 `None`。
    fn fusion_signal(&self, _ctx: &FusionCtx<'_>) -> Option<f32> {
        None
    }

    /// 【V2 Step 9 / FR-35 新增，**provided**】[`Self::weights_override`] 的
    /// **应用通道**（闭合机制）。默认实现 = `self.fuse(lanes, k)` ⇒
    /// **既有实现一行不改**、**默认逐位一致**。
    ///
    /// 🔴 **为什么必须单独一个方法**（设计 §4.1 / 第 2 轮评审 P3-1）：只加
    /// `weights_override` 时，「`Some(w)` 时用 `w`」没有落点——编排层只持
    /// `&dyn FusionStrategy`、而 `fuse` 读的是策略**私有**字段 ⇒ 编排层没有任何
    /// 合法通道把 `w` 送进融合计算。四条替代路（改 `fuse` 签名 / 每查询重建策略 /
    /// 策略内部可变性 / 编排层自己重算）逐条被否决，理由见设计 §4.1。
    ///
    /// ⚠️ 编排层只在**自适应开关打开**时调用本方法（开关关 ⇒ 不构造 ctx、
    /// 不调本方法 ⇒ 逐位一致是结构性的，I9-3）。
    fn fuse_adaptive(
        &self,
        lanes: &[LaneResults],
        k: usize,
        _ctx: &FusionCtx<'_>,
    ) -> Vec<(ChunkId, Score)> {
        self.fuse(lanes, k)
    }
}
