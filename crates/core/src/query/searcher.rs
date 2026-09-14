//! 编排：把两路召回、融合、精排、回捞串起来。
//!
//! # 边界（架构文档 4.2）
//!
//! 本模块**不自己实现算法**，只调用 `retriever` / `fusion` / `rerank`。
//! 正文回捞只在这里做一次（对 Top-K）。
//!
//! # 结构（P6 / I-01）
//!
//! 编排逻辑的唯一实现是自由函数 [`search_parts`]，依赖打包在 [`SearchParts`]。
//! [`QueryExecutor`]（旧名 `Searcher`）是它的薄壳：**对外签名一字不改**，
//! 供需要逐 lane 自定义组装的调用方使用（逃生舱，见 p6-design 7.3 最后一行）。
//! 门面层的 owned `Searcher`（I-05）同样复用 `search_parts`，不复制编排逻辑。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::analyze::Analyzer;
use crate::embed::Embedder;
use crate::error::{Error, Result};
use crate::fusion::{FusionStrategy, LaneResults, RrfFusion};
use crate::index::Index;
use crate::predicate::CandidateFilter;
use crate::rerank::{NoOpReranker, Reranker};
use crate::retriever::{Bm25Params, Bm25Retriever, Retriever, Scored, VectorRetriever};
use crate::types::{ChunkId, Score};
use crate::vector::{VectorIndex, VectorRoute};

use super::explain::{determine_empty_reason, matched_terms};
use super::metrics::Metrics;
use super::response::{Explain, Hit, SearchResponse};

/// 检索模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// 仅关键词路（BM25）
    Bm25,
    /// 仅语义路（向量近邻检索）
    Vector,
    /// 两路召回后融合（RRF，推荐默认）
    Hybrid,
}

impl SearchMode {
    /// 从字符串解析检索模式（大小写不敏感）。
    ///
    /// 支持 `"bm25"` / `"vector"` / `"hybrid"` 及其变体；
    /// 未知字符串返回 `Err`（Agent 场景 mode 常来自 LLM 输出，需容错提示）。
    pub fn parse(s: &str) -> std::result::Result<SearchMode, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bm25" | "keyword" | "lexical" => Ok(SearchMode::Bm25),
            "vector" | "semantic" | "embedding" => Ok(SearchMode::Vector),
            "hybrid" | "fusion" => Ok(SearchMode::Hybrid),
            other => Err(format!(
                "未知检索模式: {other:?}（支持 bm25 / vector / hybrid）"
            )),
        }
    }
}

/// `search_parts` 的全部依赖（借用型）。
///
/// 两个入口（`QueryExecutor` 与门面层 owned `Searcher`）都把自己的状态
/// 投影成这个结构再调用同一个函数，保证编排逻辑只有一份实现。
pub struct SearchParts<'a> {
    /// 倒排 + 正排 + 统计量
    pub index: &'a Index,
    /// 查询侧分词器（必须与建库侧同款，R4）
    pub analyzer: &'a dyn Analyzer,
    /// 向量化（`None` = 纯 BM25）
    pub embedder: Option<&'a dyn Embedder>,
    /// 向量索引（`None` = 纯 BM25）
    pub vector_index: Option<&'a dyn VectorIndex>,
    /// 融合策略
    pub fusion: &'a dyn FusionStrategy,
    /// 重排策略
    pub reranker: &'a dyn Reranker,
    /// BM25 参数（P5 定稿 k1=1.5 / b=0.75）
    pub bm25_params: Bm25Params,
}

