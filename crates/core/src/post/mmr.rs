//! MMR（Maximal Marginal Relevance）结果去重后处理器
//! （V2 Step 10 / `S10-05` / `T7-02` / `FR-24`；决策 `D-S10-08` / `D-S10-09`）。
//!
//! # 它做什么
//!
//! 经典 MMR 在「相关度」与「与已选集合的冗余度」之间做**贪心**折中：
//!
//! ```text
//! mmr(c) = λ · rel(c) − (1 − λ) · max_{s ∈ selected} sim(c, s)     // selected 为空时 max sim = 0
//! 取 argmax 入 selected，重复直到凑满 k 条（或候选耗尽）
//! ```
//!
//! | 符号 | 含义 |
//! | --- | --- |
//! | `rel(c)` | 该候选的**当前分数**（后处理阶段即 [`Hit::score`]，见 [`crate::post`] 的入参契约） |
//! | `sim(·, ·)` | **相似度来源**，返回值 ∈ `[0, 1]`（见下） |
//! | `λ` | 相关度权重 ∈ `[0, 1]`：`λ = 1` ⇒ 纯按分数（**退化为「原样取前 k」**）；`λ = 0` ⇒ 纯按多样性 |
//!
//! ⚠️ **并列入选的 tie-break**：`mmr` 相等时按 `chunk_id` **升序**取，保证输出**确定**（NFR-06 同款口径）。
//!
//! # 🔴 本实现**改变 `hits` 的排序依据**（`D-S10-09` / 架构 `R62`）
//!
//! 输出按 **MMR 选择顺序**排列 —— 它**不再是** `score` 降序（多样性序本来就要求「高分但不冗余」
//! 的条目插到前面）。这正是 [`Hit::score`] 的 rustdoc 所声明的「`hits` 恒按本字段降序」被
//! **有意打破**的一档 ⇒ 按 `D-S10-09` 的三条处置：① [`crate::query::response::Explain::mmr_selected`]
//! 留**可观测信号**；② **默认关**（不调用 [`crate::search::SearchIndexBuilder::mmr`] 即 `post: None`）；
//! ③ [`Hit::score`] 的 rustdoc 已重写、写明三档语义。
//!
//! ⚠️ **不改 `Hit::score` 本身**（与 [`super::TimeDecay`] 不同）：MMR 只改**次序**、不改分值 ——
//! 所以「`score` 与 `fused_score` 的关系」不受影响，受影响的只有「`hits` 的序」。
//!
//! # 相似度来源（`D-S10-08`：**起步取文本侧**；来源可替换 ⇒ `S10-T16`）
//!
//! | 构造入口 | 相似度来源 | 用途 |
//! | --- | --- | --- |
//! | [`Mmr::new`] | **文本侧**：`Analyzer::analyze_doc` 的 term 集合 **Jaccard** | 设计 §4.4 的「起步」取形（零新 API、对 `mode` 无偏） |
//! | [`Mmr::with_similarity`] | 调用方注入（[`SimilaritySource`]） | 测试注入桩（`S10-T16`）；将来若要**向量侧**（doc↔doc 余弦）也走这里 |
//!
//! ⚠️ **文本侧是「词面」不是「语义」**（`R44` 一族的口径）：同义改写不会被判为冗余 —— 这是
//! `D-S10-08` 明确登记的代价，**不**在本阶段解决（设计 §9.2 的 `Q10-2`）。
//!
//! ⚠️ **成本**：`sim` 的比较次数是 `O(k · W)`（`W` = 窗口条数），每次比较可能触发 `2` 次分词；
//! [`Mmr::new`] 的默认来源**带按正文串的缓存** ⇒ 默认路径下每条正文**只分词一次**（O(W)），
//! 注入来源则由调用方自理（契约不承诺缓存）。
//!
//! # 窗口必须放大（否则空转，架构 `R61` 同族）
//!
//! MMR 要从**比 `k` 多**的候选里挑出「多样的一组」；若窗口里恰好只有 `k` 条，选 `k` 条
//! 等于「只重排」、多样性无从发生。⇒ [`PostProcessor::candidate_window`] 返回
//! **`pool.max(k)`**（`pool` = 构造时显式给定的候选池大小，见 [`Mmr::new`]）。
//!
//! ⚠️ 与 [`super::TimeDecay`] 的对比：衰减**不需要**额外候选（用 provided 默认 = `k`），
//! MMR **需要** ⇒ 二者对 `candidate_window` 的取形相反，这是**语义决定**而非风格。
//!
//! # 默认关（零行为变化）
//!
//! 只有调用方显式启用才生效 ⇒ 未启用时全链路与引入前**逐位一致**（`S10-T9`）。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::analyze::Analyzer;
use crate::error::{Error, Result};
use crate::post::PostProcessor;
use crate::query::response::Hit;

