//! `idx bench` —— 效果与性能评测（P5 / T5-03b，p5-design.md 第 7 章）。
//!
//! # 执行流程（7.4）
//!
//! 加载快照（或 --input 重建）→ 加载 judgments（source 校验）→
//! 阶段 A【效果】→ 阶段 B【延迟】→（--grid）阶段 C【网格】→ 输出。
//!
//! # 关键口径
//!
//! - 延迟用**裸 LocalEmbedder**（禁 CachedEmbedder：bench 对同一 query 重复
//!   reps，缓存必然命中会系统性低估 vector/hybrid 延迟）
//! - 模型加载 / HNSW 图重建耗时**单独打印**，不计入任何 NFR 口径
//! - 全部延迟数字要求 --release；debug 构建给醒目警告
//! - `--runs N > 1`：每次**重建 HNSW 图**再评测（模拟跨进程图差异，R-P5-13
//!   抖动披露；brute 后端无抖动，跳过）
//! - 主表 Recall/MRR 双列（thr=1 / thr=2）+ graded NDCG

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::Args;

use index_core::analyze::MixedAnalyzer;
use index_core::bench::{self, Judgment, QueryMetrics};
use index_core::embed::LocalEmbedder;
use index_core::fusion::RrfFusion;
use index_core::index::Index;
use index_core::query::{SearchMode, Searcher};
use index_core::retriever::Bm25Params;
use index_core::storage;
use index_core::types::ChunkId;
use index_core::vector::{BruteForceIndex, HnswRsIndex, NormalizedVector, VectorIndex};

/// 预期占优（分桶假设，9.4）：检验"哪路占优"，非自我实现预言
const EXPECTED_WINNER: &[(&str, &str)] = &[
    ("mixed", "hybrid"),
    ("natural", "hybrid"),
    ("exact", "bm25"),
    ("paraphrase", "vector"),
];

#[derive(Args)]
pub struct BenchArgs {
    /// 快照文件（build --vectors 产出；与 --input 二选一）
    #[arg(short, long)]
    pub index: Option<PathBuf>,
    /// 语料 JSONL（重建模式；与 --index 二选一）
    #[arg(short, long)]
    pub input: Option<PathBuf>,
    /// 评测查询（judgments）文件
    #[arg(short, long, default_value = "data/t2-queries.jsonl")]
    pub queries: PathBuf,
    /// 参评模式（逗号分隔：bm25,vector,hybrid）
    #[arg(long, default_value = "bm25,vector,hybrid")]
    pub modes: String,
    /// 检索深度 = 指标 @K
    #[arg(short, long, default_value_t = 10)]
    pub k: usize,
    /// 覆盖 BM25 k1
    #[arg(long)]
    pub k1: Option<f32>,
    /// 覆盖 BM25 b
    #[arg(long)]
    pub b: Option<f32>,
    /// 覆盖 RRF k
    #[arg(long, default_value_t = 60.0)]
    pub rrf_k: f32,
    /// Recall/MRR 的相关性阈值（1 或 2；主表两列都输出）
    #[arg(long, default_value_t = 1)]
    pub rel_threshold: u8,
    /// BM25 k1×b 网格搜索（T5-05：16 格 × Recall/MRR/NDCG 三列）
    #[arg(long)]
    pub grid: bool,
    /// 延迟测量：每 query 重复次数
    #[arg(long, default_value_t = 20)]
    pub reps: usize,
    /// 延迟测量：每 query 预热次数
    #[arg(long, default_value_t = 3)]
    pub warmup: usize,
    /// 向量索引后端：hnsw（默认）| brute（诊断：隔离 ANN 近似误差，8.6）
    #[arg(long, default_value = "hnsw")]
    pub vector_index: String,
    /// 覆盖 HNSW ef_search（8.6 最小校准）
    #[arg(long)]
    pub ef_search: Option<usize>,
    /// 重复运行次数（>1 时每轮重建 HNSW 图并输出 run-to-run 抖动，R-P5-13）
    #[arg(long, default_value_t = 1)]
    pub runs: usize,
    /// 机器可读 JSON 输出路径（per-query 明细 + 网格表；T5-03 验收必需）
    #[arg(long)]
    pub json: Option<PathBuf>,
    /// 跳过延迟测量（只跑效果；网格搜索时用）
    #[arg(long)]
    pub no_latency: bool,
}