/// 编排的唯一实现：两路召回 → 融合前过滤 → 融合 → 窗口回捞 → 精排 → 补齐 explain。
///
/// 所有检索入口（`QueryExecutor::search` / 门面 `Searcher::search`）最终都到这里，
/// 不存在第二份编排逻辑。
///
/// # 精排窗口的三条不变式（V2 Step 7 / D-S7-01~03，设计 §4.2.3）
///
/// ```text
/// window            = Reranker::candidate_window(k)      // trait provided，默认 k
/// candidate_k       = max(3k, window, 10)                // 恒 ≥ window
/// take_n            = min(max(k, window), 融合条数)        // 实际交给精排的条数
/// metrics.rerank_window = take_n                         // 上式的可观测落点
/// ```
///
/// 1. `window ≤ candidate_k`（否则窗口被候选池**静默封顶**）；
/// 2. `metrics.rerank_window ≤ k` ⟺ 本次**没有**可用的额外候选（放开失效）；
/// 3. 默认实现下 `window == k` ⇒ `take_n == k`、`candidate_k == max(3k, 10)`
///    ⇒ 与精排引入前**逐位一致**。
pub fn search_parts(
    parts: &SearchParts<'_>,
    query: &str,
    mode: SearchMode,
    k: usize,
    filter: Option<&crate::schema::Filter>,
) -> Result<SearchResponse> {
    let started = Instant::now();
    let mut metrics = Metrics::default();

    let index_is_empty = parts.index.num_chunks() == 0;
    let query_tokens = parts.analyzer.analyze_query(query);
    let query_is_empty = query_tokens.is_empty();

    if index_is_empty {
        // D-S5-08：`metrics.took` 必须与响应的 `took` **同值**（旧代码保持
        // `Duration::ZERO` ⇒ 装进响应后立刻违反 I7「metrics.took == took」）。
        // 先算一次再共用，避免两次 `elapsed()` 的纳秒差让 I7 变成近似断言。
        //
        // ⚠️ 本路径**刻意不调 `metrics.log`**（与 Step 5 之前一致）：索引为空的
        // 诊断价值在 `empty_reason`，不必让每个空库查询都刷一行日志。
        // `took` 仍可由 `SearchResponse.metrics` 观测到。
        let took = started.elapsed();
        metrics.took = took;
        return Ok(empty_response(
            Some(super::response::EmptyReason::NoDocuments),
            took,
            metrics,
        ));
    }

    // 候选预算：融合时多看几倍，给精排留余地。
    //
    // ⚠️ **顺序是硬要求**：`candidate_k` 被下方**两路召回**（单路分支与 Hybrid 的
    // `rayon::join` 两臂）与 `fuse(.., candidate_k)` 消费 ⇒ 精排窗口必须在**召回之前**
    // 问出来，否则窗口拿不到该拿的候选。（此处刻意不写行号：本项目已因行号漂移
    // 吃过亏，见 `v2-step7-design.md` 附录 C 的锚点纪律。）
    //
    // V2 Step 7 / D-S7-01~03：窗口由**精排器**给出（`Reranker::candidate_window`，
    // trait provided、默认 `k`），且**必须**参与 `candidate_k` 的 `max`：
    // 只把截断从 `k` 改成 `R` 而候选池不动，窗口会被 `candidate_k` **静默封顶**
    // （`R = 100, k = 10` ⇒ 实际只有 30 条）—— 设计 §2.2 的设计期新发现 A，
    // 由 `S7_T3` 用间谍向量后端钉住「候选池真的放大了」。
    //
    // ⚠️ 默认 `R = k` ⇒ 本行是**恒等变换**，与精排引入前逐位一致（`S7_T1`）。
    let window = parts.reranker.candidate_window(k);
    let candidate_k = k.saturating_mul(3).max(window).max(10);

    // 1. 过滤求值 → 候选谓词（**下推的数据源**，只求值一次；空集直接短路）
    let t0 = Instant::now();
    let predicate = match super::filter::try_build_predicate(parts.index, filter) {
        Some(p) => Some(p),
        None => {
            // 过滤条件排空了所有文档：不必进两路召回（省掉一次完整检索）。
            //
            // 空原因与下方 `fused.is_empty()` 分支**同口径**（§5.8.1：query 侧信号优先）：
            // query 本身就无命中时，报"你的过滤太窄"是误导——Agent 会去调过滤条件，
            // 而真正的问题在 query。故此处也跑一次词典探针（O(query 词数)，
            // 相比省下的一次完整检索可忽略）。
            let reason = if query_is_empty || !query_has_hits(parts.index, parts.analyzer, query) {
                determine_empty_reason(false, query_is_empty, 0)
            } else {
                Some(super::response::EmptyReason::FilteredOut)
            };
            metrics.filter_eval = t0.elapsed();
            // D-S5-08（第二条）：旧代码此处 `metrics.took` 仍是 `Duration::ZERO`
            // ⇒ 日志打印 `took_ms=0` 而响应的 `took` 是真值，「日志与响应各说一套」。
            let took = started.elapsed();
            metrics.took = took;
            metrics.log(query);
            return Ok(empty_response(reason, took, metrics));
        }
    };
    metrics.filter_eval = t0.elapsed();
    metrics.allowed = predicate.as_ref().map_or(0, |p| p.allowed_count());

    // 2. 两路召回（Hybrid 并行，单路只跑一路）
    //
    // ⚠️ BM25 路在**无用户过滤**时传 `None`（P1-4）：`Index::remove` 已在删除时
    // 物理摘除 postings，活 postings 里不可能有死 chunk。传 `None` 省掉热路径上
    // 每 posting 一次 `dyn contains()` 虚调用——这是所有无过滤查询的必经之路。
    // 向量路**必须**始终传谓词：hnsw_rs 无法从图中摘除已删向量（Q-C1）。
    let pred = predicate.as_deref();
    // BM25：无用户过滤时传 None（postings 已物理摘除死 chunk，无需再判活）
    let bm25_f = if filter.is_some() { pred } else { None };
    // 向量：恒传谓词（hnsw_rs 无法摘除已删向量，Q-C1）
    let vec_f = pred;

    // 向量路依赖**只解析一次**：`Vector` / `Hybrid` 都要用它，`Bm25` 根本不需要。
    let vec_parts = match mode {
        SearchMode::Bm25 => None,
        SearchMode::Vector | SearchMode::Hybrid => {
            Some(require_vector(parts.embedder, parts.vector_index)?)
        }
    };

    // V2 Step 5：向量路**分派**（ANN / 精确）与**记账**（`Metrics.vector_route`）。
    // 策略由后端 `VectorIndex::prefers_exact` 给出（见 `VectorRetriever::plan`），
    // 编排层只做二选一——阈值规则不在这一层复制。
    // `SearchMode::Bm25` 不走向量路 ⇒ `vector_route` 保持默认 `None`（本次检索
    // 有没有向量路是编排层的信息，不该由后端编码）。
    let vec_route = match vec_parts {
        Some((e, vi)) => VectorRetriever::new(e, vi).plan(vec_f),
        None => VectorRoute::None,
    };

    let (bm25_lane, vector_lane) = match mode {
        SearchMode::Bm25 => {
            let bm25 =
                Bm25Retriever::new(parts.index, parts.analyzer).with_params(parts.bm25_params);
            let t = Instant::now();
            let lane = to_lane(bm25.search_filtered(query, candidate_k, bm25_f)?);
            metrics.bm25_elapsed = t.elapsed();
            (Some(lane), None)
        }
        SearchMode::Vector => {
            // 上方已 `require_vector` 过一次，这里复用；用 `ok_or` 而非 `expect`
            // ⇒ 检索主路径不留 panic 分支（不可达，但不可达不等于该 panic）。
            let (e, vi) = vec_parts.ok_or(Error::NoEmbedder)?;
            let vec = VectorRetriever::new(e, vi);
            metrics.vector_route = vec_route;
            let t = Instant::now();
            let lane = to_lane(dispatch_vector(&vec, vec_route, query, candidate_k, vec_f)?);
            metrics.vector_elapsed = t.elapsed();
            (None, Some(lane))
        }
        SearchMode::Hybrid => {
            let (e, vi) = vec_parts.ok_or(Error::NoEmbedder)?;
            let bm25 =
                Bm25Retriever::new(parts.index, parts.analyzer).with_params(parts.bm25_params);
            let vec = VectorRetriever::new(e, vi);
            metrics.vector_route = vec_route;
            // 并行执行；谓词是 Send + Sync，可安全跨 rayon 线程共享。
            // 每路各自计时：Hybrid 下单一 `took` 无法归因"是哪一路慢"
            // （D-S5-06；架构 §8.3 的示例日志本就预期 bm25=/vector= 两列）。
            let (r1, r2) = rayon::join(
                || {
                    let t = Instant::now();
                    let r = bm25.search_filtered(query, candidate_k, bm25_f);
                    (r, t.elapsed())
                },
                || {
                    let t = Instant::now();
                    let r = dispatch_vector(&vec, vec_route, query, candidate_k, vec_f);
                    (r, t.elapsed())
                },
            );
            metrics.bm25_elapsed = r1.1;
            metrics.vector_elapsed = r2.1;
            (Some(to_lane(r1.0?)), Some(to_lane(r2.0?)))
        }
    };

    metrics.bm25 = bm25_lane.as_ref().map_or(0, |l| l.len());
    metrics.vector = vector_lane.as_ref().map_or(0, |l| l.len());
    // 缺口必须相对「本该拿到的条数」，而不是 `candidate_k`——候选池本身就不足
    // `candidate_k` 时（小语料 / 高选择度过滤）`candidate_k - len` 恒为正，指标会被
    // 噪声淹没，而它的用途是判断 filtered-ANN 是否**真的**降级了（V2.1 prefilter 判据）。
    metrics.vector_shortfall = vector_lane.as_ref().map_or(0, |l| {
        candidate_k.min(metrics.allowed).saturating_sub(l.len())
    });

    // 3. 融合（或单路直通）。**过滤已在召回期下推**，这里不再做 retain（原 1.5 节已删）
    let mut lanes: Vec<LaneResults> = Vec::new();
    if let Some(l) = &bm25_lane {
        lanes.push(l.clone());
    }
    if let Some(l) = &vector_lane {
        lanes.push(l.clone());
    }

    // 单路模式：直接用该路结果作为"融合"输出（分数即单路分数）
    let fused: Vec<(ChunkId, Score)> = if mode == SearchMode::Hybrid {
        parts.fusion.fuse(&lanes, candidate_k)
    } else {
        lanes.into_iter().flatten().collect()
    };

    metrics.candidates = fused.len();

    if fused.is_empty() {
        // 语义（§5.8.1）：**query 侧信号优先**——对 Agent 更可操作，
        // 「你的词是幻觉词」（AllTermsUnmatched）比「你的过滤太窄」（FilteredOut）更有指导性。
        //
        // 下推后 lane 结果已是过滤后的，"本来有候选但被过滤光"这个信号丢失了，
        // 因此用 `query_has_hits` 词典探针还原它（O(query 词数)，成本可忽略）。
        let reason = if query_is_empty || !query_has_hits(parts.index, parts.analyzer, query) {
            determine_empty_reason(false, query_is_empty, 0)
        } else if filter.is_some() {
            Some(super::response::EmptyReason::FilteredOut)
        } else {
            determine_empty_reason(false, query_is_empty, 0)
        };
        let took = started.elapsed();
        metrics.took = took;
        metrics.log(query);
        return Ok(empty_response(reason, took, metrics));
    }

    // 3. 对**候选窗口**做一次正排回捞，**只填便宜字段**（D-S7-06 的第一步）
    //
    // 窗口 `take_n = min(max(k, R), 融合条数)`：默认 `R == k` ⇒ 与精排引入前逐位一致。
    //
    // ⚠️ 这里**刻意不算 `matched_terms`**（= `analyze_doc(整段)`）：它的成本随窗口
    // **线性放大**（设计 §2.4 的设计期新发现 B），而窗口里被精排截掉的候选（最多
    // `R − k` 条）算了就白算 ⇒ 推迟到第 5 步、只对最终 ≤ `k` 条算。
    // 代价是精排器**看不到** `explain` 的这部分（读侧契约，见 `Reranker::rerank`）。
    let lane_rank = |lane: &Option<LaneResults>| -> HashMap<ChunkId, (u32, Score)> {
        lane.as_ref()
            .map(|l| {
                l.iter()
                    .enumerate()
                    .map(|(i, (id, s))| (*id, (i as u32 + 1, *s)))
                    .collect()
            })
            .unwrap_or_default()
    };
    let bm25_rank = lane_rank(&bm25_lane);
    let vector_rank = lane_rank(&vector_lane);

    let take_n = window.max(k).min(fused.len());
    let mut proto: Vec<Hit> = Vec::with_capacity(take_n);
    for (chunk_id, fused_score) in fused.into_iter().take(take_n) {
        let Some(chunk) = parts.index.chunk(chunk_id) else {
            continue;
        };
        let doc = parts
            .index
            .doc(chunk.doc_id)
            .ok_or(Error::ChunkNotFound(chunk_id))?;

        proto.push(Hit {
            chunk_id,
            doc_id: chunk.doc_id,
            score: fused_score,
            text: chunk.text.clone(),
            source: doc.source.clone(),
            metadata: doc.metadata.clone(),
            // 只带 `fused_score`：`matched_terms` 与 lane rank/score 留空（第 5 步补）。
            explain: Explain {
                fused_score,
                ..Default::default()
            },
        });
    }

    // 4. 精排（V2 Step 7）。入参是**候选窗口**（可能 > `k`），出参应 ≤ `k` 条。
    //
    // `NoOpReranker`（默认，D-S7-04）= 原样 `take(k)`，且此时 `take_n == k`
    // ⇒ 全链路零回归。真精排器（`LocalReranker`）在此返回 `σ(logit)` 并把原始分
    // 写进 `explain.rerank_score`（D-S7-05）。
    let t_rerank = Instant::now();
    let mut hits = parts.reranker.rerank(query, proto, k)?;
    metrics.rerank_elapsed = t_rerank.elapsed();
    metrics.rerank_window = take_n;

    // 安全网（纵深防御）：出参契约要求精排器自己截到 `top_n`（见 `Reranker::rerank`），
    // 这里再截一次 ⇒ release 下不留「> k 条」的越界输出。
    // ⚠️ **不得静默**（NFR-07）：触发即表示上游实现违约，与 `rerank::scoring::apply_scores`
    // 的两条防御路径同族 —— 那条越界 `index` 走 `debug_assert!` + `warn!`，这条
    // 没有"可解释的退化"可言（多出来的条数无法判断该丢哪条），故只 `warn!` + 截断。
    if hits.len() > k {
        tracing::warn!(
            returned = hits.len(),
            k,
            "精排器返回条数超过 top_n（违反 Reranker::rerank 的出参契约），已截断"
        );
        hits.truncate(k);
    }

    // 5. 补齐 `explain`（D-S7-06 的第二步 / 契约的 C2）
    //
    // ⚠️ **C2 不得覆盖 C1**：精排器通过写 `explain` 归还的原始分
    // （`rerank_score`，σ 不可逆 ⇒ 编排层算不出来）**必须原样保留**，否则
    // 「谁后写谁生效」会把 D-S7-05 的信号冲掉（`Reranker::rerank` 的出参契约）。
    // 本循环只写下面 5 个字段 ⇒ 天然不碰 `rerank_score`；`S7_T12` 有用例钉住这一点。
    for h in &mut hits {
        h.explain.matched_terms = matched_terms(parts.analyzer, query, &h.text);
        h.explain.bm25_score = bm25_rank.get(&h.chunk_id).map(|(_, s)| *s);
        h.explain.bm25_rank = bm25_rank.get(&h.chunk_id).map(|(r, _)| *r);
        h.explain.vector_score = vector_rank.get(&h.chunk_id).map(|(_, s)| *s);
        h.explain.vector_rank = vector_rank.get(&h.chunk_id).map(|(r, _)| *r);
    }

    metrics.fused = hits.len();
    let took = started.elapsed();
    metrics.took = took;
    metrics.log(query);

    Ok(SearchResponse {
        hits,
        total_candidates: metrics.candidates,
        empty_reason: None,
        took,
        metrics,
    })
}

