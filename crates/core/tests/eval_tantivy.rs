//! T5-04 / ADR-007：自研 BM25 与 tantivy 在**真实 T2Ranking 语料**上的排序对照。
//!
//! # 方法（p5-design.md 8.3，复用 T1-15 模式）
//!
//! 喂两侧完全相同的 token 流：`MixedAnalyzer` 分出的 token 空格拼接喂 tantivy 的
//! `WhitespaceTokenizer`——分词/停用词/归一化差异完全消除，只比 BM25 排序数学。
//!
//! **参数口径**：tantivy 0.26 的 K1=1.2 / B=0.75 是硬编码常量（`query/bm25.rs:8-9`，
//! 无公开 API 可改）→ 自研侧显式 `Bm25Params { k1: 1.2, b: 0.75 }` 对齐。
//! 已知差异来源：tantivy fieldnorm 8bit 量化（长文档 dl 压缩到量化格点）。
//!
//! # 断言（D6）
//!
//! 自研与 tantivy 的 NDCG@10 **相对差 ≤ 5%**；逐 query 分值表打印入报告。
//!
//! # 运行（#[ignore]：依赖本地评测数据 + tantivy 是 dev 依赖）
//!
//! ```bash
//! cargo test -p helix-core --release --test eval_tantivy -- --ignored --nocapture
//! ```
//!
//! 语料路径可用环境变量 `IDX_T2_CORPUS` / `IDX_T2_QUERIES` 覆盖（默认
//! `data/t2-corpus.jsonl` / `data/t2-queries.jsonl`，相对仓库根执行）。

use std::collections::HashMap;
use std::path::PathBuf;

use helix_core::analyze::{Analyzer, MixedAnalyzer};
use helix_core::bench::{self, Judgment};
use helix_core::chunk::Chunker;
use helix_core::document::Document;
use helix_core::index::Index;
use helix_core::retriever::{Bm25Params, Bm25Retriever, Retriever};

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED};
use tantivy::tokenizer::WhitespaceTokenizer;
use tantivy::{doc, Index as TantivyIndex, IndexWriter};

const K: usize = 10;

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var(var)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(default))
}

/// 读 t2-corpus.jsonl → (source, text) 列表。
fn load_corpus(path: &PathBuf) -> Vec<(String, String)> {
    let content = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "读取语料失败 {}: {e}（先跑 t2_prep 或设 IDX_T2_CORPUS）",
            path.display()
        )
    });
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("corpus 行必须是 JSON");
            (
                v["source"].as_str().expect("source 字段").to_string(),
                v["text"].as_str().expect("text 字段").to_string(),
            )
        })
        .collect()
}

