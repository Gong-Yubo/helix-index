//! token budget 裁剪后处理器
//! （V2 Step 10 / `S10-06` / `T7-03` / `FR-25`；决策 `D-S10-09` / `D-S10-10`）。
//!
//! # 它做什么
//!
//! 按**调用方给的计数口径**对窗口内候选逐条计数，累加到**不超过预算**为止，
//! 超出预算的条目**及其后全部丢弃**（= **第二道截断**）：
//!
//! ```text
//! used = 0; out = []
//! for h in 窗口内候选（保持传入序）:
//!     if out.len() == k: break                    // ① `k` 仍是候选上界
//!     c = counter.count(h.text)
//!     if used + c > budget: break                 // ② 预算：放不下就**停**
//!     used += c; h.explain.budget_units = Some(c); out.push(h)
//! ```
//!
//! 🔴 **超预算时的行为 = 「顺序截断」，不是「换更小的条目」**（用户 **2026-10-10** 拍板，
//! 定值见设计 §20）。判据：被丢的是**序靠后**的条目，与「`k` 是候选上界、budget 是第二道截断」
//! 的叙述一致、可证伪；换条目会让**入选集合**依赖全文长度分布（更难解释，也更难测）。
//!
//! # 🔴 计数口径：本实现**数不一定是 token**（`D-S10-10`）
//!
//! 内核**没有**目标模型的 tokenizer（设计 §2.5 **N4**：全仓 `token_budget` / `count_tokens`
//! **0 命中**）⇒ 预算的**单位由注入的计数器定义**：
//!
//! | 构造入口 | 计数单位 | 说明 |
//! | --- | --- | --- |
//! | [`TokenBudget::new`] | **字符数**（[`CharCounter`]） | ⚠️ **这不是 token 数**：中文场景下字符数与 token 数**不成固定比例**（架构 `R63`） |
//! | [`TokenBudget::with_counter`] | 调用方定义 | 想要真值 ⇒ 按目标模型实现 [`TokenCounter`] 并注入 |
//!
//! **两条禁止**（设计 §4.5 / `D-S10-10`）：
//!
//! 1. **不得把字符数当 token 数上报** ⇒ 上报面一律用**单位中立**的字段名
//!    （[`crate::query::response::Explain::budget_units`] /
//!    [`crate::query::metrics::Metrics::budget_units`]），**不叫** `token_*`；
//! 2. **不得静默截断** ⇒ ① 被计入的条目逐条写 `explain.budget_units`（`is_some()` ⟺ 本阶段计过它）；
//!    ② 聚合值写 `Metrics.budget_units`；③ 若**截到 0 条**，编排层把
//!    [`crate::query::response::EmptyReason::PostEmptied`] 填进响应（见 §20 定值）。
//!
//! ⚠️ **本实现不产生 `Err`**：预算是 `usize`，**没有非法值**；`budget == 0` 是**合法**配置
//! —— 此时**只有计 0 单位**的条目（空正文）能入选，所以输出**通常**为空、由 `empty_reason` 说明
//! （⚠️ 严格说「`budget == 0` ⇒ 输出恒空」**不成立**，见 `S10_T38`）。这**不是**「静默降级」——
//! 与 [`super::TimeDecay`] 的「缺字段 ⇒ `Err`」不同：那里有**真**非法输入，这里没有。
//!
//! # 窗口**不**放大（与 `TimeDecay` 同族，与 `Mmr` 相反）
//!
//! 预算只裁**已有**候选 ⇒ 不覆写 [`PostProcessor::candidate_window`]（用 provided 默认 = `k`）。
//! 放大窗口在**预算不变**时只会让「被丢掉的那一截」更长，不改变输出语义。
//!
//! # `D-S10-09`（改变 `hits` 的长度）
//!
//! 本实现**不改** `Hit::score`、也**不改** `hits` 的排序依据（输入序原样保留）—— 它只改**长度**。
//! 按 `D-S10-09` 的三条处置：① 可观测信号 = `explain.budget_units` 与 `Metrics.budget_units`
//! （外加截到 0 条时的 `empty_reason`）；② **默认关**
//! （不调用 [`crate::search::SearchIndexBuilder::token_budget`] 即 `post: None`）；
//! ③ `Hit::score` 的 rustdoc 三档表已把本阶段归入「后处理」一档。
//!
//! # 默认关（零行为变化）
//!
//! 只有调用方显式启用才生效 ⇒ 未启用时全链路与引入前**逐位一致**。

use std::sync::Arc;

use crate::error::Result;
use crate::post::PostProcessor;
use crate::query::response::Hit;

