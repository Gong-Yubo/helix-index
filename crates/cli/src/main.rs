//! idx —— index-demo 的命令行工具。
//!
//! P1 阶段：`build` + `search --mode bm25`。
//! 快照落盘是 P4（T4-01），故 `search` 目前通过 `--input` 重新读取语料重建索引；
//! `--index` 在 P4 快照落地后启用。

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use index_core::analyze::{Analyzer, MixedAnalyzer};
use index_core::chunk::Chunker;
use index_core::document::Document;
use index_core::embed::{Embedder, LocalEmbedder};
use index_core::index::Index;
use index_core::retriever::{Bm25Retriever, Retriever, VectorRetriever};
use index_core::vector::{BruteForceIndex, NormalizedVector, VectorIndex};

#[derive(Parser)]
#[command(name = "idx", version, about = "index-demo 检索内核命令行工具")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 建索引（P1：内存索引，快照在 P4）
    Build(BuildArgs),
    /// 检索（P1 支持 --mode bm25；vector/hybrid 在 P2/P3）
    Search(SearchArgs),
    /// 三种模式对比（P3 实现）
    Compare(CompareArgs),
    /// 效果评测（P5 实现）
    Bench(BenchArgs),
}

#[derive(clap::Args)]
struct BuildArgs {
    /// 语料文件（JSONL：每行 {"source": "...", "text": "..."}）
    #[arg(short, long)]
    input: PathBuf,
    /// 输出快照路径（P4 启用；当前仅打印）
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(clap::Args)]
struct SearchArgs {
    /// 语料文件（P4 快照落地后改用 --index）
    #[arg(short, long)]
    input: PathBuf,
    /// 检索模式
    #[arg(short, long, default_value = "bm25")]
    mode: String,
    /// 返回条数
    #[arg(short, long, default_value_t = 10)]
    k: usize,
    /// 查询文本
    query: String,
}

#[derive(clap::Args)]
struct CompareArgs {
    #[arg(short, long)]
    input: PathBuf,
    #[arg(short, long, default_value_t = 10)]
    k: usize,
    query: String,
}

#[derive(clap::Args)]
struct BenchArgs {
    #[arg(short, long)]
    index: PathBuf,
    #[arg(short, long)]
    queries: PathBuf,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Build(args) => build(args),
        Command::Search(args) => search(args),
        Command::Compare(_) => bail!("compare 在 P3 实现"),
        Command::Bench(_) => bail!("bench 在 P5 实现"),
    }
}

/// 从 JSONL 语料构建索引。
fn load_corpus(path: &PathBuf) -> Result<(Index, MixedAnalyzer)> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("读取语料失败: {}", path.display()))?;

    let analyzer = MixedAnalyzer::new();
    let chunker = Chunker::default();
    let mut index = Index::new();

    let mut count = 0usize;
    for (lineno, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("第 {} 行不是合法 JSON", lineno + 1))?;
        let source = v
            .get("source")
            .and_then(|s| s.as_str())
            .unwrap_or("<unknown>")
            .to_string();
        let text = v
            .get("text")
            .and_then(|s| s.as_str())
            .with_context(|| format!("第 {} 行缺少 text 字段", lineno + 1))?;
        let metadata = v.get("metadata").cloned().unwrap_or(serde_json::json!({}));

        let doc = Document {
            doc_id: 0, // 由 Index 分配
            source,
            metadata,
            content_hash: 0, // 幂等去重是 P4（T4-04）
        };
        let chunks = chunker.chunk(0, text);
        index.add(doc, chunks, &analyzer)?;
        count += 1;
    }

    if count == 0 {
        bail!("语料为空: {}", path.display());
    }
    Ok((index, analyzer))
}

fn build(args: BuildArgs) -> Result<()> {
    let (index, _) = load_corpus(&args.input)?;
    println!("索引构建完成:");
    println!("  分片数   = {}", index.num_chunks());
    println!("  词项总数 = {}", index.total_len());
    println!("  平均分片 = {:.2}", index.avgdl());
    if let Some(out) = &args.output {
        println!(
            "  ⚠️  快照落盘在 P4 实现，当前忽略 --output {}（索引仅存在于本次进程）",
            out.display()
        );
    }
    Ok(())
}

fn search(args: SearchArgs) -> Result<()> {
    match args.mode.as_str() {
        "bm25" => search_bm25(&args),
        "vector" => search_vector(&args),
        other => bail!("不支持的 --mode {other:?}（P2 支持 bm25 / vector）"),
    }
}

fn search_bm25(args: &SearchArgs) -> Result<()> {
    let (index, analyzer) = load_corpus(&args.input)?;
    let retriever = Bm25Retriever::new(&index, &analyzer);
    let hits = retriever.search(&args.query, args.k)?;

    if hits.is_empty() {
        println!("无结果");
        return Ok(());
    }

    println!("查询: {}\n", args.query);
    for (rank, h) in hits.iter().enumerate() {
        let chunk = index.chunk(h.chunk_id).expect("命中分片应存活");
        let doc = index.doc(chunk.doc_id).expect("命中文档应存活");
        // 匹配词：query 分词中真正命中该分片的词
        let q_tokens = analyzer.analyze_query(&args.query);
        let mut matched: Vec<String> = q_tokens
            .iter()
            .filter(|t| {
                index.doc_freq(t.term.as_str()) > 0
                    && analyzer
                        .analyze_doc(&chunk.text)
                        .iter()
                        .any(|c| c.term == t.term)
            })
            .map(|t| t.term.to_string())
            .collect();
        matched.sort();
        matched.dedup();

        let snippet: String = chunk.text.chars().take(60).collect();
        println!("#{:<2} score={:.4}  [{}]", rank + 1, h.score, doc.source);
        println!("    匹配词: {}", matched.join(", "));
        println!("    {}", snippet);
    }
    Ok(())
}

fn search_vector(args: &SearchArgs) -> Result<()> {
    let (index, _) = load_corpus(&args.input)?;

    // 建向量索引：把每个分片文本 embed 后存入暴力索引（P2 规模，见 p2-design.md D3）
    let embedder = LocalEmbedder::new()?;
    let entries: Vec<(u32, String)> = index
        .live_chunks()
        .map(|c| (c.chunk_id, c.text.clone()))
        .collect();
    let texts: Vec<String> = entries.iter().map(|(_, t)| t.clone()).collect();
    let vecs = embedder.embed_documents(&texts)?;

    let mut vindex = BruteForceIndex::new();
    for ((chunk_id, _), v) in entries.iter().zip(vecs.into_iter()) {
        vindex.add(*chunk_id, NormalizedVector::new(v))?;
    }

    let retriever = VectorRetriever::new(&embedder, &vindex);
    let hits = retriever.search(&args.query, args.k)?;

    if hits.is_empty() {
        println!("无结果");
        return Ok(());
    }

    println!("查询: {}\n", args.query);
    for (rank, h) in hits.iter().enumerate() {
        let chunk = index.chunk(h.chunk_id).expect("命中分片应存活");
        let doc = index.doc(chunk.doc_id).expect("命中文档应存活");
        let snippet: String = chunk.text.chars().take(60).collect();
        println!(
            "#{:<2} similarity={:.4}  [{}]",
            rank + 1,
            h.score,
            doc.source
        );
        println!("    {}", snippet);
    }
    Ok(())
}
