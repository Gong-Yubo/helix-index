//! 后处理阶段：`PostProcessor` trait —— 时间衰减 / MMR / token budget 的**共同通道**。
//!
//! # 位置（V2 Step 10 / D-S10-02，**由源码限死**）
//!
//! 后处理在**窗口回捞之后、精排之前**执行：
//!
//! ```text
//! ① 召回 → ② 融合 → ③ 窗口回捞（→ 带 metadata / text 的 Hit）→ ④ 后处理【本模块】→ ⑤ 精排 → ⑥ 补 explain
//! ```
//!
//! ⚠️ **不能**把后处理做成 [`crate::fusion::FusionStrategy`] 的一个方法：`fusion` 模块的硬约束是
//! **只操作 `(chunk_id, score)`、不回捞正文**（模块头），而时间衰减要读时间字段、MMR 要读正文、
//! token budget 要数正文 ⇒ 那三件事在融合层**结构上做不到**。
//!
//! ⚠️ **也不**借用 [`crate::rerank::Reranker`] 通道（D-S10-01 的 A 案否决理由）：
//! ① 语义借用会让 `Metrics.rerank_*` / `Explain.rerank_score` 失真；
//! ② 编排层只有**一个** `reranker` 位 ⇒ MMR 与真精排**不能并存**，而 FR-24 / FR-25 与 FR-18
//! 是**独立**需求。
//!
//! # 与 `Reranker` 的次序是**固定**的
//!
//! 后处理 → 精排。若要「精排之后再 MMR」需**另立**阶段；本步**不做**（登记为设计 §9.2 的 `Q10-5`）。
//!
//! # 默认关（零行为变化）
//!
//! 编排层的 `SearchParts::post` 默认 `None` ⇒ **本阶段整个不执行**，全链路与引入前**逐位一致**
//! （同 `NoOpReranker` 的处置口径，D-S7-04 先例）。
//!
//! # 已有实现（V2 Step 10）
//!
//! - [`TimeDecay`]（**时间衰减**，`S10-04` / `FR-33`）—— 语义、公式与三条定值（时钟源 / 缺字段上抛 /
//!   改写后重排）见 [`time_decay`]。⚠️ 它**不改窗口**（用 [`PostProcessor::candidate_window`] 的
//!   **provided 默认** = `k`）：衰减只重加权已有候选，不需要额外候选。
//! - [`Mmr`]（**结果去重**，`S10-05` / `FR-24`）—— 语义、公式与相似度来源见 [`mmr`]。
//!   ⚠️ 与 `TimeDecay` **相反**：它**必须放大窗口**（要从比 `k` 多的候选里挑多样的一组，
//!   否则多样性无从发生）⇒ 显式覆盖 [`PostProcessor::candidate_window`]。⚠️ 它还**改变
//!   `hits` 的排序依据**（输出按 MMR 选择顺序，不再是 `score` 降序）⇒ 属 `D-S10-09` /
//!   架构 `R62` 明文处置的一档。
//! - [`TokenBudget`]（**预算裁剪**，`S10-06` / `FR-25`）—— 计数口径（注入计数器 / 默认字符数）、
//!   超预算行为（**顺序截断**）与两条禁止见 [`token_budget`]。
//!   ⚠️ 与 `TimeDecay` **同族**：**不改窗口**（预算只裁已有候选 ⇒ 用 provided 默认 = `k`）。
//!   ⚠️ 与 `Mmr` 的**分野**：MMR 改**序**不改分；本阶段改**长度**、不动序也不动分
//!   （第二道截断）⇒ 同样属 `D-S10-09` 的一档。

pub mod mmr;
pub mod time_decay;
pub mod token_budget;

pub use mmr::Mmr;
pub use time_decay::TimeDecay;
pub use token_budget::{CharCounter, TokenBudget, TokenCounter};

use crate::error::Result;
use crate::query::response::Hit;