/// 相似度来源：给定两条正文，返回 `[0, 1]` 的相似度。
///
/// ⚠️ **返回 `[0, 1]` 是契约**：越界或 `NaN` 会被 [`Mmr::process`] **夹到 `[0, 1]`**
/// （`NaN` 视作 `0.0`）—— 否则 `λ` 的语义会失真（`rel` 与 `sim` 必须同量纲才能线性折中）。
pub type SimilaritySource = Arc<dyn Fn(&str, &str) -> f32 + Send + Sync>;

/// MMR 去重（语义、公式与两条契约见模块文档）。
pub struct Mmr {
    /// 相关度权重 λ ∈ `[0, 1]`
    lambda: f64,
    /// 候选池大小（`candidate_window` 的上界；建议 **> `k`** 才有多样性空间）
    pool: usize,
    /// 相似度来源（默认 = 文本侧 term 集合 Jaccard，见 [`Mmr::new`]）
    similarity: SimilaritySource,
}

impl Mmr {
    /// 文本侧默认构造：用 `analyzer` 的 term 集合算 **Jaccard**（`D-S10-08` 的「起步」取形）。
    ///
    /// - `lambda` = 相关度权重 ∈ `[0, 1]`（`1.0` ⇒ 纯按分数）；
    /// - `analyzer` = 分词器（与建库时**同一实现**才保证 term 口径一致）；
    /// - `pool` = **候选池大小**（[`PostProcessor::candidate_window`] 返回 `pool.max(k)`）。
    ///   建议取 `k` 的若干倍（如 `4 * k`）；传 `0` 时退化为「窗口 = `k`」（不报错，但多样性空间为零）。
    ///
    /// ⚠️ **构造期不校验** `lambda`（有限性 / 区间在 [`PostProcessor::process`] 里上抛 `Err`）
    /// —— 与 [`super::TimeDecay::new`] 同口径：校验放到真正消费它的那一刻。
    pub fn new(lambda: f64, analyzer: Arc<dyn Analyzer>, pool: usize) -> Self {
        Self::with_similarity(lambda, text_jaccard_source(analyzer), pool)
    }

    /// 注入相似度来源的构造（`S10-T16`：为「来源可替换」留缝；将来上向量侧走这里）。
    ///
    /// `pool` 语义同 [`Mmr::new`]。
    pub fn with_similarity(lambda: f64, similarity: SimilaritySource, pool: usize) -> Self {
        Self {
            lambda,
            pool,
            similarity,
        }
    }
}

