//! V2 Step 5 · 查询性能与可观测——端到端集成（S5-06 / S5-T9 + T13 的合成小规模版）。
//!
//! 单元测试覆盖的是各层契约（`vector/*`、`query/*`）；本文件把**真实后端 + 真实
//! 谓词 + 真实编排**串起来，验证 Step 5 的两条对外承诺：
//!
//! 1. 低选择度过滤**真的**走精确路径，`Metrics.vector_route == Exact` 且
//!    （本 fixture 图覆盖完整，故）`vector_shortfall` 为 0（R18 结案的可观测形态，§4.4）；
//!    ⚠️ 「`Exact` ⇒ 缺口 0」**不是恒等式**：`allowed` 来自 `Index`、扫描枚举的是图中
//!    的点；图滞后于索引时 `Exact` 的缺口仍会 > 0（那时它是「图未覆盖 allowed」的信号）。
//! 2. 响应口径自洽——`metrics.took == took`、`metrics.candidates == total_candidates`
//!    （I7），含空结果路径。
//!
//! 合成小规模版（N=400，16 维假 embedder），**秒级**、可进 CI；
//! 10 万级 / 真实 embedder 的延迟标定见 `scripts/eval_filter.sh` 与 `eval-report.md` §8.9
//! （延迟数字在 CI 共享 runner 上不可引用）。

// 测试名保留 S5_T 前缀以便与设计文档的任务编号对账
#![allow(non_snake_case)]

use std::time::Duration;

use helix_core::analyze::MixedAnalyzer;
use helix_core::chunk::Chunker;
use helix_core::document::DocRecord;
use helix_core::embed::Embedder;
use helix_core::error::Result;
use helix_core::index::Index;
use helix_core::predicate::CandidateFilter;
use helix_core::query::{QueryExecutor, SearchMode, SearchResponse, VectorRoute};
use helix_core::schema::Filter;
use helix_core::types::ChunkId;
use helix_core::vector::{BruteForceIndex, HnswRsIndex, NormalizedVector, VectorIndex};

/// 确定性假 Embedder：文本 → FNV 哈希 → 16 维归一化向量（不依赖模型下载）。
struct FakeEmbedder;

impl Embedder for FakeEmbedder {
    fn dim(&self) -> usize {
        16
    }
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| fake_vec(t)).collect())
    }
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(fake_vec(text))
    }
}

