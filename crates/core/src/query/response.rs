//! 结构化返回类型：`Hit` / `Explain` / `SearchResponse` / `EmptyReason`。
//!
//! 这是架构文档 5.8 的唯一定义源落地，语义务必严格对齐（见各字段注释）。

use std::time::Duration;

use crate::types::{ChunkId, DocId, Score};

use super::metrics::Metrics;

/// 一条最终命中结果（已回捞正文与元数据）。
#[derive(Debug, Clone)]
pub struct Hit {
    /// 命中的分片 ID（检索的最小单位）
    pub chunk_id: ChunkId,
    /// 所属文档 ID（一个文档可能含多个分片）
    pub doc_id: DocId,
    /// **当前排序依据**（`hits` 恒按本字段降序；同分按 `chunk_id` 升序，NFR-06）。
    ///
    /// # ⚠️ 取值随「精排开 / 关」而变（V2 Step 7 / D-S7-05 / D-S7-09）
    ///
    /// | 精排 | 三种 mode 下的取值 |
    /// | --- | --- |
    /// | **关**（默认，D-S7-04） | 融合分（`Hybrid`）/ 单路分（`bm25` / `vector`）—— 与精排引入前**逐位一致** |
    /// | **开** | `σ(logit)` = `1 / (1 + e^(−logit))` ∈ `[0, 1]`；**三种 mode 一视同仁**（精排在融合之后，D-S7-09） |
    ///
    /// # ⚠️ 「`hits` 恒按本字段降序」有三档例外（V2 Step 10 / D-S10-09 / 架构 R62）
    ///
    /// 后处理阶段（见 [`crate::post`]）里的两个机制会**改变次序**（且都**默认关**、都留信号）：
    ///
    /// | 机制 | 对本字段 | 对 `hits` 的次序 | 信号 |
    /// | --- | --- | --- | --- |
    /// | [`crate::post::TimeDecay`] | **改写**（乘衰减系数） | 仍按本字段降序（改写后**重排**） | [`Explain::decay_factor`] |
    /// | [`crate::post::Mmr`] | **不改** | ⚠️ **不再按本字段降序** —— 按 MMR 的选择序（多样性序） | [`Explain::mmr_selected`] |
    ///
    /// ⇒ 于是「`hits` 恒按本字段降序」在 **MMR 生效时不再成立**。下游若据本字段的**单调性**
    /// 做假设（设阈值 / 二分 / 短路 / 缓存排序结果），**必须先看 [`Explain::mmr_selected`]**
    /// 是否为 `Some` —— 它是「次序被有意打破」的唯一信号（NFR-07 / R46 一族）。
    ///
    /// ⚠️ **MMR 不是最后一道排序**：后处理固定在**精排之前**（`D-S10-02`）⇒ 会重排的精排器
    /// （如 `LocalReranker`）会在其后**再按本字段排一次**（此时 `mmr_selected` 的序位与
    /// `hits` 的下标**错开**，见该字段的 rustdoc）。两者都**不**使本条恢复「降序」保证。
    ///
    /// ⚠️ **不可逆**：`σ` 不是单射，无法从本字段还原融合分。想知道融合分读
    /// [`Explain::fused_score`]；想知道精排原始分读 [`Explain::rerank_score`]。
    ///
    /// ⚠️ **本次到底换没换，看 [`Explain::rerank_score`]`.is_some()`** —— 「分数被替换」
    /// **不得静默**（NFR-07）。下游若用本字段设阈值、跨配置比较分数或缓存排序结果，
    /// **必须先看那个信号**，否则会以完全不同的量纲工作（架构 R46）。
    pub score: Score,
    /// 分片正文
    pub text: String,
    /// 出处：文件路径 / URL / 标题——溯源用（FR-12）
    pub source: String,
    /// 业务自定义元数据（过滤用，FR-14）
    pub metadata: serde_json::Value,
    /// 命中解释：为什么召回这条（FR-13）
    pub explain: Explain,
}

impl Hit {
    /// 直接产出可拼进 prompt 的上下文块，带出处标注（FR-12）。
    pub fn to_context_block(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("[来源: {}]\n", self.source));
        s.push_str(&self.text);
        if !self.explain.matched_terms.is_empty() {
            s.push_str(&format!(
                "\n(命中词: {})",
                self.explain.matched_terms.join(", ")
            ));
        }
        s
    }
}

