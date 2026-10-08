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

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)] // 中文测试名含英文缩写（S10_T7）

    use super::*;
    use crate::query::response::Explain;

    /// 固定时钟的读数（**可复现的关键**：内核不持时钟 ⇒ 测试注入恒返回定值的闭包）。
    const NOW: i64 = 1_700_000_000_000;

    fn fixed_clock(now: i64) -> Arc<dyn Fn() -> i64 + Send + Sync> {
        Arc::new(move || now)
    }

    /// `ts_ms: None` ⇒ metadata 里**没有**该字段（用于缺字段臂）。
    fn hit(chunk_id: u32, score: f32, ts_ms: Option<i64>) -> Hit {
        let metadata = match ts_ms {
            Some(t) => serde_json::json!({ "ts_ms": t }),
            None => serde_json::json!({}),
        };
        Hit {
            chunk_id,
            doc_id: chunk_id,
            score,
            text: format!("正文 {chunk_id}"),
            source: format!("doc-{chunk_id}"),
            metadata,
            explain: Explain {
                fused_score: score,
                ..Default::default()
            },
        }
    }

    fn ids(hits: &[Hit]) -> Vec<u32> {
        hits.iter().map(|h| h.chunk_id).collect()
    }

    /// **S10-T7（性质测试）**：同一批候选、**时间权重增大 ⇒ 新条目的名次不变差**。
    ///
    /// ⚠️ **刻意让输入顺序 ≠ 期望顺序**：输入是「老 → 新」（`[3, 2, 1]`，越靠后越新），
    /// 而 `weight == 0` 时输出**逐位保持输入顺序**（id=1 排最后 ⇒ 名次 = 2）。
    /// 若输入本身就按期望顺序排，这条断言会「看着过了其实什么都没验」。
    #[test]
    fn S10_T7_权重增大时新条目名次不变差() {
        let ages_ms = [86_400_000i64, 3_600_000, 0]; // 24h / 1h / 0
        let mut prev_rank: Option<usize> = None;
        for w in [0.0f64, 1e-5, 1e-3] {
            let hits: Vec<Hit> = ages_ms
                .iter()
                .enumerate()
                .map(|(i, age)| hit(3 - i as u32, 1.0, Some(NOW - age)))
                .collect();
            assert_eq!(
                ids(&hits),
                vec![3, 2, 1],
                "前提：输入顺序 = 老→新（**不是**期望顺序）"
            );
            let td = TimeDecay::new(w, "ts_ms", fixed_clock(NOW));
            let out = td.process(hits, 10).unwrap();
            let rank = out
                .iter()
                .position(|h| h.chunk_id == 1)
                .expect("最新条目必须在输出里");
            println!("weight={w} ⇒ 输出={:?}，最新条目名次={rank}", ids(&out));
            if let Some(p) = prev_rank {
                assert!(
                    rank <= p,
                    "🔴 权重增大后**最新条目的名次变差**了：{p} → {rank}（weight={w}）"
                );
            }
            prev_rank = Some(rank);
        }
        assert_eq!(prev_rank, Some(0), "大权重下最新条目必须排第一");
    }

    /// **缺字段 / 非整数 ⇒ `Err`**（NFR-07：不得静默降级）+ 消息必须**可定位**。
    #[test]
    fn S10_T8_时间字段缺失或非整数上抛Err() {
        let td = TimeDecay::new(1e-3, "ts_ms", fixed_clock(NOW));

        // ① 缺字段
        let e = td.process(vec![hit(7, 1.0, None)], 10).unwrap_err();
        let msg = e.to_string();
        assert!(
            msg.contains("ts_ms") && msg.contains('7'),
            "🔴 报错消息必须带**字段名 + chunk_id**（否则调用方分不清「字段名写错」与「个别文档缺数据」）：{msg}"
        );

        // ② 浮点（非整数毫秒）
        let mut h = hit(8, 1.0, None);
        h.metadata = serde_json::json!({ "ts_ms": 1.5 });
        let e = td.process(vec![h], 10).unwrap_err();
        assert!(e.to_string().contains("不是整数"), "实得：{e}");

        // ③ 字符串
        let mut h2 = hit(9, 1.0, None);
        h2.metadata = serde_json::json!({ "ts_ms": "2026-10-08" });
        assert!(td.process(vec![h2], 10).is_err(), "字符串时间戳必须报错");

        // 对照臂：**合法输入不得报错** —— 否则上面三条可能只是「任何输入都 Err」的空断言
        assert!(
            td.process(vec![hit(1, 1.0, Some(NOW))], 10).is_ok(),
            "对照臂：合法输入必须成功"
        );
    }

    /// **边界：没有真的改写时 ⇒ 信号 `None`、顺序逐位保持**（设计 §4.3 的 `is_some() ⟺ 被改写`）。
    #[test]
    fn S10_T10_未改写时信号为None且顺序逐位保持() {
        // arm A：`weight == 0`（开了但无事发生）
        let hits = vec![
            hit(3, 0.5, Some(NOW - 1000)),
            hit(1, 0.5, Some(NOW - 2000)),
            hit(2, 0.5, Some(NOW)),
        ];
        let base = ids(&hits);
        let out0 = TimeDecay::new(0.0, "ts_ms", fixed_clock(NOW))
            .process(hits.clone(), 10)
            .unwrap();
        assert_eq!(
            ids(&out0),
            base,
            "weight == 0 ⇒ 一条都不改写 ⇒ 顺序逐位保持"
        );
        assert!(
            out0.iter().all(|h| h.explain.decay_factor.is_none()),
            "无改写 ⇒ 信号恒 None"
        );

        // arm B：`age == 0`（时间戳正好等于时钟）
        let out1 = TimeDecay::new(1e-3, "ts_ms", fixed_clock(NOW))
            .process(vec![hit(1, 1.0, Some(NOW))], 10)
            .unwrap();
        assert_eq!(
            out1[0].score, 1.0,
            "age == 0 ⇒ factor == 1.0 ⇒ 分数逐位不变"
        );
        assert!(
            out1[0].explain.decay_factor.is_none(),
            "未被改写 ⇒ 信号为 None（口径：is_some() ⟺ 被改写）"
        );
    }

    /// **改写后必须重排**：输出按 `score` 降序、同分按 `chunk_id` 升序（NFR-06 的既有契约）。
    ///
    /// ⚠️ 刻意让**输入顺序 ≠ 期望顺序**（输入 `[9, 4, 1]`、期望 `[4, 9, 1]`）。
    #[test]
    fn S10_T9_改写后按分数降序且同分按chunk_id升序() {
        let hits = vec![
            hit(9, 1.0, Some(NOW - 1000)),
            hit(4, 1.0, Some(NOW - 1000)), // 与 9 同 age ⇒ 改写后**同分**
            hit(1, 1.0, Some(NOW - 86_400_000)), // 最老 ⇒ 被压到最后
        ];
        let out = TimeDecay::new(1e-3, "ts_ms", fixed_clock(NOW))
            .process(hits, 10)
            .unwrap();
        assert_eq!(
            ids(&out),
            vec![4, 9, 1],
            "🔴 契约：分数降序 + 同分按 chunk_id 升序（不是「同分保持输入相对顺序」）"
        );
        for w in out.windows(2) {
            assert!(w[0].score >= w[1].score, "必须降序");
            if w[0].score == w[1].score {
                assert!(w[0].chunk_id < w[1].chunk_id, "同分必须 chunk_id 升序");
            }
        }
        for h in &out {
            let f = h.explain.decay_factor.expect("被改写的条目必须有信号");
            assert!(
                (h.score - h.explain.fused_score * f).abs() < f32::EPSILON * 8.0,
                "口径：`score == fused_score × factor`（chunk_id={}）",
                h.chunk_id
            );
        }
    }

    /// **非有限权重 ⇒ `Err`**（而不是悄悄产出 NaN 分数污染排序）。
    #[test]
    fn S10_T11_非有限权重上抛Err() {
        for w in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let td = TimeDecay::new(w, "ts_ms", fixed_clock(NOW));
            let e = td
                .process(vec![hit(1, 1.0, Some(NOW - 1000))], 10)
                .unwrap_err();
            assert!(e.to_string().contains("有限数"), "weight={w} 实得：{e}");
        }
    }

    /// **负权重**（调用方自担）：越**新**的条目分越**高** ⇒ 信号可以 `> 1`。
    #[test]
    fn S10_T13_负权重时越新越高且信号可大于一() {
        let hits = vec![
            hit(1, 1.0, Some(NOW - 86_400_000)), // 老
            hit(2, 1.0, Some(NOW)),              // 新（age == 0 ⇒ 不改写）
        ];
        let out = TimeDecay::new(-1e-5, "ts_ms", fixed_clock(NOW))
            .process(hits, 10)
            .unwrap();
        assert_eq!(
            ids(&out),
            vec![1, 2],
            "负权重 ⇒ 老条目 factor > 1 ⇒ 反而靠前"
        );
        let f = out[0].explain.decay_factor.expect("老条目必须被改写");
        assert!(f > 1.0, "负权重下信号允许 > 1（实得 {f}）");
    }

    /// **边界：未来时间戳** ⇒ `age` 必须钳到 0 ⇒ `factor == 1.0` ⇒ **不改写、不放大**。
    ///
    /// 若去掉 `.max(0)`，未来时间戳会得到 `factor > 1`（**凭空加分**）—— 这条就是那个钳位的牙齿。
    #[test]
    fn S10_T14_未来时间戳被钳为零龄() {
        let td = TimeDecay::new(1e-3, "ts_ms", fixed_clock(NOW));
        let out = td
            .process(vec![hit(1, 1.0, Some(NOW + 3_600_000))], 10)
            .unwrap();
        assert_eq!(out[0].score, 1.0, "未来时间戳不得被放大（age 必须钳到 0）");
        assert!(
            out[0].explain.decay_factor.is_none(),
            "未被改写 ⇒ 信号 None"
        );
    }

    /// **名字与「默认窗口」**：衰减**不需要额外候选** ⇒ 必须走 provided 默认（`k`）。
    #[test]
    fn S10_T12_名字与默认窗口() {
        let td = TimeDecay::new(1e-3, "ts_ms", fixed_clock(NOW));
        assert_eq!(td.name(), "time-decay");
        for k in [1usize, 10, 100] {
            assert_eq!(
                td.candidate_window(k),
                k,
                "🔴 时间衰减只重加权、不需要额外候选 ⇒ 不得覆盖 provided 默认（否则白付召回与回捞成本）"
            );
        }
        assert_eq!(td.field(), "ts_ms");
        assert_eq!(td.now_ms(), NOW);
    }
}
