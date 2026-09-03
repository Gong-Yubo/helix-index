//! helix —— HelixIndex 的命令行工具。
//!
//! 已实现：
//! - `build`：摄入语料并**落盘快照**（`--vectors` 同时保存向量）
//! - `search --mode {bm25|vector|hybrid}`：`--input` 重建 或 `--index` 从快照加载
//! - `compare`：三路同屏对比
//!
//! 向量索引用 `HnswRsIndex`（P4 A/B 结论：原生增量 insert，见 p4-design.md）。

mod bench;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use helix_core::analyze::MixedAnalyzer;
use helix_core::chunk::Chunker;
use helix_core::document::{content_hash, Document};
use helix_core::embed::{Embedder, LocalEmbedder};
use helix_core::index::Index;
use helix_core::query::{EmptyReason, Hit, SearchMode, SearchResponse, Searcher};
use helix_core::schema::Filter;
use helix_core::storage;
use helix_core::types::ChunkId;
use helix_core::vector::{HnswRsIndex, NormalizedVector, VectorIndex};

#[derive(Parser)]
#[command(name = "helix", version, about = "HelixIndex 检索内核命令行工具")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 建索引并落盘快照
    Build(BuildArgs),
    /// 检索（--mode bm25 / vector / hybrid；--input 重建 或 --index 快照）
    Search(SearchArgs),
    /// 三种模式同屏对比（调试主入口）
    Compare(CompareArgs),
    /// 效果评测（P5 实现）
    Bench(bench::BenchArgs),
}

#[derive(clap::Args)]
struct BuildArgs {
    /// 语料文件（JSONL：每行 {"source": "...", "text": "..."}）
    #[arg(short, long)]
    input: PathBuf,
    /// 输出快照路径（P4 起真正落盘）
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// 同时嵌入并保存向量（vector/hybrid 检索需要；首次会下载模型）
    #[arg(long)]
    vectors: bool,
    /// 每段落强制单 chunk（评测口径：段落级标注防多 chunk 双计，NFR-03 对齐"1 万 chunk"）
    #[arg(long)]
    single_chunk: bool,
}

#[derive(clap::Args)]
struct SearchArgs {
    /// 语料文件（与 --index 二选一，每次重建索引）
    #[arg(short, long)]
    input: Option<PathBuf>,
    /// 快照文件（与 --input 二选一，秒级加载）
    #[arg(long)]
    index: Option<PathBuf>,
    /// 检索模式
    #[arg(short, long, default_value = "bm25")]
    mode: String,
    /// 返回条数
    #[arg(short, long, default_value_t = 10)]
    k: usize,
    /// 元数据过滤（field=value，等值匹配，可多次出现）
    #[arg(long)]
    filter: Vec<String>,
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

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Build(args) => build(args),
        Command::Search(args) => search(args),
        Command::Compare(args) => compare(args),
        Command::Bench(args) => bench::run(args),
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

/// 解析 `field=value` 形式的过滤条件（等值匹配）。
fn parse_filters(specs: &[String]) -> Result<Option<Filter>> {
    if specs.is_empty() {
        return Ok(None);
    }
    let mut conditions = Vec::with_capacity(specs.len());
    for s in specs {
        match s.split_once('=') {
            Some((f, v)) => conditions.push(Filter::eq(f, v)),
            None => bail!("过滤条件格式应为 field=value，收到 {s:?}"),
        }
    }
    Ok(Some(Filter::And(conditions)))
}

/// 从 JSONL 语料构建索引（默认 Chunker 512/64）。
pub(crate) fn load_corpus(path: &Path) -> Result<(Index, MixedAnalyzer)> {
    load_corpus_with(path, &Chunker::default())
}

/// 从 JSONL 语料构建索引，指定 Chunker（bench 的"每段落强制单 chunk"
/// 评测路径用——T2Ranking 段落级标注防多 chunk 双计，见 p5-design 5.2 步骤 8）。
pub(crate) fn load_corpus_with(path: &Path, chunker: &Chunker) -> Result<(Index, MixedAnalyzer)> {
    let analyzer = MixedAnalyzer::new();
    let index = build_index(path, chunker, &analyzer)?;
    Ok((index, analyzer))
}

/// 底层构建：用调用方提供的 analyzer 从 JSONL 构建索引。
///
/// 与 [`load_corpus_with`] 的区别仅在于 analyzer 由外部注入——
/// 供 bench 的 `--analyzer charabia` 对照实验在**索引侧切换分词器**（R4：
/// 索引/查询两侧必须用同一 Analyzer，因此 analyzer 由调用方持有并复用）。
pub(crate) fn build_index(
    path: &Path,
    chunker: &Chunker,
    analyzer: &dyn helix_core::analyze::Analyzer,
) -> Result<Index> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("读取语料失败: {}", path.display()))?;

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
            content_hash: content_hash(text), // 幂等 upsert（FR-15）
        };
        let chunks = chunker.chunk(0, text);
        index.add(doc, chunks, analyzer)?;
        count += 1;
    }

    if count == 0 {
        bail!("语料为空: {}", path.display());
    }
    Ok(index)
}

