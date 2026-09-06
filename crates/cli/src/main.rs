//! helix —— HelixIndex 的命令行工具。
//!
//! 已实现：
//! - `build`：摄入语料并**落盘快照**（`--vectors` 同时保存向量）
//! - `search --mode {bm25|vector|hybrid}`：`--input` 重建 或 `--index` 从快照加载
//! - `compare`：三路同屏对比
//!
//! P6 起经门面层（`SearchIndex` / `Searcher`）组装；快照加载校验配置指纹（B1）。

mod bench;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use helix_core::chunk::Chunker;
use helix_core::document::{content_hash, DocRecord, Document};
use helix_core::embed::{Embedder, LocalEmbedder};
use helix_core::index::Index;
use helix_core::query::{EmptyReason, Hit, SearchMode, SearchResponse};
use helix_core::schema::Filter;
use helix_core::search::SearchIndex;
use helix_core::types::ChunkId;

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
    /// 不落图 sidecar（V2 Step 2 逃生舱：写库但不持久化 HNSW 图，
    /// 省约 1.6× 磁盘，代价是下次冷启动重建图 —— 磁盘紧张 / 排查图问题用）
    #[arg(long)]
    no_graph_persist: bool,
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
    /// 元数据过滤（`field=value` 等值 / `field>=v` / `field<v` 数值范围；
    /// 逗号分隔或多次出现，语义为 And）。范围语义是 `[下界, 上界)`，
    /// 故只提供 `>=` 与 `<`；需要闭区间上界请写 `< 上界+1`
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

/// 解析过滤条件：`field=value`（等值）/ `field>=v` 或 `field<v`（数值范围）。
///
/// 多个条件用逗号分隔或重复传参，语义为 **And**。
///
/// # 为什么只支持 `>=` 与 `<`
///
/// [`Filter::Range`] 的语义是 **`[gte, lte)`**（下界含、上界不含）。把 `<=v`
/// 静默改写成 `<(v + ε)` 会引入浮点陷阱——ε 的选取没有唯一解（v 是 100 还是
/// 1e-9 差着十几个数量级），改写后边界行为取决于 ε，等于把正确性押在常量上。
/// 因此这里**只暴露与 Range 语义天然对齐的两个运算符**；需要闭区间上界时请
/// 显式写 `< 上界+1`（数值字段）或改用等值。
pub(crate) fn parse_filters(specs: &[String]) -> Result<Option<Filter>> {
    let mut conditions = Vec::new();
    for spec in specs {
        for s in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            conditions.push(parse_one_filter(s)?);
        }
    }
    match conditions.len() {
        0 => Ok(None),
        1 => Ok(conditions.pop()),
        _ => Ok(Some(Filter::And(conditions))),
    }
}

/// 解析单个 `field<op>value` 条件。
fn parse_one_filter(s: &str) -> Result<Filter> {
    // ⚠️ 顺序有讲究，改动前先想清楚：
    //   1. `<=` / `>` 必须在 `<` / `>=` **之前**判掉。否则 `score<=100` 会被
    //      `split_once('<')` 切成 ("score", "=100")，报 "invalid float literal" ——
    //      错误能被捕获，但信息完全误导（用户看不出是运算符不支持）。
    //   2. `>=` 必须在 `>` 之后无关，但必须在 `=` 之前（`>=` 含 `=`）。
    if s.contains("<=") || s.contains('>') && !s.contains(">=") {
        bail!(
            "不支持的运算符：{s:?}。Filter::Range 语义是 [gte, lte)，\
             只能用 `>=` 与 `<`；需要闭区间上界请写 `< 上界+1`"
        );
    }
    if let Some((f, v)) = s.split_once(">=") {
        let gte: f64 = v
            .trim()
            .parse()
            .with_context(|| format!("范围下界不是数值: {s:?}"))?;
        return Ok(Filter::Range {
            field: f.trim().to_string(),
            gte,
            lte: f64::INFINITY,
        });
    }
    if let Some((f, v)) = s.split_once('<') {
        if v.trim().is_empty() {
            bail!("过滤条件缺少上界: {s:?}");
        }
        let lte: f64 = v
            .trim()
            .parse()
            .with_context(|| format!("范围上界不是数值: {s:?}"))?;
        return Ok(Filter::Range {
            field: f.trim().to_string(),
            gte: f64::NEG_INFINITY,
            lte,
        });
    }
    match s.split_once('=') {
        Some((f, v)) => Ok(Filter::eq(f.trim(), v.trim())),
        None => bail!("过滤条件格式应为 field=value / field>=v / field<v，收到 {s:?}"),
    }
}

