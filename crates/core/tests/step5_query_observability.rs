//! V2 Step 5 · 查询性能与可观测——端到端集成（S5-06 / S5-T9 + T13 的合成小规模版）。
//!
//! 单元测试覆盖的是各层契约（`vector/*`、`query/*`）；本文件把**真实后端 + 真实
//! 谓词 + 真实编排**串起来，验证 Step 5 的两条对外承诺：
//!
//! 1. 低选择度过滤**真的**走精确路径，`Metrics.vector_route == Exact` 且
//!    `vector_shortfall` 结构性归零（R18 结案的可观测形态，§4.4）；
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
use helix_core::query::{QueryExecutor, SearchMode, VectorRoute};
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
/// 400 篇 ⇒ `allowed = 40`，远低于默认阈值 1024 ⇒ 向量路必然走精确路径。
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
        "选择度 10% / allowed=40 ≤ 阈值 1024 ⇒ 必须走精确路径"
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
        "精确路径下缺口结构性归零（判读仍须连看 vector_route）"
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
