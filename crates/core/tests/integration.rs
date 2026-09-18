//! T4-09 集成测试：端到端快照 + 并发检索。
//!
//! 用确定性假 Embedder（FNV 哈希向量），**不依赖模型下载**。
//!
//! 另含 V2 Step 1 的两条核心验收（T1 / T2，对应 `plan-v2.md` Step 1 与设计文档 §9）：
//! 软删除的向量不再以「幽灵候选」形式被召回，包括跨快照的场景。

// 测试名保留 T1_/T2_ 前缀以便与设计文档的任务编号对账
#![allow(non_snake_case)]
// ⚠️ 本文件沿用旧所有权 API（`into_searcher` / `into_index`）以**锁住其行为不变**
// （`S8-02` 起二者为 `#[deprecated]` 薄封装）。生产调用点已在 `cli` / `examples` 迁移。
#![allow(deprecated)]

use std::sync::Arc;

use helix_core::analyze::MixedAnalyzer;
use helix_core::chunk::Chunker;
use helix_core::document::{content_hash, DocRecord, Document};
use helix_core::embed::Embedder;
use helix_core::error::Result;
use helix_core::index::Index;
use helix_core::query::{QueryExecutor, SearchMode};
use helix_core::search::{SearchIndex, VectorBackend};
use helix_core::storage;
use helix_core::types::{ChunkId, DocId};
use helix_core::vector::{BruteForceIndex, NormalizedVector};

/// 确定性假 Embedder：文本 → FNV 哈希 → 16 维归一化向量。
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

const CORPUS: &[(&str, &str)] = &[
    (
        "bm25.md",
        "BM25 是一种基于词频的经典检索排序算法，参数 k1 控制词频饱和，b 控制长度归一化",
    ),
    (
        "inverted.md",
        "倒排索引维护词项到文档列表的映射，是全文检索的核心数据结构",
    ),
    (
        "vector.md",
        "向量检索把文本编码成高维向量，用余弦相似度召回语义相近的内容",
    ),
    (
        "tokenize.md",
        "中文分词把连续汉字切分成有意义的词，jieba 是最常用的分词工具",
    ),
    (
        "rrf.md",
        "倒数排名融合 RRF 只用排名不用分数，免疫两路量纲差异",
    ),
];

/// 覆盖面广的查询：命中多篇文档，使"名额是否被占"具备鉴别力。
const QUERY: &str = "检索";

fn build_full_index() -> (Index, MixedAnalyzer) {
    let analyzer = MixedAnalyzer::new();
    let chunker = Chunker::default();
    let mut index = Index::new();
    for (source, text) in CORPUS {
        let doc = DocRecord {
            doc_id: 0,
            source: source.to_string(),
            metadata: serde_json::json!({}),
            content_hash: content_hash(text),
        };
        index.add(doc, chunker.chunk(0, text), &analyzer).unwrap();
    }
    (index, analyzer)
}

/// 构建向量索引。
///
/// **刻意用 `BruteForceIndex` 而非 `HnswRsIndex`**：本集成测试验证的是
/// 「快照 save → load 后检索结果一致」这一 round-trip 正确性，
/// 不应混入 ANN 图构建的随机性——`hnsw_rs` 用 OS 熵建图（R-P5-13），
/// 同一份向量两次建图的结果可能不同，会让本测试 flaky
/// （已在 CI Linux 上复现：vector 模式断言失败，bm25 模式始终通过）。
fn build_vector_index(index: &Index) -> BruteForceIndex {
    let entries: Vec<_> = index
        .live_chunks()
        .map(|c| (c.chunk_id, NormalizedVector::new(fake_vec(&c.text))))
        .collect();
    BruteForceIndex::from_entries(entries)
}

fn search_ids(searcher: &QueryExecutor, query: &str, mode: SearchMode) -> Vec<(ChunkId, f32)> {
    searcher
        .search(query, mode, 10)
        .unwrap()
        .hits
        .into_iter()
        .map(|h| (h.chunk_id, h.score))
        .collect()
}