/// 底层构建：用调用方提供的 analyzer 从 JSONL 构建索引。
///
/// 供 bench 的 `--analyzer charabia` 对照实验在**索引侧切换分词器**（R4：
/// 索引/查询两侧必须用同一 Analyzer，因此 analyzer 由调用方持有并复用）。
/// （I-10 后 main 三命令走门面层，此函数仅 bench.rs 使用；I-12 迁 bench 后移除。）
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

        let doc = DocRecord {
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
/// （I-10 后仅 bench.rs 使用；I-12 迁 bench 后移除。）
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

fn build(args: BuildArgs) -> Result<()> {
    let started = std::time::Instant::now();

    // 门面层装配（p6-design 4.1）：默认 MixedAnalyzer + bge-small-zh + HNSW；
    // --single-chunk 切评测口径 chunker；--vectors 决定是否配 embedder。
    let chunker = if args.single_chunk {
        Chunker::new(200_000, 0)
    } else {
        Chunker::default()
    };
    let mut builder = SearchIndex::builder().chunker(chunker);
    if !args.vectors {
        builder = builder.embedder(None);
    }
    // V2 Step 2：图持久化逃生舱（磁盘紧张 / 排查图问题时不落图）
    if args.no_graph_persist {
        builder = builder.without_graph_persist();
    }
    let mut index = builder.build();

    // 读语料 → 批量摄入（倒排 + 写缓冲；向量延后到 commit）
    let docs = read_corpus_documents(&args.input)?;
    index.add_documents(docs)?;

    println!("索引构建完成:");
    println!("  文档数   = {}", index.num_docs());
    println!("  分片数   = {}", index.num_chunks());
    println!("  词项总数 = {}", index.total_len());
    println!("  平均分片 = {:.2}", index.avgdl());

    let Some(out) = args.output else {
        println!("  （未指定 --output，索引仅存在于本次进程）");
        return Ok(());
    };

    // 向量嵌入：--vectors 时 commit 冲刷残余缓冲；embed 实际分散在 add_documents
    // 的自动 flush 里，故耗时取门面层累计值（而非此处 commit 计时，否则只测到最后一批）
    if args.vectors {
        index.commit()?;
        println!(
            "  embed {} 条 耗时 {:?}（NFR-03 口径 = embed，不含 HNSW / 落盘）",
            index.num_chunks(),
            index.embed_elapsed()
        );
    }

    let t_save = std::time::Instant::now();
    index.save(&out)?;
    println!(
        "  快照已写入 {}（{}，{}）耗时 {:?}",
        out.display(),
        if args.vectors {
            "含向量"
        } else {
            "纯文本"
        },
        humansize(&out),
        t_save.elapsed()
    );
    println!(
        "  总耗时 {:?}（含 embed / 落盘 / 图 dump）",
        started.elapsed()
    );

    // V2 Step 2：图 sidecar 落盘观测（S2-08；体积增量 / 点数 / dump 耗时，验收 7 数据来源）
    if let Some(dump) = index.graph_dump_elapsed() {
        println!("  图 sidecar dump 耗时 {dump:?}（纯落盘，不含 HNSW 建图）");
    }
    if args.vectors && !args.no_graph_persist {
        let paths = helix_core::storage::graph_paths(&out);
        if let Ok(Some(m)) = helix_core::storage::read_manifest(&paths.manifest) {
            let graph_bytes = m.graph_len + m.data_len;
            println!(
                "  图 sidecar 落盘 {} 点 / {:.1} MB（graph {:.1} MB + data {:.1} MB；\
                 快照 {:.1} MB，磁盘增量 {:.1}×）",
                m.nb_point,
                graph_bytes as f64 / (1024.0 * 1024.0),
                m.graph_len as f64 / (1024.0 * 1024.0),
                m.data_len as f64 / (1024.0 * 1024.0),
                m.snapshot_len as f64 / (1024.0 * 1024.0),
                (m.snapshot_len + graph_bytes) as f64 / m.snapshot_len.max(1) as f64,
            );
        } else {
            println!(
                "  图 sidecar 未落盘（纯 BM25 / Brute 后端，或落盘失败——见上方警告；\
                 下次加载将走降级重建）"
            );
        }
    }
    Ok(())
}

/// 从 JSONL 语料读取为 `Document` 输入 DTO（build / search --input / compare 共用）。
fn read_corpus_documents(path: &Path) -> Result<Vec<Document>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("读取语料失败: {}", path.display()))?;

    let mut docs = Vec::new();
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
        docs.push(
            Document::new(text)
                .with_source(source)
                .with_metadata(metadata),
        );
    }

    if docs.is_empty() {
        bail!("语料为空: {}", path.display());
    }
    Ok(docs)
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

    // --index 与 --input 二选一，经门面层组装
    let searcher = match (&args.index, &args.input) {
        (Some(idx_path), None) => {
            let t = std::time::Instant::now();
            // 门面层 load（默认装配 + 配置指纹校验，修 B1：不再写死 MixedAnalyzer）
            let index = SearchIndex::load(idx_path)
                .with_context(|| format!("加载快照失败: {}", idx_path.display()))?;
            eprintln!("[快照加载 {} 耗时 {:?}]", idx_path.display(), t.elapsed());
            // V2 Step 2：图 sidecar 状态（NFR-07 —— 降级不能静默，必须显式可见）
            match index.graph_status() {
                helix_core::search::GraphStatus::Loaded => {
                    eprintln!("[向量图加载：持久化图（快路径）]")
                }
                helix_core::search::GraphStatus::Rebuilt(reason) => eprintln!(
                    "[向量图加载：⚠️ 降级重建（原因：{reason}）—— 冷启动会变慢，\
                     重建耗时已计入上方「快照加载」]"
                ),
                helix_core::search::GraphStatus::NotApplicable => {
                    eprintln!("[向量图加载：不适用（Brute 后端 / 纯 BM25 / 已关闭持久化）]")
                }
            }
            index.into_searcher()?
        }
        (None, Some(input)) => {
            // 现场建库（默认装配：MixedAnalyzer + bge + HNSW，向量模式可用）
            let mut index = SearchIndex::builder().build();
            let docs = read_corpus_documents(input)?;
            index.add_documents(docs)?;
            index.into_searcher()?
        }
        _ => bail!("--index 与 --input 必须二选一"),
    };

    // 检索（builder 承载 mode / top_n / filter）
    let mut req = searcher.search_with(&args.query).mode(mode).top_n(args.k);
    if let Some(f) = filter.as_ref() {
        req = req.filter(f);
    }
    let resp = req.exec()?;
    print_response(&resp, &args.query, args.explain);
    Ok(())
}

