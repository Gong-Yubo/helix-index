//! D8：`HnswRsIndex` vs `BruteForceIndex` 召回对照（真实语料 + 评测 query）。
//!
//! P4 的 0.970 重合率测于**随机向量**，不能代表真实场景——bge 真实 embedding
//! 的相关文档常处边界位置，恰是最易翻转的区间。D8 决策：在 T2Ranking 真实
//! embedding + 评测 query 上验证 HNSW 召回一致性（相对暴力全量扫描的 Top-K 重合率）。
//!
//! # 运行（#[ignore]：依赖评测数据 + 模型下载，必须 release）
//!
//! ```bash
//! cargo test -p helix-core --release --test vector_ab -- --ignored --nocapture
//! ```
//!
//! 语料/查询路径可用环境变量 `IDX_T2_CORPUS` / `IDX_T2_QUERIES` 覆盖（默认
//! `../../data/t2-corpus.jsonl` / `../../data/t2-queries.jsonl`，相对仓库根执行）。

#![cfg(feature = "local-embed")]
#![allow(non_snake_case)] // 中文测试名

use std::path::PathBuf;
use std::time::Instant;

use helix_core::embed::{Embedder, LocalEmbedder};
use helix_core::vector::{BruteForceIndex, HnswRsIndex, NormalizedVector, VectorIndex};

/// 语料子集规模（embed 吞吐 ~50 条/s，2000 段约 40s，控制测试时长）。
const N_CORPUS: usize = 2000;
/// 评测 query 数量。
const N_QUERY: usize = 100;
const K: usize = 10;

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var(var)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(default))
}

/// 读 t2-corpus.jsonl 前 `limit` 段 → (chunk_id, text)。
fn load_corpus(path: &PathBuf, limit: usize) -> Vec<(u32, String)> {
    let content = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "读取语料失败 {}: {e}（先跑 t2_prep 或设 IDX_T2_CORPUS）",
            path.display()
        )
    });
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(limit)
        .enumerate()
        .map(|(i, line)| {
            let v: serde_json::Value = serde_json::from_str(line).expect("语料行非法 JSON");
            let text = v["text"].as_str().expect("缺 text").to_string();
            (i as u32, text)
        })
        .collect()
}

/// 读 t2-queries.jsonl 前 `limit` 条 query 文本。
fn load_queries(path: &PathBuf, limit: usize) -> Vec<String> {
    let content = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "读取查询失败 {}: {e}（先跑 t2_prep 或设 IDX_T2_QUERIES）",
            path.display()
        )
    });
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(limit)
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).expect("查询行非法 JSON");
            v["query"].as_str().expect("缺 query").to_string()
        })
        .collect()
}

fn topk_overlap(
    hnsw: &HnswRsIndex,
    brute: &BruteForceIndex,
    queries: &[NormalizedVector],
    k: usize,
) -> f32 {
    let mut total = 0.0;
    for q in queries {
        let h: Vec<u32> = hnsw.search(q, k).unwrap().iter().map(|(i, _)| *i).collect();
        let b: Vec<u32> = brute
            .search(q, k)
            .unwrap()
            .iter()
            .map(|(i, _)| *i)
            .collect();
        let hset: std::collections::HashSet<u32> = h.iter().copied().collect();
        total += b.iter().filter(|x| hset.contains(x)).count() as f32 / k as f32;
    }
    total / queries.len() as f32
}

#[test]
#[ignore]
fn 真实语料召回对照() {
    let corpus_path = env_path("IDX_T2_CORPUS", "../../data/t2-corpus.jsonl");
    let queries_path = env_path("IDX_T2_QUERIES", "../../data/t2-queries.jsonl");

    let corpus = load_corpus(&corpus_path, N_CORPUS);
    let query_texts = load_queries(&queries_path, N_QUERY);
    println!(
        "\n========== HNSW vs 暴力召回对照（真实语料 {N_CORPUS} 段 × {N_QUERY} query，release） ==========\n"
    );

    // ---- embed：语料入库侧（无前缀）+ query 查询侧（BGE 前缀）----
    let embedder = LocalEmbedder::new().expect("模型初始化失败");
    let t = Instant::now();
    let doc_texts: Vec<String> = corpus.iter().map(|(_, t)| t.clone()).collect();
    let doc_vecs = embedder
        .embed_documents(&doc_texts)
        .expect("embed 语料失败");
    println!("embed {N_CORPUS} 段 耗时 {:?}", t.elapsed());

    let query_vecs: Vec<NormalizedVector> = query_texts
        .iter()
        .map(|q| NormalizedVector::new(embedder.embed_query(q).expect("embed query 失败")))
        .collect();

    // ---- 构建两侧索引 ----
    let entries: Vec<(u32, NormalizedVector)> = corpus
        .iter()
        .zip(doc_vecs)
        .map(|((id, _), v)| (*id, NormalizedVector::new(v)))
        .collect();

    let mut hnsw = HnswRsIndex::with_capacity(N_CORPUS.max(1024));
    let t = Instant::now();
    for (id, v) in &entries {
        hnsw.add(*id, v.clone()).expect("HNSW insert 失败");
    }
    println!("HNSW 构建 {N_CORPUS} 条 耗时 {:?}", t.elapsed());

    let brute = BruteForceIndex::from_entries(entries);

    // ---- 重合率对照 ----
    let overlap = topk_overlap(&hnsw, &brute, &query_vecs, K);
    println!("\nTop-{K} vs 暴力重合率 = {overlap:.3}（判据 ≥ 0.95）\n");

    assert!(overlap >= 0.95, "HNSW 召回重合率未达标: {overlap}");
}