impl PostProcessor for Mmr {
    fn name(&self) -> &'static str {
        "mmr"
    }

    /// 返回 `pool.max(k)` —— MMR 要**比 `k` 多**的候选才有多样性空间（见模块文档）。
    fn candidate_window(&self, k: usize) -> usize {
        self.pool.max(k)
    }

    fn process(&self, hits: Vec<Hit>, k: usize) -> Result<Vec<Hit>> {
        if !self.lambda.is_finite() || !(0.0..=1.0).contains(&self.lambda) {
            return Err(Error::InvalidInput(format!(
                "MMR：lambda 必须是 [0, 1] 内的有限数（实得 {}）",
                self.lambda
            )));
        }
        // `k == 0` ⇒ 一条都不要（与 `search_parts` 的退化输入护栏同口径）。
        if k == 0 || hits.is_empty() {
            return Ok(Vec::new());
        }

        // ⚠️ 用 `Option` 槽位（而不是 `clone`）：选中即 `take()` ⇒ 输出**零拷贝**。
        // ⚠️ 另设 `picked` 标记「本项已被选中」—— **不能**在循环里就把槽位 `take()` 掉，
        //    因为后续轮次的 `max_sim` 还要读**已选项的正文**（take 掉就读不到了）。
        let n = hits.len();
        let mut slots: Vec<Option<Hit>> = hits.into_iter().map(Some).collect();
        let target = k.min(n);
        let mut selected: Vec<usize> = Vec::with_capacity(target);
        let mut picked = vec![false; n];

        while selected.len() < target {
            let mut best: Option<(usize, f64)> = None;
            for i in 0..n {
                if picked[i] {
                    continue; // 已被选中
                }
                let slot = slots[i].as_ref().expect("picked[i] == false ⇒ 槽位仍持有");
                let rel = slot.score as f64;
                // `max_{s ∈ selected} sim(c, s)`；selected 为空 ⇒ 0（公式约定）。
                let mut max_sim = 0.0f64;
                for &s in &selected {
                    let other = slots[s]
                        .as_ref()
                        .map(|h| h.text.as_str())
                        .unwrap_or_default();
                    let raw = (self.similarity)(slot.text.as_str(), other) as f64;
                    // 契约外的返回（NaN / 越界）**夹到 [0, 1]** 而不是上抛：
                    // 相似度是**启发式输入**，越界不构成「调用方参数非法」（与 lambda 不同）。
                    let sim = if raw.is_nan() {
                        0.0
                    } else {
                        raw.clamp(0.0, 1.0)
                    };
                    if sim > max_sim {
                        max_sim = sim;
                    }
                }
                let mmr = self.lambda * rel - (1.0 - self.lambda) * max_sim;
                // ⚠️ tie-break = `chunk_id` **升序**（NFR-06 同款）⇒ 输出确定。
                let better = match best {
                    None => true,
                    Some((bi, bs)) => {
                        mmr > bs
                            || (mmr == bs && slot.chunk_id < slots[bi].as_ref().unwrap().chunk_id)
                    }
                };
                if better {
                    best = Some((i, mmr));
                }
            }
            // `selected.len() < target ≤ n` ⇒ 必有未取项 ⇒ `best` 恒 `Some`。
            let (idx, _) = best.expect("selected.len() < n ⇒ 必有未选中的候选");
            picked[idx] = true;
            selected.push(idx);
        }

        let mut out = Vec::with_capacity(selected.len());
        for (order, &i) in selected.iter().enumerate() {
            if let Some(mut h) = slots[i].take() {
                // 信号：`Some(序位)` ⟺ 本条由 MMR 按多样性序选中（`D-S10-09` ①）。
                h.explain.mmr_selected = Some(order as u32);
                out.push(h);
            }
        }
        Ok(out)
    }
}

/// term 集合（按正文串缓存 ⇒ 默认路径下每条正文**只分词一次**）。
type TermCache = Arc<Mutex<HashMap<String, Arc<HashSet<String>>>>>;

/// 文本侧相似度来源：`analyzer` 的 term 集合 **Jaccard**。
fn text_jaccard_source(analyzer: Arc<dyn Analyzer>) -> SimilaritySource {
    let cache: TermCache = Arc::new(Mutex::new(HashMap::new()));
    Arc::new(move |a: &str, b: &str| {
        let sa = cached_terms(&cache, &*analyzer, a);
        let sb = cached_terms(&cache, &*analyzer, b);
        jaccard(&sa, &sb)
    })
}

/// 取（并缓存）一条正文的 term 集合。
///
/// ⚠️ 锁中毒时**不 panic**（`into_inner` 取回内部值）：缓存只是加速手段，
/// 中毒不影响正确性 —— 这里没有「静默降级」问题（NFR-07 针对的是**结果**的失真）。
fn cached_terms(cache: &TermCache, analyzer: &dyn Analyzer, text: &str) -> Arc<HashSet<String>> {
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hit) = guard.get(text) {
        return Arc::clone(hit);
    }
    let set: HashSet<String> = analyzer
        .analyze_doc(text)
        .into_iter()
        .map(|t| t.term.to_string())
        .collect();
    let set = Arc::new(set);
    guard.insert(text.to_string(), Arc::clone(&set));
    set
}

/// Jaccard 相似度 ∈ `[0, 1]`。
///
/// ⚠️ **两条都无词项 ⇒ `0.0`**（不是 `1.0`）：「空文档之间没有相似性证据」，
/// 取 `1.0` 会让空串候选把整组判断带偏（`K` 条空文档会互相「完全相似」）。
/// 该边界由 `S10_T15` 钉住。
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}