/// 评测环境：索引 + 分词器 + 可选向量后端。
struct Setup {
    index: Index,
    analyzer: MixedAnalyzer,
    vectors: Vec<(ChunkId, Vec<f32>)>,
    embedder: Option<LocalEmbedder>,
    backend: Option<VectorBackend>,
    #[allow(dead_code)]
    vector_kind: String,
}

enum VectorBackend {
    Hnsw(HnswRsIndex),
    Brute(BruteForceIndex),
}

impl VectorBackend {
    fn as_index(&self) -> &dyn VectorIndex {
        match self {
            Self::Hnsw(h) => h,
            Self::Brute(b) => b,
        }
    }
}

/// 单 query 双阈值指标。
struct PerQuery {
    qid: String,
    qtype: String,
    thr1: QueryMetrics,
    thr2: QueryMetrics,
}

struct ModeResult {
    per_query: Vec<PerQuery>,
}

impl ModeResult {
    fn agg(&self, f: fn(&PerQuery) -> &QueryMetrics) -> bench::Aggregate {
        bench::aggregate(&self.per_query.iter().map(|p| *f(p)).collect::<Vec<_>>())
    }
    fn agg_type(&self, qtype: &str, f: fn(&PerQuery) -> &QueryMetrics) -> bench::Aggregate {
        bench::aggregate(
            &self
                .per_query
                .iter()
                .filter(|p| p.qtype == qtype)
                .map(|p| *f(p))
                .collect::<Vec<_>>(),
        )
    }
}