/// 命中解释：告诉 Agent "为什么召回这条"（FR-13 / 架构 4.5）。
#[derive(Debug, Clone, Default)]
pub struct Explain {
    /// 查询分词中真正命中该分片的词（BM25 才有意义，向量路为空）
    pub matched_terms: Vec<String>,
    /// BM25 路分数；`None` = 该 lane 未召回此文档（诊断信号，勿用 0 或 usize::MAX）
    pub bm25_score: Option<Score>,
    /// BM25 路排名（从 1 起）；`None` 同 `bm25_score`
    pub bm25_rank: Option<u32>,
    /// 向量路余弦相似度；`None` = 未召回
    pub vector_score: Option<Score>,
    /// 向量路排名（从 1 起）；`None` 同 `vector_score`
    pub vector_rank: Option<u32>,
    /// 融合后的最终分数（**恒为融合/单路分，精排不改写它**，D-S7-05）
    pub fused_score: Score,
    /// 精排器的**原始分**（`LocalReranker` = 原始 logit）；`None` = 本条**未被精排替换**。
    ///
    /// # 这是「`score` 语义被替换」的可观测信号（D-S7-05 / NFR-07）
    ///
    /// `is_some()` ⟺ 该条的 [`Hit::score`] 由精排器写入。因为 `score = σ(logit)` 而
    /// **`σ` 不可逆**，原始分只能由精排器**写回**（见 `Reranker::rerank` 的出参契约），
    /// 编排层**自己算不出来**。
    ///
    /// ⚠️ `None` 有两种来源，必须**连看配置**才能区分：
    /// ① 精排未生效（`NoOpReranker`，D-S7-04 的默认 ⇒ 恒 `None`）；
    /// ② 精排生效但**该条没被给分**（精排器覆盖不足时走「保持输入分」路径，
    ///    见 `rerank::scoring::apply_scores`）。
    ///
    /// ⚠️ **别用 `0.0` / `1.0` 当「没打分」的哨兵**：σ 在 f32 下会**精确饱和**到这两个值
    /// （`σ(16.7) == 1.0`、`σ(−89.0) == 0.0`，实测）⇒ 哨兵与真值**不可区分**。
    ///
    /// ⚠️ 本字段是**加在公开结构体上的新字段**（`Explain` 字段全 `pub`）⇒ 下游以字面量
    /// 构造 `Explain` 的代码会编译失败（破坏性，见 CHANGELOG）。库内构造点已同步。
    pub rerank_score: Option<Score>,
    /// 时间衰减的**改写系数**（V2 Step 10 / `S10-04`；`Some(f)` ⟺ 该条**被衰减改写**）。
    ///
    /// 语义：后处理阶段把 `hit.score` 乘上 `f = exp(−λ · age_secs)`（λ = 调用方给的衰减率、
    /// `age_secs` = 「现在 − 文档时间戳」，**向下钳到 0**）⇒ 于是
    /// `hit.score == fused_score × f`。**`lambda`/`age`/`f` 的完整定义与三条实现期定值
    /// 见 [`crate::post::TimeDecay`]**。
    ///
    /// ⚠️ `f == 1.0`（`age == 0` 或 `λ == 0`）时**分数逐位不变** ⇒ 本字段保持 `None`
    /// （即「`is_some()` ⟺ 真的被改写」）—— 与 [`Self::rerank_score`] 的口径同款。
    ///
    /// ⚠️ `None` 有**三种**来源，必须**连看配置**才能区分：
    /// ① **未启用**时间衰减（默认 ⇒ 恒 `None`）；② 启用但本条 `f == 1.0`（没被改写）；
    /// ③ 该条**没走到**后处理阶段（三条早退 / 过滤清空）。
    ///
    /// ⚠️ 本字段是**加在公开结构体上的新字段**（`Explain` 字段全 `pub`）⇒ 下游以字面量
    /// 构造 `Explain` 的代码会编译失败（破坏性，见 CHANGELOG）。
    pub decay_factor: Option<Score>,
    /// MMR 去重的**多样性序位次**（V2 Step 10 / `S10-05`）：
    /// `Some(i)` ⟺ 本条由 MMR **按多样性序**选中，且它是该序的第 `i` 条（**0-based**）；
    /// `None` = 本条**未经过** MMR（未启用 / 不在窗口内 / 该条落在 `k` 之外被丢弃）。
    ///
    /// # 语义
    ///
    /// MMR 对窗口内候选做贪心选择（`λ·rel − (1−λ)·max_sim`，见 [`crate::post::Mmr`]），
    /// 其**输出**按该选择顺序排列 ⇒ 于是 `hits[i].explain.mmr_selected == Some(i as u32)`
    /// 在**后处理刚结束时**成立。
    ///
    /// 🔴 **但 `hits` 的最终次序可能被精排再改一次**（`D-S10-02` 把后处理**固定**在
    /// 「回捞之后、**精排之前**」）⇒ 本字段记的是**精排前**那个快照的序位：
    ///
    /// | 配置 | `hits[i].mmr_selected == Some(i)` |
    /// | --- | --- |
    /// | MMR 生效 + **默认 `NoOpReranker`**（不改次序） | ✅ **成立**（恒等） |
    /// | MMR 生效 + **会重排的精排器**（如 `LocalReranker`，其 `apply_scores` 按 `score` 重排） | ❌ **不成立** —— 序位与**位置**错开 |
    ///
    /// ⇒ **本字段的语义是「本条在 MMR 多样性序里排第几」，不是「本条落在 `hits` 的第几位」。**
    /// 上面那条恒等式只是「精排不重排」时的**特例**，**不可**当作普适断言写进下游代码。
    /// 即使在错开的配置下，`is_some()` 仍**可靠**地回答「本条由 MMR 按多样性序选中」——
    /// 这正是 `D-S10-09` ① 要求可观测的那件事。
    ///
    /// ⚠️ 之所以**不**在精排后把错位的信号改写为 `None`：那会**丢掉**「本条被 MMR 选中」
    /// 这条信息本身，而它恰是 [`Hit::score`] 排序依据被打破的**唯一**证据（`R62` 的处置）。
    ///
    /// ⚠️ **这是「`hits` 排序依据被有意打破」的可观测信号**（`D-S10-09` ① / 架构 `R62`）：
    /// [`Hit::score`] 的 rustdoc 声明「`hits` 恒按本字段降序」，而 MMR 生效时**不再成立**
    /// （高分但冗余的条目会被排到后面）—— 下游若据 `score` 单调性做假设，**必须先看本字段**。
    ///
    /// ⚠️ 与 [`Self::decay_factor`] 的差别：那个信号说明「**分数被改写**」，本信号说明
    /// 「**次序被重排**」（MMR **不改** `Hit::score`）。两者**可以同时**为 `Some`（实现自定）。
    ///
    /// ⚠️ `None` 有**三种**来源，必须**连看配置**才能区分：
    /// ① **未启用** MMR（默认 ⇒ 恒 `None`）；② 该条**没走到**后处理阶段（三条早退 / 过滤清空）；
    /// ③ MMR 生效但该条**未入选**（窗口里排在前 `k` 之外）。
    ///
    /// ⚠️ 本字段是**加在公开结构体上的新字段**（`Explain` 字段全 `pub`）⇒ 下游以字面量
    /// 构造 `Explain` 的代码会编译失败（破坏性，见 CHANGELOG）。库内构造点已同步。
    pub mmr_selected: Option<u32>,
}