/// 向量路的分派（D-S5-01 的"分派归编排层"落地）。
///
/// 只有两臂可达：`plan()` 返回的是 `Ann` / `Exact`。`VectorRoute::None` 表示
/// "本次检索没有向量路"，由编排层在 `SearchMode::Bm25` 下写入
/// `Metrics.vector_route`——它**不会**流到这里。
///
/// ⚠️ 即便"不可达"，也**不给库的检索主路径留 panic 分支**（同 R19 精神）：万一将来
/// 有人把 `None` 传进来，退化为 `Ann`（正确但没有兜底收益）并由 `debug_assert!`
/// 在测试期炸出来。`VectorRoute::None` 在**同一个枚举**里本就是可达概念，只是
/// `plan()` 当下不返回它——一个 `unreachable!()` 会让这个区分变成线上 500。
fn dispatch_vector(
    vec: &VectorRetriever<'_>,
    route: VectorRoute,
    query: &str,
    k: usize,
    filter: Option<&dyn CandidateFilter>,
) -> Result<Vec<Scored>> {
    debug_assert!(
        !matches!(route, VectorRoute::None),
        "plan() 不返回 VectorRoute::None（Bm25 模式在编排层就写好 route，不走向量路）"
    );
    match route {
        VectorRoute::Exact => vec.search_exact_filtered(query, k, filter),
        // `Ann` 与（不可达的）`None` 都退化为 ANN
        _ => vec.search_filtered(query, k, filter),
    }
}