/// 后处理抽象：在**窗口回捞之后、精排之前**对候选做一次变换。
///
/// 三个已知用途（各自是**独立**的后续任务，本模块只给通道）：
///
/// | 用途 | 需求 | 实现落点 |
/// | --- | --- | --- |
/// | 时间衰减打分钩子 | FR-33 | `S10-04` |
/// | MMR 结果去重 | FR-24 | `S10-05` |
/// | token budget 裁剪 | FR-25 | `S10-06` |
///
/// # 契约
///
/// ## 入参
///
/// `hits` 是**窗口内可回捞**的候选（长度 `≤ max(k, rerank_window, post_window)`，
/// 见 [`crate::query::searcher::search_parts`] 的三条不变式），**已带 `metadata` / `text`**
/// （回捞在**本阶段之前**完成）—— 实现可以自由读它们：时间衰减读 `metadata` 里的时间字段、
/// MMR 读 `text` 算相似度、token budget 数 `text`。
///
/// ⚠️ `hits[i].explain` 在**本阶段之前只带 `fused_score`**（`matched_terms` / 各 lane 的
/// rank 与 score 被推迟到**精排之后**才组装，为省掉「窗口 − k」条整段分词的成本）⇒
/// **读侧不得依赖**那些字段。这是**有意的接口收缩**，与 [`crate::rerank::Reranker::rerank`]
/// 的读侧契约**同款**。
///
/// ## 出参
///
/// 实现**可以且应当**通过**写** `hits[i].explain` 把「本阶段做过什么」归还编排层 ——
/// 编排层在精排之后补 `explain` 时**必须保留**这些字段（与 `Reranker` 的 C1 → C2 顺序同款）。
///
/// ⚠️ **改变了 `hits` 的排序依据或长度就必须可观测**（设计 D-S10-09 / 架构 `R46` 一族）：
/// `Hit::score` 的 rustdoc 声明「`hits` 恒按本字段降序、同分按 `chunk_id` 升序（NFR-06）」，
/// 下游可能据此设阈值 / 跨配置比较 / 缓存排序结果 ⇒ 静默改变是**缺陷**（NFR-07）。
///
/// ⚠️ **返回空是允许的**：编排层会照常产出响应。自 `S10-06`（`PR10-5`）起，
/// 「**入参非空、出参为空**」会被编排层记为
/// [`crate::query::response::EmptyReason::PostEmptied`] —— 设计 §9.2 `Q10-6` 已按用户
/// 2026-10-10 拍板**新增变体**收口（定值 → 设计 §20 `I-37`）。此前该出口**没有原因**
/// （静默空集，来源 = `PR10-2` 第 1 轮评审 `P3-1` / §15 `I-17`）。
/// ⚠️ 变体名**不指名机制**：编排层只看得见「入参非空、出参为空」，分不清是哪个实现清空的。
///
/// ⚠️ **出参长度不设上界**：token budget **必然**会改变长度（它是「第二道截断」），
/// 所以本 trait **不能**照抄 `Reranker::rerank` 的「调用方按 `top_n` 再截一次」安全网。
/// 编排层的唯一硬约束是「不得越界超 `k`」**不适用**于本阶段 —— 但实现仍**必须**把
/// 长度变化通过 `Explain` / `Metrics` 暴露出来。
///
/// ## 失败
///
/// 返回 [`Result`]。**不得静默降级**（NFR-07）：时间字段解析失败 / 预算参数非法这类情况
/// 应上抛 `Err`，而不是「悄悄不处理」—— 后者会让调用方以为机制生效了。
pub trait PostProcessor: Send + Sync {
    /// 本后处理器的名字（诊断 / 观测用；同一装配下应稳定）。
    fn name(&self) -> &'static str;

    /// 后处理**希望拿到**的候选条数（供编排层在召回**之前**决定候选池与回捞窗口）。
    ///
    /// 语义：返回值 `w` 表示「请把融合结果的前 `max(k, w)` 条交给我」。
    ///
    /// ⚠️ 默认实现返回 `k` —— 即**不放开窗口**。这保证既有实现与下游自定义实现**一行不改**。
    ///
    /// ⚠️ **返回值参与两处**（缺一处就会被静默封顶，架构 `R61`）：
    /// ① 候选池 `candidate_k = max(3k, rerank_window, post_window, 10)`；
    /// ② 回捞 `take_n = max(k, rerank_window, post_window)`。
    /// **只做 ①** 时，候选池够大而回捞仍只取 `max(k, rerank_window)` 条 ⇒ 本阶段看到的
    /// 候选数与「没装后处理器」时**一样多** ⇒ 机制空转。
    ///
    /// ⚠️ **返回值的上界由实现负责**（编排层不设防），语义与
    /// [`crate::rerank::Reranker::candidate_window`] 完全一致 —— 包括「失控大数会传到
    /// ANN 路径」那条已知风险（挂账 issue #54）。
    fn candidate_window(&self, k: usize) -> usize {
        k
    }

    /// 对窗口内的候选做一次变换，返回新的候选集。
    ///
    /// `k` 是调用方请求的 `top_n`（= 最终要的条数），不是入参长度。
    fn process(&self, hits: Vec<Hit>, k: usize) -> Result<Vec<Hit>>;
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)] // 中文测试名含英文缩写（S10_T1）

    use super::*;

    /// 空实现（只为验默认方法，不构成可用实现）。
    struct Null;

    impl PostProcessor for Null {
        fn name(&self) -> &'static str {
            "null"
        }
        fn process(&self, hits: Vec<Hit>, _k: usize) -> Result<Vec<Hit>> {
            Ok(hits)
        }
    }

    /// **S10-T1（trait 侧零回归）**：`candidate_window` 的**默认实现必须等于 `k`**。
    ///
    /// 这是「未装后处理器 ⇒ 全链路逐位一致」的**唯一**依据：只要默认值不是 `k`，
    /// 编排层的 `candidate_k` / `take_n` 就会跟着变。
    #[test]
    fn S10_T1_默认窗口等于k() {
        let p = Null;
        for k in [1usize, 10, 100] {
            assert_eq!(p.candidate_window(k), k, "默认实现不得改变候选池与回捞窗口");
        }
        assert_eq!(p.name(), "null");
    }
}
