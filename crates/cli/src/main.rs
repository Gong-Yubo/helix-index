//! idx —— index-demo 的命令行工具。
//!
//! 已实现：`build`、`search --mode {bm25|vector|hybrid}`、`compare`。
//! 快照落盘是 P4（T4-01），故 `search` 目前通过 `--input` 重新读取语料重建索引。

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use index_core::analyze::MixedAnalyzer;
use index_core::chunk::Chunker;
use index_core::document::Document;
use index_core::embed::{Embedder, LocalEmbedder};
use index_core::index::Index;
use index_core::query::{EmptyReason, Hit, SearchMode, SearchResponse, Searcher};
use index_core::vector::{BruteForceIndex, NormalizedVector, VectorIndex};

#[derive(Parser)]
#[command(name = "idx", version, about = "index-demo 检索内核命令行工具")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 建索引（内存索引，快照在 P4）
    Build(BuildArgs),
    /// 检索（--mode bm25 / vector / hybrid）
    Search(SearchArgs),
    /// 三种模式同屏对比（调试主入口）
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
    /// 打印 explain 详情（匹配词 + 两路 rank/score）
    #[arg(long)]
    explain: bool,
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
        Command::Compare(args) => compare(args),
        Command::Bench(_) => bail!("bench 在 P5 实现"),
    }
}

fn parse_mode(s: &str) -> Result<SearchMode> {
    match s {
        "bm25" => Ok(SearchMode::Bm25),
        "vector" => Ok(SearchMode::Vector),
        "hybrid" => Ok(SearchMode::Hybrid),
        other => bail!("不支持的 --mode {other:?}（支持 bm25 / vector / hybrid）"),
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
            doc_id: 0,
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

/// 把每个分片文本 embed 后建暴力向量索引。
fn build_vector_index(index: &Index, embedder: &LocalEmbedder) -> Result<BruteForceIndex> {
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
    Ok(vindex)
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
    let mode = parse_mode(&args.mode)?;
    let (index, analyzer) = load_corpus(&args.input)?;

    // 延迟初始化，让 embedder / vindex 的生命周期覆盖到 search
    let embedder;
    let vindex;
    let mut searcher = Searcher::new(&index, &analyzer);
    if mode != SearchMode::Bm25 {
        embedder = LocalEmbedder::new()?;
        vindex = build_vector_index(&index, &embedder)?;
        searcher = searcher.with_vector(&embedder, &vindex);
    }

    let resp = searcher.search(&args.query, mode, args.k)?;
    print_response(&resp, &args.query, args.explain);
    Ok(())
}

fn compare(args: CompareArgs) -> Result<()> {
    let (index, analyzer) = load_corpus(&args.input)?;
    let embedder = LocalEmbedder::new()?;
    let vindex = build_vector_index(&index, &embedder)?;
    let searcher = Searcher::new(&index, &analyzer).with_vector(&embedder, &vindex);

    println!("查询: {}\n", args.query);
    println!("=== BM25 ===");
    print_hits(&searcher.search(&args.query, SearchMode::Bm25, args.k)?.hits);
    println!("\n=== Vector ===");
    print_hits(
        &searcher
            .search(&args.query, SearchMode::Vector, args.k)?
            .hits,
    );
    println!("\n=== Hybrid (RRF) ===");
    print_hits(
        &searcher
            .search(&args.query, SearchMode::Hybrid, args.k)?
            .hits,
    );
    Ok(())
}

fn print_response(resp: &SearchResponse, query: &str, explain: bool) {
    println!("查询: {}\n", query);
    if let Some(reason) = resp.empty_reason {
        println!("无结果（{}）", empty_reason_text(reason));
        return;
    }
    for (rank, hit) in resp.hits.iter().enumerate() {
        print_hit(rank, hit, explain);
    }
}

fn print_hits(hits: &[Hit]) {
    for (rank, hit) in hits.iter().enumerate() {
        print_hit(rank, hit, false);
    }
}

fn print_hit(rank: usize, hit: &Hit, explain: bool) {
    let snippet: String = hit.text.chars().take(60).collect();
    println!("#{:<2} score={:.4}  [{}]", rank + 1, hit.score, hit.source);
    println!("    {}", snippet);
    if explain {
        let e = &hit.explain;
        println!(
            "    └─ 匹配词: {} | bm25(rank={},score={}) vector(rank={},score={})",
            if e.matched_terms.is_empty() {
                "-".to_string()
            } else {
                e.matched_terms.join(",")
            },
            opt_fmt(e.bm25_rank),
            opt_score(e.bm25_score),
            opt_fmt(e.vector_rank),
            opt_score(e.vector_score),
        );
    }
}

fn opt_fmt(v: Option<u32>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".to_string())
}

fn opt_score(v: Option<f32>) -> String {
    v.map(|x| format!("{x:.4}"))
        .unwrap_or_else(|| "-".to_string())
}

fn empty_reason_text(r: EmptyReason) -> &'static str {
    match r {
        EmptyReason::NoDocuments => "索引为空",
        EmptyReason::AllTermsUnmatched => "查询词全部未命中（可能含幻觉词）",
        EmptyReason::FilteredOut => "候选被过滤条件全部排除",
    }
}
