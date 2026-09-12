//! V2 Step 6 / PR 5 的**门面层集成测试**：S6-T1 / T2 / T3 / T8 / T9 / T13
//! + 一条语义边界测试（同名 `source` 不替换）。
//!
//! 这些用例模拟 `helix build --index <快照> --input <delta>` 的**追加语义**
//! （`load` → `add_documents` → `commit` → `save`），但**不走 CLI**：CLI 只是
//! 这三步的串接，真正的正确性风险落在门面层与存储层之间。
//!
//! # 比对口径（设计 §4.5.3，⚠️ 别照直觉改）
//!
//! 追加语义下 ID 的分配顺序与「全量重建」**不保证**相同（`insert_*` 用 `len()`
//! 分配，且 base 可能带尾部空洞）。因此本文件的对照一律**映射到 `source` 再比**，
//! 不对 `doc_id` / `chunk_id` 做逐位断言 —— 唯一例外是 S6-T8，它断言的正是
//! **ID 分配本身**的不变式。

#![allow(non_snake_case)]

mod common;

use std::ops::Range;

use helix_core::document::Document;
use helix_core::query::SearchMode;
use helix_core::search::{GraphStatus, SearchIndexBuilder, Searcher, VectorBackend};
use helix_core::storage;

/// 装配：纯 BM25（无向量 lane）——计数与 BM25 对照用，不依赖测试 embedder。
fn bm25_builder() -> SearchIndexBuilder {
    SearchIndexBuilder::default()
        .embedder(None)
        .vector_backend(VectorBackend::Brute)
        // 强制单 chunk：doc == chunk，计数断言才直观
        .chunker(helix_core::chunk::Chunker::new(200_000, 0))
        .batch_size(1024)
}

/// 造 `range` 范围内的文档（确定性）。
///
/// - `source = doc-<i>.md`（对照锚点）
/// - 正文含 `词条<i>` 专属词 + 公共词；尾部按 `i % 5` 加不等长填充
///   ⇒ 让 BM25 分数在文档间有区分度，避免大量并列分数掩盖排序差异。
fn corpus(range: Range<usize>) -> Vec<Document> {
    range
        .map(|i| {
            Document::new(format!(
                "公共词 词条{i} 文档编号{i} 追加构建 验证{}",
                "填充".repeat(i % 5)
            ))
            .with_source(format!("doc-{i}.md"))
        })
        .collect()
}

/// 检索 BM25 并把结果映射为 `(source, score)` 序列（**不比较 id**）。
fn bm25_sequence(searcher: &Searcher, query: &str, k: usize) -> Vec<(String, f32)> {
    searcher
        .search_with(query)
        .mode(SearchMode::Bm25)
        .top_n(k)
        .exec()
        .unwrap()
        .hits
        .into_iter()
        .map(|h| (h.source, h.score))
        .collect()
}

/// 读快照里记录的配置指纹（不触发图加载）。
fn fingerprint_of(path: &std::path::Path) -> String {
    storage::load_with_crc(path).unwrap().2.to_string()
}

/// 读图 sidecar manifest 的 `nb_point`（图点数）。
fn graph_nb_point(path: &std::path::Path) -> u64 {
    let paths = storage::graph_paths(path);
    storage::read_manifest(&paths.manifest)
        .unwrap()
        .expect("manifest 应已发布（追加后必须重发）")
        .nb_point
}

/// 建 base 快照：`corpus(0..base_len)` 全量建库并落盘。
fn build_base(path: &std::path::Path, base_len: usize) -> SearchIndexBuilder {
    let mut base = bm25_builder().build();
    base.add_documents(corpus(0..base_len)).unwrap();
    base.commit().unwrap();
    base.save(path).unwrap();
    bm25_builder()
}

// ---------------------------------------------------------------------------
// S6-T1：追加后三个计数 == 全量重建
// ---------------------------------------------------------------------------

#[test]
fn S6_T1_追加后计数与全量重建一致() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t1.idx");
    let builder = build_base(&path, 80);

    // 追加路径：load → add(后 20 篇) → commit → save
    let mut inc = builder.load(&path).unwrap();
    inc.add_documents(corpus(80..100)).unwrap();
    inc.commit().unwrap();
    inc.save(&path).unwrap();

    // 对照：同样 100 篇全量重建
    let mut full = bm25_builder().build();
    full.add_documents(corpus(0..100)).unwrap();
    full.commit().unwrap();

    assert_eq!(inc.num_docs(), 100, "追加后文档数");
    assert_eq!(inc.num_docs(), full.num_docs());
    assert_eq!(
        inc.num_chunks(),
        full.num_chunks(),
        "分片数应与全量重建一致"
    );
    assert_eq!(
        inc.total_len(),
        full.total_len(),
        "词项总数应与全量重建一致"
    );
    assert!(
        (inc.avgdl() - full.avgdl()).abs() < 1e-3,
        "avgdl 应一致：追加 {} vs 全量 {}",
        inc.avgdl(),
        full.avgdl()
    );
}

