//! 可观测性：检索过程的轻量指标（NFR-07）。
//!
//! # 三条受众不同的通道（V2 Step 5 / D-S5-05）
//!
//! `Metrics` 走**三条**通道，三者受众不同、不是重复：
//!
//! | 通道 | 受众 | 入口 |
//! | --- | --- | --- |
//! | [`SearchResponse::metrics`] | **调用方**（可单测、可断言、可聚合） | `query::Metrics` 再导出 |
//! | `tracing::info!`（[`Metrics::log`]） | **宿主程序**（挂 subscriber 即得结构化日志） | 事件名 `search` |
//! | bench 采集点 | **决策**（`eval-report` / V2.1 的 prefilter 判据） | `crates/cli/src/bench.rs` |
//!
//! Step 5 之前只有第二条，于是「宿主不挂 subscriber 就静默丢弃 / **无法单测** /
//! **无法被 bench 聚合**」——而 `vector_shortfall` 的定位正是「V2.1 是否引入
//! prefilter 的判据」。本 Step 把前两条补上。
//!
//! # ⚠️ `vector_shortfall == 0` 不再等于"没有 prefilter 需求"
//!
//! **精确恒等式**（[`VectorRoute::Exact`] 下）：
//!
//! ```text
//! shortfall = min(candidate_k, allowed) − min(candidate_k, |图 ∩ allowed|)
//! ```
//!
//! `allowed` 来自 **`Index`**（存活 ∩ 谓词），而扫描枚举的是**图里的点**——两个独立来源。
//! 于是「`Exact` ⇒ 缺口恒为 0」**只在图覆盖了全部 allowed chunk 时成立**：一旦图滞后于
//! 索引（图里少了 allowed 的点，例如 Lenient 重载出的不完整图），缺口就会 > 0。
//!
//! ⇒ 正确读法是**条件式**，而且那个条件本身就是信号：
//!
//! - `Exact` + 缺口 `== 0` ⇒ 这一档没走 ANN（**不是**"ANN 没有缺口"）；
//! - `Exact` + 缺口 `> 0` ⇒ **图未覆盖全部 allowed chunk**（图滞后于索引）。
//!
//! 两种读数都必须**连看** [`Metrics::vector_route`]（设计 §4.4 推论 1）。
//!
//! [`SearchResponse::metrics`]: crate::query::SearchResponse::metrics

use std::time::Duration;

use crate::vector::VectorRoute;