pub fn run(args: BenchArgs) -> Result<()> {
    if cfg!(debug_assertions) {
        eprintln!("⚠️⚠️⚠️  debug 构建：延迟数字无效！评测必须 cargo run --release  ⚠️⚠️⚠️");
    }

    let modes = parse_modes(&args.modes)?;
    let need_vector = modes.contains(&SearchMode::Vector) || modes.contains(&SearchMode::Hybrid);
    let bm25_params = Bm25Params {
        k1: args.k1.unwrap_or(1.2),
        b: args.b.unwrap_or(0.75),
    };

    // ---- 1. 加载 ----
    let mut setup = load_setup(&args, need_vector)?;
    let judgments = bench::load_judgments(&args.queries)
        .with_context(|| format!("加载 judgments 失败: {}", args.queries.display()))?;
    bench::validate_sources(&judgments, &setup.index)?;
    println!(
        "评测配置: {} queries × {} 段落，K={}，BM25(k1={}, b={})，RRF k={}，向量后端 {}{}",
        judgments.len(),
        setup.index.num_chunks(),
        args.k,
        bm25_params.k1,
        bm25_params.b,
        args.rrf_k,
        args.vector_index,
        args.ef_search
            .map(|e| format!("(ef_search={e})"))
            .unwrap_or_default()
    );

    let mut json = serde_json::json!({
        "config": {
            "k": args.k, "k1": bm25_params.k1, "b": bm25_params.b, "rrf_k": args.rrf_k,
            "rel_threshold": args.rel_threshold, "vector_index": args.vector_index,
            "ef_search": args.ef_search, "modes": args.modes,
            "queries": args.queries.display().to_string(),
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        },
        "n_queries": judgments.len(),
        "n_corpus": setup.index.num_chunks(),
    });

    // ---- 2. 阶段 A：效果（--runs 轮，每轮重建 HNSW 图模拟跨进程差异）----
    let runs = args.runs.max(1);
    let mut run_reports: Vec<serde_json::Value> = Vec::new();
    let mut first_results: Option<HashMap<String, ModeResult>> = None;

    for run_i in 1..=runs {
        if run_i > 1 {
            if let Some(backend_slot) = &mut setup.backend {
                // 重建 HNSW 图：模拟跨进程（hnsw_rs 用 OS 熵，每次图不同）
                if matches!(backend_slot, VectorBackend::Hnsw(_)) {
                    *backend_slot = build_backend(&setup.vectors, &args)?;
                }
            }
        }
        let mut results: HashMap<String, ModeResult> = HashMap::new();
        for &mode in &modes {
            let searcher = make_searcher(&setup, bm25_params, args.rrf_k, mode)?;
            results.insert(
                mode_name(mode).to_string(),
                eval_effect(&searcher, &judgments, mode, args.k),
            );
        }
        if run_i == 1 {
            print_effect(&results, &judgments, args.k);
            first_results = Some(results);
        } else {
            // 后续轮：只输出整体 NDCG（抖动观测）
            let summary: serde_json::Value = serde_json::Map::from_iter(
                results
                    .iter()
                    .map(|(name, r)| (name.clone(), serde_json::json!(r.agg(|p| &p.thr1).ndcg))),
            )
            .into();
            run_reports.push(serde_json::json!({"run": run_i, "ndcg": summary}));
        }
    }

    let first = first_results.expect("runs >= 1");

    // 符号检验：hybrid vs 每个单路（per-query NDCG delta，二项精确单侧）
    if let Some(hybrid) = first.get("hybrid") {
        let mut sig = serde_json::Map::new();
        for other_name in ["bm25", "vector"] {
            if let Some(other) = first.get(other_name) {
                let (pos, neg, zero, p) = sign_compare(hybrid, other);
                println!(
                    "符号检验 hybrid vs {other_name}: 正 {pos} / 负 {neg} / 零 {zero}，单侧 p = {p:.4}{}",
                    if p < 0.05 { "（显著）" } else { "" }
                );
                sig.insert(
                    format!("hybrid_vs_{other_name}"),
                    serde_json::json!({"positive": pos, "negative": neg, "zero": zero, "p_one_sided": p}),
                );
            }
        }
        json["significance"] = sig.into();
    }

    // per-query 明细进 json
    let mut json_modes = serde_json::Map::new();
    for (name, r) in &first {
        json_modes.insert(name.clone(), mode_to_json(r));
    }
    json["modes"] = json_modes.into();

    // ---- 3. 阶段 B：延迟 ----
    if !args.no_latency {
        let mut latency = serde_json::Map::new();
        println!(
            "\n== 延迟（warmup {} + {} reps × {} queries）==",
            args.warmup,
            args.reps,
            judgments.len()
        );
        println!("{:<8} {:>10} {:>10}", "mode", "P50(ms)", "P99(ms)");
        for &mode in &modes {
            let searcher = make_searcher(&setup, bm25_params, args.rrf_k, mode)?;
            let lat = eval_latency(&searcher, &judgments, mode, args.k, args.warmup, args.reps);
            println!(
                "{:<8} {:>10.2} {:>10.2}",
                mode_name(mode),
                lat.p50_ms,
                lat.p99_ms
            );
            latency.insert(
                mode_name(mode).to_string(),
                serde_json::json!({"p50_ms": lat.p50_ms, "p99_ms": lat.p99_ms, "n_samples": lat.n}),
            );
        }
        json["latency"] = latency.into();
    }

    // ---- 4. 阶段 C：网格 ----
    if args.grid {
        let grid = eval_grid(&setup, &judgments, args.k, args.rrf_k)?;
        json["grid"] = serde_json::to_value(&grid).unwrap_or_default();
        if !run_reports.is_empty() {
            json["runs"] = run_reports.into();
        }
    } else if !run_reports.is_empty() {
        json["runs"] = run_reports.into();
    }

    // ---- 5. --json 输出 ----
    if let Some(path) = &args.json {
        std::fs::write(path, serde_json::to_string_pretty(&json)?)?;
        println!("\nJSON 明细已写入 {}", path.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 加载
// ---------------------------------------------------------------------------

fn load_setup(args: &BenchArgs, need_vector: bool) -> Result<Setup> {
    let (index, vectors) = match (&args.index, &args.input) {
        (Some(path), None) => {
            let t = Instant::now();
            let (index, vectors) =
                storage::load(path).with_context(|| format!("加载快照失败: {}", path.display()))?;
            println!("[快照加载 耗时 {:?}（NFR-04 口径之一）]", t.elapsed());
            (index, vectors)
        }
        (None, Some(path)) => {
            let (index, _) = crate::load_corpus(path)?;
            (index, Vec::new())
        }
        _ => bail!("--index 与 --input 必须二选一"),
    };

    let mut vectors = vectors;
    let mut embedder = None;
    if need_vector {
        if index.num_chunks() == 0 {
            bail!("索引为空");
        }
        let t = Instant::now();
        let e = LocalEmbedder::new()?;
        println!("[模型加载 耗时 {:?}（不计入延迟口径）]", t.elapsed());
        if vectors.is_empty() {
            if args.index.is_some() {
                bail!("快照不含向量：vector/hybrid 需要 `build --vectors` 重建");
            }
            vectors = crate::embed_chunks(&index, &e)?;
        }
        embedder = Some(e);
    }

    let backend = if need_vector {
        Some(build_backend(&vectors, args)?)
    } else {
        None
    };

    Ok(Setup {
        index,
        analyzer: MixedAnalyzer::new(),
        vectors,
        embedder,
        backend,
        vector_kind: args.vector_index.clone(),
    })
}

fn build_backend(vectors: &[(ChunkId, Vec<f32>)], args: &BenchArgs) -> Result<VectorBackend> {
    match args.vector_index.as_str() {
        "hnsw" => {
            let t = Instant::now();
            let mut idx = HnswRsIndex::with_capacity(vectors.len().max(1024));
            if let Some(ef) = args.ef_search {
                idx = idx.with_ef_search(ef);
            }
            for (id, v) in vectors {
                idx.add(*id, NormalizedVector::new(v.clone()))?;
            }
            eprintln!(
                "[HNSW 图重建 {} 条 耗时 {:?}（跨进程不确定 R-P5-13；图构建属 NFR-04 冷启动口径，勿并入 NFR-03）]",
                vectors.len(),
                t.elapsed()
            );
            Ok(VectorBackend::Hnsw(idx))
        }
        "brute" => {
            let entries = vectors
                .iter()
                .map(|(id, v)| (*id, NormalizedVector::new(v.clone())))
                .collect();
            Ok(VectorBackend::Brute(BruteForceIndex::from_entries(entries)))
        }
        other => bail!("不支持的 --vector-index {other:?}（支持 hnsw / brute）"),
    }
}

fn make_searcher<'a>(
    setup: &'a Setup,
    params: Bm25Params,
    rrf_k: f32,
    mode: SearchMode,
) -> Result<Searcher<'a>> {
    let mut s = Searcher::new(&setup.index, &setup.analyzer).with_bm25_params(params);
    if mode != SearchMode::Bm25 {
        let e = setup
            .embedder
            .as_ref()
            .context("vector/hybrid 模式需要 embedder")?;
        let vi = setup
            .backend
            .as_ref()
            .context("vector/hybrid 模式需要向量后端")?;
        // bench 一律覆盖 fusion（哪怕 hybrid 用默认 k）——保证 --rrf-k 生效
        s = s
            .with_vector(e, vi.as_index())
            .with_fusion(Box::new(RrfFusion::new(rrf_k, vec![1.0, 1.0])));
    }
    Ok(s)
}

// ---------------------------------------------------------------------------
// 阶段 A：效果
// ---------------------------------------------------------------------------

fn eval_effect(
    searcher: &Searcher,
    judgments: &[Judgment],
    mode: SearchMode,
    k: usize,
) -> ModeResult {
    let mut per_query = Vec::with_capacity(judgments.len());
    for j in judgments {
        let resp = searcher
            .search(&j.query, mode, k)
            .unwrap_or_else(|e| panic!("检索失败（qid={}）: {e}", j.qid));
        let ranked: Vec<&str> = resp.hits.iter().map(|h| h.source.as_str()).collect();
        let grades = j.grades();
        let thr1 = bench::evaluate(&ranked, &grades, k, 1);
        let thr2 = bench::evaluate(&ranked, &grades, k, 2);
        per_query.push(PerQuery {
            qid: j.qid.clone(),
            qtype: j.qtype.clone(),
            thr1,
            thr2,
        });
    }
    ModeResult { per_query }
}

fn print_effect(results: &HashMap<String, ModeResult>, judgments: &[Judgment], k: usize) {
    println!(
        "\n== 效果评测（{} queries，K={}，宏平均，thr=1 与 thr=2 双列 + graded NDCG）==",
        judgments.len(),
        k
    );
    println!(
        "{:<8} {:>16} {:>16} {:>12}",
        "mode", "Recall@K(t1/t2)", "MRR@K(t1/t2)", "NDCG@K"
    );
    for name in ["bm25", "vector", "hybrid"] {
        if let Some(r) = results.get(name) {
            let a1 = r.agg(|p| &p.thr1);
            let a2 = r.agg(|p| &p.thr2);
            println!(
                "{:<8} {:>7.4}/{:<7.4} {:>7.4}/{:<7.4} {:>12.4}",
                name, a1.recall, a2.recall, a1.mrr, a2.mrr, a1.ndcg
            );
        }
    }

    // 分桶表（NDCG@10 + 预期占优）
    let has_all = ["bm25", "vector", "hybrid"]
        .iter()
        .all(|n| results.contains_key(*n));
    if has_all {
        println!("\n== 分桶（NDCG@10）==");
        println!(
            "{:<11} {:>4} {:>8} {:>8} {:>8}  预期占优",
            "type", "n", "bm25", "vector", "hybrid"
        );
        for (qtype, expected) in EXPECTED_WINNER {
            let n = judgments.iter().filter(|j| &j.qtype == qtype).count();
            let b = results["bm25"].agg_type(qtype, |p| &p.thr1);
            let v = results["vector"].agg_type(qtype, |p| &p.thr1);
            let h = results["hybrid"].agg_type(qtype, |p| &p.thr1);
            let note = if n < 30 {
                "（<30 条：探索性）"
            } else {
                ""
            };
            println!(
                "{:<11} {:>4} {:>8.4} {:>8.4} {:>8.4}  {}{}",
                qtype, n, b.ndcg, v.ndcg, h.ndcg, expected, note
            );
        }
    }
}

/// hybrid vs other 的逐 query NDCG 符号检验（返回 正/负/零/单侧 p）
fn sign_compare(hybrid: &ModeResult, other: &ModeResult) -> (usize, usize, usize, f64) {
    let mut pos = 0usize;
    let mut neg = 0usize;
    for (h, o) in hybrid.per_query.iter().zip(&other.per_query) {
        let d = h.thr1.ndcg - o.thr1.ndcg;
        if d > 1e-12 {
            pos += 1;
        } else if d < -1e-12 {
            neg += 1;
        }
    }
    let n = pos + neg;
    let zero = hybrid.per_query.len() - n;
    let p = if n > 0 { bench::sign_test(pos, n) } else { 1.0 };
    (pos, neg, zero, p)
}

// ---------------------------------------------------------------------------
// 阶段 B：延迟
// ---------------------------------------------------------------------------

struct LatencyResult {
    p50_ms: f64,
    p99_ms: f64,
    n: usize,
}

fn eval_latency(
    searcher: &Searcher,
    judgments: &[Judgment],
    mode: SearchMode,
    k: usize,
    warmup: usize,
    reps: usize,
) -> LatencyResult {
    let mut samples: Vec<f64> = Vec::with_capacity(judgments.len() * reps);
    for j in judgments {
        for _ in 0..warmup {
            let _ = searcher.search(&j.query, mode, k);
        }
        for _ in 0..reps {
            let t = Instant::now();
            let _ = searcher.search(&j.query, mode, k);
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    samples.sort_by(|a, b| a.partial_cmp(b).expect("延迟样本无 NaN"));
    let n = samples.len();
    LatencyResult {
        p50_ms: bench::percentile(&samples, 50.0),
        p99_ms: bench::percentile(&samples, 99.0),
        n,
    }
}

// ---------------------------------------------------------------------------
// 阶段 C：BM25 网格（T5-05）
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct GridCell {
    k1: f32,
    b: f32,
    recall: f64,
    mrr: f64,
    ndcg: f64,
}

fn eval_grid(setup: &Setup, judgments: &[Judgment], k: usize, rrf_k: f32) -> Result<Vec<GridCell>> {
    println!("\n== BM25 网格搜索（k1 × b，宏平均 NDCG@10 选优；Recall/MRR 不退化约束）==");
    let mut cells = Vec::new();
    for k1 in [1.0f32, 1.2, 1.5, 2.0] {
        for b in [0.3f32, 0.5, 0.75, 0.9] {
            let searcher = make_searcher(setup, Bm25Params { k1, b }, rrf_k, SearchMode::Bm25)?;
            let r = eval_effect(&searcher, judgments, SearchMode::Bm25, k);
            let a = r.agg(|p| &p.thr1);
            println!(
                "  k1={:<4} b={:<5} NDCG={:.4} Recall={:.4} MRR={:.4}",
                k1, b, a.ndcg, a.recall, a.mrr
            );
            cells.push(GridCell {
                k1,
                b,
                recall: a.recall,
                mrr: a.mrr,
                ndcg: a.ndcg,
            });
        }
    }
    // 选优：NDCG 最高；并列取更接近默认 (1.2, 0.75) 的（防噪声过拟合 tie-break）
    let dist = |c: &GridCell| (c.k1 - 1.2).abs() + (c.b - 0.75).abs();
    let mut best: Vec<&GridCell> = cells
        .iter()
        .filter(|c| {
            (c.ndcg
                - cells
                    .iter()
                    .map(|x| x.ndcg)
                    .fold(f64::NEG_INFINITY, f64::max))
            .abs()
                < 1e-9
        })
        .collect();
    best.sort_by(|x, y| dist(x).partial_cmp(&dist(y)).unwrap());
    if let Some(winner) = best.first() {
        println!(
            "  → 最优: k1={}, b={}（NDCG={:.4}）；定稿前检查 Recall/MRR 相对默认不退化",
            winner.k1, winner.b, winner.ndcg
        );
    }
    Ok(cells)
}

// ---------------------------------------------------------------------------
// 输出辅助
// ---------------------------------------------------------------------------

fn mode_name(m: SearchMode) -> &'static str {
    match m {
        SearchMode::Bm25 => "bm25",
        SearchMode::Vector => "vector",
        SearchMode::Hybrid => "hybrid",
    }
}

fn parse_modes(s: &str) -> Result<Vec<SearchMode>> {
    let mut out = Vec::new();
    for part in s.split(',') {
        match part.trim() {
            "bm25" => out.push(SearchMode::Bm25),
            "vector" => out.push(SearchMode::Vector),
            "hybrid" => out.push(SearchMode::Hybrid),
            other => bail!("不支持的 --modes 项 {other:?}（bm25 / vector / hybrid）"),
        }
    }
    if out.is_empty() {
        bail!("--modes 不能为空");
    }
    Ok(out)
}

fn mode_to_json(r: &ModeResult) -> serde_json::Value {
    let a1 = r.agg(|p| &p.thr1);
    let a2 = r.agg(|p| &p.thr2);
    let mut buckets = serde_json::Map::new();
    for qtype in ["mixed", "natural", "exact", "paraphrase"] {
        let b1 = r.agg_type(qtype, |p| &p.thr1);
        buckets.insert(
            qtype.to_string(),
            serde_json::json!({
                "n": b1.n,
                "recall": b1.recall, "mrr": b1.mrr, "ndcg": b1.ndcg,
            }),
        );
    }
    let per_query: Vec<serde_json::Value> = r
        .per_query
        .iter()
        .map(|p| {
            serde_json::json!({
                "qid": p.qid, "type": p.qtype,
                "recall": p.thr1.recall, "mrr": p.thr1.mrr, "ndcg": p.thr1.ndcg,
                "recall_thr2": p.thr2.recall, "mrr_thr2": p.thr2.mrr,
            })
        })
        .collect();
    serde_json::json!({
        "thr1": {"recall": a1.recall, "mrr": a1.mrr, "ndcg": a1.ndcg, "n": a1.n},
        "thr2": {"recall": a2.recall, "mrr": a2.mrr, "n": a2.n},
        "buckets": buckets,
        "per_query": per_query,
    })
}