// ---------------------------------------------------------------------------
// S6-T2：追加后 BM25 检索的 (source, score) 序列 == 全量重建
// ---------------------------------------------------------------------------

#[test]
fn S6_T2_追加后bm25检索序列与全量重建一致() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t2.idx");
    let builder = build_base(&path, 80);

    let mut inc = builder.load(&path).unwrap();
    inc.add_documents(corpus(80..100)).unwrap();
    inc.commit().unwrap();
    inc.save(&path).unwrap();

    // 对照：同样 100 篇全量重建。`SearchIndex` 不可 Clone，
    // 故先 `into_searcher()` 交出读端（`Searcher` 可反复检索），再逐 query 比。
    let mut full = bm25_builder().build();
    full.add_documents(corpus(0..100)).unwrap();
    full.commit().unwrap();
    let full_s = full.into_searcher().unwrap();

    // `词条1` 同时命中 1 / 10..19 ⇒ 11 条，足够验排序而不至于全是并列分数
    for query in ["词条1", "公共词", "词条87"] {
        // 追加侧每次从快照重新加载（便宜），保证比的是**落盘后**的状态
        let inc_s = bm25_builder().load(&path).unwrap().into_searcher().unwrap();
        let a = bm25_sequence(&inc_s, query, 20);
        let b = bm25_sequence(&full_s, query, 20);
        assert!(!a.is_empty(), "query={query:?} 应有命中");
        assert_eq!(
            a, b,
            "query={query:?} 的 (source, score) 序列应逐位一致（不比 id）"
        );
    }
}

// ---------------------------------------------------------------------------
// S6-T3：重复追加同一 delta 幂等（content_hash 双保险）
// ---------------------------------------------------------------------------

#[test]
fn S6_T3_重复追加同一delta计数不变() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t3.idx");
    let builder = build_base(&path, 80);

    let mut first = builder.load(&path).unwrap();
    let outcomes = first.add_documents(corpus(80..100)).unwrap();
    assert!(
        outcomes.iter().all(|o| !o.deduped),
        "首次追加不应有去重命中"
    );
    first.commit().unwrap();
    first.save(&path).unwrap();
    let after_first = (first.num_docs(), first.num_chunks(), first.total_len());

    // 第二次追加**同一** delta：全部命中 content_hash ⇒ deduped
    let mut second = bm25_builder().load(&path).unwrap();
    let outcomes = second.add_documents(corpus(80..100)).unwrap();
    assert!(
        outcomes.iter().all(|o| o.deduped),
        "重复追加应全部被去重（第一道 doc_id_by_hash 或第二道 content_hashes）"
    );
    second.commit().unwrap();
    second.save(&path).unwrap();

    assert_eq!(
        (second.num_docs(), second.num_chunks(), second.total_len()),
        after_first,
        "重复追加后文档数/分片数/词项总数必须不变"
    );

    // 跨快照仍是幂等的（去重必须跨 save/load 存活，而不是只在内存里）
    let mut third = bm25_builder().load(&path).unwrap();
    let outcomes = third.add_documents(corpus(80..100)).unwrap();
    assert!(outcomes.iter().all(|o| o.deduped), "跨快照去重必须仍然生效");
    assert_eq!(third.num_docs(), after_first.0);
}

// ---------------------------------------------------------------------------
// S6-T8：追加分配的新 ID 严格大于历史最大 ID（含尾部墓碑场景）
// ---------------------------------------------------------------------------

#[test]
fn S6_T8_追加分配的id严格大于历史最大id() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t8.idx");

    let mut idx = bm25_builder().build();
    let out = idx.add_documents(corpus(0..3)).unwrap();
    let max_doc = out.iter().map(|o| o.doc_id).max().unwrap();
    let max_chunk = out
        .iter()
        .flat_map(|o| o.chunk_ids.iter().copied())
        .max()
        .unwrap();

    // 删**尾部**那篇 ⇒ 正排尾部留下空洞。尾部空洞是 ID 复用最容易发生的地方：
    // 若分配用「第一个空槽」而非 `len()`，新文档就会顶掉旧 doc_id，
    // 而 `raw_vectors` / 图 sidecar 里按 chunk_id 存的旧向量会因此张冠李戴。
    idx.remove(max_doc).unwrap();
    idx.commit().unwrap();
    idx.save(&path).unwrap();

    let mut inc = bm25_builder().load(&path).unwrap();
    let out2 = inc.add_documents(corpus(3..5)).unwrap();
    let new_doc = out2.iter().map(|o| o.doc_id).min().unwrap();
    let new_chunk = out2
        .iter()
        .flat_map(|o| o.chunk_ids.iter().copied())
        .min()
        .unwrap();

    assert!(
        new_doc as usize > max_doc as usize,
        "新 doc_id {new_doc} 必须严格大于历史最大 doc_id {max_doc}（尾部空洞不得复用）"
    );
    assert!(
        new_chunk as usize > max_chunk as usize,
        "新 chunk_id {new_chunk} 必须严格大于历史最大 chunk_id {max_chunk}"
    );
    assert_eq!(
        new_doc as usize,
        max_doc as usize + 1,
        "分配应紧接历史末尾（=len()），而不是填补尾部空洞"
    );
}