/// embed 所有分片，返回 (chunk_id, 原始向量)。
pub(crate) fn embed_chunks(
    index: &Index,
    embedder: &LocalEmbedder,
) -> Result<Vec<(ChunkId, Vec<f32>)>> {
    let entries: Vec<(ChunkId, String)> = index
        .live_chunks()
        .map(|c| (c.chunk_id, c.text.clone()))
        .collect();
    let texts: Vec<String> = entries.iter().map(|(_, t)| t.clone()).collect();
    let vecs = embedder.embed_documents(&texts)?;
    Ok(entries
        .into_iter()
        .zip(vecs)
        .map(|((id, _), v)| (id, v))
        .collect())
}

/// 从原始向量重建向量索引（HnswRsIndex，A/B 结论：原生增量）。
fn rebuild_vector_index(vectors: &[(ChunkId, Vec<f32>)]) -> Result<HnswRsIndex> {
    let mut vi = HnswRsIndex::with_capacity(vectors.len().max(1024));
    for (id, v) in vectors {
        vi.add(*id, NormalizedVector::new(v.clone()))?;
    }
    Ok(vi)
}

fn build(args: BuildArgs) -> Result<()> {
    let started = std::time::Instant::now();
    let (index, _) = if args.single_chunk {
        // 评测口径：每段落强制单 chunk（对齐 NFR-01/03 的"1 万 chunk"目标规模）
        load_corpus_with(&args.input, &Chunker::new(200_000, 0))?
    } else {
        load_corpus(&args.input)?
    };
    println!("索引构建完成:");
    println!("  文档数   = {}", index.num_docs());
    println!("  分片数   = {}", index.num_chunks());
    println!("  词项总数 = {}", index.total_len());
    println!("  平均分片 = {:.2}", index.avgdl());

    let Some(out) = args.output else {
        println!("  （未指定 --output，索引仅存在于本次进程）");
        return Ok(());
    };

    // 可选：嵌入并保存向量
    let vectors = if args.vectors {
        let embedder = LocalEmbedder::new()?;
        let t = std::time::Instant::now();
        let v = embed_chunks(&index, &embedder)?;
        println!(
            "  embed {} 条 耗时 {:?}（NFR-03 口径 = embed + 落盘，不含 HNSW）",
            v.len(),
            t.elapsed()
        );
        v
    } else {
        Vec::new()
    };

    storage::save(&out, &index, &vectors)?;
    println!(
        "  快照已写入 {}（{}，{}）耗时 {:?}",
        out.display(),
        if vectors.is_empty() {
            "纯文本"
        } else {
            "含向量"
        },
        humansize(&out),
        started.elapsed()
    );
    Ok(())
}

fn humansize(p: &Path) -> String {
    match std::fs::metadata(p) {
        Ok(m) => {
            let b = m.len();
            if b > 1024 * 1024 {
                format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
            } else if b > 1024 {
                format!("{:.1} KB", b as f64 / 1024.0)
            } else {
                format!("{b} B")
            }
        }
        Err(_) => "未知大小".to_string(),
    }
}

fn search(args: SearchArgs) -> Result<()> {
    let mode = parse_mode(&args.mode)?;
    let filter = parse_filters(&args.filter)?;

    // --index 与 --input 二选一
    let (index, analyzer, snapshot_vectors) = match (&args.index, &args.input) {
        (Some(idx_path), None) => {
            let t = std::time::Instant::now();
            let (index, vectors) = storage::load(idx_path)
                .with_context(|| format!("加载快照失败: {}", idx_path.display()))?;
            eprintln!("[快照加载 {} 耗时 {:?}]", idx_path.display(), t.elapsed());
            (index, MixedAnalyzer::new(), vectors)
        }
        (None, Some(input)) => {
            let (index, analyzer) = load_corpus(input)?;
            (index, analyzer, Vec::new())
        }
        _ => bail!("--index 与 --input 必须二选一"),
    };

    // 向量模式：查询侧向量化必须要 LocalEmbedder；
    // 文档向量优先用快照里的（build 时已 embed），--input 时现场 embed。
    let embedder;
    let vi;
    let mut searcher = Searcher::new(&index, &analyzer);
    if mode != SearchMode::Bm25 {
        embedder = LocalEmbedder::new()?;
        let vectors = if !snapshot_vectors.is_empty() {
            snapshot_vectors
        } else {
            embed_chunks(&index, &embedder)?
        };
        vi = rebuild_vector_index(&vectors)?;
        searcher = searcher.with_vector(&embedder, &vi);
    }

    let resp = searcher.search_filtered(&args.query, mode, args.k, filter.as_ref())?;
    print_response(&resp, &args.query, args.explain);
    Ok(())
}

fn compare(args: CompareArgs) -> Result<()> {
    let (index, analyzer) = load_corpus(&args.input)?;
    let embedder = LocalEmbedder::new()?;
    let vectors = embed_chunks(&index, &embedder)?;
    let vi = rebuild_vector_index(&vectors)?;
    let searcher = Searcher::new(&index, &analyzer).with_vector(&embedder, &vi);

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