/// query 是否在词典里**有任何命中**（§5.8.1 的词典探针）。
///
/// 过滤下推后 lane 结果已是过滤后的产物，"query 本来有没有命中"这个信号丢失了，
/// 本函数用 `Index::term_id` + `postings_by_id` 还原它——postings 在删除时已物理
/// 摘除，因此"有非空 postings"即"存在活候选"。
///
/// 成本 O(|query 词数|) 次哈希查找，相对两路召回可忽略。
///
/// # ⚠️ 适用边界：这是一个 **BM25 词典探针**，不是通用"有没有候选"探针
///
/// 它只看倒排链，因此**只在 BM25 / Hybrid 模式下代表"有没有候选"**。
/// `SearchMode::Vector` 下向量路的召回与该探针无关，于是：
///
/// - 向量索引为空、或 query 的分词结果与入库侧不一致时，即使向量路本该有候选，
///   本函数也返回 `false` → 空结果会被报成 `AllTermsUnmatched`（"你的词是幻觉词"），
///   而真实原因可能在向量侧。
/// - 该行为**不是 V2 Step 1 引入的**（改动前后一致），且已被 T16 钉成契约。
///
/// 若要真正区分，需要向量路自己的探针（例如"向量索引非空且最近邻距离在阈值内"），
/// 属 V2.1 议题。在那之前，读这个指标/原因时要记住它只覆盖 BM25 侧。
fn query_has_hits(index: &Index, analyzer: &dyn Analyzer, query: &str) -> bool {
    let mut seen = std::collections::HashSet::new();
    analyzer
        .analyze_query(query)
        .iter()
        .filter(|t| seen.insert(t.term.as_str()))
        .filter_map(|t| index.term_id(t.term.as_str()))
        .any(|id| !index.postings_by_id(id).is_empty())
}

/// 向量路依赖检查：embedder 与 vector_index 必须成对出现。
fn require_vector<'a>(
    embedder: Option<&'a dyn Embedder>,
    vector_index: Option<&'a dyn VectorIndex>,
) -> Result<(&'a dyn Embedder, &'a dyn VectorIndex)> {
    match (embedder, vector_index) {
        (Some(e), Some(vi)) => Ok((e, vi)),
        _ => Err(Error::NoEmbedder),
    }
}

/// 检索编排器（旧名 `Searcher`，P6/I-01 改名）。持有各层的引用，不拥有它们。
///
/// 适合需要逐 lane 自定义组装的调用方（逃生舱，p6-design 7.3 最后一行）；
/// 一般用途请用门面层的 owned `Searcher`（I-05）。
pub struct QueryExecutor<'a> {
    index: &'a Index,
    analyzer: &'a dyn Analyzer,
    embedder: Option<&'a dyn Embedder>,
    vector_index: Option<&'a dyn VectorIndex>,
    fusion: Box<dyn FusionStrategy>,
    reranker: Box<dyn Reranker>,
    /// BM25 参数（P5 网格搜索从外部注入；默认 Bm25Params::default()）
    bm25_params: Bm25Params,
}

impl<'a> QueryExecutor<'a> {
    /// 只含 BM25 路（向量路留空）。
    pub fn new(index: &'a Index, analyzer: &'a dyn Analyzer) -> Self {
        Self {
            index,
            analyzer,
            embedder: None,
            vector_index: None,
            fusion: Box::new(RrfFusion::default()),
            reranker: Box::new(NoOpReranker),
            bm25_params: Bm25Params::default(),
        }
    }

    /// 覆盖 BM25 参数（T5-05 网格搜索注入口；现状内部用默认参数）。
    pub fn with_bm25_params(mut self, params: Bm25Params) -> Self {
        self.bm25_params = params;
        self
    }

    /// 接入向量路：embedder 负责查询侧向量化，vector_index 负责近邻检索。
    pub fn with_vector(
        mut self,
        embedder: &'a dyn Embedder,
        vector_index: &'a dyn VectorIndex,
    ) -> Self {
        self.embedder = Some(embedder);
        self.vector_index = Some(vector_index);
        self
    }

    /// 覆盖融合策略（默认 `RrfFusion`）。
    pub fn with_fusion(mut self, fusion: Box<dyn FusionStrategy>) -> Self {
        self.fusion = fusion;
        self
    }

    /// 覆盖重排策略（默认 `NoOpReranker`，P7 再接真实 rerank）。
    pub fn with_reranker(mut self, reranker: Box<dyn Reranker>) -> Self {
        self.reranker = reranker;
        self
    }

    /// 把自身状态投影成借用型 `SearchParts`（编排内核的输入）。
    fn parts(&self) -> SearchParts<'_> {
        SearchParts {
            index: self.index,
            analyzer: self.analyzer,
            embedder: self.embedder,
            vector_index: self.vector_index,
            fusion: self.fusion.as_ref(),
            reranker: self.reranker.as_ref(),
            bm25_params: self.bm25_params,
        }
    }

    /// 执行一次检索，返回结构化响应（含 hits / explain / took）。
    pub fn search(&self, query: &str, mode: SearchMode, k: usize) -> Result<SearchResponse> {
        self.search_filtered(query, mode, k, None)
    }

    /// 带元数据过滤的检索（FR-14 / T4-07）。
    ///
    /// 过滤在**融合前**对 lane 结果做（chunk_id 位图），查询路径零 IO。
    /// 有候选但被过滤条件全部排除时，`empty_reason = FilteredOut`。
    pub fn search_filtered(
        &self,
        query: &str,
        mode: SearchMode,
        k: usize,
        filter: Option<&crate::schema::Filter>,
    ) -> Result<SearchResponse> {
        let parts = self.parts();
        search_parts(&parts, query, mode, k, filter)
    }
}

