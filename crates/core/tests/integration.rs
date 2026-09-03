//! T4-09 集成测试：端到端快照 + 并发检索。
//!
//! 用确定性假 Embedder（FNV 哈希向量），**不依赖模型下载**。

use std::sync::Arc;

use helix_core::analyze::MixedAnalyzer;
use helix_core::chunk::Chunker;
use helix_core::document::{content_hash, Document};
use helix_core::embed::Embedder;
use helix_core::error::Result;
use helix_core::index::Index;
use helix_core::query::{SearchMode, Searcher};
use helix_core::storage;
use helix_core::types::ChunkId;
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

fn build_full_index() -> (Index, MixedAnalyzer) {
    let analyzer = MixedAnalyzer::new();
    let chunker = Chunker::default();
    let mut index = Index::new();
    for (source, text) in CORPUS {
        let doc = Document {
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

fn search_ids(searcher: &Searcher, query: &str, mode: SearchMode) -> Vec<(ChunkId, f32)> {
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
    let searcher = Searcher::new(&index, &analyzer).with_vector(&e, &vi);

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
    storage::save(&path, &index, &vectors).unwrap();

    let (loaded, lv) = storage::load(&path).unwrap();
    assert_eq!(lv.len(), vectors.len(), "向量应随快照保存");

    // 加载后：重建向量索引（D1：存原始数据，加载重建）。
    // 与 before 侧保持同一实现（BruteForceIndex），确保对比的是快照正确性本身。
    let vi2 = BruteForceIndex::from_entries(
        lv.iter()
            .map(|(id, v)| (*id, NormalizedVector::new(v.clone())))
            .collect(),
    );
    let analyzer2 = MixedAnalyzer::new();
    let searcher2 = Searcher::new(&loaded, &analyzer2).with_vector(&e, &vi2);

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
    let searcher = Arc::new(Searcher::new(&index, &analyzer).with_vector(&e, &vi));

    let expected = search_ids(&searcher, "检索", SearchMode::Hybrid);

    // scoped threads：Searcher 借用了栈上的 index/analyzer/e/vi，
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