fn fake_vec(text: &str) -> Vec<f32> {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let mut v: Vec<f32> = (0..16)
        .map(|i| {
            let byte = (h >> (i * 4)) as u8;
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

/// 选择度 10% 的语料：每 10 篇里 1 篇 `tag = keep`。
///
/// 400 篇 ⇒ `allowed = 40`，远低于默认阈值（`BRUTE_FALLBACK_MAX_ALLOWED = 8192`）
/// ⇒ 向量路必然走精确路径。
const N_DOCS: usize = 400;
const KEEP_EVERY: usize = 10;

/// 建语料 + 同源向量（HNSW 与 Brute 共用同一批向量，才能做逐位 oracle 比对）。
fn build_fixture() -> (Index, MixedAnalyzer, Vec<(ChunkId, NormalizedVector)>) {
    let analyzer = MixedAnalyzer::new();
    let chunker = Chunker::default();
    let mut index = Index::new();
    for i in 0..N_DOCS {
        let tag = if i % KEEP_EVERY == 0 { "keep" } else { "drop" };
        let doc = DocRecord {
            doc_id: 0,
            source: format!("doc-{i}"),
            metadata: serde_json::json!({ "tag": tag }),
            content_hash: 0,
        };
        // 文本单调递增 ⇒ FNV 向量互异，避免并列距离掩盖排序问题
        let text = format!("检索 文档编号 {i} 内容 {i}");
        index.add(doc, chunker.chunk(0, &text), &analyzer).unwrap();
    }
    let vectors: Vec<(ChunkId, NormalizedVector)> = index
        .live_chunks()
        .map(|c| (c.chunk_id, NormalizedVector::new(fake_vec(&c.text))))
        .collect();
    (index, analyzer, vectors)
}

fn build_hnsw(vectors: &[(ChunkId, NormalizedVector)]) -> HnswRsIndex {
    let mut idx = HnswRsIndex::with_capacity(vectors.len().max(1024));
    for (id, v) in vectors {
        idx.add(*id, v.clone()).unwrap();
    }
    idx
}

/// **S5-T12 的小规模版 / R18 的可观测验收**：低选择度过滤 ⇒ `route == Exact`，
/// `vector_shortfall == 0`，且返回的每一条都真的满足谓词。
#[test]
fn S5_T13_低选择度端到端走精确路径且缺口归零() {
    let (index, analyzer, vectors) = build_fixture();
    let hnsw = build_hnsw(&vectors);
    let e = FakeEmbedder;

    let filter = Filter::eq("tag", "keep");
    let searcher = QueryExecutor::new(&index, &analyzer).with_vector(&e, &hnsw);
    let resp = searcher
        .search_filtered(
            "检索 文档编号 200 内容 200",
            SearchMode::Vector,
            10,
            Some(&filter),
        )
        .unwrap();

    // 1. 路径可见性（D-S5-07）：兜底真的生效了，不需要靠延迟反推
    assert_eq!(
        resp.metrics.vector_route,
        VectorRoute::Exact,
        "选择度 10% / allowed=40 ≤ 默认阈值 8192 ⇒ 必须走精确路径"
    );

    // 2. 覆盖度：candidate_k = k×3 = 30，allowed = 40 ⇒ 精确路径应拿满 30 条
    let allowed = N_DOCS / KEEP_EVERY;
    assert_eq!(resp.metrics.allowed, allowed, "allowed 口径应等于 10% 语料");
    assert_eq!(
        resp.metrics.vector, 30,
        "精确路径必须拿满 min(candidate_k=30, allowed=40)"
    );
    assert_eq!(
        resp.metrics.vector_shortfall, 0,
        "本 fixture 图覆盖完整 ⇒ 精确路径缺口为 0（判读仍须连看 vector_route）"
    );

    // 3. soundness：最终命中的每一条都确实通过谓词
    for hit in &resp.hits {
        let chunk = index.chunk(hit.chunk_id).expect("命中必须能回捞");
        let meta = &index.doc(chunk.doc_id).unwrap().metadata;
        assert_eq!(meta["tag"], "keep", "命中了未通过过滤的 chunk");
    }

    // 4. 口径自洽（I7）
    assert_eq!(resp.metrics.took, resp.took);
    assert_eq!(resp.metrics.candidates, resp.total_candidates);
    assert!(resp.took > Duration::ZERO);
}

/// **S5-T3（集成层）/ I4**：同一批向量与谓词下，精确路径与 `BruteForceIndex`
/// 的 Top-K **逐位一致**——用真实后端把"结构性同源"验到端到端。
#[test]
fn S5_T3_精确路径与暴力oracle逐位一致() {
    let (index, _analyzer, vectors) = build_fixture();
    let hnsw = build_hnsw(&vectors);
    let brute = BruteForceIndex::from_entries(vectors.clone());

    let filter = Filter::eq("tag", "keep");
    let predicate = helix_core::query::filter::try_build_predicate(&index, Some(&filter))
        .expect("10% 选择度下谓词非空");
    let pred: &dyn CandidateFilter = predicate.as_ref();

    // 多 query × 多 k：把"恒等"钉在整条序列上而不是单点
    for probe in [0usize, 37, 199, 388] {
        let q = NormalizedVector::new(fake_vec(&format!("检索 文档编号 {probe} 内容 {probe}")));
        for k in [1usize, 10, 30, allowed_of(&index, &filter)] {
            let exact = hnsw.search_exact_filtered(&q, k, Some(pred)).unwrap();
            let oracle = brute.search_filtered(&q, k, Some(pred)).unwrap();
            assert_eq!(
                exact, oracle,
                "probe={probe} k={k}：精确路径与 Brute 不逐位一致"
            );
            assert_eq!(
                exact.len(),
                k.min(pred.allowed_count()),
                "probe={probe} k={k}：条数必须是 min(k, allowed)"
            );
        }
    }
}

fn allowed_of(index: &Index, filter: &Filter) -> usize {
    helix_core::query::filter::allowed_chunk_count(
        &helix_core::query::filter::doc_bits(filter, index),
        index,
    )
}

/// **S5-T7（集成层）**：`with_brute_fallback(None)` 时行为回到 Step 5 之前
/// （`route == Ann`），且结果**允许**少于精确路径——这正是 A/B 对照的另一半。
#[test]
fn S5_T7_关闭兜底后回到ANN路径() {
    let (index, analyzer, vectors) = build_fixture();
    let hnsw = build_hnsw(&vectors).with_brute_fallback(None);
    assert_eq!(hnsw.brute_fallback(), None, "开关必须真的被关掉");
    let e = FakeEmbedder;

    let filter = Filter::eq("tag", "keep");
    let searcher = QueryExecutor::new(&index, &analyzer).with_vector(&e, &hnsw);
    let resp = searcher
        .search_filtered(
            "检索 文档编号 200 内容 200",
            SearchMode::Vector,
            10,
            Some(&filter),
        )
        .unwrap();

    assert_eq!(
        resp.metrics.vector_route,
        VectorRoute::Ann,
        "--brute-fallback off ⇒ 不兜底，走 ANN"
    );
    // ANN 在低选择度下允许有缺口（R11/R17 的可见信号）；这里不硬断言缺口 >0
    // （hnsw_rs 的召回表现随拓扑抖动，断言它会 flaky），只钉"路径选择"。
    assert!(
        resp.metrics.vector <= resp.metrics.allowed,
        "ANN 不可能返回多于 allowed 条"
    );
    assert_eq!(resp.metrics.took, resp.took);
}

/// **S5-T9（集成层）**：`SearchMode::Bm25` 下 `vector_route == None`——"未走向量路"
/// 由类型表达，而不是让后端去编码"这次检索有没有向量路"。
#[test]
fn S5_T9_纯BM25模式route为None() {
    let (index, analyzer, _) = build_fixture();
    let searcher = QueryExecutor::new(&index, &analyzer);
    let resp = searcher.search("检索", SearchMode::Bm25, 10).unwrap();
    assert_eq!(resp.metrics.vector_route, VectorRoute::None);
    assert_eq!(resp.metrics.vector, 0);
    assert_eq!(resp.metrics.vector_shortfall, 0);
    assert_eq!(resp.metrics.took, resp.took);
    assert_eq!(resp.metrics.candidates, resp.total_candidates);
    // BM25 路耗时被填（D-S5-06），向量路未跑 ⇒ 保持零
    assert!(resp.metrics.bm25_elapsed > Duration::ZERO);
    assert_eq!(resp.metrics.vector_elapsed, Duration::ZERO);
}

/// **S5-T9（空结果）**：空索引 / 过滤排空两条早退路径的 `metrics` 必须与响应自洽
/// （D-S5-08 的端到端回归）。
#[test]
fn S5_T9_空索引与过滤排空的指标自洽() {
    use helix_core::query::EmptyReason;

    // ① 空索引
    let analyzer = MixedAnalyzer::new();
    let empty = Index::new();
    let s = QueryExecutor::new(&empty, &analyzer);
    let resp = s.search("检索", SearchMode::Bm25, 10).unwrap();
    assert_eq!(resp.empty_reason, Some(EmptyReason::NoDocuments));
    assert_eq!(resp.metrics.took, resp.took, "空索引：口径必须自洽");
    assert!(
        resp.took > Duration::ZERO,
        "空索引：took 必须非 0（D-S5-08）"
    );

    // ② 过滤排空（query 有命中，过滤无匹配 ⇒ FilteredOut）
    let (index, analyzer, _) = build_fixture();
    let s = QueryExecutor::new(&index, &analyzer);
    let resp = s
        .search_filtered(
            "检索",
            SearchMode::Bm25,
            10,
            Some(&Filter::eq("tag", "no_such_tag")),
        )
        .unwrap();
    assert_eq!(resp.empty_reason, Some(EmptyReason::FilteredOut));
    assert_eq!(resp.metrics.took, resp.took, "过滤排空：口径必须自洽");
    assert!(
        resp.took > Duration::ZERO,
        "过滤排空：took 必须非 0（D-S5-08）"
    );

    // ③ 融合为空（query 不在词典）
    let resp = s.search("zzz_不在词典", SearchMode::Bm25, 10).unwrap();
    assert_eq!(resp.empty_reason, Some(EmptyReason::AllTermsUnmatched));
    assert_eq!(resp.metrics.took, resp.took);
    assert!(resp.took > Duration::ZERO);
}

/// **S5-T2（集成层）**：`k = 0` 边界在真实后端上返回空，且不 panic。
#[test]
fn S5_T2_k为零返回空() {
    let (index, _analyzer, vectors) = build_fixture();
    let hnsw = build_hnsw(&vectors);
    let filter = Filter::eq("tag", "keep");
    let predicate = helix_core::query::filter::try_build_predicate(&index, Some(&filter)).unwrap();
    let q = NormalizedVector::new(fake_vec("检索"));

    assert!(hnsw
        .search_exact_filtered(&q, 0, Some(predicate.as_ref()))
        .unwrap()
        .is_empty());
    assert!(hnsw.search_exact_filtered(&q, 0, None).unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// S5-T7 的行为层护栏：真实软删除语料 + 同一张图上的策略开关
// ---------------------------------------------------------------------------

/// 只读的**策略开关包装**：转发到真实的 `HnswRsIndex`，**只**改 `prefers_exact` 的
/// 回答。
///
/// # 为什么需要它（而不是 `with_brute_fallback(None)` 建两张图）
///
/// `with_brute_fallback` 会**移动** `self`；而 `hnsw_rs` 每次建图都用
/// `StdRng::from_os_rng()` 播种（`hnsw.rs:328`，无 seed API）⇒ **两张独立的图拓扑
/// 不同**，ANN 的 Top-K 本就可能不同。于是「跨两张图断言 on/off 结果逐位一致」会是
/// 一条**拓扑相关**的 flaky 断言。
///
/// 本包装让两个策略**共用同一张图**，把「开关不改热路径结果」从一句拓扑相关的期望
/// 变成一条**确定性**的断言。
struct ToggleFallback<'a> {
    inner: &'a HnswRsIndex,
    /// 策略开关：`false` 与 `HnswRsIndex::with_brute_fallback(None)` 的分派行为等价。
    exact_by_policy: bool,
}

impl VectorIndex for ToggleFallback<'_> {
    fn add(&mut self, _id: ChunkId, _vec: NormalizedVector) -> Result<()> {
        unreachable!("查询侧测试替身：只读，不参与建库")
    }

    fn search_filtered(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        self.inner.search_filtered(query, k, filter)
    }

    fn search_exact_filtered(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        self.inner.search_exact_filtered(query, k, filter)
    }

    fn prefers_exact(&self, filter: &dyn CandidateFilter) -> bool {
        // 关掉时恒 `false` ⇒ 与 `with_brute_fallback(None)` 的分派逐位等价
        self.exact_by_policy && self.inner.prefers_exact(filter)
    }

    fn len(&self) -> usize {
        self.inner.len()
    }
}

/// 与 [`build_fixture`] 同源，但额外**软删除** `dead_docs` 里的文档，并返回被删分片 id。
///
/// 顺序刻意是「**先建图，后软删除**」：图里残留死向量（Q-C1 的真实形态）、存活位图
/// **非满** ⇒ `AliveOnly` 谓词这次真的在过滤东西，而不是恒真。
fn build_fixture_with_tombstones(
    dead_docs: &[u32],
) -> (
    Index,
    MixedAnalyzer,
    Vec<(ChunkId, NormalizedVector)>,
    Vec<ChunkId>,
) {
    let analyzer = MixedAnalyzer::new();
    let chunker = Chunker::default();
    let mut index = Index::new();
    let mut vectors: Vec<(ChunkId, NormalizedVector)> = Vec::new();
    let mut dead_chunks: Vec<ChunkId> = Vec::new();

    for i in 0..N_DOCS {
        let tag = if i % KEEP_EVERY == 0 { "keep" } else { "drop" };
        let doc = DocRecord {
            doc_id: 0,
            source: format!("doc-{i}"),
            metadata: serde_json::json!({ "tag": tag }),
            content_hash: 0,
        };
        let text = format!("检索 文档编号 {i} 内容 {i}");
        let (doc_id, chunk_ids) = index.add(doc, chunker.chunk(0, &text), &analyzer).unwrap();
        assert_eq!(chunk_ids.len(), 1, "本用例假定「一文档一分片」");
        vectors.push((chunk_ids[0], NormalizedVector::new(fake_vec(&text))));
        if dead_docs.contains(&doc_id) {
            dead_chunks.push(chunk_ids[0]);
        }
    }

    for d in dead_docs {
        index.remove(*d, &analyzer).unwrap();
    }
    assert_eq!(
        index.alive_count(),
        N_DOCS - dead_docs.len(),
        "存活位图必须非满（否则本用例退化为无软删除的路径）"
    );

    (index, analyzer, vectors, dead_chunks)
}

/// **S5-T7（行为层，真实软删除语料）**：`FilterKind::Alive` 路径（无用户过滤）
/// **不得被兜底劫持**——策略 on/off 必须给出**逐位一致**的结果。
///
/// 与 `searcher.rs::S5_T1`（间谍后端）的分工：那条钉**分派**（spy 的 `calls()` 计数），
/// 本条钉**真后端 + 真软删除位图**下的行为。两者合起来才是 R15「热路径 fast-return
/// 不回归」的完整证据——此前只有前者，没有真后端的行为护栏。
#[test]
fn S5_T7_真实软删除语料下兜底开关不改热路径() {
    // 含 `keep`（110 是 10 的倍数）与 `drop` 各若干，让死 chunk 落在两个标签上
    let dead_docs: Vec<u32> = vec![3, 5, 13, 105, 110, 207];
    let (index, analyzer, vectors, dead_chunks) = build_fixture_with_tombstones(&dead_docs);
    // 图按**全量**向量建好（vectors 是软删除前采集的 ⇒ 图中含死向量）
    let hnsw = build_hnsw(&vectors);

    let e = FakeEmbedder;
    let on = ToggleFallback {
        inner: &hnsw,
        exact_by_policy: true,
    };
    let off = ToggleFallback {
        inner: &hnsw,
        exact_by_policy: false,
    };
    let s_on = QueryExecutor::new(&index, &analyzer).with_vector(&e, &on);
    let s_off = QueryExecutor::new(&index, &analyzer).with_vector(&e, &off);

    let ids = |r: &SearchResponse| -> Vec<ChunkId> { r.hits.iter().map(|h| h.chunk_id).collect() };
    let query = "检索 文档编号 200 内容 200";

    // ── ① 热路径（无用户过滤 ⇒ `AliveOnly` 谓词）：开关不得改变任何东西 ──
    let a = s_on
        .search(query, SearchMode::Vector, 10)
        .expect("热路径检索失败");
    let b = s_off
        .search(query, SearchMode::Vector, 10)
        .expect("热路径检索失败");

    assert_eq!(
        a.metrics.vector_route,
        VectorRoute::Ann,
        "Alive 谓词不得触发精确路径（否则 R15 的 fast-return 就被劫持了）"
    );
    assert_eq!(b.metrics.vector_route, VectorRoute::Ann);
    assert_eq!(
        ids(&a),
        ids(&b),
        "同一张图 + 同一分派结论 ⇒ on/off 结果必须逐位一致"
    );
    assert_eq!(a.metrics.vector_shortfall, b.metrics.vector_shortfall);

    // 真实软删除语料下，热路径必须**一条死 chunk 都不漏**（AliveOnly 谓词的语义）
    for r in [&a, &b] {
        assert_eq!(
            r.metrics.allowed,
            index.alive_count(),
            "Alive 谓词的 allowed_count 必须等于存活数"
        );
        for hit in &r.hits {
            assert!(
                !dead_chunks.contains(&hit.chunk_id),
                "热路径漏出了已软删除的 chunk {}",
                hit.chunk_id
            );
        }
    }

    // ── ② 低选择度用户过滤（`Filtered`）：同一张图上开关**必须**改变路径 ──
    // 这一半与 ① 互为对照 —— 证明 ① 的"开关没改变结果"不是开关坏了，
    // 而是 `Alive` 谓词本就不该兜底。
    let filter = Filter::eq("tag", "keep");
    let fa = s_on
        .search_filtered(query, SearchMode::Vector, 10, Some(&filter))
        .expect("过滤检索失败");
    let fb = s_off
        .search_filtered(query, SearchMode::Vector, 10, Some(&filter))
        .expect("过滤检索失败");
    assert_eq!(
        fa.metrics.vector_route,
        VectorRoute::Exact,
        "Filtered 且选择度低 ⇒ 开关打开时走精确路径"
    );
    assert_eq!(
        fb.metrics.vector_route,
        VectorRoute::Ann,
        "同一张图、开关关闭 ⇒ 回到 ANN（证明本用例的开关真的起作用）"
    );
}
