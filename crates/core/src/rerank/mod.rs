//! 精排：`Reranker` trait + 两个实现（`NoOpReranker` / `LocalReranker`）。
//!
//! # 边界（架构文档 4.2）
//!
//! 精排位于**融合之后**（[`crate::query::searcher::search_parts`]），因此**与 `mode`
//! 无关**：`bm25` / `vector` / `hybrid` 三条路都会经过它（D-S7-09）。⇒ 单路模式下
//! 精排一旦生效，`Hit.score` 的语义也随之改变（见 D-S7-05）。
//!
//! # 两个实现
//!
//! | 实现 | feature | 说明 |
//! | --- | --- | --- |
//! | [`NoOpReranker`] | 无（恒可用） | 原样返回：调用链完整、零副作用，**默认装配**（D-S7-04） |
//! | `LocalReranker` | `local-rerank`（**非默认**） | `bge-reranker-v2-m3` 交叉编码器；模型 ≈2.19GB ⇒ 不随默认构建 |
//!
//! 精排**默认关**：零配置仍是 `NoOpReranker`（D-S7-04，理由 = 收益未证 + 首次要下
//! 2.19GB + 端到端 P99 会从 ~3ms 涨到百毫秒级 ⇒ NFR-02 名义失效）。
//!
//! # 窗口：精排器 → 编排层的单向通道（H3 / D-S7-01 / D-S7-02）
//!
//! 编排层在**两路召回之前**就得知道「要给精排留多少候选」，所以窗口必须是
//! [`Reranker`] 的**策略**而不是编排层的常数 ⇒ [`Reranker::candidate_window`]。
//! 它是 **provided** 方法（默认 `k`）⇒ 既有实现与下游自定义实现**一行不改**、
//! 未装精排器时全链路与 Step 6 **逐位一致**。
//!
//! ⚠️ 编排层会把它与候选池**联动**（`candidate_k = max(3k, w, 10)`）：只把截断
//! 从 `k` 改成 `R` 而不管候选池，窗口会被 `candidate_k` **静默封顶**
//! （`R=100` / `k=10` ⇒ 实际只有 30 条）——设计 §2.2 的设计期新发现 A。

mod noop;

#[cfg(feature = "local-rerank")]
mod local;

/// 纯策略层（分数变换 / 按 `index` 回填 / 排序 / 截断 / 身份字符串）。
///
/// ⚠️ 只在 `local-rerank` 或**测试构建**下编译：它存在的唯一理由是给
/// `LocalReranker` 提供**不依赖 fastembed 的注入接缝**（设计 §3.5），
/// 使策略能在 CI 里用可控打分序列秒级钉住。默认（非测试、非 `local-rerank`）
/// 构建里不编译它 ⇒ 不留无用代码面。
#[cfg(any(feature = "local-rerank", test))]
mod scoring;

pub use noop::NoOpReranker;

#[cfg(feature = "local-rerank")]
pub use local::{LocalReranker, DEFAULT_RERANK_MAX_LENGTH, DEFAULT_RERANK_WINDOW};

use crate::error::Result;
use crate::query::response::Hit;

/// 重排抽象：在粗召回 + 融合之后，对候选重新排序。
///
/// 默认装配 [`NoOpReranker`]（D-S7-04）；真实模型见 `LocalReranker`（feature
/// `local-rerank`）。
pub trait Reranker: Send + Sync {
    /// 精排**希望拿到**的候选条数（供编排层在召回**之前**决定候选池）。
    ///
    /// 语义：返回值 `w` 表示「请把融合结果的前 `max(k, w)` 条交给我」。
    ///
    /// ⚠️ 默认实现返回 `k` —— 即**不放开窗口**。这保证：
    /// ① 既有实现（[`NoOpReranker`] 与下游自定义实现）**一行不改**；
    /// ② 未装精排器时，`candidate_k` / 截断 / `hits` **与之前逐位一致**。
    ///
    /// ⚠️ 上层会把它与 `candidate_k = max(3k, w, 10)` **联动**：返回 `w > 3k` 时
    /// 候选池会随之放大 —— 否则窗口被静默封顶（见模块文档）。
    ///
    /// # ⚠️ 返回值的上界由**实现**负责（编排层不设防）
    ///
    /// 编排层把返回值**原样**放进 `candidate_k` 并**原样下传**给向量后端
    /// （`S7_T3` 用「后端实际收到的 `k`」钉住了这一点）。因此：
    ///
    /// - **返回值应当是合理的候选规模上界**（建议 ≤ 语料规模 `Index::num_chunks()`）；
    /// - 失控的大数（如 `usize::MAX`）在 **ANN 路径**上会传到 `hnsw_rs`，而那里
    ///   `ef = ef_arg.max(knbn)` 会把库内的 `ef` 上限（`EF_FILTER_MAX = 256`）
    ///   **抵消**，进而落在 `BinaryHeap::with_capacity(ef)` 上 —— 实测该算式在
    ///   `n` 溢出时 debug 直接 panic、`usize::MAX` 时会以极大的容量申请内存
    ///   （PR #53 评审 P3-3 的加强版：**不是在 `HnswRsIndex` 的 clamp 链上被挡住**）。
    /// - ⚠️ 该风险**不是本 trait 引入的**：`candidate_k` 在本 trait 存在之前就无上限
    ///   （`k.saturating_mul(3)`，见 `011bb16`）⇒ 库内两个实现都安全，但**公开 trait 的
    ///   第三方实现**会走这条路径。运行期钳制**本 PR 不做**（属 `vector/` 模块 + 会与
    ///   设计 §4.3.1 的公式产生偏差），已单独挂账：**issue #54**。
    fn candidate_window(&self, k: usize) -> usize {
        k
    }