/// 计数口径：把一段正文折算成**预算单位**。
///
/// ⚠️ **provided 默认实现 = 字符数**，而它**不是**任何模型的 token 数（`D-S10-10`）：
/// 内核没有目标模型的 tokenizer（设计 §2.5 `N4`）⇒ 想要真值只能由调用方注入。
///
/// ⚠️ **单位由实现定义**：`count` 返回什么单位，**预算**与**观测面**就以什么单位读
/// （默认实现下 = 字符）⇒ 上报字段一律叫 `budget_units`，**不叫** `token_*`
/// （避免「把字符数当 token 数上报」，`D-S10-10` 的禁止 ①）。
///
/// ⚠️ 实现**不得 panic**：正文是任意用户数据，`count` 在编排热路径上被逐条调用。
pub trait TokenCounter: Send + Sync {
    /// 把 `text` 折算成预算单位数。
    ///
    /// 默认实现 = **字符数**（`str::chars()` 的个数，按 **Unicode 标量值**计，
    /// 与 `len()` / `len_utf16()` 都不同）。
    fn count(&self, text: &str) -> usize {
        text.chars().count()
    }
}

/// **默认计数器 = 字符数**（[`TokenCounter`] 的 provided 默认，未覆写即此语义）。
///
/// ⚠️ 名字里的 `Char` 是**故意的**：它数的是**字符**，不是 token —— 调用方在
/// `Metrics` / `Explain` 上看到 `budget_units` 时，**若用的是本计数器，单位就是字符**。
pub struct CharCounter;

impl TokenCounter for CharCounter {}

/// token budget 裁剪（语义、计数口径与两条禁止见模块文档）。
pub struct TokenBudget {
    /// 预算（单位 = `counter` 的单位；默认计数器下即**字符数**）
    budget: usize,
    /// 计数器（[`TokenBudget::new`] 给的是 [`CharCounter`]）
    counter: Arc<dyn TokenCounter>,
}

impl TokenBudget {
    /// 用**默认计数器（字符数）**构造。
    ///
    /// `budget` 的单位因此是**字符** —— 调用方若把它当 token 预算用，会**静默失准**
    /// （中文下两者不成固定比例，架构 `R63`）⇒ 想要真 token 预算请用
    /// [`TokenBudget::with_counter`] 注入按目标模型实现的计数器。
    pub fn new(budget: usize) -> Self {
        Self::with_counter(budget, Arc::new(CharCounter))
    }

    /// 注入计数器的构造（预算单位随 `counter` 的定义）。
    pub fn with_counter(budget: usize, counter: Arc<dyn TokenCounter>) -> Self {
        Self { budget, counter }
    }
}

impl PostProcessor for TokenBudget {
    fn name(&self) -> &'static str {
        "token-budget"
    }

    // ⚠️ **有意不覆写 `candidate_window`**：预算只裁**已有**候选，不需要额外候选
    //    （用 provided 默认 = `k`）。放大窗口在预算不变时只让「被丢掉的那一截」更长。
    //    与 `Mmr` 的取形相反 —— 那一个必须放大，这是**语义决定**而非风格。

    fn process(&self, hits: Vec<Hit>, k: usize) -> Result<Vec<Hit>> {
        // `k == 0` ⇒ 一条都不要（与 `search_parts` 的退化输入护栏同口径）。
        if k == 0 {
            return Ok(Vec::new());
        }
        // ① `k` 仍是候选上界（第二道截断不能**放宽**第一道）。
        let limit = k.min(hits.len());
        let mut out: Vec<Hit> = Vec::with_capacity(limit);
        let mut used = 0usize;
        for mut h in hits {
            if out.len() >= limit {
                break;
            }
            let units = self.counter.count(&h.text);
            // ② 预算：**放不下就停**（顺序截断 —— 不跳过后面的更小条目，见模块文档）。
            //    ⚠️ 用 `saturating_add` 兜极端大数：`used + units` 在 release 下会**回绕**，
            //    那会让本判据变成 **false** ⇒ 静默把超预算条目放行。
            if used.saturating_add(units) > self.budget {
                break;
            }
            used += units;
            // 逐条信号（`D-S10-09` ①）：`is_some()` ⟺ 本阶段给它计过数。
            // ⚠️ `u32` **饱和**（不是回绕、也不上抛）：观测面字段，计数本身已经发生；
            //    单条正文超过 42 亿字符属实际不可达，饱和只是把「不可能」写清楚。
            h.explain.budget_units = Some(u32::try_from(units).unwrap_or(u32::MAX));
            out.push(h);
        }
        Ok(out)
    }
}