// ---------------------------------------------------------------------------
// S6-T9：追加前后配置指纹不变
// ---------------------------------------------------------------------------

#[test]
fn S6_T9_追加前后配置指纹不变() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t9.idx");
    let builder = build_base(&path, 80);

    let before = fingerprint_of(&path);

    let mut inc = builder.load(&path).unwrap();
    inc.add_documents(corpus(80..100)).unwrap();
    inc.commit().unwrap();
    inc.save(&path).unwrap();

    let after = fingerprint_of(&path);
    assert_eq!(
        before, after,
        "追加不得改动配置指纹（否则老快照再也加载不回来）"
    );
}

// ---------------------------------------------------------------------------
// S6-T13：追加后图 sidecar manifest 与冷启动 Loaded（不是 Rebuilt）
// ---------------------------------------------------------------------------

#[test]
fn S6_T13_追加后图manifest重发且冷启动loaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t13.idx");

    // base：8 篇（HNSW + 确定性 TestEmbedder，不依赖真实模型）
    let mut base = common::builder_hnsw().build();
    base.add_documents(corpus(0..8)).unwrap();
    base.commit().unwrap();
    base.save(&path).unwrap();
    assert_eq!(graph_nb_point(&path), 8, "base 的图点数");

    // 追加 2 篇：load 应命中持久化图
    let mut inc = common::builder_hnsw().load(&path).unwrap();
    assert!(
        matches!(inc.graph_status(), GraphStatus::Loaded),
        "base 冷启动应命中图，实际 {:?}",
        inc.graph_status()
    );
    inc.add_documents(corpus(8..10)).unwrap();
    inc.commit().unwrap();
    inc.save(&path).unwrap();

    // ⚠️ 这条是 Step 4 踩过的坑：图变了却不重发 manifest ⇒ 新图永远匹配不上
    // ⇒ 每次冷启动都降级重建（≈10s），NFR-04 静默失效。
    assert_eq!(
        graph_nb_point(&path),
        10,
        "追加后 manifest 必须重发（nb_point 跟随新增分片）"
    );

    let reloaded = common::builder_hnsw().load(&path).unwrap();
    assert!(
        matches!(reloaded.graph_status(), GraphStatus::Loaded),
        "追加后冷启动必须仍是 Loaded，实际 {:?}",
        reloaded.graph_status()
    );
    assert_eq!(reloaded.num_chunks(), 10);
}

// ---------------------------------------------------------------------------
// 语义边界（设计 §4.5.5）：追加**不做** upsert-by-source
// ---------------------------------------------------------------------------

/// 同一 `source`、内容不同 ⇒ 是**一篇新文档**，旧文档仍在（两篇都可检索到）。
///
/// 这条边界必须钉住：它决定用户「改了正文再追加」时的预期。若要替换语义，
/// 用户需要显式 `remove` 旧文档，或走 `helix compact` 配合（见 §4.5.5）。
#[test]
fn 追加不是upsert_by_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("upsert.idx");

    let mut base = bm25_builder().build();
    base.add_documents(vec![Document::new("苹果 香蕉 樱桃").with_source("same.md")])
        .unwrap();
    base.commit().unwrap();
    base.save(&path).unwrap();

    let mut inc = bm25_builder().load(&path).unwrap();
    let out = inc
        .add_documents(vec![
            Document::new("榴莲 山竹 红毛丹").with_source("same.md")
        ])
        .unwrap();
    assert!(!out[0].deduped, "内容不同 ⇒ 不是重复文档");
    inc.commit().unwrap();
    inc.save(&path).unwrap();

    assert_eq!(inc.num_docs(), 2, "同名 source 的两篇都应保留（非替换）");

    // 两篇各自的专属词都能检索到 ⇒ 旧文档确实还在
    let s = bm25_builder().load(&path).unwrap().into_searcher().unwrap();
    for q in ["苹果", "榴莲"] {
        let hits = s
            .search_with(q)
            .mode(SearchMode::Bm25)
            .top_n(10)
            .exec()
            .unwrap()
            .hits;
        assert_eq!(hits.len(), 1, "query={q:?} 应命中 1 篇");
        assert_eq!(hits[0].source, "same.md");
    }
}
