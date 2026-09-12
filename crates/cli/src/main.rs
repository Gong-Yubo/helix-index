//! helix —— HelixIndex 的命令行工具。
//!
//! 已实现：
//! - `build`：摄入语料并**落盘快照**（`--vectors` 同时保存向量）；
//!   `--index <既有快照> --input <delta>` 走**增量追加**（V2 Step 6 / T7-11 / FR-28）
//! - `search --mode {bm25|vector|hybrid}`：`--input` 重建 或 `--index` 从快照加载
//! - `compare`：三路同屏对比
//! - `compact`（V2 Step 4 / S4-08）：墓碑物理回收，`--dry-run` 只读预览
//!
//! P6 起经门面层（`SearchIndex` / `Searcher`）组装；快照加载校验配置指纹（B1）。

mod bench;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use helix_core::chunk::Chunker;
use helix_core::document::{content_hash, DocRecord, Document};
use helix_core::embed::{Embedder, LocalEmbedder};
use helix_core::index::Index;
use helix_core::query::{EmptyReason, Hit, SearchMode, SearchResponse};
use helix_core::schema::Filter;
use helix_core::search::{required_local_embedder, GraphStatus, SearchIndex, SearchIndexBuilder};
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
    //
    // `override_usage`：缺参时 clap 把**可选**的 `--output` 与必填项并列进 usage
    // （`helix build --output <OUTPUT> --input <INPUT>`），读起来像两个都必须给，
    // 与 `required_unless_present` 想表达的语义正好相反（PR #43 评审 P3-4）。
    // ⚠️ 用普通注释而不是 `///`：doc comment 会被 clap 当用户可见的帮助文本渲染。
    #[command(override_usage = "helix build [OPTIONS] (--input <INPUT> | --index <INDEX>)")]
    Build(BuildArgs),
    /// 检索（--mode bm25 / vector / hybrid；--input 重建 或 --index 快照）
    Search(SearchArgs),
    /// 三种模式同屏对比（调试主入口）
    Compare(CompareArgs),
    /// 墓碑物理回收（V2 Step 4：compaction 重新物化 + ID 重编号）
    Compact(CompactArgs),
    /// 效果评测（P5 实现）
    Bench(bench::BenchArgs),
}