/// 端到端：摄入 → 快照 → 加载 → 检索结果完全一致（三模式）。
#[test]
fn 快照加载后检索结果一致() {
    let (index, analyzer) = build_full_index();
    let vi = build_vector_index(&index);
    let e = FakeEmbedder;
    let searcher = QueryExecutor::new(&index, &analyzer).with_vector(&e, &vi);

    let query = "检索算法";

    // 快照前检索
    let bm25_before = search_ids(&searcher, query, SearchMode::Bm25);
    let vector_before = search_ids(&searcher, query, SearchMode::Vector);
    let hybrid_before = search_ids(&searcher, query, SearchMode::Hybrid);
    assert!(!bm25_before.is_empty());
    assert!(!vector_before.is_empty());
    assert!(!hybrid_before.is_empty());

    // 落盘 → 加载
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("integration.idx");
    let vectors: Vec<(ChunkId, Vec<f32>)> = index
        .live_chunks()
        .map(|c| (c.chunk_id, fake_vec(&c.text)))
        .collect();
    let fingerprint = helix_core::storage::ConfigFingerprint {
        analyzer_id: "mixed".to_string(),
        embedder_id: "fake".to_string(),
        dim: 16,
        chunker: (512, 64),
    };
    storage::save(&path, &index, &vectors, &fingerprint).unwrap();

    let (loaded, lv, _fp) = storage::load(&path).unwrap();
    assert_eq!(lv.len(), vectors.len(), "向量应随快照保存");

    // 加载后：重建向量索引（D1：存原始数据，加载重建）。
    // 与 before 侧保持同一实现（BruteForceIndex），确保对比的是快照正确性本身。
    let vi2 = BruteForceIndex::from_entries(
        lv.iter()
            .map(|(id, v)| (*id, NormalizedVector::new(v.clone())))
            .collect(),
    );
    let analyzer2 = MixedAnalyzer::new();
    let searcher2 = QueryExecutor::new(&loaded, &analyzer2).with_vector(&e, &vi2);

    // 三模式结果完全一致
    assert_eq!(bm25_before, search_ids(&searcher2, query, SearchMode::Bm25));
    assert_eq!(
        vector_before,
        search_ids(&searcher2, query, SearchMode::Vector)
    );
    assert_eq!(
        hybrid_before,
        search_ids(&searcher2, query, SearchMode::Hybrid)
    );
}

/// 多线程并发检索：无 panic、结果确定（NFR-06 + T4-09）。
#[test]
fn 并发检索安全且确定() {
    let (index, analyzer) = build_full_index();
    let vi = build_vector_index(&index);
    let e = FakeEmbedder;
    let searcher = Arc::new(QueryExecutor::new(&index, &analyzer).with_vector(&e, &vi));

    let expected = search_ids(&searcher, "检索", SearchMode::Hybrid);

    // scoped threads：QueryExecutor 借用了栈上的 index/analyzer/e/vi，
    // 不能用 thread::spawn（要求 'static），thread::scope 允许借用局部变量
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let s = searcher.clone();
            let value = expected.clone();
            scope.spawn(move || {
                for _ in 0..50 {
                    let got = search_ids(&s, "检索", SearchMode::Hybrid);
                    assert_eq!(value, got, "并发检索结果应与串行一致");
                }
            });
        }
    });
}

// ------------------------------------------------- V2 Step 1：幽灵候选（Q-C1）

/// 30 篇短文档（确定性生成）：幽灵候选测试需要「大量已删向量」来制造名额压力。
///
/// 配比刻意选 **30 篇删 20 留 10 / K=5**：存活数是 K 的 2 倍，
/// 这样即便 ANN 少枚举 1~2 个节点（`hnsw_rs` 的固有近似误差，见 T1 注释），
/// 存活候选仍足够填满 K，测试才不会 flaky。
fn ghost_corpus() -> Vec<(String, String)> {
    (0..30)
        .map(|i| {
            (
                format!("note{i:02}.md"),
                format!("第 {i:02} 篇笔记：检索系统里的向量召回与倒排索引"),
            )
        })
        .collect()
}

/// 返回（索引, [(doc_id, 该文档的分片)]），顺序与 [`ghost_corpus`] 一致。
fn build_ghost_facade(backend: VectorBackend) -> (SearchIndex, Vec<(DocId, Vec<ChunkId>)>) {
    let mut idx = SearchIndex::builder()
        .embedder(Some(Arc::new(FakeEmbedder)))
        .vector_backend(backend)
        .build();
    let mut per_doc = Vec::new();
    for (source, text) in ghost_corpus() {
        let out = idx.add(Document::new(text).with_source(source)).unwrap();
        per_doc.push((out.doc_id, out.chunk_ids));
    }
    idx.commit().unwrap();
    (idx, per_doc)
}

