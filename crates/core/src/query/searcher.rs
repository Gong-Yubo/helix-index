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
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::analyze::Analyzer;
use crate::embed::Embedder;
use crate::error::{Error, Result};
use crate::fusion::{FusionCtx, FusionStrategy, LaneResults, LaneStats, RrfFusion};
use crate::index::Index;
use crate::predicate::CandidateFilter;
use crate::rerank::{NoOpReranker, Reranker};
use crate::retriever::{
    union_route, Bm25Params, Bm25Retriever, Retriever, Scored, SegmentSet, SegmentedBm25Retriever,
    SegmentedVectorRetriever, VectorRetriever, VectorSegmentRef,
};
use crate::types::{ChunkId, Score};
use crate::vector::{VectorIndex, VectorRoute};

use super::explain::{determine_empty_reason, matched_terms};
use super::filter::PredicateBuilder;
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
    /// **跨段**检索输入（`S8-04`）：`Some` ⇒ 走 `main + deltas` 的跨段路径；
    /// `None` ⇒ 单段（只用 `index`，与 `S8-03` 之前**逐位一致**）。
    ///
    /// ⚠️ 由门面层按「是否真的多段 / 有跨段墓碑」决定是否填 —— 单段时必须留 `None`，
    /// 否则热路径会平白多一层段遍历（R52 的零谓词/短路策略见设计 §4.7.3）。
    pub segment_set: Option<SegmentSet<'a>>,
    /// 跨段谓词构造器（与 `segment_set` **成对**给出）。
    pub predicate_builder: Option<Box<dyn PredicateBuilder + 'a>>,
    /// **逐段向量载具**（`S8-05`）：`Some` ⇒ 向量路**对每段各查一次**再全局归并；
    /// `None` ⇒ 单段（只用 `vector_index`，本地 `chunk_id` == 全局，与 `S8-03` 之前逐位一致）。
    ///
    /// ⚠️ **与 `segment_set` 同样按「是否真的多段 / 有跨段墓碑」填** —— 计数口径必须一致，
    /// 否则 `Metrics.vector_segments` 与 `Metrics.segments` 会分叉（`S8-05` 的验收项
    /// 是两者**恒等**）。
    ///
    /// 🔑 为什么**不复用** `segment_set`：那会把向量路与 BM25 路的类型绑在一起，而本仓
    /// 有「两条 lane 不得互相引用」的硬约束（`retriever` 模块头注释）⇒ 载具各自一份。
    pub vector_segments: Option<Vec<VectorSegmentRef<'a>>>,
    /// **自适应融合开关**（V2 Step 9 / D-S9-03 / FR-35；**默认 `false`**）。
    ///
    /// - `false` ⇒ 编排层**不构造 `FusionCtx`、不调 `fuse_adaptive`**（仍走
    ///   `parts.fusion.fuse(&lanes, candidate_k)`）⇒ S9-2 的逐位一致是**结构性的**，
    ///   不靠「调用者记得别传」的约定；
    /// - `true` ⇒ 构造 `FusionCtx` → 调 `fuse_adaptive(..)`；并**再调一次**
    ///   `weights_override` / `fusion_signal` 专用于填 `Metrics`（规则 3：两者都是
    ///   `&self` 纯函数 ⇒ 再调必然与决策同值）。
    ///
    /// ⚠️ 开关只决定「走不走自适应通道」；**规则本体**（信号 + θ）在策略里
    /// （`RrfFusion::new_adaptive`）——开了开关而策略未配规则 ⇒ `weights_override`
    /// 返回 `None` ⇒ 行为与开关关**逐位一致**（`Metrics.fusion_weights` 报 `None`）。
    pub adaptive_fusion: bool,
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
/// take_n            = k == 0 ? 0 : min(max(k, window), 融合条数)
/// handed            = 窗口内**可回捞**的条数（≤ take_n；陈旧 chunk_id 会让它更小）
/// metrics.rerank_window = handed                         // ⚠️ 记**实际交接**值，非公式值
/// ```
///
/// 1. `window ≤ candidate_k`（否则窗口被候选池**静默封顶**）；
/// 2. `metrics.rerank_window ≤ k` ⟺ 本次**没有**可用的额外候选（放开失效）；
/// 3. 默认实现下 `window == k` ⇒ `handed == take_n == k`、`candidate_k == max(3k, 10)`
///    ⇒ 与精排引入前**逐位一致**；
/// 4. `k == 0` 时**一条都不交给精排**（退化输入的浪费护栏，见下文注释）。
pub fn search_parts(
    parts: &SearchParts<'_>,
    query: &str,
    mode: SearchMode,
    k: usize,
    filter: Option<&crate::schema::Filter>,
) -> Result<SearchResponse> {
    let started = Instant::now();
    let mut metrics = Metrics::default();

    // 跨段载具（`Some` ⇒ 走 `main + deltas`）
    let segmented = parts.segment_set.as_ref();
    // 🔴 段数 / 墓碑数必须在**空库早退之前**填（`S8-03` 评审 P2-2 的最低限观测，NFR-07）：
    //    否则三条早退路径留下的都是草稿缓冲区的 `0`，而「`segments` 到底几个」恰恰是
    //    调用方判断「本次结果是不是只来自一部分内容」的唯一信号（与 `vector_segments` 连看）。
    //    ⚠️ 单段装配（`segment_set == None`）下 `segments = 1`：视图里确实有一个 `main` 段
    //    （可能是空的）—— 「没有 delta」与「没有段」是两件事。
    metrics.segments = segmented.map_or(1, |set| set.segments.len());
    metrics.tombstoned = segmented.map_or(0, |set| set.tombstones);
    // 空库判定：跨段时看**全局** N（`main` 空但有 delta 时库不是空的）
    let index_is_empty = match segmented {
        Some(set) => set.n == 0,
        None => parts.index.num_chunks() == 0,
    };
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
    // 🔑 `S8-05`：跨段路径走 `build_all` —— **一次**求值同时拿到两种形态：
    //    ① `global`（全局 `chunk_id` 语义）：BM25 路 + **单段**向量路用；
    //    ② `per_segment`（**段内本地**语义，`S8-05`）：**跨段**向量路用（向量索引只认本段本地 id）。
    //    分成两次构造会让逐段 `doc_bits`（字段索引求值）白算一遍 ⇒ 这是单一入口的理由。
    let built = match parts.predicate_builder.as_deref() {
        // 跨段：谓词构造在门面层完成（它才持有 `View`）
        Some(b) => b.build_all(filter),
        None => crate::query::filter::BuiltPredicates {
            global: super::filter::try_build_predicate(parts.index, filter),
            // 单段：本地 `chunk_id` == 全局 ⇒ 逐段形态无意义（向量路走单索引分支）
            per_segment: None,
        },
    };
    let per_seg_filters = built.per_segment;
    let predicate = match built.global {
        Some(p) => Some(p),
        None => {
            // 过滤条件排空了所有文档：不必进两路召回（省掉一次完整检索）。
            //
            // 空原因与下方 `fused.is_empty()` 分支**同口径**（§5.8.1：query 侧信号优先）：
            // query 本身就无命中时，报"你的过滤太窄"是误导——Agent 会去调过滤条件，
            // 而真正的问题在 query。故此处也跑一次词典探针（O(query 词数)，
            // 相比省下的一次完整检索可忽略）。
            let reason = if query_is_empty || !query_has_hits_any(parts, segmented, query) {
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
    // BM25：无用户过滤时传 None（postings 已物理摘除死 chunk，无需再判活）。
    //
    // ⚠️ `S8-04`：**有跨段墓碑时必须传**（`R52` / §4.7.3）—— 墓碑指向的 doc 在它所属的
    // 段里**仍然存活**、postings 也还在，只有谓词能挡住它。判据就是「有墓碑」，
    // 而不是「有多段」：多段但无墓碑时活 postings 依然是干净的（段内删除总是物理摘除）。
    let bm25_needs_filter = filter.is_some() || segmented.is_some_and(|set| set.tombstones > 0);
    let bm25_f = if bm25_needs_filter { pred } else { None };
    // 向量：恒传谓词（hnsw_rs 无法摘除已删向量，Q-C1）
    let vec_f = pred;

    // 向量路依赖**只解析一次**：`Vector` / `Hybrid` 都要用它，`Bm25` 根本不需要。
    //
    // `S8-05`：**两种形态**——跨段（`vector_segments` 有值）⇒ 逐段载具 + 逐段谓词 + 逐段 route；
    // 单段 ⇒ 单索引（本地 `chunk_id` == 全局），与 `S8-03` 之前**同一份实现**（零回归）。
    let vec_lane: Option<VecLane<'_>> = match mode {
        SearchMode::Bm25 => None,
        SearchMode::Vector | SearchMode::Hybrid => {
            let e = parts.embedder.ok_or(Error::NoEmbedder)?;
            match (parts.vector_segments.as_deref(), per_seg_filters.as_deref()) {
                (Some(segs), Some(per)) => {
                    debug_assert_eq!(
                        per.len(),
                        segs.len(),
                        "逐段谓词必须与向量段列表一一对应（含顺序）"
                    );
                    let filters: Vec<Option<&dyn CandidateFilter>> =
                        per.iter().map(|b| Some(b.as_ref())).collect();
                    // **逐段** route（各段问自己那份谓词）——`plan` 是它们的**并集分类**
                    let routes: Vec<VectorRoute> = segs
                        .iter()
                        .enumerate()
                        .map(|(i, s)| match s.index {
                            Some(vi) => VectorRetriever::new(e, vi).plan(filters[i]),
                            // 该段没有向量索引 ⇒ 不参与（`union_route` 会忽略）
                            None => VectorRoute::None,
                        })
                        .collect();
                    Some(VecLane::Segmented {
                        embedder: e,
                        segments: segs,
                        filters,
                        routes,
                    })
                }
                // 🔑 **结构性兜底**（跨段载具必须与逐段谓词成对出现，见 `Searcher::parts`）：
                //    万一只有前者，退回**单索引**（结果**正确但不完整** —— 即 `S8-05` 之前的
                //    半盲态），而**不是**「所有段共用全局谓词」（那在 delta 段上会**错过滤**，
                //    因为全局谓词的 `contains` 收的是全局 id）。两者的区别是「少召回」vs「错答」，
                //    兜底必须选前者；`debug_assert` 让这条不该发生的路径在测试期炸出来。
                _ => {
                    debug_assert!(
                        parts.vector_segments.is_none(),
                        "跨段向量载具必须与逐段谓词成对出现（`build_all` 的 `per_segment`）"
                    );
                    let (e2, vi) = require_vector(parts.embedder, parts.vector_index)?;
                    Some(VecLane::Single {
                        embedder: e2,
                        index: vi,
                        route: VectorRetriever::new(e2, vi).plan(vec_f),
                    })
                }
            }
        }
    };
    // 向量路本次**实际覆盖的段数**（`S8-03` 评审 P2-2）：`0` = 本次没走向量路。
    // 🔴 `S8-05`（PR5）之前恒为 1 —— `Searcher::parts` 只把 `view.main` 的向量索引交下来
    //    ⇒ `deltas` 里已提交的内容不可向量召回；默认 `mode` 是 `Hybrid` ⇒ 不是边角情形
    //    （只要有向量能力，**每次 `commit()` 之后、`save`/`compact` 之前**都是这个状态）。
    //    ✅ **`S8-05` 起**：跨段时 = 载具里的段数 ⇒ **恒等于 `metrics.segments`**（验收项）。
    metrics.vector_segments = vec_lane.as_ref().map_or(0, |l| l.covered_segments());

    // V2 Step 5：向量路**分派**（ANN / 精确）与**记账**（`Metrics.vector_route`）。
    // 策略由后端 `VectorIndex::prefers_exact` 给出，编排层只做二选一——阈值规则不在这一层复制。
    // `S8-05`：跨段时是**逐段判定 + 并集分类**（全 Ann/全 Exact/混合 ⇒ `Mixed`，见 `union_route`）。
    // `SearchMode::Bm25` 不走向量路 ⇒ `vector_route` 保持默认 `None`（本次检索
    // 有没有向量路是编排层的信息，不该由后端编码）。
    let vec_route = vec_lane.as_ref().map_or(VectorRoute::None, |l| l.route());

    let (bm25_lane, vector_lane) = match mode {
        SearchMode::Bm25 => {
            let t = Instant::now();
            let lane = to_lane(bm25_recall(parts, segmented, query, candidate_k, bm25_f)?);
            metrics.bm25_elapsed = t.elapsed();
            (Some(lane), None)
        }
        SearchMode::Vector => {
            // 上方已解析过一次，这里复用；用 `ok_or` 而非 `expect`
            // ⇒ 检索主路径不留 panic 分支（不可达，但不可达不等于该 panic）。
            let vec = vec_lane.as_ref().ok_or(Error::NoEmbedder)?;
            metrics.vector_route = vec_route;
            let t = Instant::now();
            let lane = to_lane(vec.run(query, candidate_k, vec_f)?);
            metrics.vector_elapsed = t.elapsed();
            (None, Some(lane))
        }
        SearchMode::Hybrid => {
            let vec = vec_lane.as_ref().ok_or(Error::NoEmbedder)?;
            metrics.vector_route = vec_route;
            // 并行执行；谓词是 Send + Sync，可安全跨 rayon 线程共享。
            // 每路各自计时：Hybrid 下单一 `took` 无法归因"是哪一路慢"
            // （D-S5-06；架构 §8.3 的示例日志本就预期 bm25=/vector= 两列）。
            let (r1, r2) = rayon::join(
                || {
                    let t = Instant::now();
                    let r = bm25_recall(parts, segmented, query, candidate_k, bm25_f);
                    (r, t.elapsed())
                },
                || {
                    let t = Instant::now();
                    let r = vec.run(query, candidate_k, vec_f);
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

    // 单路模式：直接用该路结果作为"融合"输出（分数即单路分数）。
    // V2 Step 9（D-S9-01 / D-S9-03）：自适应只在 Hybrid 下有语义（单路没有
    // 「两路分歧」可言）⇒ 开关只在 Hybrid 分支被消费。
    let fused: Vec<(ChunkId, Score)> = if mode == SearchMode::Hybrid {
        if parts.adaptive_fusion {
            // 规则 2：构造 FusionCtx（零拷贝借用 lanes；df 探针只查词典、不回捞正文）
            // → 走自适应通道（决策与应用都在策略内部——编排层不消费 `w`）。
            let lane_stats: Vec<LaneStats<'_>> =
                lanes.iter().map(|l| LaneStats::new(l.as_slice())).collect();
            let ctx = FusionCtx {
                lanes: &lane_stats,
                query_terms: query_tokens.len(),
                query_chars: query.chars().count(),
                has_ascii: query.chars().any(|c| c.is_ascii_alphanumeric()),
                query_df: df_coverage(parts.index, segmented, &query_tokens),
                top_k: k,
            };
            let fused = parts.fusion.fuse_adaptive(&lanes, candidate_k, &ctx);
            // 规则 3（第 3 轮评审 P4-9）：`weights_override` / `fusion_signal` 都是
            // `&self` 纯函数 ⇒ 对同一 ctx 再调一次必然得到与决策时相同的值。
            // （为什么不让 `fuse_adaptive` 顺路带回：那会破坏它与 `fuse` 同形的
            // 「默认转发」、也破坏 S9-T6 的等价性判据；共享可变状态则破坏
            // `Send + Sync` 与并发逐位一致——排除法见设计 §4.1。）
            metrics.fusion_weights = parts.fusion.weights_override(&ctx);
            metrics.fusion_signal = parts.fusion.fusion_signal(&ctx);
            fused
        } else {
            // 规则 1：开关关 ⇒ 不构造 ctx、不调 fuse_adaptive（S9-2 结构性逐位一致）
            parts.fusion.fuse(&lanes, candidate_k)
        }
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
        let reason = if query_is_empty || !query_has_hits_any(parts, segmented, query) {
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

    // ⚠️ **`k == 0` 必须单独挡掉**（PR #53 评审 P3-1）：`window.max(k)` 会让
    // `take_n == window`（> 0）⇒ 把**整个窗口**交给精排、跑完模型再被安全网截回 0 条
    // —— 输出对，但成本白花。而 `LocalReranker::candidate_window` **不看 `k`**
    // （恒返回配置的 `R`）⇒ `k = 0` + `local-rerank` 时**每次查询白跑 `R` 次推理**。
    // 改动前 `.take(k=0)` 给精排喂的是空 vec，而 `LocalReranker` 对空输入**早退**
    // ⇒ 近零成本。本 PR 若不挡，会**无意放大这个退化输入的成本**。
    // 可达性：`helix search --k 0`（`k: usize` 无下界，`SearchRequest::top_n` 也不钳制）。
    // ⚠️ `window == 0`（trait 契约没写「必须 ≥ 1」）**不需要**额外处理：`max(k)` 已兜底。
    let take_n = if k == 0 {
        0
    } else {
        window.max(k).min(fused.len())
    };
    let mut proto: Vec<Hit> = Vec::with_capacity(take_n);
    for (chunk_id, fused_score) in fused.into_iter().take(take_n) {
        // ⚠️ 回捞必须**跨段**（`S8-04`）：`S8-03` 起内容分布在 `main + deltas` 里，
        //    只看 `parts.index`（= 主段）会把 delta 的命中**静默丢掉**。
        //    路径 A：按全局 ID 定位段；路径 B（单段）：与 `S8-03` 之前逐字相同。
        let (chunk, seg_index, base_doc) = match segmented {
            Some(set) => {
                // 陈旧 `chunk_id`（图/索引不同步）⇒ 该条不进 `proto`，故 `proto.len()`
                // 可能**小于** `take_n`（见下方 `handed` 的记账口径）。
                let Some((seg, local)) = set.locate(chunk_id) else {
                    continue;
                };
                let Some(chunk) = seg.index.chunk(local) else {
                    continue;
                };
                (chunk, seg.index, seg.base_doc)
            }
            None => {
                let Some(chunk) = parts.index.chunk(chunk_id) else {
                    continue;
                };
                (chunk, parts.index, 0)
            }
        };
        let doc = seg_index
            .doc(chunk.doc_id)
            .ok_or(Error::ChunkNotFound(chunk_id))?;

        proto.push(Hit {
            chunk_id,
            // 段内 → 全局（`Hit.doc_id` 的对外契约是**全局** ID）
            doc_id: base_doc + chunk.doc_id,
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
    // ⚠️ **记的是「实际交接条数」`handed`，不是公式值 `take_n`**（PR #53 评审 P3-2）：
    // 第 3 步的陈旧 `chunk_id` 防御路径会让 `proto.len() < take_n`，而字段自称的是
    // 「**实际**交给精排的候选条数」⇒ 记公式值会在那条路径上**高报**。
    // 正常路径两者恒等（所有现有断言不受影响）；**两者之差本身是诊断信号**
    // （差 > 0 ⇒ 本次窗口里有已不可回捞的 chunk）。
    let handed = proto.len();
    let t_rerank = Instant::now();
    let mut hits = parts.reranker.rerank(query, proto, k)?;
    metrics.rerank_elapsed = t_rerank.elapsed();
    metrics.rerank_window = handed;

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

/// **向量路的两种形态**（`S8-05`）。
///
/// 抽成一个枚举的理由：`Vector` / `Hybrid` 两个模式臂与它们的计时/记账都要用同一份
/// 「单段 or 跨段」的判断 —— 散在三处写会让「跨段时忘了逐段查」这类回退**静默**发生
/// （那正是 `S8-05` 要修的半盲态）。
enum VecLane<'v> {
    /// **单段**：一个索引 + 全局谓词。本地 `chunk_id` == 全局 ⇒ 与 `S8-03` 之前逐位一致。
    Single {
        embedder: &'v dyn Embedder,
        index: &'v dyn VectorIndex,
        /// 单段 route（`plan` 的结果，构造时算好）
        route: VectorRoute,
    },
    /// **跨段**（`S8-05`）：逐段索引（FIFO）+ 逐段**段内**谓词 + 逐段 route。
    Segmented {
        embedder: &'v dyn Embedder,
        segments: &'v [VectorSegmentRef<'v>],
        /// 与 `segments` 一一对应；`None` 项 = 该段不过滤
        filters: Vec<Option<&'v dyn CandidateFilter>>,
        /// 与 `segments` 一一对应；无向量索引的段为 `VectorRoute::None`（不参与并集）
        routes: Vec<VectorRoute>,
    },
}

impl VecLane<'_> {
    /// 本次向量路**覆盖**的段数（`Metrics.vector_segments`）。
    ///
    /// ⚠️ 口径 = 「**参与归并**的段数」（跨段时是载具长度），**不是**「实际查到东西的段数」：
    /// 某段内容为空 / 无向量索引时它对归并的贡献是**空集**，但它**被覆盖了**
    /// （不存在「漏召回」）。`S8-05` 的验收项是 `vector_segments == segments` ⇒ 必须用这个口径。
    fn covered_segments(&self) -> usize {
        match self {
            VecLane::Single { .. } => 1,
            VecLane::Segmented { segments, .. } => segments.len(),
        }
    }

    /// 本次向量路的 route（单段 = 它自己的；跨段 = **逐段并集分类**）。
    fn route(&self) -> VectorRoute {
        match self {
            VecLane::Single { route, .. } => *route,
            VecLane::Segmented { routes, .. } => union_route(routes),
        }
    }

    /// 执行向量路召回。
    fn run(
        &self,
        query: &str,
        k: usize,
        global_filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<Scored>> {
        match self {
            VecLane::Single {
                embedder,
                index,
                route,
            } => {
                let r = VectorRetriever::new(*embedder, *index);
                dispatch_vector(&r, *route, query, k, global_filter)
            }
            VecLane::Segmented {
                embedder,
                segments,
                filters,
                routes,
            } => SegmentedVectorRetriever::new(*embedder, segments).search_segmented(
                query,
                k,
                filters,
                |i| routes[i],
            ),
        }
    }
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

/// **BM25 召回**（单段 / 跨段统一入口，`S8-04`）。
///
/// `Some(set)` ⇒ [`SegmentedBm25Retriever`]（term 外层、**段**内层，全局整数统计量）；
/// `None` ⇒ 单段 [`Bm25Retriever`]（与 `S8-03` 之前**同一份实现**，零回归）。
fn bm25_recall(
    parts: &SearchParts<'_>,
    segmented: Option<&SegmentSet<'_>>,
    query: &str,
    k: usize,
    filter: Option<&dyn CandidateFilter>,
) -> Result<Vec<Scored>> {
    match segmented {
        Some(set) => SegmentedBm25Retriever::new(set, parts.analyzer)
            .with_params(parts.bm25_params)
            .search_filtered(query, k, filter),
        None => Bm25Retriever::new(parts.index, parts.analyzer)
            .with_params(parts.bm25_params)
            .search_filtered(query, k, filter),
    }
}

/// 词典探针（单段 / 跨段统一入口）：query 的词在**任何段**里有 posting 吗？
///
/// 用于空结果时区分「query 侧无命中」与「过滤太窄」（§5.8.1：query 侧信号优先）。
fn query_has_hits_any(
    parts: &SearchParts<'_>,
    segmented: Option<&SegmentSet<'_>>,
    query: &str,
) -> bool {
    match segmented {
        Some(set) => set.any_term_hits(parts.analyzer, query),
        None => query_has_hits(parts.index, parts.analyzer, query),
    }
}

/// query 词项的**索引侧 df 覆盖率**（V2 Step 9：s3 信号的数据源，I9-1——只查
/// 词典、不取正文，与「融合不回捞正文」的硬约束不冲突）。
///
/// 口径：**去重**词项中 `df > 0` 的占比 ∈ [0, 1]（BM25 累加刻意不去重，但
/// 「覆盖率」问的是 query 与语料的词面交集，重复词不改变事实）。跨段时对
/// **每段**各查一次（任一段有即算覆盖——`set.segments` 含 `main`）。
/// 空 query（0 词项）⇒ `0.0`（该情形 BM25 lane 必空，tier 1 已兜住）。
///
/// ⚠️ **`df > 0` 的含义是「词典收录」，不是「可召回」**（评审 P4-6）：
/// `Index::doc_freq` = `postings[id].len()`，计数**含未物理化的墓碑条目**
/// （物理摘除由删除路径负责）⇒ 某词若只出现在已删、未 compact 的分片里，
/// 覆盖率会被**轻微高估**。方向上保守（信号偏高 ⇒ `s < θ` 更难成立 ⇒
/// 更少弃 BM25 路，不会误伤该保留的路），不改实现；只在此把口径写明。
///
/// 成本 O(|去重词项| × 段数) 次哈希查找，与 `query_has_hits` 同族，可忽略。
fn df_coverage(
    index: &Index,
    segmented: Option<&SegmentSet<'_>>,
    query_tokens: &[crate::analyze::Token],
) -> Score {
    let mut seen = std::collections::HashSet::new();
    let distinct: Vec<&str> = query_tokens
        .iter()
        .filter(|t| seen.insert(t.term.as_str()))
        .map(|t| t.term.as_str())
        .collect();
    if distinct.is_empty() {
        return 0.0;
    }
    let covered = distinct
        .iter()
        .filter(|t| match segmented {
            Some(set) => set.segments.iter().any(|seg| seg.index.doc_freq(t) > 0),
            None => index.doc_freq(t) > 0,
        })
        .count();
    covered as Score / distinct.len() as Score
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
    /// 重排策略。⚠️ 与 `fusion` 的 `Box` 不同，这里是 **`Arc`**（V2 Step 7 / S7-03）：
    /// 精排器可能持有**需常驻的大对象**（`LocalReranker` 的 ONNX 会话 ≈2.19GB，
    /// 架构 R44），而 `bench` 会对「每 mode × 每 run」各装配一次 searcher
    /// ⇒ 必须能**共享同一实例**，否则会重复加载模型（秒级 × N，且内存峰值叠加）。
    reranker: Arc<dyn Reranker>,
    /// BM25 参数（P5 网格搜索从外部注入；默认 Bm25Params::default()）
    bm25_params: Bm25Params,
    /// 自适应融合开关（V2 Step 9；**默认 `false`**——见 `SearchParts::adaptive_fusion`）
    adaptive_fusion: bool,
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
            reranker: Arc::new(NoOpReranker),
            bm25_params: Bm25Params::default(),
            adaptive_fusion: false,
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

    /// 打开自适应融合通道（V2 Step 9 / T7-14 / FR-35；默认关 ⇒ 逐位一致）。
    ///
    /// ⚠️ 开关只决定编排层走不走 `fuse_adaptive`；**规则本体**（信号 + θ）在
    /// 策略里——请配套 `with_fusion(Box::new(RrfFusion::new_adaptive(..)))`。
    /// 开了开关而策略未配规则 ⇒ 行为与关**逐位一致**（`weights_override` 返回
    /// `None` 退回 `fuse`）。
    pub fn with_adaptive_fusion(mut self, on: bool) -> Self {
        self.adaptive_fusion = on;
        self
    }

    /// 覆盖重排策略（默认 `NoOpReranker`，P7 再接真实 rerank）。
    ///
    /// ⚠️ 本方法**签名与语义一字未改**（V2 Step 7 / S7-03 把内部存储换成 `Arc`
    /// 属纯实现细节）⇒ 既有调用方（含 `tests`）零改动。
    ///
    /// 若精排器是**需共享的重对象**（如 `LocalReranker` 的 2.19GB ONNX 会话），
    /// 用 [`Self::with_reranker_arc`] 而不是每次新建。
    pub fn with_reranker(mut self, reranker: Box<dyn Reranker>) -> Self {
        self.reranker = Arc::from(reranker);
        self
    }

    /// 覆盖重排策略（**共享**形态：`Arc`）。
    ///
    /// # 为什么需要它（V2 Step 7 / S7-03 实现期发现）
    ///
    /// `bench` 的装配是「**每 mode × 每 run 各一次** `QueryExecutor`」
    /// （`crates/cli/src/bench.rs` 的 `make_searcher`，4 处调用点）。而逐 lane
    /// 自定义组装的逃生舱只有 [`Self::with_reranker`]（收 `Box`）⇒ 想要**复用**
    /// 同一个精排器实例，只能自己写一个转发 trait 的包装类型 —— 那会在
    /// [`Reranker`] 将来新增方法时**静默漏转发**（`candidate_window` 漏了就是
    /// 窗口静默失效，正是 NFR-07 禁止的那类）。⇒ 在库内给出 `Arc` 入口，
    /// 让「底层组装」与「门面装配」（`Config::reranker` 本就是 `Arc`）同型。
    pub fn with_reranker_arc(mut self, reranker: Arc<dyn Reranker>) -> Self {
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
            // ⚠️ `QueryExecutor` 是**单段逃生舱**（它持 `&Index`，不认识 `View` / 分段）
            // ⇒ 两个跨段字段恒为空 ⇒ 走的还是 `S8-03` 之前那条单段路径（逐位不变）。
            segment_set: None,
            predicate_builder: None,
            vector_segments: None,
            adaptive_fusion: self.adaptive_fusion,
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
    use std::sync::Arc;

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
        /// 两条路径收到的 `k` 参数的**最大值**（V2 Step 7 / `S7_T3` 的空转防护）。
        ///
        /// `k` 是候选池大小进后端的**唯一入口**：编排层若只把截断改成窗口 `R` 而没联动
        /// `candidate_k`，这里读到的就会**小于** `R` ⇒ 判据可证伪（同 Step 6 的 F1 一族：
        /// 「改对了参数、但被上游的上限吃掉」）。
        max_k: AtomicUsize,
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
                max_k: AtomicUsize::new(0),
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

        /// 后端实际见过的最大 `k`（= 编排层下发的候选池大小）。
        fn max_k(&self) -> usize {
            self.max_k.load(AtomicOrdering::SeqCst)
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
            k: usize,
            _filter: Option<&dyn CandidateFilter>,
        ) -> Result<Vec<(ChunkId, f32)>> {
            self.ann_calls.fetch_add(1, AtomicOrdering::SeqCst);
            self.max_k.fetch_max(k, AtomicOrdering::SeqCst);
            Ok(Self::fixed(&self.ann_ids))
        }

        fn search_exact_filtered(
            &self,
            _query: &crate::vector::NormalizedVector,
            k: usize,
            _filter: Option<&dyn CandidateFilter>,
        ) -> Result<Vec<(ChunkId, f32)>> {
            self.exact_calls.fetch_add(1, AtomicOrdering::SeqCst);
            self.max_k.fetch_max(k, AtomicOrdering::SeqCst);
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

    // ========================================================================
    // V2 Step 7 / S7-02：精排窗口打通（`candidate_k` 联动 / `take_n` / `explain` 时机 /
    // `Metrics` 采集点 / `Explain.rerank_score`）。
    //
    // 全部用**间谍**，不装真模型：2.19GB 的 `LocalReranker` 进不了 CI，而且那会把
    // 「**编排层**做错了」与「模型行为」混在一起 —— 同 `S5_T1` 用间谍向量后端的理由。
    // 真模型的确定性探针在 `tests/step7_rerank_local.rs`（全 `#[ignore]`）。
    // ========================================================================

    /// 12 条语料（`chunk_id = 0..12`）+ 间谍向量后端（ANN 返回**全部 12 条**）。
    ///
    /// `k = 10` 而融合得 12 条 ⇒ 「精排窗口」与「候选池」的区别才**可观测**
    /// （若语料 ≤ k，窗口放开与不放开的结果必然相同 ⇒ 用例变空转）。
    fn build_window_fixture() -> (Index, MixedAnalyzer, SpyVectorIndex) {
        let texts: Vec<String> = (0..12).map(|i| format!("检索 文档 {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let (index, analyzer) = build_tagged_index(&refs, "kept");
        let ids: Vec<ChunkId> = (0..12).collect();
        // 策略为假 ⇒ 走 ANN（单路模式下 `prefers_exact` 看的是 `FilterKind::Alive`）
        (
            index,
            analyzer,
            SpyVectorIndex::new(false, ids.clone(), ids),
        )
    }

    fn ids_of(r: &SearchResponse) -> Vec<ChunkId> {
        r.hits.iter().map(|h| h.chunk_id).collect()
    }

    /// 间谍向量后端返回的「距离」是 `d = id × 0.5`，而实现里
    /// `score = 1 − d/2`（`VectorRetriever::to_scored`）。
    /// 三处期望值都走这一个函数，避免「测试自己把公式抄错」。
    fn lane_score_of(chunk_id: ChunkId) -> f32 {
        1.0 - (chunk_id as f32 * 0.5) / 2.0
    }

    /// `explain` 的**五字段**快照（D-S7-06 的回归护栏：推迟只改时机、不改结果）。
    /// `rerank_score` 单列（它是新增的第六个字段，语义不同类）。
    type ExplainSnapshot = (
        Vec<String>,
        Option<Score>,
        Option<u32>,
        Option<Score>,
        Option<u32>,
        Score,
    );

    fn explain_snapshot(h: &Hit) -> ExplainSnapshot {
        (
            h.explain.matched_terms.clone(),
            h.explain.bm25_score,
            h.explain.bm25_rank,
            h.explain.vector_score,
            h.explain.vector_rank,
            h.explain.fused_score,
        )
    }

    /// 间谍精排器的**观测窗**：`Arc` 共享 ⇒ 它被装进 `Box<dyn Reranker>` 移走后，
    /// 测试仍能读到「它看见了什么」。
    #[derive(Clone, Default)]
    struct SpyObs {
        seen_len: Arc<AtomicUsize>,
        seen_scored_in: Arc<AtomicUsize>,
    }

    impl SpyObs {
        /// 最近一次 `rerank` 收到的候选条数（= 编排层算出的 `take_n`）。
        fn seen_len(&self) -> usize {
            self.seen_len.load(AtomicOrdering::SeqCst)
        }

        /// 最近一次 `rerank` 入参里 `explain.rerank_score.is_some()` 的条数。
        ///
        /// **恒为 0 就是读侧收缩（D-S7-06）的证据**：编排层在精排**之前**不写
        /// `rerank_score` ⇒ 精排器拿不到「谁被打分过」。
        fn seen_scored_in(&self) -> usize {
            self.seen_scored_in.load(AtomicOrdering::SeqCst)
        }
    }

    /// 间谍精排器：记录入参，并按开关施加**可预测**的变换。
    struct SpyReranker {
        /// `candidate_window` 的返回值（H3 的 `R`）
        window: usize,
        /// `true` ⇒ 用「输入位置越靠后 logit 越大」替换 `score = σ(logit)` 并写回
        /// `explain.rerank_score`（D-S7-05）。这样**窗口外**的候选才有能力翻上来 ——
        /// 那正是放开窗口想要的那部分收益。
        promote_tail: bool,
        /// `true` ⇒ 故意**不截断**（复现「实现违反出参契约」，验编排层的安全网）。
        violate_contract: bool,
        obs: SpyObs,
    }

    impl SpyReranker {
        /// 返回 `(间谍, 观测窗)`。
        ///
        /// ⚠️ 刻意 `sleep(1ms)`：让 `metrics.rerank_elapsed` 的断言**不依赖计时器
        /// 分辨率**（`> Duration::ZERO` 在粗糙时钟上有假红风险），同时证明该字段包的
        /// 确实是 `rerank()` 这**一次**调用，而不是别的东西。
        fn new(window: usize, promote_tail: bool, violate_contract: bool) -> (Self, SpyObs) {
            let obs = SpyObs::default();
            (
                Self {
                    window,
                    promote_tail,
                    violate_contract,
                    obs: obs.clone(),
                },
                obs,
            )
        }
    }

    impl Reranker for SpyReranker {
        fn candidate_window(&self, _k: usize) -> usize {
            self.window
        }

        fn rerank(&self, _query: &str, hits: Vec<Hit>, top_n: usize) -> Result<Vec<Hit>> {
            self.obs.seen_len.store(hits.len(), AtomicOrdering::SeqCst);
            self.obs.seen_scored_in.store(
                hits.iter()
                    .filter(|h| h.explain.rerank_score.is_some())
                    .count(),
                AtomicOrdering::SeqCst,
            );
            std::thread::sleep(Duration::from_millis(1));

            let mut out = hits;
            if self.promote_tail {
                for (i, h) in out.iter_mut().enumerate() {
                    // logit 随**输入位置**单调增 ⇒ 与输入序**刻意反序**（否则
                    // 「按 index 回填」与「按位置 zip」的结果恰好一样，断言没有鉴别力）。
                    let logit = (i + 1) as f32;
                    h.explain.rerank_score = Some(logit);
                    h.score = 1.0 / (1.0 + (-logit).exp());
                }
                // D-S7-07：score 降序、并列 chunk_id 升序
                out.sort_by(|a, b| {
                    b.score
                        .total_cmp(&a.score)
                        .then(a.chunk_id.cmp(&b.chunk_id))
                });
            }
            if !self.violate_contract {
                out.truncate(top_n);
            }
            Ok(out)
        }
    }

    /// **S7-T1（D-S7-02 的零回归）**：默认窗口 == `k` ⇒ 候选池与交给精排的条数
    /// **都不变**；且结果与 `NoOpReranker` 逐字段一致。
    ///
    /// ⚠️ 本用例的**牙齿**在 `rerank_window == k` 与 `max_k == 3k` 这两条上，**不在**
    /// 「结果是否相同」上：若默认实现被改成 `k + 1`，多出来的那条会被精排截掉 ⇒
    /// 最终 `hits` 看上去一模一样。这正是设计要「钉空转防护」而不是只比结果的原因。
    #[test]
    fn S7_T1_默认窗口不放大候选池且与NoOp逐字段一致() {
        let (index, analyzer, vi) = build_window_fixture();
        let e = FakeEmbedder;

        // ① 默认装配（`NoOpReranker`）：窗口 = k
        let s_noop = QueryExecutor::new(&index, &analyzer).with_vector(&e, &vi);
        let r_noop = s_noop.search("检索", SearchMode::Vector, 10).unwrap();

        // ② 间谍精排器，声明的窗口也 == k（行为等价于 NoOp）
        let (spy, obs) = SpyReranker::new(10, false, false);
        let s_spy = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_reranker(Box::new(spy));
        let r_spy = s_spy.search("检索", SearchMode::Vector, 10).unwrap();

        assert_eq!(r_spy.metrics.rerank_window, 10, "默认窗口必须 == k");
        assert_eq!(obs.seen_len(), 10, "take_n == k ⇒ 精排只该收到 k 条");
        assert_eq!(
            vi.max_k(),
            30,
            "candidate_k 必须仍是 max(3k, 10) = 30（窗口未参与放大）"
        );

        assert_eq!(ids_of(&r_noop), ids_of(&r_spy), "hits 顺序逐位一致");
        for (a, b) in r_noop.hits.iter().zip(&r_spy.hits) {
            assert_eq!(a.score.to_bits(), b.score.to_bits(), "分数逐位一致");
            assert_eq!(
                explain_snapshot(a),
                explain_snapshot(b),
                "explain 五字段逐字段一致"
            );
            assert_eq!(a.explain.rerank_score, None);
            assert_eq!(b.explain.rerank_score, None);
        }
    }

    /// **S7-T2（H3 的核心）**：`window = 100 > k = 10` ⇒ 交给精排的是
    /// `min(max(k, window), 融合条数) = 12` 条，而**返回条数仍 ≤ `k`**。
    #[test]
    fn S7_T2_窗口大于k时交给精排的是窗口条数() {
        let (index, analyzer, vi) = build_window_fixture();
        let e = FakeEmbedder;
        let (spy, obs) = SpyReranker::new(100, false, false);
        let s = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_reranker(Box::new(spy));

        let r = s.search("检索", SearchMode::Vector, 10).unwrap();

        assert_eq!(obs.seen_len(), 12, "take_n = min(max(k, R), 12) = 12");
        assert_eq!(
            r.metrics.rerank_window, 12,
            "NFR-07：实际交给精排的条数必须可观测"
        );
        assert_eq!(r.hits.len(), 10, "返回条数仍由 top_n = k 决定");
        assert_eq!(vi.max_k(), 100, "候选池必须随窗口放大到 ≥ R");
    }

    /// **S7-T3（D-S7-03 的空转防护）**：`R` 必须真的传导到**向量后端收到的 `k`**。
    ///
    /// 用**四档** `R`（< 3k / == 3k / > 3k / 远大于 3k）读后端实际收到的 `k`：
    /// 只把截断改成 `R` 而 `candidate_k` 不联动时，后两档会立刻暴露
    /// （后端收到的仍是 30 ⇒ `R = 200` 的窗口被静默封顶）。
    #[test]
    fn S7_T3_候选池必须随窗口真的放大() {
        let (index, analyzer, _) = build_window_fixture();
        let e = FakeEmbedder;
        const K: usize = 10;

        for (window, expected_k, label) in [
            (10usize, 30usize, "R <= 3k：候选池由 3k 兜底"),
            (30, 30, "R == 3k：正好对齐"),
            (100, 100, "R > 3k：必须由 R 决定（否则窗口被静默封顶）"),
            (200, 200, "R >> 3k"),
        ] {
            let ids: Vec<ChunkId> = (0..12).collect();
            let vi = SpyVectorIndex::new(false, ids.clone(), ids);
            let (spy, _obs) = SpyReranker::new(window, false, false);
            let s = QueryExecutor::new(&index, &analyzer)
                .with_vector(&e, &vi)
                .with_reranker(Box::new(spy));
            let r = s.search("检索", SearchMode::Vector, K).unwrap();

            assert_eq!(vi.max_k(), expected_k, "{label}：后端收到的 k 参数");
            assert!(
                vi.max_k() >= window,
                "{label}：不变式 `window <= candidate_k` 必须成立"
            );
            assert_eq!(
                r.metrics.rerank_window,
                window.min(12),
                "{label}：窗口记账（融合只有 12 条 ⇒ 上界 12）"
            );
        }
    }

    /// **S7-T4**：窗口对 `NoOp` 路径**零影响** —— `R` 取到 `R > 融合条数` 也与默认窗口
    /// **逐位一致**。
    ///
    /// 机制：窗口只让**末尾**多进几条候选，而 `NoOp` 保序 ⇒ 截到 `k` 后前缀不变。
    #[test]
    fn S7_T4_窗口对NoOp路径结果零影响() {
        let (index, analyzer, vi) = build_window_fixture();
        let e = FakeEmbedder;

        let s_default = QueryExecutor::new(&index, &analyzer).with_vector(&e, &vi);
        let base = s_default.search("检索", SearchMode::Vector, 10).unwrap();
        assert_eq!(base.metrics.rerank_window, 10, "基准 = 默认窗口");
        assert_eq!(base.hits.len(), 10);

        for window in [10usize, 11, 12, 50, 100] {
            let (spy, obs) = SpyReranker::new(window, false, false);
            let s = QueryExecutor::new(&index, &analyzer)
                .with_vector(&e, &vi)
                .with_reranker(Box::new(spy));
            let r = s.search("检索", SearchMode::Vector, 10).unwrap();

            assert_eq!(ids_of(&base), ids_of(&r), "R = {window}：hits 顺序必须不变");
            for (a, b) in base.hits.iter().zip(&r.hits) {
                assert_eq!(
                    a.score.to_bits(),
                    b.score.to_bits(),
                    "R = {window}：分数必须不变"
                );
                assert_eq!(explain_snapshot(a), explain_snapshot(b));
            }
            assert_eq!(
                r.metrics.rerank_window,
                window.min(12),
                "R = {window}：窗口如实记账"
            );
            // ⚠️ 前提自证：越过 k 的那几档**确实**多给了候选，否则本用例是空转
            assert_eq!(obs.seen_len(), window.min(12), "R = {window}：入参条数");
        }
    }

    /// **S7-T5（编排层侧）**：窗口放开要能**真的改变结果**。
    ///
    /// 同一个精排器（「越靠后的候选 logit 越大」）在两档下给出**不同冠军**：
    /// `R = k = 10` 时只看得到前 10 条 ⇒ 冠军是窗口内最后一条（`chunk_id = 9`）；
    /// `R = 12` 时看到全部 12 条 ⇒ 冠军变成原本**在窗口外**的 `chunk_id = 11`。
    /// ⚠️ **两档的结论都要写出来**：只断言「`R = 12` 出 `chunk_id = 11`」无法排除巧合。
    #[test]
    fn S7_T5_窗口放开能真的改变最终结果() {
        let (index, analyzer, vi) = build_window_fixture();
        let e = FakeEmbedder;
        let run = |window: usize| -> (Vec<ChunkId>, usize) {
            let (spy, obs) = SpyReranker::new(window, true, false);
            let s = QueryExecutor::new(&index, &analyzer)
                .with_vector(&e, &vi)
                .with_reranker(Box::new(spy));
            let r = s.search("检索", SearchMode::Vector, 10).unwrap();
            (ids_of(&r), obs.seen_len())
        };

        let (narrow, seen_narrow) = run(10);
        assert_eq!(seen_narrow, 10, "R = k ⇒ 精排只看得到前 k 条");
        assert_eq!(
            narrow[0], 9,
            "R = k 时冠军只能是窗口内最后一条（chunk_id = 9）"
        );
        assert_eq!(narrow.len(), 10);

        let (wide, seen_wide) = run(12);
        assert_eq!(seen_wide, 12, "R = 12 ⇒ 精排看到全部 12 条");
        assert_eq!(
            wide[0], 11,
            "R = 12 时冠军变成原本在窗口外的那条（chunk_id = 11）"
        );
        assert_ne!(narrow[0], wide[0], "本用例的鉴别力：两档冠军必须不同");
        assert_eq!(wide.len(), 10, "两档都截到 k");
    }

    /// **S7-T5b（本 PR 新发现的防御路径）**：精排器违反出参契约（返回 > `top_n` 条）时，
    /// 编排层**截断并告警**，不把越界条数漏给调用方。
    ///
    /// ⚠️ 设计 §4.3 的伪码里有 `hits.truncate(k)`（安全网），但**没写它不得静默**；
    /// 本条按 NFR-07 补上 `warn!`，并在这里用「不截断的间谍」把**效果**钉住。
    /// 变异：去掉安全网 ⇒ 本条报红（12 ≠ 10）。
    #[test]
    fn S7_T5b_精排器违约返回超k条时被安全网截断() {
        let (index, analyzer, vi) = build_window_fixture();
        let e = FakeEmbedder;
        let (spy, obs) = SpyReranker::new(12, false, true); // violate_contract = true
        let s = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_reranker(Box::new(spy));

        let r = s.search("检索", SearchMode::Vector, 10).unwrap();
        assert_eq!(obs.seen_len(), 12, "前提自证：精排器确实收到了 12 条");
        assert_eq!(r.hits.len(), 10, "安全网必须把违约的 12 条截回 k = 10");
        assert_eq!(
            r.metrics.fused, 10,
            "口径自洽：fused 记的是**截断后**的条数"
        );
    }

    /// **S7-T6（D-S7-06 的回归护栏）**：`NoOp` 路径下 `explain` 的五个字段与
    /// 「先组装、再精排」的旧口径**逐字段一致** —— 推迟只改**时机**、不改**结果**。
    ///
    /// 用间谍向量后端（返回 id 升序、距离 `d = id × 0.5`）⇒ 每条 hit 的
    /// `vector_rank` / `vector_score` 都能**精确**断言，而不是"看起来有值"。
    #[test]
    fn S7_T6_NoOp下explain五字段口径不变() {
        let (index, analyzer, vi) = build_window_fixture();
        let e = FakeEmbedder;
        let s = QueryExecutor::new(&index, &analyzer).with_vector(&e, &vi);
        let r = s.search("检索", SearchMode::Vector, 10).unwrap();

        assert_eq!(r.hits.len(), 10);
        for (i, h) in r.hits.iter().enumerate() {
            let id = i as ChunkId;
            assert_eq!(h.chunk_id, id, "NoOp 保序 ⇒ 第 i 条就是 lane 的第 i 条");
            assert_eq!(
                h.explain.vector_rank,
                Some(id + 1),
                "rank 从 1 起、按 lane 顺序"
            );
            assert_eq!(
                h.explain.vector_score,
                Some(lane_score_of(id)),
                "lane 分原样透传"
            );
            assert_eq!(h.explain.bm25_rank, None, "Vector 模式没有 BM25 路");
            assert_eq!(h.explain.bm25_score, None);
            assert_eq!(
                h.explain.fused_score.to_bits(),
                lane_score_of(id).to_bits(),
                "单路模式下融合分即该路分"
            );
            assert_eq!(h.score.to_bits(), h.explain.fused_score.to_bits());
            assert_eq!(h.explain.rerank_score, None, "NoOp 从不写该信号");
            assert!(
                h.explain.matched_terms.contains(&"检索".to_string()),
                "第 {i} 条：正文含『检索』⇒ matched_terms 必须非空"
            );
        }
    }

    /// **S7-T7（NFR-07 / NFR-12 的前提）**：两个新 `Metrics` 字段被填且口径自洽。
    #[test]
    fn S7_T7_Metrics新字段被填且口径自洽() {
        let (index, analyzer, vi) = build_window_fixture();
        let e = FakeEmbedder;

        // ── 有结果路径：窗口如实记账；耗时**包住了 `rerank()` 本身** ──
        let (spy, _obs) = SpyReranker::new(100, false, false);
        let s = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_reranker(Box::new(spy));
        let r = s.search("检索", SearchMode::Vector, 10).unwrap();
        assert_eq!(r.metrics.rerank_window, 12);
        assert!(
            r.metrics.rerank_elapsed >= Duration::from_millis(1),
            "间谍 `sleep(1ms)` ⇒ 该字段必须 ≥ 1ms（证明它包的是 rerank() 本身）"
        );
        assert!(
            r.metrics.rerank_elapsed <= r.metrics.took,
            "分段耗时不得大于总耗时"
        );
        assert_eq!(r.metrics.took, r.took, "I7 仍自洽");

        // ── NoOp + 默认窗：`rerank_window == min(k, 融合条数)` ──
        let s_noop = QueryExecutor::new(&index, &analyzer).with_vector(&e, &vi);
        let base = s_noop.search("检索", SearchMode::Vector, 10).unwrap();
        assert_eq!(base.metrics.rerank_window, 10);
        assert_eq!(base.metrics.rerank_window, base.hits.len().min(10));

        // ── 不变式 2 的边界：k > 融合条数 ⇒ 窗口只能取到融合条数 ──
        let r_small = s_noop.search("检索", SearchMode::Vector, 15).unwrap();
        assert_eq!(
            r_small.metrics.rerank_window, 12,
            "融合只有 12 条 ⇒ 窗口封顶在 12（不是 15）"
        );

        // ── 早退路径：默认值必须**不撒谎**（0 = 本次没跑精排）──
        let empty_index = Index::new();
        let analyzer2 = MixedAnalyzer::new();
        let s_empty = QueryExecutor::new(&empty_index, &analyzer2);
        let r_empty = s_empty.search("x", SearchMode::Bm25, 10).unwrap();
        assert_eq!(r_empty.metrics.rerank_window, 0);
        assert_eq!(r_empty.metrics.rerank_elapsed, Duration::ZERO);
    }

    /// **S7-T12（D-S7-05 的编排层侧）**：`rerank_score` 信号端到端成立。
    ///
    /// ① **读侧收缩**：精排器入参里**没有任何** `rerank_score`（`seen_scored_in == 0`）；
    /// ② `score` 必须仍是精排器写的 `σ(logit)`（**编排层不得二次改写** ⇒ 用同一公式复算）；
    /// ③ 融合分**不被改写**（留在 `fused_score`）；
    /// ④ `hits` 降序 ⟺ `rerank_score` 降序（σ 单调，**非**同一公式复算，防「断言=实现」）；
    /// ⑤ **C2 不覆盖 C1**：补齐 `matched_terms` / lane rank 之后 `rerank_score` 仍在。
    #[test]
    fn S7_T12_rerank_score信号成立且C2不覆盖C1() {
        let (index, analyzer, vi) = build_window_fixture();
        let e = FakeEmbedder;
        let (spy, obs) = SpyReranker::new(12, true, false);
        let s = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_reranker(Box::new(spy));
        let r = s.search("检索", SearchMode::Vector, 10).unwrap();

        assert_eq!(
            obs.seen_scored_in(),
            0,
            "① 读侧收缩：编排层在精排**之前**不得写 rerank_score"
        );

        for h in &r.hits {
            let logit = h.explain.rerank_score.expect("② 精排器已给每条打分");
            let expected = 1.0f32 / (1.0f32 + (-logit).exp());
            assert_eq!(
                h.score.to_bits(),
                expected.to_bits(),
                "② chunk {}：编排层不得二次改写 score",
                h.chunk_id
            );
            assert_eq!(
                h.explain.fused_score.to_bits(),
                lane_score_of(h.chunk_id).to_bits(),
                "③ chunk {}：融合分必须原样保留（D-S7-05）",
                h.chunk_id
            );
            assert_ne!(
                h.score.to_bits(),
                h.explain.fused_score.to_bits(),
                "③ 两套分必须是两个量"
            );
            // ⑤ C2 补齐生效，且**没有**冲掉 C1 写的 rerank_score
            assert!(
                !h.explain.matched_terms.is_empty(),
                "C2 必须补齐 matched_terms"
            );
            assert_eq!(
                h.explain.vector_rank,
                Some(h.chunk_id + 1),
                "C2 必须补齐 lane rank"
            );
        }

        // ④ σ 单调 ⇒ 「score 降序」必须等价于「logit 降序」（换了种算法，不是抄公式）
        for w in r.hits.windows(2) {
            let a = w[0].explain.rerank_score.unwrap();
            let b = w[1].explain.rerank_score.unwrap();
            assert!(
                a > b,
                "σ 单调 ⇒ score 降序必须等价于 logit 降序（{a} vs {b}）"
            );
        }
        assert_eq!(
            r.hits[0].explain.rerank_score,
            Some(12.0),
            "冠军是 logit 最大的那条（最后一个位置的输入）"
        );
        assert_eq!(r.hits[0].chunk_id, 11);
    }

    /// **S7-T13（PR #53 评审 P3-1 的护栏）**：`k == 0` 时**一条都不交给精排**。
    ///
    /// 为什么需要这条：`window.max(k)` 在 `k == 0` 时给出 `window`（> 0）⇒ 会把整个窗口
    /// 交给精排（每条含 `text` / `metadata` 克隆）跑完模型再被安全网截回 0 条 ——
    /// **输出对、成本白花**。而 `LocalReranker::candidate_window` **不看 `k`**
    /// （恒返回配置的 `R`）⇒ `k = 0` + `local-rerank` 就是**每次查询白跑 `R` 次推理**。
    /// 改动前 `.take(0)` 给的是空 vec，而 `LocalReranker` 对空输入**早退** ⇒ 近零成本。
    ///
    /// ⚠️ 变异：去掉 `if k == 0 { 0 }` 这个分支 ⇒ 本条报红（`seen_len` / `rerank_window` 都变 12）。
    #[test]
    fn S7_T13_k为零时不给精排喂窗口() {
        let (index, analyzer, vi) = build_window_fixture();
        let e = FakeEmbedder;
        let (spy, obs) = SpyReranker::new(12, false, false);
        let s = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_reranker(Box::new(spy));

        // ── 前提自证（A/B 的 A 档）：k = 1 时窗口确实被放开到 12 ──
        // 用同一个 searcher、同一个间谍：k=1 ⇒ `take_n = max(1, 12).min(12) = 12`。
        // 若没有下面那条护栏，k=0 会得到同样的 12 —— 本档就是证明「k=0 的 0 是护栏给的」。
        let wide = s.search("检索", SearchMode::Vector, 1).unwrap();
        assert_eq!(
            obs.seen_len(),
            12,
            "k = 1 ⇒ 窗口仍是 12（说明 R = 12 真的生效）"
        );
        assert_eq!(wide.metrics.rerank_window, 12);

        let r = s.search("检索", SearchMode::Vector, 0).unwrap();
        assert_eq!(obs.seen_len(), 0, "k = 0 ⇒ 精排不该收到任何候选");
        assert_eq!(
            r.metrics.rerank_window, 0,
            "口径自洽：实际交接条数为 0（不是窗口值 12）"
        );
        assert!(r.hits.is_empty(), "k = 0 ⇒ 输出必然为空");
        assert_eq!(r.metrics.fused, 0);
        // 候选池仍按公式算：`candidate_k = max(3*0, 12, 10) = 12`（不是 30）。
        // ⚠️ 如实说明**残留**：召回那一半在 `k = 0` 时**照样跑**（改动前后都如此，非本 PR 引入）；
        // 本护栏只挡掉「窗口交给精排」这一半。让 `k = 0` 在 API 边界不可达（CLI `--k` 下界）
        // 是更治本的做法，归 **PR 3（S7-03）** 的接口面。
        assert_eq!(vi.max_k(), 12, "candidate_k = max(3*0, 12, 10) = 12");
    }

    /// **S7-T14（PR #53 评审 P3-2 的护栏）**：`metrics.rerank_window` 记**实际交接**条数，
    /// 而不是窗口公式值。
    ///
    /// 构造：向量后端多返回一个**陈旧 `chunk_id`**（不在 `index` 里，模拟图 / 索引不同步）
    /// ⇒ 窗口 `take_n = min(max(k,R), 13) = 13`，而第 3 步的防御路径跳过陈旧条
    /// ⇒ **实际交给精排的是 12 条**。字段自称「**实际**交给精排的候选条数」⇒ 必须记 12。
    ///
    /// ⚠️ 变异：把 `metrics.rerank_window = handed` 改回 `= take_n` ⇒ 本条报红（13 ≠ 12）。
    #[test]
    fn S7_T14_陈旧chunk_id时记实际交接条数() {
        let texts: Vec<String> = (0..12).map(|i| format!("检索 文档 {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let (index, analyzer) = build_tagged_index(&refs, "kept");

        let mut ids: Vec<ChunkId> = (0..12).collect();
        ids.push(999); // 陈旧 chunk_id：不在 index 里
        let vi = SpyVectorIndex::new(false, ids.clone(), ids);
        let e = FakeEmbedder;

        let (spy, obs) = SpyReranker::new(100, false, false);
        let s = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_reranker(Box::new(spy));
        let r = s.search("检索", SearchMode::Vector, 10).unwrap();

        assert_eq!(
            r.metrics.candidates, 13,
            "前提自证：融合结果确实是 13 条（含陈旧条）"
        );
        assert_eq!(obs.seen_len(), 12, "陈旧条不得进 `proto`");
        assert_eq!(
            r.metrics.rerank_window, 12,
            "必须记**实际交接**条数（12），不是窗口公式值（13）"
        );
        assert_eq!(
            r.metrics.rerank_window,
            obs.seen_len(),
            "口径自洽：字段 == 精排实际入参条数"
        );
        assert!(
            r.hits.iter().all(|h| h.chunk_id != 999),
            "陈旧条不得出现在结果里"
        );
    }
    // ---- V2 Step 9 / T7-14：自适应融合（编排层判据 S9-T1/T4/T5/T7/T10 + df 探针） ----

    use crate::fusion::{AdaptiveRule, AdaptiveSignal};
    use crate::retriever::{SegmentRef, SegmentSet};

    /// S9 夹具：带向量路的 Hybrid 装配（全确定性：FakeEmbedder + brute 后端）。
    fn build_hybrid_fixture(texts: &[&str]) -> (Index, MixedAnalyzer, BruteForceIndex) {
        let (index, analyzer) = build_index(texts);
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
        (index, analyzer, vi)
    }

    /// 武装态规则（s4：`has_ascii` 的 0/1 —— 中文 query 恒触发、ASCII query 恒不触发，
    /// 刻意用它让「触发/不触发」在编排层**确定性可达**，不依赖语料的 df 分布）。
    fn armed_shape_rule(theta: f32) -> AdaptiveRule {
        AdaptiveRule {
            signal: AdaptiveSignal::QueryShape,
            theta,
        }
    }

    /// 逐位比较 hits（`f32` 用 `to_bits`，与既有判据同口径）。
    fn s9_hits_bits(r: &SearchResponse) -> Vec<(ChunkId, u32)> {
        r.hits
            .iter()
            .map(|h| (h.chunk_id, h.score.to_bits()))
            .collect()
    }

    /// **S9-T1（编排层一半）**：开关关 ⇒ 与「今天的行为」（默认 `RrfFusion` 现算，
    /// 不硬编码期望值）**逐位一致**——哪怕策略里**配了必触发的规则**。
    ///
    /// 🔑 判据的牙齿：对照臂 (b) 先断言 `fusion_weights == Some(..)`（证明规则真的
    /// 被消费了）——否则「(c) == (a)」可能是「规则根本没接线」的假一致。
    #[test]
    fn S9_T1_开关关时与今天逐位一致() {
        let (index, analyzer, vi) = build_hybrid_fixture(&[
            "BM25 是经典关键词检索算法，参数 k1 控制词频饱和",
            "向量检索把文本编码成向量计算余弦相似度",
            "混合检索融合关键词和向量两路结果",
        ]);
        let e = FakeEmbedder;

        // (a) 今天的行为：默认装配（RrfFusion::default，无规则、开关关）
        let a = QueryExecutor::new(&index, &analyzer).with_vector(&e, &vi);
        let ra = a.search("检索", SearchMode::Hybrid, 10).unwrap();

        // (b) 开关开 + 必触发规则（中文 query ⇒ s=0 < θ=0.5）⇒ 自适应真的生效
        let b = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_fusion(Box::new(RrfFusion::new_adaptive(
                60.0,
                vec![1.0, 1.5],
                armed_shape_rule(0.5),
            )))
            .with_adaptive_fusion(true);
        let rb = b.search("检索", SearchMode::Hybrid, 10).unwrap();
        assert_eq!(
            rb.metrics.fusion_weights,
            Some(vec![0.0, 1.5]),
            "前提自证：规则真的被消费（否则下方的『一致』是假一致）"
        );

        // (c) 同一策略、但开关**关** ⇒ 编排层根本不调 fuse_adaptive
        let c = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_fusion(Box::new(RrfFusion::new_adaptive(
                60.0,
                vec![1.0, 1.5],
                armed_shape_rule(0.5),
            )));
        let rc = c.search("检索", SearchMode::Hybrid, 10).unwrap();
        assert_eq!(
            s9_hits_bits(&rc),
            s9_hits_bits(&ra),
            "🔴 开关关时必须与今天逐位一致（配了规则也不行，I9-3）"
        );
        assert!(rc.metrics.fusion_weights.is_none(), "关 ⇒ 可观测面也不写");
    }

    /// **S9-T4（I9-4）**：自适应**不改候选池口径**——开关两态对同一 query 的
    /// `Metrics.candidates`（融合去重后候选数）与两路 lane 计数完全一致。
    /// 用**触发态**规则做对照，证明「池不变」不是靠「规则没触发」。
    ///
    /// 🔑 夹具要害（评审 P4-4 补行为断言时发现——首轮夹具**两路名次恰好同序**，
    /// 触发态名次不变，行为断言无从落地）。分歧设计（两文档，手算锚点）：
    ///
    /// - doc0 =「检索段落」：BM25 命中「检索」（tf=1）⇒ BM25 路 rank 1；
    ///   向量余弦 **−0.53**（FakeEmbedder 哈希）⇒ vector 路 rank 2。
    /// - doc1 =「段落段落」：不含「检索」⇒ BM25 路**不命中**；
    ///   余弦 **+0.88** ⇒ vector 路 rank 1。
    ///
    /// 固定权重（1.0, 1.5）：doc0 = 1.0/61 + 1.5/62 ≈ **0.04059** ＞ doc1 = 1.5/61 ≈
    /// 0.02459 ⇒ **doc0 第一**；触发态（w_bm25=0）：doc0 = 1.5/62 ≈ 0.02419 ＜ doc1
    /// ≈ 0.02459 ⇒ **doc1 第一**——名次**必然翻转**，两态对照不是「规则没触发」的假对照。
    #[test]
    fn S9_T4_自适应不改候选池口径() {
        let (index, analyzer, vi) = build_hybrid_fixture(&["检索段落", "段落段落"]);
        let e = FakeEmbedder;
        let off = QueryExecutor::new(&index, &analyzer).with_vector(&e, &vi);
        let on = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_fusion(Box::new(RrfFusion::new_adaptive(
                60.0,
                vec![1.0, 1.5],
                armed_shape_rule(0.5),
            )))
            .with_adaptive_fusion(true);

        let r_off = off.search("检索", SearchMode::Hybrid, 10).unwrap();
        let r_on = on.search("检索", SearchMode::Hybrid, 10).unwrap();
        // 前提自证 ①（Metrics 面）：触发态（权重真的变了）
        assert_eq!(r_on.metrics.fusion_weights, Some(vec![0.0, 1.5]));
        // 前提自证 ②（行为面，评审 P4-4 补）：触发态下**名次真的变了**——
        // 若这条不成立，「池不变」的对照就是「规则没触发」的假对照。
        // （s4 触发 ⇒ w_bm25=0 ⇒ hybrid 名次 ≡ vector 单路名次，与固定权重
        // 的 hybrid 不同——用 chunk_id 序列断言即可，score 量纲两态不同不比。）
        let ids_off: Vec<ChunkId> = r_off.hits.iter().map(|h| h.chunk_id).collect();
        let ids_on: Vec<ChunkId> = r_on.hits.iter().map(|h| h.chunk_id).collect();
        assert_ne!(
            ids_off, ids_on,
            "🔴 前提破裂：触发态未改变名次 ⇒ 本用例的对照退化为「规则没触发」"
        );
        assert_eq!(r_off.metrics.candidates, r_on.metrics.candidates);
        assert_eq!(
            r_off.metrics.bm25, r_on.metrics.bm25,
            "BM25 lane 计数不受开关影响"
        );
        assert_eq!(
            r_off.metrics.vector, r_on.metrics.vector,
            "vector lane 计数不受开关影响"
        );
        assert_eq!(r_off.metrics.allowed, r_on.metrics.allowed);
    }

    /// **S9-T5（I9-6）**：融合开关与精排窗口**正交**——开关两态下，`Reranker`
    /// 声明的窗口（这里 12 > k）与实际交接条数（`rerank_window`）一字不差。
    #[test]
    fn S9_T5_融合开关与精排窗口正交() {
        // 语料必须 > 窗口（12）：否则 `take_n` 被融合条数封顶，窗口是否生效不可观测
        // （`rerank_window` 记的是**实际交接**条数，公式值被截短时两者不相等）
        let texts: Vec<String> = (0..14)
            .map(|i| format!("第{i}篇：检索算法与向量混合的语料样本"))
            .collect();
        let texts: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let (index, analyzer, vi) = build_hybrid_fixture(&texts);
        let e = FakeEmbedder;
        let mk = |adaptive: bool| {
            QueryExecutor::new(&index, &analyzer)
                .with_vector(&e, &vi)
                .with_fusion(Box::new(RrfFusion::new_adaptive(
                    60.0,
                    vec![1.0, 1.5],
                    armed_shape_rule(0.5),
                )))
                .with_adaptive_fusion(adaptive)
                .with_reranker(Box::new(SpyReranker::new(12, false, false).0))
        };
        let r_off = mk(false).search("检索", SearchMode::Hybrid, 10).unwrap();
        let r_on = mk(true).search("检索", SearchMode::Hybrid, 10).unwrap();
        assert_eq!(
            r_off.metrics.rerank_window, 12,
            "前提：窗口 12 生效（off 臂）"
        );
        assert_eq!(
            r_on.metrics.rerank_window, 12,
            "🔴 自适应不得牵连精排窗口（I9-6：Reranker 一字不动）"
        );
        assert_eq!(
            r_off.metrics.vector_route, r_on.metrics.vector_route,
            "向量路分派也不受影响"
        );
    }

    /// **S9-T7（编排层）**：BM25 lane 为空（query 词全不在词典）时，
    /// ① `Metrics.fusion_weights == Some([0.0, w])`（tier 1 的可观测判据）；
    /// ② 结果名次 ≡ `Vector` 单路（空 lane 零贡献 ⇒ ② 独立于 tier 1，如实两记）。
    #[test]
    fn S9_T7_编排层tier1可观测面() {
        let (index, analyzer, vi) = build_hybrid_fixture(&[
            "BM25 是经典关键词检索算法，参数 k1 控制词频饱和",
            "向量检索把文本编码成向量计算余弦相似度",
        ]);
        let e = FakeEmbedder;
        // 规则用 θ=0 的纯 tier 1（tier 2 永不触发）⇒ 观测到的 [0.0, 1.5] 只能来自 tier 1
        let s = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_fusion(Box::new(RrfFusion::new_adaptive(
                60.0,
                vec![1.0, 1.5],
                AdaptiveRule {
                    signal: AdaptiveSignal::DfCoverage,
                    theta: 0.0,
                },
            )))
            .with_adaptive_fusion(true);
        // query 词不在中文语料词典里 ⇒ BM25 lane 空；FakeEmbedder 仍给出确定向量 ⇒ vector lane 非空
        let r = s.search("zqxjkwv", SearchMode::Hybrid, 10).unwrap();
        assert!(
            !r.hits.is_empty(),
            "前提：vector 路有结果（否则空早退路径测不到）"
        );
        assert_eq!(r.metrics.bm25, 0, "前提自证：BM25 lane 确实为空");
        assert_eq!(
            r.metrics.fusion_weights,
            Some(vec![0.0, 1.5]),
            "🔴 tier 1 生效的可观测证据（S9-T7 ①）"
        );
        // ② 名次等价于 Vector 单路（ids 序列比较——两侧 score 量纲不同，只比名次）
        let v = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .search("zqxjkwv", SearchMode::Vector, 10)
            .unwrap();
        let ids_hybrid: Vec<ChunkId> = r.hits.iter().map(|h| h.chunk_id).collect();
        let ids_vector: Vec<ChunkId> = v.hits.iter().map(|h| h.chunk_id).collect();
        assert_eq!(
            ids_hybrid, ids_vector,
            "空 BM25 路 ⇒ hybrid 名次 ≡ vector 单路"
        );
    }

    /// **S9-T10**：`Metrics` 两个新字段的语义——关时不写、开且未配规则时不写
    /// （「开了白开」也如实报 `None`）、开且配了规则时写对（权重原样 + 信号值）。
    #[test]
    fn S9_T10_Metrics两字段的写入语义() {
        let (index, analyzer, vi) = build_hybrid_fixture(&[
            "BM25 是经典关键词检索算法，参数 k1 控制词频饱和",
            "向量检索把文本编码成向量计算余弦相似度",
        ]);
        let e = FakeEmbedder;

        // ① 关 ⇒ 不写（哪怕策略里配了规则）
        let off = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_fusion(Box::new(RrfFusion::new_adaptive(
                60.0,
                vec![1.0, 1.5],
                armed_shape_rule(0.5),
            )));
        let r = off.search("检索", SearchMode::Hybrid, 10).unwrap();
        assert!(r.metrics.fusion_weights.is_none(), "关 ⇒ 不写");
        assert!(r.metrics.fusion_signal.is_none(), "关 ⇒ 不写");

        // ② 开 + 未配规则（默认 RrfFusion）⇒ 仍 None（「开了白开」如实可见）
        let on_no_rule = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_adaptive_fusion(true);
        let r = on_no_rule.search("检索", SearchMode::Hybrid, 10).unwrap();
        assert!(
            r.metrics.fusion_weights.is_none(),
            "策略未配规则 ⇒ 退回 fuse，报 None"
        );
        assert!(r.metrics.fusion_signal.is_none());

        // ③ 开 + 配规则 + 不触发（θ=0.0 ⇒ s=0 不小于 0）⇒ 权重原样 + 信号值如实
        let on_idle = QueryExecutor::new(&index, &analyzer)
            .with_vector(&e, &vi)
            .with_fusion(Box::new(RrfFusion::new_adaptive(
                60.0,
                vec![1.0, 1.5],
                AdaptiveRule {
                    signal: AdaptiveSignal::DfCoverage,
                    theta: 0.0,
                },
            )))
            .with_adaptive_fusion(true);
        let r = on_idle.search("检索", SearchMode::Hybrid, 10).unwrap();
        assert_eq!(
            r.metrics.fusion_weights,
            Some(vec![1.0, 1.5]),
            "不触发 ⇒ 权重原样"
        );
        // 「检索」的去重词项全部在语料里 ⇒ df 覆盖率 = 1.0
        assert_eq!(r.metrics.fusion_signal, Some(1.0), "信号值如实上报");

        // ④ 单路模式（Bm25）⇒ 不写（开关只在 Hybrid 分支被消费）
        let r = on_idle.search("检索", SearchMode::Bm25, 10).unwrap();
        assert!(r.metrics.fusion_weights.is_none(), "单路无融合语义 ⇒ 不写");
    }

    /// `df_coverage` 探针的口径（s3 信号的数据源）：
    /// 单段的部分覆盖、空 query、以及**跨段**（词只在 delta 段也要算覆盖）。
    #[test]
    fn df覆盖率探针单段与跨段口径() {
        let (index, analyzer) = build_index(&["检索算法经典", "向量检索余弦"]);

        // 部分覆盖：一个词在语料里、一个不在 ⇒ 0.5（先钉 token 前提再报数）
        let tokens = analyzer.analyze_query("检索 xyzwq");
        assert!(
            tokens.iter().filter(|t| t.term == "xyzwq").count() >= 1,
            "前提自证：未知词 xyzwq 必须成为 token（否则覆盖率的分母不对）"
        );
        let distinct: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            tokens
                .iter()
                .filter(|t| seen.insert(t.term.to_string()))
                .map(|t| t.term.to_string())
                .collect()
        };
        assert_eq!(
            distinct.len(),
            2,
            "前提自证：恰好两个去重词项（检索 / xyzwq）"
        );
        let cov = df_coverage(&index, None, &tokens);
        assert!((cov - 0.5).abs() < 1e-6, "半覆盖应为 0.5，实际 {cov}");

        // 全覆盖与空 query
        let tokens = analyzer.analyze_query("检索");
        assert_eq!(df_coverage(&index, None, &tokens), 1.0);
        assert_eq!(df_coverage(&index, None, &[]), 0.0, "空 query ⇒ 0.0");

        // 跨段：词只在 delta 段 ⇒ 任一段有即算覆盖（main 查不到 ≠ 覆盖 0）
        let (delta, _) = build_index(&["独此一份的稀有词汇"]);
        let set = SegmentSet {
            segments: vec![
                SegmentRef {
                    index: &index,
                    base_doc: 0,
                    base_chunk: 0,
                },
                SegmentRef {
                    index: &delta,
                    base_doc: 100,
                    base_chunk: 100,
                },
            ],
            n: 0, // 探针不消费段数（只查词典），口径与本测试无关
            total_len: 0,
            tombstones: 0,
        };
        let tokens = analyzer.analyze_query("检索 稀有词汇");
        let cov = df_coverage(&index, Some(&set), &tokens);
        assert!(
            (cov - 1.0).abs() < 1e-6,
            "词分布在两段 ⇒ 覆盖率应为 1.0（任一段有即算），实际 {cov}"
        );
    }
}