#[test]
#[ignore]
fn 自研bm25与tantivy真实语料对照() {
    // cargo test 的 cwd 是 crates/core，默认路径相对仓库根
    let corpus_path = env_path("IDX_T2_CORPUS", "../../data/t2-corpus.jsonl");
    let queries_path = env_path("IDX_T2_QUERIES", "../../data/t2-queries.jsonl");
    let judgments: Vec<Judgment> = bench::load_judgments(&queries_path).expect("加载 judgments");
    let corpus = load_corpus(&corpus_path);
    println!("语料 {} 段落 / {} 查询", corpus.len(), judgments.len());

    let analyzer = MixedAnalyzer::new();

    // ---- 自研索引：评测口径（每段落强制单 chunk，p5-design 5.2 步骤 8）----
    let chunker = Chunker::new(200_000, 0);
    let mut ours = Index::new();
    for (source, text) in &corpus {
        let doc = Document {
            doc_id: 0,
            source: source.clone(),
            metadata: serde_json::json!({"origin": "t2ranking"}),
            content_hash: 0,
        };
        let chunks = chunker.chunk(0, text);
        assert_eq!(
            chunks.len(),
            1,
            "强制单 chunk 路径下段落 {source} 应恰为 1 chunk"
        );
        ours.add(doc, chunks, &analyzer).unwrap();
    }

    // ---- tantivy 索引：相同 token 流（WhitespaceTokenizer 消除分词差异）----
    let mut schema_builder = Schema::builder();
    let body = schema_builder.add_text_field(
        "body",
        TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("ws")
                    .set_index_option(IndexRecordOption::WithFreqs),
            )
            .set_stored(),
    );
    let id_field = schema_builder.add_u64_field("id", STORED);
    let schema = schema_builder.build();

    let tindex = TantivyIndex::create_in_ram(schema.clone());
    tindex
        .tokenizers()
        .register("ws", WhitespaceTokenizer::default());
    let mut writer: IndexWriter = tindex.writer_with_num_threads(1, 50_000_000).unwrap();

    // pid → tantivy doc id（行号），两侧通过 source 关联
    let mut pid_to_row: HashMap<String, u64> = HashMap::new();
    for (row, (_source, text)) in corpus.iter().enumerate() {
        let tokens = analyzer.analyze_doc(text);
        let joined: Vec<&str> = tokens.iter().map(|t| t.term.as_str()).collect();
        writer
            .add_document(doc!(body => joined.join(" "), id_field => row as u64))
            .unwrap();
        pid_to_row.insert(_source.clone(), row as u64);
    }
    writer.commit().unwrap();
    let row_to_pid: HashMap<u64, &str> = pid_to_row
        .iter()
        .map(|(pid, &row)| (row, pid.as_str()))
        .collect();

    let reader = tindex.reader().unwrap();
    let tsearcher = reader.searcher();
    let query_parser = QueryParser::for_index(&tindex, vec![body]);

    // ---- 自研检索器：k1/b 与 tantivy 硬编码对齐（tantivy 0.26 无公开参数 API）----
    let retriever =
        Bm25Retriever::new(&ours, &analyzer).with_params(Bm25Params { k1: 1.2, b: 0.75 });

    // ---- 逐 query 双侧检索 → NDCG@10 ----
    let mut ours_ndcgs = Vec::with_capacity(judgments.len());
    let mut tan_ndcgs = Vec::with_capacity(judgments.len());
    let mut top10_overlap_sum = 0.0f32;

    for j in &judgments {
        // 自研：chunk_id → doc_id → source
        let our_hits = retriever.search(&j.query, K).unwrap();
        let our_sources: Vec<&str> = our_hits
            .iter()
            .filter_map(|h| {
                ours.chunk(h.chunk_id)
                    .and_then(|c| ours.doc(c.doc_id))
                    .map(|d| d.source.as_str())
            })
            .collect();

        // tantivy：query token 流 → top-10 行号 → pid
        let q_tokens = analyzer.analyze_query(&j.query);
        let tan_sources: Vec<&str> = if q_tokens.is_empty() {
            Vec::new()
        } else {
            let q_joined: Vec<&str> = q_tokens.iter().map(|t| t.term.as_str()).collect();
            let tq = query_parser.parse_query(&q_joined.join(" ")).unwrap();
            let top = tsearcher
                .search(&tq, &TopDocs::with_limit(K).order_by_score())
                .unwrap();
            top.iter()
                .map(|(_score, addr)| {
                    let d: tantivy::TantivyDocument = tsearcher.doc(*addr).unwrap();
                    let row = d.get_first(id_field).unwrap().as_u64().unwrap();
                    row_to_pid[&row]
                })
                .collect()
        };

        // Top-10 集合重叠率（诊断参考）
        let our_set: std::collections::HashSet<&str> = our_sources.iter().copied().collect();
        let inter = tan_sources.iter().filter(|s| our_set.contains(*s)).count();
        top10_overlap_sum += inter as f32 / K as f32;

        let grades = j.grades();
        let m_ours = bench::evaluate(&our_sources, &grades, K, 1);
        let m_tan = bench::evaluate(&tan_sources, &grades, K, 1);
        ours_ndcgs.push(m_ours.ndcg);
        tan_ndcgs.push(m_tan.ndcg);
    }

    let avg = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let ours_ndcg = avg(&ours_ndcgs);
    let tan_ndcg = avg(&tan_ndcgs);
    let rel_diff = ((ours_ndcg - tan_ndcg) / tan_ndcg).abs();
    let overlap = top10_overlap_sum / judgments.len() as f32;

    println!(
        "== tantivy 基线（k1=1.2/b=0.75 对齐，{} 查询）==",
        judgments.len()
    );
    println!("  自研 BM25   NDCG@10 = {ours_ndcg:.4}");
    println!("  tantivy     NDCG@10 = {tan_ndcg:.4}");
    println!("  相对差      |自研−tantivy|/tantivy = {rel_diff:.4}（D6 阈值 ≤ 5%）");
    println!("  Top-10 平均集合重叠率 = {overlap:.4}（fieldnorm 8bit 量化会拉低）");

    assert!(
        rel_diff <= 0.05,
        "自研 BM25 与 tantivy 的 NDCG@10 相对差 {rel_diff:.4} 超过 D6 阈值 5%"
    );
}