fn compare(args: CompareArgs) -> Result<()> {
    // 门面层默认装配（含向量），一次建库三路同屏
    let mut index = SearchIndex::builder().build();
    let docs = read_corpus_documents(&args.input)?;
    index.add_documents(docs)?;
    let searcher = index.into_searcher()?;

    println!("查询: {}\n", args.query);
    println!("=== BM25 ===");
    print_hits(
        &searcher
            .search_with(&args.query)
            .mode(SearchMode::Bm25)
            .top_n(args.k)
            .exec()?
            .hits,
    );
    println!("\n=== Vector ===");
    print_hits(
        &searcher
            .search_with(&args.query)
            .mode(SearchMode::Vector)
            .top_n(args.k)
            .exec()?
            .hits,
    );
    println!("\n=== Hybrid (RRF) ===");
    print_hits(
        &searcher
            .search_with(&args.query)
            .mode(SearchMode::Hybrid)
            .top_n(args.k)
            .exec()?
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

#[cfg(test)]
mod tests {
    //! `--filter` 的解析是 CLI 侧少数有真实分支逻辑的地方，且有**顺序依赖**
    //! （`<=` / `>` 必须在 `<` / `>=` 之前判掉，否则报错信息会误导），
    //! 因此在这里钉住。CI 的 smoke 只覆盖到「跑通不报错」。

    use super::{parse_filters, parse_one_filter};
    use helix_core::schema::Filter;

    #[test]
    fn 等值过滤() {
        assert!(matches!(
            parse_one_filter("topic=vector").unwrap(),
            Filter::Eq { ref field, ref value } if field == "topic" && value == "vector"
        ));
    }

    #[test]
    fn 范围过滤_半开区间() {
        // Filter::Range 语义是 [gte, lte)，只开半边时另一端取无穷
        let open_lo = parse_one_filter("score<100").unwrap();
        assert!(matches!(
            open_lo,
            Filter::Range { ref field, gte, lte } if field == "score" && gte == f64::NEG_INFINITY && lte == 100.0
        ));
        let open_hi = parse_one_filter("score>=0").unwrap();
        assert!(matches!(
            open_hi,
            Filter::Range { ref field, gte, lte } if field == "score" && gte == 0.0 && lte == f64::INFINITY
        ));
    }

    #[test]
    fn 支持的组合会合成_and() {
        let f = parse_filters(&["score>=0".to_string(), "score<100".to_string()])
            .unwrap()
            .expect("两个条件应合成一个 Filter");
        assert!(matches!(f, Filter::And(ref v) if v.len() == 2));
    }

    #[test]
    fn 逗号分隔与多次传参等价() {
        let a = parse_filters(&["topic=vector,lang=zh".to_string()]).unwrap();
        let b = parse_filters(&["topic=vector".to_string(), "lang=zh".to_string()]).unwrap();
        assert!(matches!(a, Some(Filter::And(ref v)) if v.len() == 2));
        assert!(matches!(b, Some(Filter::And(ref v)) if v.len() == 2));
    }

    #[test]
    fn 空输入是不过滤() {
        assert!(parse_filters(&[]).unwrap().is_none());
        assert!(parse_filters(&["".to_string()]).unwrap().is_none());
    }

    /// ⚠️ 顺序敏感的回归测试：`<=` 若被 `split_once('<')` 截走，会切成
    /// ("score", "=100") 然后报 "invalid float literal" —— 错误能被捕获，
    /// 但用户看不出是**运算符不支持**。这里断言错误信息指向运算符。
    #[test]
    fn 不支持的运算符必须明确指出而非报数值解析失败() {
        for spec in ["score<=100", "score>5"] {
            let err = parse_one_filter(spec).unwrap_err().to_string();
            assert!(
                err.contains("不支持的运算符"),
                "{spec:?} 应明确提示运算符不支持，实际报错: {err}"
            );
        }
    }

    #[test]
    fn 缺上界与非法数值会报错() {
        assert!(parse_one_filter("score<").is_err());
        assert!(parse_one_filter("score>=abc").is_err());
        assert!(parse_one_filter("score").is_err());
    }
}