    /// 对候选重排，返回前 `top_n` 条。
    ///
    /// # 入参契约（V2 Step 7 / D-S7-06）
    ///
    /// ⚠️ `hits` 的长度是**候选窗口**（`min(max(k, candidate_window(k)), 融合条数)`），
    /// 可能**大于** `top_n`；实现**必须**按自己的打分把结果排好并截到 `top_n`。
    ///
    /// ⚠️ **读侧**：**不得依赖 `hits[i].explain`**。为了省掉 `(窗口 − k) × 整段分词` 的
    /// 成本，`explain` 的 `matched_terms` / 各 lane 的 rank/score 被**推迟到精排截断之后**
    /// 才组装（设计 §4.3.3）⇒ 实现拿到的 `explain` 可能**只有 `fused_score`**。
    /// 需要正文请用 `hits[i].text`。这是**有意的接口收缩**，不是遗漏。
    ///
    /// # 出参契约（**写侧** —— PR #52 评审意见 1 的落地）
    ///
    /// 返回类型只有 `Vec<Hit>`，而 `Hit` 里除 `score` 外**唯一能携带额外信息**的字段就是
    /// `explain` ⇒ 实现**可以且应当**通过**写** `hits[i].explain` 把「原始分」归还编排层
    /// （`LocalReranker` 的做法见 D-S7-05 / §4.4.3：`score = σ(logit)`，原始 logit 进
    /// `explain.rerank_score`）。理由是 `σ` **不可逆** ⇒ 编排层**自己算不出来**原始分，
    /// 必须由实现写回。**「不得依赖 `explain`」只约束读侧**，与本节不冲突。
    ///
    /// ⚠️ **编排层的义务**：按 D-S7-06 在精排**之后**补齐 `explain`（`matched_terms` /
    /// lane rank/score）时，**必须保留**精排器已写入的字段（`rerank_score` 等）——否则
    /// 「谁后写谁生效」，把精排器写回的信号**冲掉**。本 trait 是 C1 与 C2 之间的**唯一**
    /// 契约面：C1 = 精排器写、C2 = 编排层补，**顺序固定为 C1 → C2**，C2 不得覆盖 C1 的键。
    ///
    /// # 分数语义（D-S7-05）
    ///
    /// 实现若替换了 `Hit::score` 的含义，**必须**让这件事可观测 ——「分数被替换」**不得静默**
    /// （NFR-07）。**信号的定义**：`explain.rerank_score.is_some()` ⟺ 「该条的 `score` 由本
    /// 实现替换过」；未替换的条（含实现**没给分**的候选）应保持 `rerank_score == None`
    /// —— 这就是该信号能被下游当作判据的原因。
    fn rerank(&self, query: &str, hits: Vec<Hit>, top_n: usize) -> Result<Vec<Hit>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **S7-T1（trait 侧零回归）**：`candidate_window` 的**默认实现必须等于 `k`**。
    ///
    /// 这是「未装精排器 ⇒ 全链路逐位一致」的**唯一**依据：只要默认值不是 `k`，
    /// 编排层的 `candidate_k` 与截断就会跟着变。
    #[test]
    fn 默认窗口等于k() {
        let r = NoOpReranker;
        for k in [1usize, 10, 100] {
            assert_eq!(r.candidate_window(k), k, "默认实现不得改变候选池");
        }
    }

    /// 空输入：`NoOpReranker` 原样返回空（`take` 不 panic）。
    #[test]
    fn noop空输入不panic() {
        let out = NoOpReranker.rerank("q", vec![], 10).unwrap();
        assert!(out.is_empty());
    }
}