/// 一次检索的指标快照。
#[derive(Debug, Clone, Copy, Default)]
pub struct Metrics {
    /// 总耗时（含两路召回 + 融合 + 回捞）
    pub took: Duration,
    /// BM25 路候选数
    pub bm25: usize,
    /// 向量路候选数
    pub vector: usize,
    /// 融合去重后候选总数
    pub candidates: usize,
    /// 最终输出条数
    pub fused: usize,
    /// 过滤求值 + 谓词构建耗时（O(query 词数 + 匹配文档数)，不再是 O(N) 全扫）
    pub filter_eval: Duration,
    /// 通过过滤的候选总数（`CandidateFilter::allowed_count`；无过滤时为存活 chunk 数）
    pub allowed: usize,
    /// 向量路「本该拿到 `min(candidate_k, allowed)` 条、实得 n 条」的缺口。
    ///
    /// 归一到 `allowed`（通过过滤的候选总数）而非裸 `candidate_k`：候选池本身不足
    /// `candidate_k` 时（小语料、高选择度过滤）`candidate_k - len` 恒为正，会让
    /// 「filtered-ANN 是否降级」这个信号彻底失去鉴别力。
    ///
    /// > 0 表示低选择度下 ANN 没凑够候选——不是错误，但会让召回静默下降，
    /// > 必须可观测（V2.1 是否引入 prefilter 结构的判据）。
    ///
    /// ⚠️ 走精确路径（[`VectorRoute::Exact`]）时该值**通常**为 0，但那**不是恒等式**：
    /// `allowed` 来自 `Index`、扫描枚举的是图里的点，图滞后于索引时缺口仍会 > 0。
    /// ⇒ `Exact` + 缺口 `> 0` 反过来是「**图未覆盖全部 allowed chunk**」的诊断信号；
    /// 而 `Exact` + 缺口 `== 0` 才说明"这一档没走 ANN"。两者都**不能**读成
    /// "无缺口需求"——见模块文档。
    pub vector_shortfall: usize,
    /// 向量路本次**实际**走的路径（D-S5-07）。
    ///
    /// 它是"兜底到底生效了没有"的**直接**判据——`VectorRoute::None` = 本次检索
    /// 没走向量路（`SearchMode::Bm25`），`Ann` = 走了 ANN，`Exact` = 走了精确扫描。
    /// 少了它就只能从延迟反推，而 Step 1 已经吃过"用错指标读错结论"的亏。
    pub vector_route: VectorRoute,
    /// BM25 路耗时。
    ///
    /// - 单路模式（`Bm25`）= 该路耗时；
    /// - Hybrid 下与 [`Self::vector_elapsed`] 走 `rayon::join`，**两者区间重叠**，
    ///   相加**不等于** [`Self::took`]。
    ///
    /// 存在的理由：Hybrid 下单一 `took` 无法归因"是哪一路慢"
    /// （架构 §8.3 的示例日志本就预期 `bm25=1.4ms(vector=6.1ms parallel)`）。
    pub bm25_elapsed: Duration,
    /// 向量路耗时（含精确扫描的 `O(N)` 遍历成本，若走了精确路径）。
    /// 语义与重叠关系见 [`Self::bm25_elapsed`]。
    pub vector_elapsed: Duration,
    /// 精排段耗时（`Reranker::rerank` 调用本身；**不含**窗口内的回捞与 `explain` 组装）。
    ///
    /// NFR-12 的「精排段 P99」取的就是本字段。**为什么必须进 `Metrics`**：同
    /// `vector_shortfall` 的先例（D-S5-05）——只经 `tracing` 输出时外部拿不到实例
    /// ⇒ 无法单测、无法被 bench 聚合；而端到端 `took` 里混着两路召回与窗口回捞，
    /// 反推不出精排那一段。
    ///
    /// ⚠️ 与 [`Self::bm25_elapsed`] / [`Self::vector_elapsed`] 不同：**本字段不与之重叠**
    /// （精排在两路召回**之后**串行执行）。
    pub rerank_elapsed: Duration,
    /// **实际**交给精排的候选条数（= 窗口 `min(max(k, window), 融合条数)` 内**可回捞**的条数）。
    ///
    /// ⚠️ **记的是实际交接值，不是窗口公式值**（PR #53 评审 P3-2）：窗口里若有**陈旧
    /// `chunk_id`**（图 / 索引不同步，第 3 步的防御路径会跳过它）⇒ 本字段**小于**公式值。
    /// **两者之差本身是诊断信号**（差 > 0 ⇒ 本次窗口里有已不可回捞的 chunk）。
    /// 正常路径两者恒等。`k == 0` 时为 `0`（**不把窗口交给精排**，见 `search_parts`）。
    ///
    /// 它是「本次是否**真的**放开了窗口」的**直接证据**（NFR-07）。只看精排器的 `R`
    /// 或 CLI 的 `--rerank-window` 会漏掉两种情况：① 融合结果本身不足 `R` 条；
    /// ② `candidate_k` 没与窗口联动、窗口被静默封顶（设计 §2.2 的发现 A）。
    /// 两种情形下本字段都会**如实小于** `R`，而不是"看起来开了"。
    ///
    /// ⚠️ **跨窗口不可横比**（与 [`Self::vector_shortfall`] 同族的口径变化）：
    /// 窗口变大 ⇒ `candidate_k` 变大 ⇒ `vector_shortfall` 的分母
    /// （`candidate_k.min(allowed)`）跟着变。这不是回归，是口径随配置变。
    ///
    /// 早退路径（索引为空 / 过滤排空 / 融合为空）保持默认 `0`，语义 = 「本次没跑精排」。
    pub rerank_window: usize,
    /// 本次检索所依据的**视图里的段数**（`main + deltas`）。
    ///
    /// `1` = 只有主段 ⇒ 与 `S8-03` 之前**逐位一致**（零回归）；`> 1` = 本次走了跨段路径。
    /// 空库早退路径下仍为 `1`（视图里确实有一个空的 `main` 段）。
    ///
    /// ⚠️ **与 `vector_segments` 必须连看**：段数本身不是缺陷信号，**「向量路只覆盖了一部分段」
    /// 才是**（见下）。
    pub segments: usize,
    /// 向量路本次**实际覆盖的段数**；`0` = 本次没走向量路（`SearchMode::Bm25`）。
    ///
    /// 🔴 `0 < vector_segments < segments` ⇒ **向量路只召回了一部分段的内容**。
    /// 这不是边角情形：默认 `mode` 是 `Hybrid`（`Searcher::infer_mode`）⇒ 只要有向量能力，
    /// **每次 `commit()` 之后、`save`/`compact` 之前**都处于这个状态；`helix serve` 那种
    /// 「写端持续 commit、没人调 fold」的形态下**长期**如此。
    ///
    /// ⚠️ `S8-05`（PR5）落地前**恒为 1**（向量索引只给 `main`，见 `Searcher::parts` 的注释）；
    /// PR5 之后应恒等于 [`Self::segments`]。⇒ 本字段是 NFR-07 要的那条信号：
    /// **调用方现在能发现「向量路只扫了 1/N 的内容」，而不是静默半盲**。
    pub vector_segments: usize,
    /// 本次检索所依据的视图里的**跨段墓碑条数**（`0` = 无墓碑）。
    ///
    /// 🔑 为什么要有它：墓碑 > 0 会把 BM25 路从「零谓词」推回「每 posting 一次谓词」
    /// （`R52`），也是向量路存活判定的额外一层；这个跃迁在今天**完全不可观测**。
    ///
    /// ⚠️ **口径 = 视图层的墓碑条数（精确、O(1)），不是「被挡掉的候选数」**
    /// （设计 §4.13.3 原文如此，本 PR 已同步更正该行）：按候选计数在召回层**不可良定义** ——
    /// 同一个 chunk 会被「BM25 路 / 向量路」各取一次、甚至在同一路的多个 posting 上重复出现
    /// ⇒ 计数会随 lane 数与 term 命中数虚增，读出来无法解释。「还有几条墓碑没物理化」才是
    /// 调用方真正要的那个量（`0` ⇒ 热路径已回到零谓词）。
    pub tombstoned: usize,
}