/// T1（V2 Step 1）：已删除的向量不再霸占 Top-K 名额。
///
/// # Q-C1 的真实危害（重要）
///
/// 幽灵候选**从来就进不了最终结果**——`search_parts` 回捞时用
/// `Index::chunk(id)` 取正文，墓碑位返回 `None` 直接跳过。所以「删除后还能
/// 搜到已删内容」并不是本 bug 的表现形式。
///
/// 真正的危害是**名额被占**：Top-K 的 K 个位置里混进了幽灵，回捞时才被丢弃，
/// 于是用户要 5 条实际只拿到 1~2 条，有效召回静默下降——而调用方（Agent）
/// 无法区分「库里只有 2 条」和「有 5 条但 3 个位置被幽灵占了」。
/// 因此本测试断言的是 **`hits.len() == k`**，而不是"不含幽灵 chunk_id"。
///
/// hnsw_rs 没有 remove API（设计 §2.1 / H2），已删向量物理上仍在图里，
/// 只能靠存活位图谓词在检索期挡掉，故两种后端都要覆盖。
///
/// # 关于 Hnsw 后端的 flakiness（实测，勿回退本配置）
///
/// `hnsw_rs` 的 `search` **即使 `knbn == len`、`ef=200`，也可能返回不足 `len` 条**：
/// 在 20 点图上采样 200 次，191 次返回 20 条、8 次 19 条、1 次 18 条（约 4.5% 缺口）。
/// 这是 HNSW 的固有近似误差，与存活过滤无关。
///
/// ⇒ 若配比取「存活数 == K」（初版就是 20 篇删 15 留 5 / K=5），
/// 少枚举 1 个节点就必然少 1 条存活结果，测试约 5% 概率假红
/// （CI Linux 已复现，本地 30 次挂 2 次）。
/// 现配比 **存活数 = 2×K**，少枚举 1~2 条也不会凑不满 K。
#[test]
fn T1_已删向量不霸占TopK名额() {
    for backend in [VectorBackend::Brute, VectorBackend::Hnsw] {
        let (idx, per_doc) = build_ghost_facade(backend);
        let keep = 5;
        let survive = keep * 2; // 存活文档数刻意取 K 的 2 倍，理由见函数文档
        let total = per_doc.len();
        assert!(total >= survive * 2, "语料需足够大才能制造名额压力");

        // 删除前：top_n = keep 应该被填满（基线）
        let searcher = idx.into_searcher().unwrap();
        let before = searcher
            .search_with(QUERY)
            .mode("vector")
            .top_n(keep)
            .exec()
            .unwrap();
        assert_eq!(before.hits.len(), keep, "{backend:?}：删除前基线应填满 K");

        let mut idx = searcher.into_index().unwrap();

        // 只留 survive 篇，其余删掉 ⇒ 向量索引里 2/3 是幽灵
        let (victims, kept): (Vec<_>, Vec<_>) = per_doc
            .into_iter()
            .enumerate()
            .partition(|(i, _)| *i < total - survive);
        let victim_chunks: Vec<ChunkId> = victims
            .iter()
            .flat_map(|(_, (_, chunks))| chunks.clone())
            .collect();
        let kept_chunks: Vec<ChunkId> = kept
            .iter()
            .flat_map(|(_, (_, chunks))| chunks.clone())
            .collect();
        for (_, (doc_id, _)) in &victims {
            idx.remove(*doc_id).unwrap();
        }

        let searcher = idx.into_searcher().unwrap();

        // 向量路：名额必须被存活文档填满
        let hits = searcher
            .search_with(QUERY)
            .mode("vector")
            .top_n(keep)
            .exec()
            .unwrap();
        assert_eq!(
            hits.hits.len(),
            keep,
            "{backend:?}/vector：存活 {kept_chunks:?} 共 {} 条，\
             应全部填满 K={keep}；实际只返回 {} 条 —— 名额被幽灵占了",
            kept_chunks.len(),
            hits.hits.len()
        );

        // 三模式 soundness：结果里都不该出现幽灵
        for mode in ["bm25", "vector", "hybrid"] {
            let hits = searcher
                .search_with(QUERY)
                .mode(mode)
                .top_n(keep)
                .exec()
                .unwrap();
            let ghosts: Vec<ChunkId> = hits
                .hits
                .iter()
                .map(|h| h.chunk_id)
                .filter(|id| victim_chunks.contains(id))
                .collect();
            assert!(ghosts.is_empty(), "{backend:?}/{mode}：出现幽灵 {ghosts:?}");
        }
    }
}

