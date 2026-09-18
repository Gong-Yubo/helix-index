//! 库使用端到端示例：建索引 → hybrid 检索 → 拼上下文块。
//!
//! 运行（需 local-embed feature，首次会下载 bge-small-zh-v1.5 约 91MB）：
//!   cargo run -p helix-core --example search_basic
//!
//! 演示内核对外的最小闭环（P6 门面层）：`SearchIndex::builder().build()` 零配置，
//! `index.add(doc)` 摄入，`searcher.search(query)` 检索，
//! 最终用 `Hit::to_context_block()` 产出可直接拼进 prompt 的上下文块。
//! 对比 P6 之前的 79 行 / 8 对象 / 7 步骤（见 docs/devel/p6-design.md 1.2）。

use helix_core::prelude::*;
use helix_core::search::SearchIndex;

fn main() -> anyhow::Result<()> {
    // ---- 1. 零配置装配（MixedAnalyzer + bge-small-zh + HNSW + RRF）----
    let mut index = SearchIndex::builder().build();

    // 读 demo 语料（30 篇中文技术短文，仓库根 data/corpus.jsonl）
    let corpus_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/corpus.jsonl");
    let corpus = std::fs::read_to_string(&corpus_path)
        .unwrap_or_else(|e| panic!("读取 demo 语料失败 {}: {e}", corpus_path.display()));

    // ---- 2. 摄入（分块 / 倒排 / 向量化全在 add 背后，幂等去重由库保证）----
    for line in corpus.lines() {
        let v: serde_json::Value = serde_json::from_str(line)?;
        index.add(
            Document::new(v["text"].as_str().unwrap())
                .with_source(v["source"].as_str().unwrap_or("<unknown>"))
                .with_metadata(v.get("metadata").cloned().unwrap_or(serde_json::json!({}))),
        )?;
    }

    // ---- 3. 检索（只有 query 必选；默认 Hybrid + top_n=10）----
    // S8-02：显式 `commit()` 决定可见性；`searcher(&self)` 不消耗写端。
    index.commit()?;
    let searcher = index.searcher();
    let resp = searcher.search_with("如何加快检索速度").top_n(3).exec()?;

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
