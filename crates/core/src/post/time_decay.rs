//! 时间衰减后处理器（V2 Step 10 / `S10-04` / `T7-20` / `D-S10-03`）。
//!
//! # 作用分（`D-S10-03`）
//!
//! 作用于**融合分这一档**（后处理阶段里 `Hit::score` 就等于
//! [`crate::query::response::Explain::fused_score`]），
//! 理由 = 二者**同一层**、可观测面现成；**不**改精排路径 —— 否则「衰减」与「精排」两把尺子
//! 会混在一起（架构 `R46` 的教训）。
//!
//! # 三个显式参数、无默认魔数（设计 §4.3 / `Q10-1`）
//!
//! | 参数 | 含义 |
//! | --- | --- |
//! | `weight` | **指数衰减率 λ**，单位 **1/秒** |
//! | `field` | [`Hit::metadata`] 里承载时间戳的**字段名**（内核**不猜** `ts_ms` / `created_at`） |
//! | `clock` | **时钟源**：返回「现在」的 unix **毫秒**；内核**不持时钟** |
//!
//! 公式（唯一写死的一处；`age` 以**秒**计）：
//!
//! ```text
//! age        = max(0, clock() − t) / 1000          // t = metadata[field] 的整数毫秒
//! factor     = exp(−weight × age)                  // 常参（weight ≥ 0）下 ∈ (0, 1]
//! hit.score *= factor                              // 仅当 factor ≠ 1.0
//! explain.decay_factor = factor                    // 仅当 factor ≠ 1.0（`is_some()` ⟺ 被改写）
//! ```
//!
//! ⚠️ **为什么 `clock` 是「时钟源」而不是一个固定的「基准时刻」**：后处理**没有**
//! per-query 入参通道（`process(&self, hits, k)`，见 [`PostProcessor`] 的 §15 `I-11` 定值）
//! ⇒ 基准只能**持在实现自己的字段里**。若持一个**固定值**，长期运行的服务会把「衰减」
//! **冻结在构造那一刻**（同一文档的因子永不变化）；持**时钟源**则两种语义都能表达 ——
//! 测试注入「恒返回定值」的闭包（⇒ **可复现**），生产注入真实的 `now_ms`。
//!
//! ⚠️ **窗口不变**：[`PostProcessor::candidate_window`] 用 provided 默认（= `k`）——
//! 衰减只**重加权**已有候选、不需要额外候选 ⇒ 这是「provided 默认值恰好正确」的第一个实例。
//!
//! # 缺字段 / 不可解析 ⇒ `Err`（**不得静默降级**，NFR-07）
//!
//! [`PostProcessor::process`] 的入参契约已明写「时间字段解析失败 … 应上抛 `Err`」⇒ 本实现照办：
//! 字段**缺失** / 非整数 / 超 `i64` ⇒ [`Error::InvalidInput`]，消息带**字段名 + `chunk_id`**
//! （否则调用方分不清「字段名写错」与「个别文档缺数据」）。
//! ⚠️ 「悄悄不处理」会让调用方以为衰减已生效 —— 那正是 NFR-07 禁止的静默退化。
//!
//! # 改写后**必须重排**（守住 `Hit::score` 的既有契约）
//!
//! [`Hit::score`] 的 rustdoc 声明「`hits` **恒按本字段降序**、同分按 `chunk_id` 升序（NFR-06）」
//! ⇒ 衰减改写分数后**必须**按新分数重排（同分仍 `chunk_id` 升序），否则输出**违反既有契约**。
//! ⚠️ 与 MMR / token budget **不同**：那两者会改变**排序依据本身 / 长度**（`D-S10-09`），
//! 而本阶段**不动**排序依据（仍是 `score`）⇒ 只是「分数与名次一起变」，契约不破。
//!
//! ⚠️ **只在真的发生改写时才重排**：一条都没改写（`weight == 0` / 所有 `age == 0`）时
//! **逐位保持输入顺序** ⇒ 「开了但无事发生」与「没开」仍然**逐位一致**（否则 tie-break
//! 会把并列条目的顺序悄悄换掉）。
//!
//! # 默认关（零行为变化）
//!
//! 只有调用方显式启用（[`crate::search::SearchIndexBuilder::time_decay`]）才生效 ⇒ 未启用时
//! 全链路与引入前**逐位一致**（`S10-T6` 的正向臂）。

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::post::PostProcessor;
use crate::query::response::Hit;