/// T2（V2 Step 1）：幽灵候选**不跨快照永续**（覆盖设计 §2.2）。
///
/// 构造一个「已删除、但快照里仍带着幽灵向量」的快照——这正是旧实现的真实行为：
/// `raw_vectors` 没有移除路径，`save` 原样导出、`load` 全量重灌，
/// 于是被删文档一旦落盘，下次加载后幽灵就会回来继续占名额。
///
/// 现在有两道防线：① `SearchIndex::remove` 主动摘除 `raw_vectors`；
/// ② 即便快照里仍有幽灵向量（旧快照 / 其他写入路径），存活位图也会在检索期挡掉。
/// 本测试走 storage 层直接落盘，**刻意绕过防线 ①**，专门验证防线 ②。
///
/// 断言口径同 T1：看 `hits.len() == k`（名额未被幽灵占用），而非"结果不含幽灵"。
#[test]
fn T2_幽灵候选不跨快照永续() {
    let keep = 5;
    let survive = keep * 2; // 同 T1：存活数取 K 的 2 倍，规避 HNSW 近似误差
    let analyzer = MixedAnalyzer::new();
    let chunker = Chunker::default();
    let mut index = Index::new();
    let mut per_doc: Vec<(DocId, Vec<ChunkId>)> = Vec::new();
    let mut all_vectors: Vec<(ChunkId, Vec<f32>)> = Vec::new();

    for (source, text) in ghost_corpus() {
        let doc = DocRecord {
            doc_id: 0,
            source,
            metadata: serde_json::json!({}),
            content_hash: content_hash(&text),
        };
        let (doc_id, chunk_ids) = index.add(doc, chunker.chunk(0, &text), &analyzer).unwrap();
        for id in &chunk_ids {
            all_vectors.push((*id, fake_vec(&text)));
        }
        per_doc.push((doc_id, chunk_ids));
    }

    // 删掉大部分，**但向量列表刻意原样保留**（复现旧行为：无移除路径）
    let total = per_doc.len();
    let (victims, _kept): (Vec<_>, Vec<_>) = per_doc
        .into_iter()
        .enumerate()
        .partition(|(i, _)| *i < total - survive);
    let victim_chunks: Vec<ChunkId> = victims
        .iter()
        .flat_map(|(_, (_, chunks))| chunks.clone())
        .collect();
    for (_, (doc_id, _)) in &victims {
        index.remove(*doc_id, &analyzer).unwrap();
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ghost.idx");
    let fingerprint = storage::ConfigFingerprint {
        analyzer_id: "mixed".to_string(),
        embedder_id: "fake".to_string(),
        dim: 16,
        chunker: (512, 64),
    };
    storage::save(&path, &index, &all_vectors, &fingerprint).unwrap();

    // 加载：raw_vectors 全量重灌，幽灵向量重新进入向量索引
    let (loaded, loaded_vectors, _fp) = storage::load(&path).unwrap();
    assert!(
        loaded_vectors
            .iter()
            .any(|(id, _)| victim_chunks.contains(id)),
        "前置条件失败：快照里必须带着幽灵向量，否则本测试失去鉴别力"
    );

    let vi = BruteForceIndex::from_entries(
        loaded_vectors
            .iter()
            .map(|(id, v)| (*id, NormalizedVector::new(v.clone())))
            .collect(),
    );
    let e = FakeEmbedder;
    let searcher = QueryExecutor::new(&loaded, &analyzer).with_vector(&e, &vi);

    let hits = searcher.search(QUERY, SearchMode::Vector, keep).unwrap();
    assert_eq!(
        hits.hits.len(),
        keep,
        "跨快照后 K={keep} 个名额应被存活文档填满，实际只返回 {} 条",
        hits.hits.len()
    );

    for mode in [SearchMode::Bm25, SearchMode::Vector, SearchMode::Hybrid] {
        let hits = searcher.search(QUERY, mode, keep).unwrap();
        let ghosts: Vec<ChunkId> = hits
            .hits
            .iter()
            .map(|h| h.chunk_id)
            .filter(|id| victim_chunks.contains(id))
            .collect();
        assert!(
            ghosts.is_empty(),
            "{mode:?}：幽灵候选跨快照复活了 {ghosts:?}"
        );
    }
}