/// 组装空结果响应。
///
/// `took` 由**调用方**算好后传入（而不是在这里 `started.elapsed()`）：
/// [`Metrics::took`] 与响应的 `took` 必须是**同一个值**（I7），两次 `elapsed()`
/// 之间隔着纳秒级误差，分开算会让"口径自洽"退化成近似断言。
fn empty_response(
    reason: Option<super::response::EmptyReason>,
    took: Duration,
    metrics: Metrics,
) -> SearchResponse {
    debug_assert_eq!(
        metrics.took, took,
        "I7：空结果路径的 metrics.took 必须与响应 took 逐位一致"
    );
    SearchResponse {
        hits: Vec::new(),
        total_candidates: metrics.candidates,
        empty_reason: reason,
        took,
        metrics,
    }
}

/// 把 `Vec<Scored>` 转成 `LaneResults`。
fn to_lane(v: Vec<crate::retriever::Scored>) -> LaneResults {
    v.into_iter().map(|s| (s.chunk_id, s.score)).collect()
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)] // 中文测试名含英文缩写（NoDocuments / AllTermsUnmatched）
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use crate::analyze::MixedAnalyzer;
    use crate::chunk::Chunker;
    use crate::document::DocRecord;
    use crate::vector::BruteForceIndex;

    /// 确定性假 Embedder：FNV-1a 哈希 → 8 维归一化向量（同一文本恒同一向量）。
    struct FakeEmbedder;
    impl Embedder for FakeEmbedder {
        fn dim(&self) -> usize {
            8
        }
        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| fake_vec(t)).collect())
        }
        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            Ok(fake_vec(text))
        }
    }

    fn fake_vec(text: &str) -> Vec<f32> {
        // FNV-1a（确定，跨进程稳定）
        let mut h: u64 = 0xcbf29ce484222325;
        for b in text.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let mut v: Vec<f32> = (0..8)
            .map(|i| {
                let byte = (h >> (i * 8)) as u8;
                byte as f32 / 255.0 * 2.0 - 1.0
            })
            .collect();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }

    fn build_index(texts: &[&str]) -> (Index, MixedAnalyzer) {
        let analyzer = MixedAnalyzer::new();
        let chunker = Chunker::default();
        let mut index = Index::new();
        for (i, text) in texts.iter().enumerate() {
            let doc = DocRecord {
                doc_id: 0,
                source: format!("doc-{i}"),
                metadata: serde_json::json!({}),
                content_hash: 0,
            };
            index.add(doc, chunker.chunk(0, text), &analyzer).unwrap();
        }
        (index, analyzer)
    }

    #[test]
    fn bm25模式不碰向量() {
        let (index, analyzer) = build_index(&["BM25 是检索算法", "向量检索用余弦相似度"]);
        // 只构造 QueryExecutor::new（无向量），BM25 模式应正常工作
        let searcher = QueryExecutor::new(&index, &analyzer);
        let resp = searcher.search("BM25", SearchMode::Bm25, 10).unwrap();
        assert!(!resp.hits.is_empty());
        assert_eq!(resp.empty_reason, None);
    }

    #[test]
    fn 三模式各有结果() {
        let (index, analyzer) = build_index(&[
            "BM25 是经典关键词检索算法，参数 k1 控制词频饱和",
            "向量检索把文本编码成向量计算余弦相似度",
            "混合检索融合关键词和向量两路结果",
        ]);

        let e = FakeEmbedder;
        let entries: Vec<(u32, crate::vector::NormalizedVector)> = index
            .live_chunks()
            .map(|c| {
                let v = fake_vec(&c.text);
                (c.chunk_id, crate::vector::NormalizedVector::new(v))
            })
            .collect();
        let mut vi = BruteForceIndex::new();
        for (id, v) in entries {
            vi.add(id, v).unwrap();
        }

        let searcher = QueryExecutor::new(&index, &analyzer).with_vector(&e, &vi);

        let bm25 = searcher.search("检索", SearchMode::Bm25, 10).unwrap();
        let vec = searcher.search("检索", SearchMode::Vector, 10).unwrap();
        let hybrid = searcher.search("检索", SearchMode::Hybrid, 10).unwrap();

        assert!(!bm25.hits.is_empty());
        assert!(!vec.hits.is_empty());
        assert!(!hybrid.hits.is_empty());
        // Hybrid 的 explain 两路 rank 至少有一路非 None
        assert!(hybrid
            .hits
            .iter()
            .any(|h| h.explain.bm25_rank.is_some() || h.explain.vector_rank.is_some()));
    }

    #[test]
    fn 空索引返回NoDocuments() {
        let analyzer = MixedAnalyzer::new();
        let index = Index::new();
        let searcher = QueryExecutor::new(&index, &analyzer);
        let resp = searcher.search("x", SearchMode::Bm25, 10).unwrap();
        assert_eq!(
            resp.empty_reason,
            Some(crate::query::response::EmptyReason::NoDocuments)
        );
    }

    #[test]
    fn 全停用词返回AllTermsUnmatched() {
        let (index, analyzer) = build_index(&["BM25 检索算法"]);
        let searcher = QueryExecutor::new(&index, &analyzer);
        // "的 了 在" 全是停用词
        let resp = searcher.search("的 了 在", SearchMode::Bm25, 10).unwrap();
        assert_eq!(
            resp.empty_reason,
            Some(crate::query::response::EmptyReason::AllTermsUnmatched)
        );
    }

    #[test]
    fn 过滤后是子集且全过滤触发FilteredOut() {
        use crate::schema::Filter;
        let (index, analyzer) = build_index(&[
            "BM25 是经典检索算法",
            "向量检索计算余弦相似度",
            "混合检索融合两路",
        ]);
        let searcher = QueryExecutor::new(&index, &analyzer);

        // 不过滤：应召回多条
        let all = searcher.search("检索", SearchMode::Bm25, 10).unwrap();
        assert!(all.hits.len() >= 2);

        // 过滤：构造一个只匹配部分文档的 metadata 条件——本测试语料无 metadata，
        // 因此用"永不匹配"的条件验证 FilteredOut
        let impossible = Filter::eq("__no_such_field__", "__no_such_value__");
        let filtered = searcher
            .search_filtered("检索", SearchMode::Bm25, 10, Some(&impossible))
            .unwrap();
        assert!(filtered.hits.is_empty());
        assert_eq!(
            filtered.empty_reason,
            Some(crate::query::response::EmptyReason::FilteredOut)
        );

        // 不过滤时有结果 → 证明 FilteredOut 不是因为无召回
        assert!(!all.hits.is_empty());
    }

    #[test]
    fn 连续100次结果一致() {
        let (index, analyzer) = build_index(&[
            "BM25 是经典关键词检索算法",
            "向量检索计算余弦相似度",
            "混合检索融合两路结果",
            "倒排索引维护词项到文档的映射",
        ]);
        let e = FakeEmbedder;
        let entries: Vec<(u32, crate::vector::NormalizedVector)> = index
            .live_chunks()
            .map(|c| {
                (
                    c.chunk_id,
                    crate::vector::NormalizedVector::new(fake_vec(&c.text)),
                )
            })
            .collect();
        let mut vi = BruteForceIndex::new();
        for (id, v) in entries {
            vi.add(id, v).unwrap();
        }
        let searcher = QueryExecutor::new(&index, &analyzer).with_vector(&e, &vi);

        let first: Vec<u32> = searcher
            .search("检索", SearchMode::Hybrid, 10)
            .unwrap()
            .hits
            .iter()
            .map(|h| h.chunk_id)
            .collect();
        for _ in 0..100 {
            let cur: Vec<u32> = searcher
                .search("检索", SearchMode::Hybrid, 10)
                .unwrap()
                .hits
                .iter()
                .map(|h| h.chunk_id)
                .collect();
            assert_eq!(first, cur, "第 N 次结果与首次不一致");
        }
    }

    /// T16（V2 Step 1 / P1-5）：空结果原因矩阵 2×3。
    ///
    /// 过滤下推后，lane 结果已经是「过滤后」的产物，编排层看不到
    /// 「query 本来有没有命中」这个信号，而它决定了该报
    /// `AllTermsUnmatched`（你的词是幻觉词）还是 `FilteredOut`（你的过滤太窄）。
    /// 设计 §5.8.1 定案用 `query_has_hits` 词典探针还原该信号，语义为
    /// **query 侧信号优先**——对 Agent 而言前者比后者更有指导性。
    #[test]
    fn T16_空结果原因矩阵() {
        use crate::query::response::EmptyReason;
        use crate::schema::Filter;

        let analyzer = MixedAnalyzer::new();
        let chunker = Chunker::default();
        let mut index = Index::new();
        for (i, text) in ["BM25 是经典检索算法", "向量检索计算余弦相似度"]
            .iter()
            .enumerate()
        {
            let doc = DocRecord {
                doc_id: 0,
                source: format!("doc-{i}"),
                metadata: serde_json::json!({"tag": "kept"}),
                content_hash: 0,
            };
            index.add(doc, chunker.chunk(0, text), &analyzer).unwrap();
        }
        let searcher = QueryExecutor::new(&index, &analyzer);

        let hit_q = "检索"; // 词典里有
        let miss_q = "zzz"; // 词典里没有，且不是停用词（分词后非空）
        let matching = Filter::eq("tag", "kept");
        let impossible = Filter::eq("tag", "nope");

        // ── query 有命中 ──────────────────────────────────────────────
        let r = searcher.search(hit_q, SearchMode::Bm25, 10).unwrap();
        assert!(!r.hits.is_empty(), "有命中 / 无过滤 → 应有结果");
        assert_eq!(r.empty_reason, None, "有命中 / 无过滤 → 无空原因");

        let r = searcher
            .search_filtered(hit_q, SearchMode::Bm25, 10, Some(&matching))
            .unwrap();
        assert!(!r.hits.is_empty(), "有命中 / 过滤有匹配 → 应有结果");
        assert_eq!(r.empty_reason, None, "有命中 / 过滤有匹配 → 无空原因");

        let r = searcher
            .search_filtered(hit_q, SearchMode::Bm25, 10, Some(&impossible))
            .unwrap();
        assert!(r.hits.is_empty(), "有命中 / 过滤排空 → 应无结果");
        assert_eq!(
            r.empty_reason,
            Some(EmptyReason::FilteredOut),
            "有命中 / 过滤排空 → FilteredOut"
        );

        // ── query 无命中：query 侧信号优先，三种过滤情况下都报 AllTermsUnmatched ──
        let cases: [(Option<&Filter>, &str); 3] = [
            (None, "无过滤"),
            (Some(&matching), "过滤有匹配"),
            (Some(&impossible), "过滤无匹配"),
        ];
        for (f, label) in cases {
            let r = match f {
                Some(f) => searcher
                    .search_filtered(miss_q, SearchMode::Bm25, 10, Some(f))
                    .unwrap(),
                None => searcher.search(miss_q, SearchMode::Bm25, 10).unwrap(),
            };
            assert!(r.hits.is_empty(), "无命中 / {label} → 应无结果");
            assert_eq!(
                r.empty_reason,
                Some(EmptyReason::AllTermsUnmatched),
                "无命中 / {label} → AllTermsUnmatched（query 侧信号优先于 filter）"
            );
        }
    }

    // ========================================================================
    // V2 Step 5 / S5-03 + S5-06：向量路分派、`vector_route` 记账、响应口径自洽
    // ========================================================================

    /// 间谍向量后端：**记录到底调了哪个方法**，返回值完全固定。
    ///
    /// 为什么用间谍而不是真的 `HnswRsIndex`：这一步要断言的是"**编排层分派对不对**"，
    /// 而真 HNSW 的拓扑每次建图都不同（`StdRng::from_os_rng()`）⇒ "开/关兜底结果
    /// 逐位一致"的 A/B 会混入拓扑噪声、变得不可证伪。间谍后端没有图，两条路径
    /// 返回同一份固定数据 ⇒ 差异只可能来自**分派**，这正是 S5-T7 想钉的东西。
    struct SpyVectorIndex {
        /// 策略：是否声明"该谓词下走精确路径"（模拟 `HnswRsIndex` 的阈值判断）
        exact_by_policy: bool,
        /// `search_filtered`（ANN 路径）被调用次数
        ann_calls: AtomicUsize,
        /// `search_exact_filtered` 被调用次数
        exact_calls: AtomicUsize,
        /// ANN 路径返回的 id（刻意少于 Exact ⇒ 复现 R11 的召回缺口）
        ann_ids: Vec<ChunkId>,
        /// 精确路径返回的 id（= 全部 allowed ⇒ 结构性无缺口）
        exact_ids: Vec<ChunkId>,
    }

    impl SpyVectorIndex {
        fn new(exact_by_policy: bool, ann_ids: Vec<ChunkId>, exact_ids: Vec<ChunkId>) -> Self {
            Self {
                exact_by_policy,
                ann_calls: AtomicUsize::new(0),
                exact_calls: AtomicUsize::new(0),
                ann_ids,
                exact_ids,
            }
        }

        fn calls(&self) -> (usize, usize) {
            (
                self.ann_calls.load(AtomicOrdering::SeqCst),
                self.exact_calls.load(AtomicOrdering::SeqCst),
            )
        }

        /// 固定距离序列（升序、唯一、与 id 顺序无关地确定性）
        fn fixed(ids: &[ChunkId]) -> Vec<(ChunkId, f32)> {
            ids.iter()
                .enumerate()
                .map(|(i, id)| (*id, i as f32 * 0.5))
                .collect()
        }
    }

    impl crate::vector::VectorIndex for SpyVectorIndex {
        fn add(&mut self, _id: ChunkId, _vec: crate::vector::NormalizedVector) -> Result<()> {
            Ok(())
        }

        fn search_filtered(
            &self,
            _query: &crate::vector::NormalizedVector,
            _k: usize,
            _filter: Option<&dyn CandidateFilter>,
        ) -> Result<Vec<(ChunkId, f32)>> {
            self.ann_calls.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(Self::fixed(&self.ann_ids))
        }

        fn search_exact_filtered(
            &self,
            _query: &crate::vector::NormalizedVector,
            _k: usize,
            _filter: Option<&dyn CandidateFilter>,
        ) -> Result<Vec<(ChunkId, f32)>> {
            self.exact_calls.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(Self::fixed(&self.exact_ids))
        }

        /// 策略必须与真后端**同契约**：`FilterKind::Filtered` 才谈得上兜底。
        ///
        /// ⚠️ 这条不是可选的细节：`try_build_predicate(index, None)` 对**无用户过滤**
        /// 也返回 `Some(AliveOnly)`（`filter.rs:195`），即热路径上向量 lane **总是**
        /// 拿到一个 `Some(谓词)`。若策略只看"有没有谓词"而不看 `kind()`，热路径会被
        /// 误判成低选择度过滤而走精确扫描——本用例最初就是这么红的（间谍不忠实）。
        fn prefers_exact(&self, filter: &dyn CandidateFilter) -> bool {
            self.exact_by_policy && filter.kind() == crate::predicate::FilterKind::Filtered
        }

        fn len(&self) -> usize {
            12
        }
    }

    /// 带 `tag` 元数据的语料（让 `Filter::eq("tag", ..)` 能建出 `FilterKind::Filtered` 谓词）。
    fn build_tagged_index(texts: &[&str], tag: &str) -> (Index, MixedAnalyzer) {
        let analyzer = MixedAnalyzer::new();
        let chunker = Chunker::default();
        let mut index = Index::new();
        for (i, text) in texts.iter().enumerate() {
            let doc = DocRecord {
                doc_id: 0,
                source: format!("doc-{i}"),
                metadata: serde_json::json!({ "tag": tag }),
                content_hash: 0,
            };
            index.add(doc, chunker.chunk(0, text), &analyzer).unwrap();
        }
        (index, analyzer)
    }

    /// **S5-T1 / I6**：分派与记账**都不撒谎**。
    ///
    /// - 热路径（无过滤）：两个间谍都走 ANN，结果**逐位一致**（S5-T7 的行为 A/B）；
    /// - 低选择度过滤：`exact_by_policy` 的那个走**精确**路径，`vector_route` 如实标 `Exact`；
    ///   另一个走 ANN，标 `Ann`。
    #[test]
    fn S5_T1_分派与route记账不撒谎() {
        use crate::schema::Filter;

        let texts: Vec<String> = (0..12).map(|i| format!("检索 文档 {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let (index, analyzer) = build_tagged_index(&refs, "kept");
        let e = FakeEmbedder;

        // ANN 只召回 4 条（复现 R11 缺口）；精确路径返回全部 12 条
        let ann_ids: Vec<ChunkId> = (0..4).collect();
        let exact_ids: Vec<ChunkId> = (0..12).collect();
        let on = SpyVectorIndex::new(true, ann_ids.clone(), exact_ids.clone());
        let off = SpyVectorIndex::new(false, ann_ids.clone(), exact_ids.clone());

        let filter = Filter::eq("tag", "kept");

        // ── 热路径（无过滤）：两者都必须走 ANN，且结果逐位一致 ──
        let s_on = QueryExecutor::new(&index, &analyzer).with_vector(&e, &on);
        let s_off = QueryExecutor::new(&index, &analyzer).with_vector(&e, &off);
        let hot_on = s_on.search("检索", SearchMode::Vector, 10).unwrap();
        let hot_off = s_off.search("检索", SearchMode::Vector, 10).unwrap();
        let ids =
            |r: &SearchResponse| -> Vec<ChunkId> { r.hits.iter().map(|h| h.chunk_id).collect() };
        assert_eq!(
            ids(&hot_on),
            ids(&hot_off),
            "S5-T7：无过滤时兜底开关不得改变结果（热路径一行未改）"
        );
        assert_eq!(
            hot_on.metrics.vector_route,
            VectorRoute::Ann,
            "无过滤 ⇒ 恒走 ANN"
        );
        assert_eq!(on.calls(), (1, 0), "热路径必须调 search_filtered");
        assert_eq!(off.calls(), (1, 0));

        // ── 低选择度过滤：策略为真的走精确、为假的走 ANN ──
        let filt_on = s_on
            .search_filtered("检索", SearchMode::Vector, 10, Some(&filter))
            .unwrap();
        assert_eq!(
            filt_on.metrics.vector_route,
            VectorRoute::Exact,
            "I6：route 必须与实际走的路径一致"
        );
        assert_eq!(on.calls(), (1, 1), "策略为真 ⇒ 调 search_exact_filtered");

        let filt_off = s_off
            .search_filtered("检索", SearchMode::Vector, 10, Some(&filter))
            .unwrap();
        assert_eq!(filt_off.metrics.vector_route, VectorRoute::Ann);
        assert_eq!(off.calls(), (2, 0), "策略为假 ⇒ 仍调 search_filtered");

        // ── 语义收益（§4.4）：本用例图覆盖完整 ⇒ 精确路径下缺口为 0 ──
        // ⚠️ 这不是恒等式：`allowed` 来自 `Index`、扫描枚举的是图里的点，图滞后于索引时
        // 精确路径的缺口仍会 > 0（那时它反过来是「图未覆盖 allowed」的诊断信号）。
        assert_eq!(filt_off.metrics.vector, 4, "ANN 只凑到 4 条");
        assert!(
            filt_off.metrics.vector_shortfall > 0,
            "ANN 低选择度下应有可观测缺口（R11 的可见信号）"
        );
        assert_eq!(filt_on.metrics.vector, 12, "精确路径返回全部 allowed");
        assert_eq!(
            filt_on.metrics.vector_shortfall, 0,
            "本用例图覆盖完整，故精确路径缺口应为 0；判读仍须连看 vector_route（§4.4 推论 1）"
        );
    }

    /// **S5-T7（分派对照）**：同一个 `Filtered` 谓词下，两种策略必须落到**不同**的
    /// `vector_route`，且 Hybrid 的 per-lane 耗时（D-S5-06）在并行分支里也被填上。
    ///
    /// ⚠️ 本用例**不**比较"结果是否一致"——那是 `S5_T1` 的职责（它在热路径上比
    /// `hits` 逐位相等）。这里比的是**分派**：两个间谍策略相反 ⇒ route 必须相反，
    /// 否则这个 A/B 就失去鉴别力。
    ///
    /// `FilterKind::Alive` 下"热路径不被劫持"的行为护栏在**集成层**：
    /// `tests/step5_query_observability.rs::S5_T7_真实软删除语料下兜底开关不改热路径`。
    #[test]
    fn S5_T7_两种策略在Filtered谓词下route不同且per_lane耗时被填() {
        use crate::schema::Filter;

        let (index, analyzer) = build_tagged_index(&["检索 甲", "检索 乙", "向量 丙"], "kept");
        let e = FakeEmbedder;
        let ids: Vec<ChunkId> = (0..3).collect();
        let on = SpyVectorIndex::new(true, ids.clone(), ids.clone());
        let off = SpyVectorIndex::new(false, ids.clone(), ids.clone());
        let filter = Filter::eq("tag", "kept");

        let s_on = QueryExecutor::new(&index, &analyzer).with_vector(&e, &on);
        let s_off = QueryExecutor::new(&index, &analyzer).with_vector(&e, &off);

        // Hybrid + 用户过滤 ⇒ 谓词是 Filtered（策略生效）⇒ 两者 route 必须不同
        let a = s_on
            .search_filtered("检索", SearchMode::Hybrid, 10, Some(&filter))
            .unwrap();
        let b = s_off
            .search_filtered("检索", SearchMode::Hybrid, 10, Some(&filter))
            .unwrap();
        assert_ne!(
            a.metrics.vector_route, b.metrics.vector_route,
            "两者策略不同，route 必须不同（否则本 A/B 失去鉴别力）"
        );
        // Hybrid 的向量 lane 输入不同 ⇒ 融合结果理应不同，但**两路耗时都必须被填**
        assert!(a.metrics.bm25_elapsed > Duration::ZERO, "bm25 耗时未填");
        assert!(a.metrics.vector_elapsed > Duration::ZERO, "vector 耗时未填");
        assert!(b.metrics.bm25_elapsed > Duration::ZERO);
        assert!(b.metrics.vector_elapsed > Duration::ZERO);
    }

    /// **S5-T8 / I8（D-S5-08）**：三条早退路径的 `took` 都必须是**真实值**。
    ///
    /// 旧代码里 `index_is_empty`（连 `metrics.log` 都不调）与"过滤排空"两条路径
    /// 从不设 `metrics.took`，保持 `Duration::ZERO` ⇒ 一旦 `metrics` 进响应就立刻
    /// 违反 `metrics.took == took`。
    ///
    /// ⚠️ 复核这两条**不能用 `grep "metrics.log"`**：第一条根本不调用它，
    /// 结构上永远找不到（设计 §2.5）。
    #[test]
    fn S5_T8_三条早退路径的took都是真值() {
        use crate::query::response::EmptyReason;
        use crate::schema::Filter;

        // ① index_is_empty
        let analyzer = MixedAnalyzer::new();
        let empty_index = Index::new();
        let s = QueryExecutor::new(&empty_index, &analyzer);
        let r = s.search("x", SearchMode::Bm25, 10).unwrap();
        assert_eq!(r.empty_reason, Some(EmptyReason::NoDocuments));
        assert_eq!(r.metrics.took, r.took, "I7：空索引路径口径必须自洽");
        assert!(r.took > Duration::ZERO, "① 空索引路径的 took 必须非 0");

        // ② 过滤排空
        let (index, analyzer) = build_tagged_index(&["检索 甲", "检索 乙"], "kept");
        let s = QueryExecutor::new(&index, &analyzer);
        let impossible = Filter::eq("tag", "nope");
        let r = s
            .search_filtered("检索", SearchMode::Bm25, 10, Some(&impossible))
            .unwrap();
        assert_eq!(r.empty_reason, Some(EmptyReason::FilteredOut));
        assert_eq!(r.metrics.took, r.took, "I7：过滤排空路径口径必须自洽");
        assert!(r.took > Duration::ZERO, "② 过滤排空路径的 took 必须非 0");

        // ③ 融合后为空（Step 5 之前就已设，钉住不回归）
        let r = s.search("zzz_not_in_dict", SearchMode::Bm25, 10).unwrap();
        assert_eq!(r.empty_reason, Some(EmptyReason::AllTermsUnmatched));
        assert_eq!(r.metrics.took, r.took);
        assert!(r.took > Duration::ZERO, "③ 融合为空路径的 took 必须非 0");
    }

    /// **S5-T9 / I7**：响应口径自洽——`metrics.took == took`、
    /// `metrics.candidates == total_candidates`，空结果与正常结果**两条路**都成立。
    #[test]
    fn S5_T9_响应口径自洽() {
        use crate::schema::Filter;

        let (index, analyzer) = build_tagged_index(
            &["检索 甲乙丙", "向量检索计算余弦相似度", "混合检索融合两路"],
            "kept",
        );
        let s = QueryExecutor::new(&index, &analyzer);

        // 正常结果（含 BM25 路）与两条空结果路径都要检查口径
        let cases: Vec<(Option<Filter>, &str)> = vec![
            (None, "正常"),
            (Some(Filter::eq("tag", "kept")), "过滤有匹配"),
            (Some(Filter::eq("tag", "nope")), "过滤排空"),
        ];
        for (f, label) in cases {
            let r = match &f {
                Some(f) => s
                    .search_filtered("检索", SearchMode::Bm25, 10, Some(f))
                    .unwrap(),
                None => s.search("检索", SearchMode::Bm25, 10).unwrap(),
            };
            assert_eq!(
                r.metrics.took, r.took,
                "{label}：metrics.took 与 took 不一致"
            );
            assert_eq!(
                r.metrics.candidates, r.total_candidates,
                "{label}：metrics.candidates 与 total_candidates 不一致"
            );
        }
    }

}
