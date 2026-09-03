//! 库使用端到端示例：建索引 → hybrid 检索 → 拼上下文块。
//!
//! 运行（需 local-embed feature，首次会下载 bge-small-zh-v1.5 约 91MB）：
//!   cargo run -p helix-core --example search_basic
//!
//! 演示内核对外的最小闭环：`Index` + `Analyzer` + `Chunker` 建索引，
//! `LocalEmbedder` + `HnswRsIndex` 建向量侧，`Searcher` 编排 hybrid 检索，
//! 最终用 `Hit::to_context_block()` 产出可直接拼进 prompt 的上下文块。

use helix_core::analyze::MixedAnalyzer;
use helix_core::chunk::Chunker;
use helix_core::document::{content_hash, Document};
use helix_core::embed::{Embedder, LocalEmbedder};
use helix_core::fusion::RrfFusion;
use helix_core::index::Index;
use helix_core::query::{SearchMode, Searcher};
use helix_core::vector::{HnswRsIndex, NormalizedVector, VectorIndex};

fn main() -> anyhow::Result<()> {
    // ---- 1. 建索引：MixedAnalyzer（中英混合分词）+ 默认 Chunker（512/64）----
    let analyzer = MixedAnalyzer::new();
    let chunker = Chunker::default();
    let mut index = Index::new();

    // 读 demo 语料（30 篇中文技术短文，仓库根 data/corpus.jsonl）
    // 用 CARGO_MANIFEST_DIR 定位，避免依赖 cargo run 的工作目录。
    let corpus_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/corpus.jsonl");
    let corpus = std::fs::read_to_string(&corpus_path)
        .unwrap_or_else(|e| panic!("读取 demo 语料失败 {}: {e}", corpus_path.display()));
    for line in corpus.lines() {
        let v: serde_json::Value = serde_json::from_str(line)?;
        let source = v["source"].as_str().unwrap_or("<unknown>").to_string();
        let text = v["text"].as_str().unwrap().to_string();
        let doc = Document {
            doc_id: 0,
            source,
            metadata: v.get("metadata").cloned().unwrap_or(serde_json::json!({})),
            content_hash: content_hash(&text), // 幂等 upsert（FR-15）
        };
        index.add(doc, chunker.chunk(0, &text), &analyzer)?;
    }
    println!(
        "索引就绪：{} 文档 / {} 分片",
        index.num_docs(),
        index.num_chunks()
    );

    // ---- 2. 向量侧：embed 所有分片 → HNSW 图（入库前 L2 归一化）----
    let embedder = LocalEmbedder::new()?;
    let entries: Vec<_> = index
        .live_chunks()
        .map(|c| (c.chunk_id, c.text.clone()))
        .collect();
    let texts: Vec<String> = entries.iter().map(|(_, t)| t.clone()).collect();
    let vecs = embedder.embed_documents(&texts)?;

    let mut hnsw = HnswRsIndex::with_capacity(entries.len().max(1024));
    for ((id, _), v) in entries.iter().zip(vecs) {
        hnsw.add(*id, NormalizedVector::new(v))?;
    }

    // ---- 3. hybrid 检索（RRF 融合，默认 k=60 / 权重 1:1.5）----
    let searcher = Searcher::new(&index, &analyzer)
        .with_vector(&embedder, &hnsw)
        .with_fusion(Box::new(RrfFusion::default()));
    let resp = searcher.search("如何加快检索速度", SearchMode::Hybrid, 3)?;

    // ---- 4. 拼进 prompt 的上下文块（带出处与命中词，FR-12）----
    println!(
        "\n== 检索结果（{}，{}ms）==\n",
        resp.hits.len(),
        resp.took.as_millis()
    );
    for hit in &resp.hits {
        println!("{}\n", hit.to_context_block());
    }
    Ok(())
}