/// 时间衰减：按 `metadata[field]` 的时间戳对**融合分**做指数衰减（语义与公式见模块文档）。
pub struct TimeDecay {
    /// 指数衰减率 λ（**1/秒**）
    weight: f64,
    /// 时间字段名（`metadata` 里的键）
    field: String,
    /// 时钟源：返回「现在」的 unix 毫秒
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl TimeDecay {
    /// `weight` = 指数衰减率 λ（**1/秒**，建议 `≥ 0`）；`field` = 时间字段名；
    /// `clock` = 时钟源（unix **毫秒**）。
    ///
    /// ⚠️ **构造期不校验**参数合法性（`weight` 的有限性在 [`PostProcessor::process`] 里
    /// 上抛 `Err`）—— 构造器返回 `Self` 而不是 `Result`，校验放到真正消费它的那一刻。
    pub fn new(
        weight: f64,
        field: impl Into<String>,
        clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        Self {
            weight,
            field: field.into(),
            clock,
        }
    }

    /// 「现在」的 unix 毫秒（= 时钟源的当前读数）。
    pub fn now_ms(&self) -> i64 {
        (self.clock)()
    }

    /// 时间字段名。
    pub fn field(&self) -> &str {
        &self.field
    }

    /// 从 `metadata` 读时间戳（整数毫秒）。
    ///
    /// ⚠️ 返回 `Err` 而不是「当作 0」/「跳过该条」—— 见模块文档的 NFR-07 段。
    fn timestamp_of(&self, hit: &Hit) -> Result<i64> {
        match hit.metadata.get(self.field.as_str()) {
            None => Err(Error::InvalidInput(format!(
                "时间衰减：chunk_id={} 的 metadata 缺字段 `{}`（字段名由调用方指定）",
                hit.chunk_id, self.field
            ))),
            Some(v) => v.as_i64().ok_or_else(|| {
                Error::InvalidInput(format!(
                    "时间衰减：chunk_id={} 的 `{}` 不是整数毫秒（实得 {v}）",
                    hit.chunk_id, self.field
                ))
            }),
        }
    }
}

impl PostProcessor for TimeDecay {
    fn name(&self) -> &'static str {
        "time-decay"
    }

    fn process(&self, hits: Vec<Hit>, _k: usize) -> Result<Vec<Hit>> {
        if !self.weight.is_finite() {
            return Err(Error::InvalidInput(format!(
                "时间衰减：weight 必须是有限数（实得 {}）",
                self.weight
            )));
        }
        let now = self.now_ms();
        let mut hits = hits;
        let mut changed = false;
        for hit in hits.iter_mut() {
            let t = self.timestamp_of(hit)?;
            // ⚠️ `saturating_sub`：`t` 极端大（如 `i64::MIN`）时避免 i64 溢出 panic。
            let age_secs = now.saturating_sub(t).max(0) as f64 / 1000.0;
            let factor = (-self.weight * age_secs).exp() as f32;
            // 只有**真的改写**才留信号（设计 §4.3：`is_some()` ⟺ 该条被衰减改写）——
            // `age == 0` 或 `weight == 0` ⇒ `factor == 1.0` ⇒ 分数逐位不变 ⇒ 记号保持 `None`。
            if factor != 1.0 {
                hit.score *= factor;
                hit.explain.decay_factor = Some(factor);
                changed = true;
            }
        }
        // 守住 `Hit::score` 的既有契约（降序；同分按 `chunk_id` 升序，NFR-06）。
        // ⚠️ **只在真发生改写时才重排**：一条都没改写（`λ == 0` / 所有 `age == 0`）时**逐位保持输入顺序**
        //    ⇒「开了但无事发生」与「没开」仍然逐位一致（否则会因 tie-break 把顺序悄悄换掉）。
        // ⚠️ 用 `total_cmp`（不是 `partial_cmp().unwrap()`）：分数理论上可达 NaN，
        //    而 NaN 下 `partial_cmp` 返回 `None` ⇒ `unwrap` 会 panic；`total_cmp` 恒有全序。
        if changed {
            hits.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then_with(|| a.chunk_id.cmp(&b.chunk_id))
            });
        }
        Ok(hits)
    }
}
