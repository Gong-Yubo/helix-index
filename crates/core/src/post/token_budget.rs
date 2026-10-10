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

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)] // 中文测试名含英文缩写（S10_T33）

    use super::*;
    use crate::query::response::Explain;

    fn hit(chunk_id: u32, text: &str, score: f32) -> Hit {
        Hit {
            chunk_id,
            doc_id: chunk_id,
            score,
            text: text.to_string(),
            source: format!("doc-{chunk_id}"),
            metadata: serde_json::json!({}),
            explain: Explain {
                fused_score: score,
                ..Default::default()
            },
        }
    }

    /// **确定性计数器**：按空白切词（与字符数**不同** ⇒ 用来证明「注入的计数器真的被用了」）。
    struct WordCounter;
    impl TokenCounter for WordCounter {
        fn count(&self, text: &str) -> usize {
            text.split_whitespace().count()
        }
    }

    fn ids(hs: &[Hit]) -> Vec<u32> {
        hs.iter().map(|h| h.chunk_id).collect()
    }

    fn units(hs: &[Hit]) -> Vec<u32> {
        hs.iter()
            .map(|h| h.explain.budget_units.expect("保留的条目必须带计数"))
            .collect()
    }

    /// **S10-T33（`S10-5` 验收① / `D-S10-10`）**：注入确定性计数器 ⇒ 输出**合计 ≤ budget**。
    #[test]
    fn S10_T33_注入计数器后输出合计不超过预算() {
        let hs: Vec<Hit> = (0..5)
            .map(|i| hit(i, "词 词", 1.0 - i as f32 * 0.1))
            .collect(); // 每条 2 词
        let out = TokenBudget::with_counter(6, Arc::new(WordCounter))
            .process(hs, 10)
            .unwrap();
        println!(
            "budget=6（每条 2 单位）⇒ {:?}，计数 {:?}",
            ids(&out),
            units(&out)
        );
        assert_eq!(ids(&out), vec![0, 1, 2], "6 单位 ⇒ 恰 3 条");
        let total: usize = out
            .iter()
            .map(|h| h.explain.budget_units.unwrap() as usize)
            .sum();
        assert!(total <= 6, "🔴 验收①：合计必须 ≤ budget（实得 {total}）");
    }

    /// **S10-T34（用户 2026-10-10 拍板 / 设计 §20 `I-36`）**：超预算 = **顺序截断**
    /// —— 放不下的那条**及其后全部丢弃**，**不跳过**它去装后面的小条目。
    ///
    /// ⚠️ 夹具刻意造成「停 vs 跳过」可分辨：序 = `4, 6, 1, 1` 字符、`budget = 5`
    /// ⇒ 顺序截断只留第 1 条；**若实现改成「换更小的条目」**，会把后面两条 1 字符装进来
    /// （输出 `{0, 2, 3}`）⇒ 本断言当场变红。
    #[test]
    fn S10_T34_超预算时顺序截断而非跳过换小条目() {
        let hs = vec![
            hit(0, "aaaa", 0.9),
            hit(1, "bbbbbb", 0.8),
            hit(2, "c", 0.7),
            hit(3, "d", 0.6),
        ];
        let out = TokenBudget::new(5).process(hs, 10).unwrap();
        println!("budget=5（字符）⇒ {:?}", ids(&out));
        assert_eq!(
            ids(&out),
            vec![0],
            "🔴 放不下第 2 条 ⇒ **停**（不得跳过它去装后面的 1 字符条目）"
        );
        assert_eq!(units(&out), vec![4]);
    }

    /// **S10-T35（`S10-5` 验收③ / `S10-T11` / 架构 `R63`）**：默认计数器数的是**字符**，
    /// 而**注入的**计数器按其自身单位读 —— 同一份输入、同一个 budget，两者结果不同。
    #[test]
    fn S10_T35_默认计数器数的是字符而注入的按其自身单位() {
        // 「中文」= 2 字符 / 1 词；"abc def" = 7 字符 / 2 词
        let mk = || vec![hit(0, "中文", 1.0), hit(1, "abc def", 0.9)];

        let chars = TokenBudget::new(3).process(mk(), 10).unwrap();
        println!(
            "默认（字符）budget=3 ⇒ {:?} {:?}",
            ids(&chars),
            units(&chars)
        );
        assert_eq!(ids(&chars), vec![0], "字符：2 + 7 > 3 ⇒ 只留首条");
        assert_eq!(
            units(&chars),
            vec![2],
            "🔴 单位 = **字符数**（不是 token、也不是字节）"
        );

        let words = TokenBudget::with_counter(3, Arc::new(WordCounter))
            .process(mk(), 10)
            .unwrap();
        println!("注入（词）budget=3 ⇒ {:?} {:?}", ids(&words), units(&words));
        assert_eq!(ids(&words), vec![0, 1], "词：1 + 2 ≤ 3 ⇒ 两条都留下");
        assert_eq!(
            units(&words),
            vec![1, 2],
            "单位随计数器变 —— 这正是「单位不由内核定」"
        );
    }

    /// **S10-T36（与 `k` 的关系）**：`k` 仍是**候选上界** —— 预算是**第二道**截断，
    /// **不得放宽**第一道。
    #[test]
    fn S10_T36_预算是第二道截断不得放宽k() {
        let hs: Vec<Hit> = (0..5).map(|i| hit(i, "abcd", 1.0)).collect(); // 每条 4 字符
        let out = TokenBudget::new(1000).process(hs, 2).unwrap();
        println!("budget=1000 + k=2 ⇒ {:?}", ids(&out));
        assert_eq!(ids(&out), vec![0, 1], "预算足够时仍受 k 截断");
    }

    /// **S10-T37（边界）**：`budget = 0` + 非空正文 ⇒ 输出为空（**合法**配置，不是错误）。
    #[test]
    fn S10_T37_预算为零时输出为空() {
        let hs = vec![hit(0, "x", 1.0), hit(1, "yy", 0.9)];
        let out = TokenBudget::new(0).process(hs, 10).unwrap();
        assert!(out.is_empty(), "budget = 0 ⇒ 任何正数计数的条目都装不下");
    }

    /// **S10-T38（边界，如实登记）**：**计 0 单位**的条目（空正文）在 `budget = 0` 下**仍入选**
    /// —— 这是「`used + units > budget` 判据」的直接推论（`0 > 0` 为假），**不是**缺陷。
    /// ⚠️ 因此「`budget = 0` ⇒ 输出恒空」**不成立**；正确说法 = 「只有 **0 成本**的条目能入选」。
    #[test]
    fn S10_T38_零成本条目在预算为零时仍入选() {
        let hs = vec![hit(0, "", 1.0), hit(1, "x", 0.9)];
        let out = TokenBudget::new(0).process(hs, 10).unwrap();
        println!("budget=0 ⇒ {:?} {:?}", ids(&out), units(&out));
        assert_eq!(
            ids(&out),
            vec![0],
            "空正文计 0 单位 ⇒ 放得下（第 2 条 1 单位 ⇒ 停）"
        );
        assert_eq!(units(&out), vec![0]);
    }

    /// **S10-T39（边界）**：恰好等于预算的条目**必须入选**（判据是 `>`，不是 `>=`）。
    #[test]
    fn S10_T39_恰好等于预算时入选() {
        let hs = vec![hit(0, "abcd", 1.0), hit(1, "e", 0.9)];
        let out = TokenBudget::new(4).process(hs, 10).unwrap();
        assert_eq!(
            ids(&out),
            vec![0],
            "4 字符 == budget ⇒ 入选；下一条会超 ⇒ 停"
        );
    }

    /// **S10-T40（退化输入）**：空入参 / `k == 0` ⇒ 返回空（不 panic、不 `Err`）。
    #[test]
    fn S10_T40_空输入与k为零返回空() {
        let tb = TokenBudget::new(10);
        assert!(
            tb.process(vec![], 10).unwrap().is_empty(),
            "空入参 ⇒ 空出参"
        );
        assert!(
            tb.process(vec![hit(0, "ab", 1.0)], 0).unwrap().is_empty(),
            "k == 0 ⇒ 一条都不要（同 `search_parts` 的退化输入护栏）"
        );
    }

    /// **S10-T41（窗口取形，与 `Mmr` 的分野）**：预算是「只裁已有候选」⇒
    /// **不覆写** `candidate_window`（provided 默认 = `k`）。
    #[test]
    fn S10_T41_窗口不放大() {
        let tb = TokenBudget::new(0);
        assert_eq!(tb.name(), "token-budget");
        for k in [0usize, 1, 7, 100] {
            assert_eq!(
                tb.candidate_window(k),
                k,
                "🔴 不得放大窗口（放大只让被丢掉的那一截更长；与 `Mmr` 的取形相反）"
            );
        }
    }

    /// **S10-T42（`D-S10-09` ①）**：逐条信号 `budget_units` = **本条计数**。
    #[test]
    fn S10_T42_逐条信号等于本条计数() {
        let hs = vec![hit(0, "abc", 1.0), hit(1, "de", 0.9), hit(2, "fghi", 0.8)];
        let out = TokenBudget::new(3).process(hs, 10).unwrap();
        assert_eq!(ids(&out), vec![0]);
        assert_eq!(units(&out), vec![3], "`budget_units` = `count(text)`");
    }
}