/// 检索响应。
#[derive(Debug, Clone)]
pub struct SearchResponse {
    /// 最终命中列表（按分数降序）
    ///
    /// ⚠️ 「分数」的含义随精排开关而变（V2 Step 7 / D-S7-09）⇒ 排序**依据**仍是
    /// [`Hit::score`]，但该字段在精排生效时代表 `σ(logit)` 而非融合分。
    /// 本次是否替换由 [`Explain::rerank_score`] 判定 —— 详见 [`Hit::score`]。
    pub hits: Vec<Hit>,
    /// 融合阶段看到的候选总数（bm25 候选 + vector 候选去重后）
    ///
    /// 恒等于 `metrics.candidates`（V2 Step 5 / I7）。
    pub total_candidates: usize,
    /// 空结果时说明原因，供 Agent 决策（FR-13）
    pub empty_reason: Option<EmptyReason>,
    /// 本次检索耗时（可观测性，NFR-07）
    ///
    /// 恒等于 `metrics.took`（V2 Step 5 / I7）。本字段保留是因为下游已在用；
    /// **空结果路径**的两个值都由 `empty_response` 一并填真值。
    pub took: Duration,
    /// 本次检索的**内核指标**（V2 Step 5 / D-S5-05，NFR-07）。
    ///
    /// 从 Step 5 起 `Metrics` 有三条通道（响应 / `tracing` / bench 采集），
    /// 本字段是给**调用方**的那一条：只有进了响应，外部才能单测它、
    /// bench 才能聚合它——而这正是 `vector_shortfall`（V2.1 prefilter 判据）
    /// 与 Step 6（NFR-10/11）实测的**前置**。
    ///
    /// # ⚠️ 破坏性变更
    ///
    /// `SearchResponse` 是公开结构体且字段全 `pub`，**新增字段会让一切以字面量
    /// 构造它的下游代码编译失败**（库内两处构造点均由内核自己产出，零影响）。
    /// 0.x 阶段按既有约定接受，见 CHANGELOG 的 `⚠️ 破坏性` 段。
    ///
    /// # 口径自洽（I7）
    ///
    /// `metrics.took == took`、`metrics.candidates == total_candidates`。
    pub metrics: Metrics,
}

/// 空结果原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyReason {
    /// 索引为空
    NoDocuments,
    /// 词全部未命中 —— 提示 query 可能含幻觉词
    AllTermsUnmatched,
    /// 有候选但被 filter 全部过滤（P4 过滤落地后启用）
    FilteredOut,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_block含出处与正文() {
        let hit = Hit {
            chunk_id: 0,
            doc_id: 0,
            score: 1.0,
            text: "中文分词是检索的第一步".into(),
            source: "chinese-tokenization.md".into(),
            metadata: serde_json::json!({}),
            explain: Explain {
                matched_terms: vec!["分词".into(), "检索".into()],
                ..Default::default()
            },
        };
        let block = hit.to_context_block();
        assert!(block.contains("[来源: chinese-tokenization.md]"));
        assert!(block.contains("中文分词是检索的第一步"));
        assert!(block.contains("分词"));
    }
}
