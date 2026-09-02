//! T1-15：自研 BM25 与 tantivy 的排序对照（ADR-007）。
//!
//! # 对照策略（p1-design.md D5 的细化）
//!
//! **喂两侧完全相同的 token 流**：把我们的 `MixedAnalyzer` 分出的 token 用空格
//! 拼接，喂给 tantivy 的 `WhitespaceTokenizer`（不 lower、不过滤）。这样：
//!
//! - 分词、停用词、归一化的差异被**完全消除**（两侧 token 一模一样）
//! - 唯一剩余变量是 **tantivy 的 fieldnorm 量化**（`dl` 被压到 8 bit，见 p1-design.md 3.2）
//!
//! 因此本测试**只验证 BM25 打分数学**（idf / tf / avgdl / Top-K 排序），
//! 这是 ADR-007 的核心目的——"证明自己算对了"。分词正确性由 analyze 模块的单测
//! 独立保证；真实语料的端到端对照放到 P5（T5-04）作为三路基线之一。
//!
//! # 阈值
//!
//! 主指标：Top-10 集合重叠率。允许因 fieldnorm 量化造成的个别排序交换，
//! 但重叠率应接近 1.0（本测试用 ≥ 0.9 兜底）；差异 case 全部打印并归因。

use index_core::analyze::{Analyzer, MixedAnalyzer};
use index_core::index::Index;
use index_core::retriever::{Bm25Retriever, Retriever};

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED};
use tantivy::tokenizer::WhitespaceTokenizer;
use tantivy::{doc, Index as TantivyIndex, IndexWriter};

/// 内置语料（自包含，不依赖外部文件）。
const CORPUS: &[&str] = &[
    "BM25 是一种基于词频的信息检索排序算法，参数 k1 控制词频饱和，b 控制长度归一化。",
    "倒排索引维护词项到文档列表的映射，词典记录词项编号，倒排表记录文档编号和词频。",
    "向量检索把文本编码成向量，用余弦相似度找语义相近的内容，能召回字面不同但语义相近的文本。",
    "中文分词把连续汉字切分成词，jieba 是最常用的中文分词工具，支持精确模式和全模式。",
    "混合检索融合关键词检索和向量检索，倒数排名融合 RRF 只关心排名不关心分数大小。",
    "停用词是高频虚词，过滤它们能减小索引体积，但云网库这类单字术语不能过滤。",
    "HNSW 是近似最近邻的图结构算法，分层小世界图平衡召回率和速度，增量插入比较复杂。",
    "Rust 通过所有权和借用机制保证内存安全，零成本抽象适合实现检索引擎。",
    "平均文档长度影响 BM25 长度归一化，中文分词后词项数偏少导致平均长度系统性偏小。",
    "向量入库前做 L2 归一化，余弦相似度等于内积，欧氏距离和余弦相似度可以换算。",
    "评价指标包括召回率、平均倒数排名、折损累积增益，评测需要人工标注相关文档。",
    "索引快照把内存索引序列化到磁盘，用魔数标识格式，用校验和防止读到损坏文件。",
];

/// 对比用查询集：覆盖术语匹配、多词、含停用词、含未登录词等边界。
const QUERIES: &[&str] = &[
    "BM25 参数",
    "向量 检索",
    "中文 分词",
    "倒排 索引 词频",
    "融合 RRF",
    "不存在的词项 xyzzy",
    "的 了 在", // 全停用词
];

fn overlap(a: &[u32], b: &[u32]) -> f32 {
    let sa: std::collections::HashSet<u32> = a.iter().copied().collect();
    let inter = b.iter().filter(|x| sa.contains(x)).count();
    inter as f32 / b.len().max(1) as f32
}

#[test]
fn bm25_排序对照_tantivy() {
    let analyzer = MixedAnalyzer::new();

    // ---- 我们这边：建索引 ----
    let mut ours = Index::new();
    for (i, text) in CORPUS.iter().enumerate() {
        let doc = index_core::document::Document {
            doc_id: 0,
            source: format!("doc-{i}"),
            metadata: serde_json::json!({}),
            content_hash: 0,
        };
        let chunks = index_core::chunk::Chunker::default().chunk(0, text);
        ours.add(doc, chunks, &analyzer).unwrap();
    }

    // ---- tantivy 这边：喂相同的 token 流 ----
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
    let mut writer: IndexWriter = tindex.writer_with_num_threads(1, 20_000_000).unwrap();

    for (i, text) in CORPUS.iter().enumerate() {
        let tokens = analyzer.analyze_doc(text);
        let joined: Vec<&str> = tokens.iter().map(|t| t.term.as_str()).collect();
        writer
            .add_document(doc!(body => joined.join(" "), id_field => i as u64))
            .unwrap();
    }
    writer.commit().unwrap();

    let reader = tindex.reader().unwrap();
    let searcher = reader.searcher();
    let query_parser = QueryParser::for_index(&tindex, vec![body]);

    // ---- 逐个 query 对比 ----
    let retriever = Bm25Retriever::new(&ours, &analyzer);

    for q in QUERIES {
        // 我们这边
        let our_hits = retriever.search(q, 10).unwrap();
        let our_docs: Vec<u32> = our_hits
            .iter()
            .filter_map(|h| ours.chunk(h.chunk_id).map(|c| c.doc_id))
            .collect();

        // tantivy 这边
        let q_tokens = analyzer.analyze_query(q);
        if q_tokens.is_empty() {
            // 全停用词 → 两侧都应返回空
            assert!(our_hits.is_empty(), "query={q} 我们不应有结果");
            continue;
        }
        let q_joined: Vec<&str> = q_tokens.iter().map(|t| t.term.as_str()).collect();
        let tq = query_parser.parse_query(&q_joined.join(" ")).unwrap();
        let top = searcher
            .search(&tq, &TopDocs::with_limit(10).order_by_score())
            .unwrap();
        let tan_docs: Vec<u32> = top
            .iter()
            .map(|(_score, addr)| searcher.doc::<tantivy::TantivyDocument>(*addr).unwrap())
            .map(|d| d.get_first(id_field).unwrap().as_u64().unwrap() as u32)
            .collect();

        let ov = overlap(&our_docs, &tan_docs);
        println!("query={q:?}  our={our_docs:?}  tantivy={tan_docs:?}  overlap={ov:.2}");

        assert!(
            ov >= 0.9,
            "query={q:?} 排序差异过大: our={our_docs:?} tantivy={tan_docs:?} overlap={ov}"
        );
    }
}