impl Metrics {
    /// 用 tracing 输出一行结构化日志。
    pub fn log(&self, query: &str) {
        tracing::info!(
            query = query,
            took_ms = self.took.as_millis() as u64,
            bm25 = self.bm25,
            vector = self.vector,
            candidates = self.candidates,
            fused = self.fused,
            filter_eval_us = self.filter_eval.as_micros() as u64,
            allowed = self.allowed,
            vector_shortfall = self.vector_shortfall,
            vector_route = ?self.vector_route,
            bm25_ms = self.bm25_elapsed.as_secs_f64() * 1000.0,
            vector_ms = self.vector_elapsed.as_secs_f64() * 1000.0,
            rerank_window = self.rerank_window,
            rerank_ms = self.rerank_elapsed.as_secs_f64() * 1000.0,
            segments = self.segments,
            vector_segments = self.vector_segments,
            tombstoned = self.tombstoned,
            "search"
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;

    /// **S5-T8（metrics.rs 首个单测）**：默认值必须是**不撒谎**的。
    ///
    /// `Metrics::default()` 是 `search_parts` 的草稿缓冲区，每条返回路径都必须
    /// 显式填好再交出去。默认 `vector_route = None`（"未走向量路"）对
    /// 索引为空 / 过滤排空 / 融合为空这三条早退路径恰好是**真话**；
    /// 而矢量路一旦真的跑了，编排层必须覆盖它——`S5-T6` 的响应往返测试钉住这点。
    #[test]
    fn 默认值不撒谎() {
        let m = Metrics::default();
        assert_eq!(m.vector_route, VectorRoute::None, "默认 = 未走向量路");
        assert_eq!(m.took, Duration::ZERO);
        assert_eq!(m.bm25_elapsed, Duration::ZERO);
        assert_eq!(m.vector_elapsed, Duration::ZERO);
        assert_eq!(m.vector_shortfall, 0);
        assert_eq!(m.allowed, 0);
        // V2 Step 7 / S7-02 的两个新字段：默认值同样必须**不撒谎**。
        // `rerank_window = 0` 对三条早退路径恰是真话（"本次没跑精排"）。
        assert_eq!(m.rerank_window, 0, "默认 = 没跑精排");
        assert_eq!(m.rerank_elapsed, Duration::ZERO);
        // V2 Step 8 / S8-03 评审批次（P2-2）的三个观测字段：默认值必须**不撒谎**。
        // ⚠️ `segments` 的默认 `0` **只在「`Metrics::default()` 草稿缓冲区」这个意义上成立** ——
        // `search_parts` 的**每一条**返回路径（含三条早退）都会在入口处把它覆盖成真实段数
        // （≥ 1，视图里至少有一个 `main` 段）。⇒ 这里断言的是"缓冲区未被填过"，
        // 不是"没走向量路"以外的语义；后者由 `vector_segments = 0` 表达。
        assert_eq!(m.segments, 0, "默认 = 草稿缓冲区（编排层入口必覆盖）");
        assert_eq!(m.vector_segments, 0, "默认 = 没走向量路");
        assert_eq!(m.tombstoned, 0, "默认 = 无跨段墓碑");
    }

    /// `VectorRoute` 三态齐全且 `Copy`（进 `Metrics` 后不该带来克隆成本）。
    #[test]
    fn 向量路径三态可辨且可复制() {
        let all = [VectorRoute::None, VectorRoute::Ann, VectorRoute::Exact];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                assert_eq!(i == j, a == b, "三态必须互不相等");
            }
        }
        let copied = VectorRoute::Exact;
        let again = copied; // Copy：移动后仍可用
        assert_eq!(copied, again);
    }

    /// `log` 在零耗时 / 各路径取值下都不得 panic（冒烟：tracing 无 subscriber 时静默丢弃）。
    #[test]
    fn log在零值与全值下都不panic() {
        Metrics::default().log("空指标");
        let full = Metrics {
            took: Duration::from_millis(12),
            bm25: 30,
            vector: 30,
            candidates: 55,
            fused: 10,
            filter_eval: Duration::from_micros(430),
            allowed: 1000,
            vector_shortfall: 0,
            vector_route: VectorRoute::Exact,
            bm25_elapsed: Duration::from_micros(1400),
            vector_elapsed: Duration::from_micros(6100),
            rerank_elapsed: Duration::from_micros(900),
            rerank_window: 20,
            segments: 3,
            vector_segments: 1,
            tombstoned: 2,
        };
        full.log("满指标");
    }
}