#[derive(clap::Args)]
struct BuildArgs {
    /// 语料文件（JSONL：每行 {"source": "...", "text": "..."}）。
    ///
    /// - 只给 `--input`：**全量构建**（V1 起的既有语义）
    /// - 同时给 `--index`：**追加**（把本文件的文档追加进既有快照）
    #[arg(short, long, required_unless_present = "index")]
    input: Option<PathBuf>,
    /// 既有快照：与 `--input` 同时给出 = **追加**；单独给出 = 仅加载后重存（往返诊断）。
    ///
    /// ⚠️ 追加要求**与建库时相同的装配**（`--single-chunk` / `--vectors`）——
    /// 否则配置指纹校验会报 `ConfigMismatch`，绝不静默换分词器/模型（修 B1/B2）。
    #[arg(long)]
    index: Option<PathBuf>,
    /// 输出快照路径。全量构建缺省时只在内存（打印提示）；
    /// **追加时缺省 = 原地覆盖 `--index`**（沿用 `helix compact` 的先例）
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// 同时嵌入并保存向量（vector/hybrid 检索需要；首次会下载模型）。
    ///
    /// 这是**显式请求向量**：拿不到本地 embedder 会**报错**而非静默退化为纯 BM25
    /// （V2 Step 6 / S6-09 / D-S6-05 方案 A）。
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
    /// 打印一行内核指标（V2 Step 5 / NFR-07「用户可自查」）。
    ///
    /// 默认关：它是**诊断**输出，不是检索结果的一部分。内容 = 向量路实际走的路由
    /// （`None`/`Ann`/`Exact`）、缺口、per-lane 耗时——低选择度档位靠它区分
    /// 「兜底生效了」与「过滤求值本身贵」。
    #[arg(long)]
    metrics: bool,
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
struct CompactArgs {
    /// 要 compact 的快照路径
    #[arg(long)]
    index: PathBuf,
    /// 另存到新路径（不写则**原地**覆盖 `--index`；用于 A/B 对比体积）
    #[arg(long)]
    output: Option<PathBuf>,
    /// 只打印墓碑统计与预估回收，**不写任何文件**（只读）
    #[arg(long)]
    dry_run: bool,
    /// 机器可读 JSON 输出（A/B 脚本透传）
    #[arg(long)]
    json: bool,
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
        Command::Compact(args) => compact(args),
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

/// 按 `--vectors` / `--single-chunk` / `--no-graph-persist` 装配门面层 builder。
///
/// `--vectors` 走**显式请求**语义（S6-09 / D-S6-05 方案 A）：要向量就必须给向量，
/// 拿不到本地 embedder 直接报错。不给时显式装 `embedder(None)`（纯 BM25）——
/// 这样「用户没要向量」与「要了但拿不到」是两条不同的路径，不会互相掩盖。
fn configured_builder(args: &BuildArgs) -> Result<SearchIndexBuilder> {
    // 门面层装配（p6-design 4.1）：默认 MixedAnalyzer + bge-small-zh + HNSW；
    // --single-chunk 切评测口径 chunker；--vectors 决定是否配 embedder。
    let chunker = if args.single_chunk {
        Chunker::new(200_000, 0)
    } else {
        Chunker::default()
    };
    let mut builder = SearchIndex::builder()
        .chunker(chunker)
        .embedder(if args.vectors {
            Some(require_local_embedder()?)
        } else {
            None
        });
    // V2 Step 2：图持久化逃生舱（磁盘紧张 / 排查图问题时不落图）
    if args.no_graph_persist {
        builder = builder.without_graph_persist();
    }
    Ok(builder)
}

/// 显式请求向量时的 embedder 解析（S6-09 / D-S6-05 方案 A）。
///
/// 与 Step 2 的 `GraphStatus` 同一条纪律：**降级必须显式可见**。
/// 「要了向量却拿到纯 BM25」不会报错、只是召回悄悄变差，用户往往在结果不对时才
/// 发现——所以这里上抛 `Err` 并给出可执行的提示。
///
/// ⚠️ 用 core 的 [`required_local_embedder`]（返回 `Result<Arc<dyn Embedder>>`）而不是
/// `resolve_embedder(.., ..).expect(..)`：后者会引入一个**不可达却无测试覆盖的 panic 分支**
/// （PR #43 评审 P3-5），且把「策略」缝暴露成公开 API。
fn require_local_embedder() -> Result<Arc<dyn Embedder>> {
    required_local_embedder().context(
        "--vectors 需要本地 embedder，但初始化失败（要向量却拿不到向量）；\
         若只想要 BM25 检索请去掉 --vectors",
    )
}

fn build(args: BuildArgs) -> Result<()> {
    let started = std::time::Instant::now();
    let builder = configured_builder(&args)?;

    match (&args.index, args.input.as_deref()) {
        // --index [+ --input]：追加 / 重存（V2 Step 6 / T7-11 / FR-28）
        (Some(idx), delta) => build_into_existing(&args, builder, idx, delta, started),
        // --input：全量构建（V1 起的既有语义）
        (None, Some(input)) => {
            let mut index = builder.build();
            // 读语料 → 批量摄入（倒排 + 写缓冲；向量延后到 commit）
            let docs = read_corpus_documents(input)?;
            index.add_documents(docs)?;

            println!("索引构建完成:");
            print_index_stats(&index);

            let Some(out) = args.output.as_deref() else {
                println!("  （未指定 --output，索引仅存在于本次进程）");
                return Ok(());
            };
            save_and_report(&args, &mut index, out, started)
        }
        // clap 的 `required_unless_present` 已挡住这一支，这里只做防御
        (None, None) => bail!("--input 与 --index 至少给出一个"),
    }
}

/// 追加构建（V2 Step 6 / T7-11 / FR-28）：加载既有快照 → 追加 delta → 落盘。
///
/// - `--index` + `--input` = **追加**；`--index` 单独给出 = 仅加载后重存（往返诊断）。
/// - `--output` 缺省时**原地覆盖 `--index`**（沿用 `helix compact` 的先例）。
/// - **幂等**：`--input` 中已存在的文档（`content_hash` 命中）被跳过，
///   同一 delta 重复追加不改变文档数/分片数/词项总数（FR-15 + FR-28）。
/// - **增量收益来自 `load` 复用快照里的 `raw_vectors` + 图**，**不是**查重：
///   `load` 把已持久化的向量直接灌回向量索引、**根本不调用 `embed_documents`**，
///   于是只有 delta 需要推理（实测 embed 217.35 s → 21.41 s，`eval-report.md` §8.11）。
///   查重（`content_hashes` / `doc_id_by_hash`）保证的是**重复追加的幂等**（S6-T3），
///   是另一个性质 —— 实测那次 `去重跳过 0` 恰好说明收益与查重无关。
///   ⚠️ 别顺着「查重」去优化增量路径，收益不会动（PR #43 评审 P3-2）。
///
/// ⚠️ **不是 upsert-by-source**：同一 `source` 内容变了就是**一篇新文档**，旧文档仍在。
/// 需要替换语义请显式 `remove` 后再 `compact`（见设计 §4.5.5）。
///
/// - ⚠️ **带 `--vectors` 追加/重存到「无向量快照」时硬失败**（PR #43 评审 P1）：
///   `load` 的 embedder 校验是**非对称**的（允许「快照无向量 + 装配有 embedder」作为
///   升级路径，见 `index.rs` 的 `embedder_ok`），故这一步**能**走到 `add_documents`；
///   但那样会**落盘**一个自称含向量、实际只有新增部分有向量的**半向量快照**
///   （指纹还会被改写成「含向量」且**不可逆**，下游 `compact --dry-run` 会把它当健康态）。
///   `--mode vector` 则静默只回答 delta 那部分语料 —— 与 `search --index` **不同构**
///   （那只是只读的、进程结束即消失，不污染磁盘）。
///   ⇒ 在 `add_documents` **之前**判「装配有向量、快照无向量」并 `bail!`。
///   ⚠️ **不加放行开关**：D-S6-05 已按评审 Q6 拍板「不额外加兼容开关」。
fn build_into_existing(
    args: &BuildArgs,
    builder: SearchIndexBuilder,
    idx_path: &Path,
    delta: Option<&Path>,
    started: std::time::Instant,
) -> Result<()> {
    let t_load = std::time::Instant::now();
    let mut index = builder.load(idx_path).with_context(|| {
        format!(
            "加载既有快照失败（追加要求与建库时相同的装配，如 --single-chunk / --vectors）: {}",
            idx_path.display()
        )
    })?;
    let load_ms = t_load.elapsed();

    println!("增量构建: {}", idx_path.display());
    println!("  加载快照耗时 {load_ms:?}");
    report_graph_status(&index);

    // ⚠️ P1（PR #43 评审）：`--vectors` 要向量，但快照里的**存活分片没有对应向量**
    // ⇒ 这是「半向量」状态。静默继续会落盘一个自称含向量、实际只有新增部分有向量的快照
    // （指纹被改写成「含向量」且**不可逆**），`--mode vector` 只答新增那部分语料。
    //
    // 判据就地取自 `tombstone_stats()`（不新增公开 API）：健康的向量快照恒有
    // `raw_vectors == chunks_alive`（`remove` 会同步 retain 掉对应向量），`<` 即
    // 「有存活分片缺向量」。放在 `add_documents` **之前** —— 落盘之后再告警就晚了；
    // 也因此覆盖 `--index` 单独给出的「仅重存」路径（那条同样会改写指纹）。
    if args.vectors {
        let st = index.tombstone_stats();
        if st.raw_vectors < st.chunks_alive {
            bail!(
                "快照里的存活分片没有向量（raw_vectors {} < chunks_alive {}）：\
                 带 --vectors 追加/重存会落盘一个「半向量」快照——\n\
                 · `--mode vector` 只召回本次新增的文档，老文档静默缺席；\n\
                 · 快照指纹会被改写成「含向量」且不可逆，之后不带 --vectors 的装配会被 ConfigMismatch 拒绝。\n\
                 要覆盖全集的向量索引，请从语料**全量重建**；只想要 BM25 增量，请去掉 --vectors。",
                st.raw_vectors,
                st.chunks_alive
            );
        }
    }

    let (before_docs, before_chunks) = (index.num_docs(), index.num_chunks());

    match delta {
        Some(p) => {
            let docs = read_corpus_documents(p)?;
            let total = docs.len();
            let outcomes = index.add_documents(docs)?;
            let deduped = outcomes.iter().filter(|o| o.deduped).count();
            // NFR-11：commit 后新增内容立即可查；写延迟 = 本批 flush 耗时
            index.commit()?;
            println!(
                "  追加 {} 篇：新增 {} / 去重跳过 {}",
                total,
                total - deduped,
                deduped
            );
        }
        None => println!("  （未指定 --input：仅加载后重存，用于往返诊断）"),
    }

    print_index_stats(&index);
    println!(
        "  变化: 文档 {} → {} ｜ 分片 {} → {}",
        before_docs,
        index.num_docs(),
        before_chunks,
        index.num_chunks()
    );

    let out = args.output.as_deref().unwrap_or(idx_path);
    if args.output.is_none() {
        println!("  （未指定 --output：原地覆盖 {}）", idx_path.display());
    }
    save_and_report(args, &mut index, out, started)
}

/// 打印索引四项计数（全量与追加共用）。
fn print_index_stats(index: &SearchIndex) {
    println!("  文档数   = {}", index.num_docs());
    println!("  分片数   = {}", index.num_chunks());
    println!("  词项总数 = {}", index.total_len());
    println!("  平均分片 = {:.2}", index.avgdl());
}

/// 落盘 + 打印（全量与追加共用的收尾）。
///
/// 开头显式 `commit()`：`save` 内部虽也 `commit`，但 `embed_count` / `embed_elapsed`
/// 是**累计量**，残余 `pending`（不足 `batch_size` 的尾批）必须在此处 flush 才会累加进去，
/// 否则 embed 读数会**少算最后一批** —— 口径类 bug 里最难发现的那一种。
/// ⚠️ 本函数已**不读** `num_chunks`（旧注释曾以此为理由）；别据此判定这次 `commit()`
/// 与 `save` 内部那次重复而删掉它（PR #43 评审 P3-1）。
fn save_and_report(
    args: &BuildArgs,
    index: &mut SearchIndex,
    out: &Path,
    started: std::time::Instant,
) -> Result<()> {
    index.commit()?;

    // 向量嵌入：embed 实际分散在 add_documents 的自动 flush 里，故耗时取门面层累计值
    //（而非在此处计时，那样只测到最后一批）
    if args.vectors {
        println!(
            "  embed {} 条 耗时 {:?}（NFR-03 口径 = embed，不含 HNSW / 落盘；\
             增量构建时这里是**本次新增**条数，不是索引总量）",
            index.embed_count(),
            index.embed_elapsed()
        );
    }

    let t_save = std::time::Instant::now();
    index.save(out)?;
    println!(
        "  快照已写入 {}（{}，{}）耗时 {:?}",
        out.display(),
        if args.vectors {
            "含向量"
        } else {
            "纯文本"
        },
        humansize(out),
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
        let paths = helix_core::storage::graph_paths(out);
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

/// 打印向量图 sidecar 状态（NFR-07：降级必须显式可见，不能只留日志）。
fn report_graph_status(index: &SearchIndex) {
    match index.graph_status() {
        GraphStatus::Loaded => eprintln!("[向量图加载：持久化图（快路径）]"),
        GraphStatus::Rebuilt(reason) => eprintln!(
            "[向量图加载：⚠️ 降级重建（原因：{reason}）—— 冷启动会变慢\
             （图是派生缓存，丢弃不影响正确性）]"
        ),
        GraphStatus::NotApplicable => {
            eprintln!("[向量图加载：不适用（Brute 后端 / 纯 BM25 / 已关闭持久化）]")
        }
        GraphStatus::PersistFailed(reason) => {
            eprintln!("[向量图落盘：⚠️ 失败（原因：{reason}）—— 快照本身完好，下次加载会重建图]")
        }
    }
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
            report_graph_status(&index);
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
    if args.metrics {
        print_kernel_metrics(&resp);
    }
    Ok(())
}

/// 打印一行内核指标（V2 Step 5 / NFR-07）。
///
/// `route` 是本次检索**实际**走的向量路径——没有它，"兜底是否生效"只能靠延迟反推。
/// `缺口` 走精确路径时**通常**为 0，但那不是恒等式（`allowed` 来自 `Index`、扫描枚举
/// 的是图中的点）：`Exact` + 缺口 > 0 反过来是「图未覆盖全部 allowed」的诊断信号。
/// 两种读数都必须**连看** `route`（设计 §4.4 推论 1）。
fn print_kernel_metrics(resp: &SearchResponse) {
    let m = &resp.metrics;
    println!(
        "\n[内核指标] route={:?} allowed={} bm25={} vector={} candidates={} 缺口={} | \
         filter_eval={:.3}ms bm25={:.3}ms vector={:.3}ms | took={:.3}ms",
        m.vector_route,
        m.allowed,
        m.bm25,
        m.vector,
        m.candidates,
        m.vector_shortfall,
        m.filter_eval.as_secs_f64() * 1000.0,
        m.bm25_elapsed.as_secs_f64() * 1000.0,
        m.vector_elapsed.as_secs_f64() * 1000.0,
        m.took.as_secs_f64() * 1000.0,
    );
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

/// V2 Step 4 / S4-08：墓碑物理回收（compaction 重新物化 + ID 重编号）。
///
/// - 默认装配 `SearchIndex::load`（配置指纹校验）⇒ 含向量快照需匹配 embedder；
///   load 会加载模型（首次下载几秒），但**不**调 embed（§4.8 已知代价）。
/// - 默认**原地**写 `--index`（`atomic_write` 崩溃安全）；`--output` 另存。
/// - 体积对比恒报「源 `--index` → 结果」两态：原地时源 = 覆盖前，`--output` 时
///   源/新分属两文件，`bytes_before`（对目标路径 stat）此时恒 null，回收看
///   `bytes_source` vs `bytes_after`（A/B）。
/// - `--dry-run` 只读：打印墓碑统计与预估回收，不写任何文件。
/// - ⚠️ compaction 会重编号 `doc_id` / `chunk_id`（D-S4-01）——跨 compact 的持久
///   引用请用 `source` / `content_hash`（rustdoc 亦已注明）。
fn compact(args: CompactArgs) -> Result<()> {
    let t0 = std::time::Instant::now();
    let idx_path = &args.index;
    let out_path = args.output.as_ref().unwrap_or(idx_path);

    // load（门面层默认装配；指纹不符或纯 BM25 快照由装配自动降级处理）
    let mut index = SearchIndex::load(idx_path).with_context(|| {
        format!(
            "加载快照失败（可能需与建库相同的 embedder）: {}",
            idx_path.display()
        )
    })?;
    let load_ms = t0.elapsed().as_millis();

    // 源索引三体积（A/B 判据的核心「原」）。须在 compact 落盘**前**采：
    // 原地模式（--output 缺省）下 compact 会覆盖 idx_path，事后采到的是新体积。
    let source_bytes = on_disk_sizes(idx_path);

    if args.dry_run {
        // 只读预览：墓碑统计 + 磁盘三体积 + 预估回收，不写任何文件
        let stats = index.tombstone_stats();
        let sizes = on_disk_sizes(idx_path);
        if args.json {
            println!(
                "{}",
                serde_json::json!({
                    "dry_run": true,
                    "chunks_total": stats.chunks_total,
                    "chunks_alive": stats.chunks_alive,
                    "docs_total": stats.docs_total,
                    "docs_alive": stats.docs_alive,
                    "graph_points": stats.graph_points,
                    "raw_vectors": stats.raw_vectors,
                    "tombstone_ratio": stats.tombstone_ratio,
                    "reclaimable_chunks": stats.chunks_total - stats.chunks_alive,
                    "reclaimable_docs": stats.docs_total - stats.docs_alive,
                    "bytes": {
                        "snapshot": sizes.0,
                        "graph": sizes.1,
                        "data": sizes.2,
                    },
                })
            );
        } else {
            println!(
                "墓碑统计（{}，--dry-run 只读，不写文件）:",
                idx_path.display()
            );
            print_tombstone(&stats);
            println!(
                "磁盘三体积: 快照 {} / graph {} / data {}",
                humansize_bytes(sizes.0),
                humansize_bytes(sizes.1),
                humansize_bytes(sizes.2)
            );
            println!(
                "预估可回收: {} 墓碑 chunk / {} 墓碑 doc / 图中 {} 墓碑点",
                stats.chunks_total - stats.chunks_alive,
                stats.docs_total - stats.docs_alive,
                stats.graph_points.saturating_sub(stats.chunks_alive),
            );
            if stats.chunks_total > stats.chunks_alive {
                println!(
                    "提示: 运行 `helix compact --index {}` 执行回收（无 --dry-run）",
                    idx_path.display()
                );
            } else {
                println!("无墓碑，无需 compact。");
            }
        }
        return Ok(());
    }

    // 真正回收：compact_and_save 到目标路径（原地 or 另存；atomic_write 崩溃安全）
    let rep = index
        .compact_and_save(out_path)
        .with_context(|| format!("compaction 失败: {}", out_path.display()))?;

    if args.json {
        let before = rep.before;
        let after = rep.after;
        println!(
            "{}",
            serde_json::json!({
                "remapped": rep.remapped,
                "before": {
                    "chunks_total": before.chunks_total,
                    "chunks_alive": before.chunks_alive,
                    "docs_total": before.docs_total,
                    "docs_alive": before.docs_alive,
                    "graph_points": before.graph_points,
                    "raw_vectors": before.raw_vectors,
                },
                "after": {
                    "chunks_total": after.chunks_total,
                    "chunks_alive": after.chunks_alive,
                    "docs_total": after.docs_total,
                    "docs_alive": after.docs_alive,
                    "graph_points": after.graph_points,
                    "raw_vectors": after.raw_vectors,
                },
                "reclaimed_chunks": rep.reclaimed_chunks,
                "reclaimed_docs": rep.reclaimed_docs,
                "reclaimed_terms": rep.reclaimed_terms,
                "reclaimed_graph_points": rep.reclaimed_graph_points,
                // A/B 体积判据：`bytes_source` 恒为 compact **前**的源 `--index` 三体积
                //（原地 / --output 都准确）；`bytes_before`/`bytes_after` 是 core 对
                // **目标路径**落盘前后的诚实 stat——`--output` 另存时目标文件原本
                // 不存在故 `bytes_before` 为 null，看回收请以 `bytes_source` 对比。
                "bytes_source": size_json(Some(to_size_bytes(source_bytes))),
                "bytes_before": size_json(rep.bytes_before),
                "bytes_after": size_json(rep.bytes_after),
                "vector_rebuild_ms": rep.vector_rebuild_ms,
                // 口径分开：`load_ms` 含模型/指纹加载；`compact_ms` = core 的
                // CompactionReport.total_ms（纯 compact+save，不含 load）。
                "load_ms": load_ms,
                "compact_ms": rep.total_ms,
                "graph_status": graph_status_label(&rep.graph_status),
            })
        );
        return Ok(());
    }

    // 人类可读输出：体积对比恒打「源索引 → 结果」两态（A/B 需要的正是这条）。
    // 原地模式 source_bytes == 落盘前 idx_path 体积；--output 模式则分源/新两文件。
    println!("墓碑物理回收: {}", idx_path.display());
    print!("  before: ");
    print_tombstone(&rep.before);
    print!("  after : ");
    print_tombstone(&rep.after);
    println!(
        "  回收: {} chunk / {} doc / {} term / 图中 {} 墓碑点",
        rep.reclaimed_chunks, rep.reclaimed_docs, rep.reclaimed_terms, rep.reclaimed_graph_points
    );
    if let Some(a) = rep.bytes_after {
        println!(
            "  体积(源→新): 快照 {} → {}；graph {} → {}；data {} → {}",
            humansize_bytes(source_bytes.0),
            humansize_bytes(a.snapshot),
            humansize_bytes(source_bytes.1),
            humansize_bytes(a.graph),
            humansize_bytes(source_bytes.2),
            humansize_bytes(a.data),
        );
        if args.output.is_some() {
            println!(
                "    （--output 另存：上表「源」是原 --index 文件体积，结果写入 {}）",
                out_path.display()
            );
        }
    } else {
        println!(
            "  体积: 源快照 {}（落盘后体积暂不可读）",
            humansize_bytes(source_bytes.0)
        );
    }
    println!(
        "  加载 {:.2}s；compact+save {:.2}s；图重建 {} ms",
        load_ms as f64 / 1000.0,
        rep.total_ms as f64 / 1000.0,
        rep.vector_rebuild_ms
    );
    println!("  graph_status = {}", graph_status_label(&rep.graph_status));
    if rep.remapped {
        println!("  ⚠️ 已重编号 doc_id/chunk_id（D-S4-01）：跨 compaction 的持久引用请用 source/content_hash");
    } else {
        println!("  无墓碑，本次为 no-op（ID 未变）");
    }
    Ok(())
}

fn print_tombstone(s: &helix_core::search::TombstoneStats) {
    println!(
        "chunks {}/{} ｜ docs {}/{} ｜ 图 {} 点 ｜ raw {} 条 ｜ 墓碑占比 {:.1}%",
        s.chunks_alive,
        s.chunks_total,
        s.docs_alive,
        s.docs_total,
        s.graph_points,
        s.raw_vectors,
        s.tombstone_ratio * 100.0
    );
}

fn humansize_bytes(b: u64) -> String {
    if b > 1024 * 1024 {
        format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
    } else if b > 1024 {
        format!("{:.1} KB", b as f64 / 1024.0)
    } else {
        format!("{b} B")
    }
}

fn size_json(s: Option<helix_core::search::SizeBytes>) -> serde_json::Value {
    match s {
        Some(x) => serde_json::json!({"snapshot": x.snapshot, "graph": x.graph, "data": x.data}),
        None => serde_json::Value::Null,
    }
}

/// 磁盘三体积元组 `(snapshot, graph, data)` → core 的 `SizeBytes`（JSON 序列化用）。
fn to_size_bytes((snapshot, graph, data): (u64, u64, u64)) -> helix_core::search::SizeBytes {
    helix_core::search::SizeBytes {
        snapshot,
        graph,
        data,
    }
}

fn graph_status_label(s: &GraphStatus) -> &'static str {
    match s {
        GraphStatus::Loaded => "Loaded",
        GraphStatus::Rebuilt(_) => "Rebuilt",
        GraphStatus::NotApplicable => "NotApplicable",
        GraphStatus::PersistFailed(_) => "PersistFailed",
    }
}

/// 磁盘三体积（快照 / graph / data）。文件缺失时该字节数为 0。
fn on_disk_sizes(path: &Path) -> (u64, u64, u64) {
    let paths = helix_core::storage::graph_paths(path);
    let snap = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let graph = std::fs::metadata(&paths.graph)
        .map(|m| m.len())
        .unwrap_or(0);
    let data = std::fs::metadata(&paths.data).map(|m| m.len()).unwrap_or(0);
    (snap, graph, data)
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
