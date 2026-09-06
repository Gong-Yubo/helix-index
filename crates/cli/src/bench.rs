//! `helix bench` —— 效果与性能评测（P5 / T5-03b，p5-design.md 第 7 章）。
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
//! - `--filter`：过滤档位（V2 Step 1 / S1-10）。有过滤时额外输出 **post-filter
//!   oracle 对照**——无过滤取 Top(k×--oracle-depth) 再按 doc 位图过滤取前 k，
//!   即 Step 1 之前的行为，作为「下推是否回退」的基线（T9 / T14）
//! - `--filter-cost`：过滤求值耗时对照（T13）——旧 `allowed_chunks` 全扫 vs
//!   新 `doc_bits` + 惰性谓词，并附 `doc_bits_scan`（降级字段实际走的路径）

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::Args;

#[cfg(feature = "charabia")]
use helix_core::analyze::CharabiaAnalyzer;
use helix_core::analyze::{Analyzer, MixedAnalyzer};
use helix_core::bench::{self, Judgment, QueryMetrics};
use helix_core::bitmap::DocBits;
use helix_core::chunk::Chunker;
use helix_core::embed::LocalEmbedder;
use helix_core::fusion::RrfFusion;
use helix_core::index::Index;
use helix_core::predicate::CandidateFilter;
use helix_core::query::filter::{
    allowed_chunk_count, allowed_chunks, doc_bits, doc_bits_scan, ChunkFilter,
};
use helix_core::query::{QueryExecutor, SearchMode};
use helix_core::retriever::Bm25Params;
use helix_core::schema::Filter;
use helix_core::storage;
use helix_core::types::ChunkId;
use helix_core::vector::{BruteForceIndex, HnswRsIndex, NormalizedVector, VectorIndex};

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
    #[arg(long)]
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
    /// 覆盖 RRF k（默认取 RrfFusion::default() 的 60）
    #[arg(long)]
    pub rrf_k: Option<f32>,
    /// RRF 路权重（"w_bm25,w_vector"，如 "1.5,1"；8.5 weights 诊断）。
    /// 默认取 RrfFusion::default() 的 [1.0, 1.5]（P5 定稿）—— 从内核派生而非
    /// CLI 硬编码，避免 bench 与 search 的"默认融合"语义分裂（V1-14）
    #[arg(long)]
    pub rrf_weights: Option<String>,
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
    /// 分词器：mixed（默认，自研链）| charabia（T5-09 对照，需 --features charabia 编译）
    #[arg(long, default_value = "mixed")]
    pub analyzer: String,
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
    /// 过滤档位（`field=value` / `field>=v` / `field<v`，逗号分隔；可重复传）。
    ///
    /// 传了它，bench 会在效果/延迟两阶段都带上过滤，并额外跑 **post-filter
    /// oracle 对照**，输出「选择度 × 延迟 × 召回」三元数据
    /// （V2 Step 1 · S1-10 的 T12 / T14；档位清单见 `scripts/eval_filter.sh`）
    #[arg(long)]
    pub filter: Vec<String>,
    /// 过滤求值耗时对照（T13）：旧 `allowed_chunks` 全扫 vs 新 `doc_bits` + 惰性谓词，
    /// 并附 `doc_bits_scan`（降级字段实际走的路径）
    #[arg(long)]
    pub filter_cost: bool,
    /// post-filter oracle 的过采样深度：无过滤取 Top (k × 该值) 后再按过滤条件截断。
    /// 上限 500：k×depth 不会超过 ORACLE_MAX_DEPTH(5000)（k=10 时的安全值；
    /// 即使传入也只会被收敛到 5000 而非 panic，见 `oracle_depth_for`）
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=500))]
    pub oracle_depth: u64,
}

/// 评测环境：索引 + 分词器 + 可选向量后端。
struct Setup {
    index: Index,
    analyzer: Box<dyn Analyzer>,
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
    /// 过滤档位下的质量指标（无过滤时为 `None`）
    filter_quality: Option<FilterQuality>,
}

/// 过滤场景的质量指标：T12 三元数据里的「召回」轴，以及 T14 的判据。
struct FilterQuality {
    /// 相对 **post-filter oracle** 的 Top-K 重合率（`|F ∩ O| / |O|`）。
    ///
    /// oracle = 无过滤取 Top(k×depth) 再按过滤条件截断取前 k，即 **Step 1 之前
    /// 的行为**。这是 T14① 的核心判据（目标 ≥ 0.99）：下推不该改变"谁最相关"，
    /// 只该改变"多快找到"。
    recall_vs_oracle: f64,
    /// 平均返回条数：低选择度下不足 k 时，暴露 R11（filtered-ANN 召回不足）
    /// 与 R17（hnsw_rs 固有近似误差）的真实量级
    mean_hits: f64,
    /// 平均**用户视角**缺口 = `min(k, allowed) - 实得条数`。
    ///
    /// ⚠️ 与 `Metrics::vector_shortfall` 口径不同：后者按融合前的候选池
    /// `min(candidate_k, allowed)` 算，bench 侧拿不到内部 `candidate_k`，
    /// 这里用 k 代替。两者都叫"缺口"，但分母不同，不可混用。
    mean_shortfall: f64,
    /// oracle 自身的平均条数——**基线的自证**。
    ///
    /// 低于 K 说明 oracle 没能凑够 K 条（过采样深度不足），此时 `recall_vs_oracle`
    /// 的分母是残缺的，重合率**不可信**，必须连同本字段一起看。
    oracle_mean_hits: f64,
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
    // 默认值来自 Bm25Params::default()（P5 定稿 k1=1.5/b=0.75），CLI 仅覆盖显式传入项
    let mut bm25_params = Bm25Params::default();
    if let Some(k1) = args.k1 {
        bm25_params.k1 = k1;
    }
    if let Some(b) = args.b {
        bm25_params.b = b;
    }
    // RRF 默认值统一从 RrfFusion::default() 派生（P5 定稿 k=60 / weights=[1.0, 1.5]），
    // CLI 只覆盖显式传入项——保证 bench 与 `search --mode hybrid` 的默认融合必然一致。
    let fusion_default = RrfFusion::default();
    let rrf_k = args.rrf_k.unwrap_or_else(|| fusion_default.k());
    let rrf_weights: Vec<f32> = match &args.rrf_weights {
        Some(s) => s
            .split(',')
            .map(|part| part.trim().parse::<f32>())
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("--rrf-weights 解析失败: {s:?}"))?,
        None => fusion_default.weights().to_vec(),
    };
    if rrf_weights.len() != 2 || rrf_weights.iter().any(|w| *w < 0.0) {
        bail!(
            "--rrf-weights 需为两个非负数（w_bm25,w_vector），收到 {:?}",
            rrf_weights
        );
    }

    // ---- 1. 加载 ----
    let mut setup = load_setup(&args, need_vector)?;
    let judgments = bench::load_judgments(&args.queries)
        .with_context(|| format!("加载 judgments 失败: {}", args.queries.display()))?;
    bench::validate_sources(&judgments, &setup.index)?;

    // 过滤档位（V2 Step 1 / S1-10）：解析一次，效果与延迟两阶段共用。
    // `allowed` 是选择度的分子——三元数据里的「选择度」轴。
    let filter = crate::parse_filters(&args.filter)?;
    let bits = filter.as_ref().map(|f| doc_bits(f, &setup.index));
    let allowed = bits
        .as_ref()
        .map_or(0, |b| allowed_chunk_count(b, &setup.index));
    println!(
        "评测配置: {} queries × {} 段落，K={}，BM25(k1={}, b={})，RRF(k={}, w={:?})，向量后端 {}{}",
        judgments.len(),
        setup.index.num_chunks(),
        args.k,
        bm25_params.k1,
        bm25_params.b,
        rrf_k,
        rrf_weights,
        args.vector_index,
        args.ef_search
            .map(|e| format!("(ef_search={e})"))
            .unwrap_or_default()
    );
    if let Some(f) = filter.as_ref() {
        let total = setup.index.num_chunks().max(1) as f64;
        // clap 校验已保证 1..=500，恒在 usize 范围内
        let oracle_depth = args.oracle_depth as usize;
        let depth_eff = oracle_depth_for(&setup.index, allowed, args.k, oracle_depth);
        println!(
            "过滤档位: {f:?}\n           allowed = {allowed} chunk（选择度 {:.4}%），\
             oracle = 无过滤 Top({depth_eff}) 后过滤（按选择度自适应）\n\
           ⚠️  过滤场景下主表的 Recall/MRR/NDCG **无意义**（相关文档大概率不在 allowed 内），\
             唯一可信的是下面的「过滤质量」表",
            allowed as f64 / total * 100.0,
        );
    }

    let mut json = serde_json::json!({
        "config": {
            "k": args.k, "k1": bm25_params.k1, "b": bm25_params.b, "rrf_k": rrf_k,
            "rel_threshold": args.rel_threshold, "vector_index": args.vector_index,
            "ef_search": args.ef_search, "modes": args.modes,
            "queries": args.queries.display().to_string(),
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "filter": args.filter.join(" AND "),
            "oracle_depth": args.oracle_depth,
        },
        "n_queries": judgments.len(),
        "n_corpus": setup.index.num_chunks(),
    });
    if filter.is_some() {
        json["filter"] = serde_json::json!({
            "spec": args.filter.join(" AND "),
            "allowed_chunks": allowed,
            "selectivity": allowed as f64 / setup.index.num_chunks().max(1) as f64,
        });
    }

    // ---- 2. 阶段 A：效果（--runs 轮，每轮重建 HNSW 图模拟跨进程差异）----
    let runs = args.runs.max(1);
    let mut run_reports: Vec<serde_json::Value> = Vec::new();
    let mut all_ndcg: Vec<HashMap<String, f64>> = Vec::new();
    let mut first_results: Option<HashMap<String, ModeResult>> = None;

    for run_i in 1..=runs {
        if run_i > 1 {
            if let Some(backend_slot) = &mut setup.backend {
                // 重建 HNSW 图：模拟跨进程（hnsw_rs 用 OS 熵，每次图不同）。
                // **刻意传 None 跳过图 sidecar**：本处就是要制造跨进程差异以观测
                // R-P5-13 抖动；若读图则每轮结果完全相同，`--runs` 失去意义
                if matches!(backend_slot, VectorBackend::Hnsw(_)) {
                    *backend_slot = build_backend(&setup.vectors, &args, None)?;
                }
            }
        }
        let mut results: HashMap<String, ModeResult> = HashMap::new();
        for &mode in &modes {
            let searcher = make_searcher(&setup, bm25_params, rrf_k, &rrf_weights, mode)?;
            results.insert(
                mode_name(mode).to_string(),
                eval_effect(
                    &searcher,
                    &setup.index,
                    &judgments,
                    mode,
                    args.k,
                    FilterCtx {
                        filter: filter.as_ref(),
                        bits: bits.as_ref(),
                        allowed,
                        // clap 校验已保证 1..=500，恒在 usize 范围内
                        oracle_depth: args.oracle_depth as usize,
                    },
                ),
            );
        }
        // 每轮 NDCG 记录（含第一轮；--runs > 1 时用于抖动区间披露）
        let ndcg_by_mode: HashMap<String, f64> = results
            .iter()
            .map(|(name, r)| {
                let ndcg = r.agg(|p| &p.thr1).ndcg;
                (name.clone(), ndcg)
            })
            .collect();
        run_reports.push(serde_json::json!({"run": run_i, "ndcg": ndcg_by_mode}));
        all_ndcg.push(
            results
                .iter()
                .map(|(name, r)| (name.clone(), r.agg(|p| &p.thr1).ndcg))
                .collect(),
        );
        if run_i == 1 {
            print_effect(&results, &judgments, args.k);
            if filter.is_some() {
                print_filter_quality(&results, args.k, allowed);
            }
            first_results = Some(results);
        } else {
            let line: Vec<String> = ndcg_by_mode
                .iter()
                .map(|(name, v)| format!("{name}={v:.4}"))
                .collect();
            println!("[run {run_i} 图已重建] NDCG@10(thr1): {}]", line.join(" "));
        }
    }

    // 抖动区间披露（R-P5-13：同进程同图确定，跨进程图不同）
    if runs > 1 {
        println!("\n== 跨图抖动（--runs {runs}）==");
        let mut names: Vec<&String> = all_ndcg[0].keys().collect();
        names.sort();
        for name in &names {
            let vals: Vec<f64> = all_ndcg
                .iter()
                .filter_map(|m| m.get(*name))
                .copied()
                .collect();
            let (min, max) = (
                vals.iter().cloned().fold(f64::INFINITY, f64::min),
                vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            );
            println!(
                "  {name:<8}: NDCG@10 min~max = {min:.4} ~ {max:.4}（极差 {:.4}）",
                max - min
            );
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
        if filter.is_some() {
            println!(
                "{:<8} {:>10} {:>10} {:>10} {:>10}",
                "mode", "P50(ms)", "P99(ms)", "平均条数", "平均缺口"
            );
        } else {
            println!("{:<8} {:>10} {:>10}", "mode", "P50(ms)", "P99(ms)");
        }
        for &mode in &modes {
            let searcher = make_searcher(&setup, bm25_params, rrf_k, &rrf_weights, mode)?;
            let lat = eval_latency(
                &searcher,
                &judgments,
                mode,
                args.k,
                args.warmup,
                args.reps,
                FilterCtx {
                    filter: filter.as_ref(),
                    bits: bits.as_ref(),
                    allowed,
                    // clap 校验已保证 1..=500，恒在 usize 范围内
                    oracle_depth: args.oracle_depth as usize,
                },
            );
            if filter.is_some() {
                println!(
                    "{:<8} {:>10.2} {:>10.2} {:>10.2} {:>10.2}",
                    mode_name(mode),
                    lat.p50_ms,
                    lat.p99_ms,
                    lat.mean_hits,
                    lat.mean_shortfall
                );
            } else {
                println!(
                    "{:<8} {:>10.2} {:>10.2}",
                    mode_name(mode),
                    lat.p50_ms,
                    lat.p99_ms
                );
            }
            latency.insert(
                mode_name(mode).to_string(),
                serde_json::json!({
                    "p50_ms": lat.p50_ms,
                    "p99_ms": lat.p99_ms,
                    "n_samples": lat.n,
                    "mean_hits": lat.mean_hits,
                    "mean_shortfall": lat.mean_shortfall,
                }),
            );
        }
        json["latency"] = latency.into();
    }

    // ---- 3.5 阶段 D：过滤求值耗时对照（T13，仅 --filter-cost）----
    if args.filter_cost {
        match filter.as_ref() {
            Some(f) => eval_filter_cost(&setup.index, f, args.warmup, args.reps.max(3))?,
            None => eprintln!("⚠️  --filter-cost 需要配合 --filter 使用，本次忽略"),
        }
    }

    // ---- 4. 阶段 C：网格 ----
    if args.grid {
        let grid = eval_grid(&setup, &judgments, args.k, rrf_k)?;
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

/// 按 `--analyzer` 构建分词器（`Box<dyn Analyzer>`）。
///
/// charabia 是 feature 隔离的验证性依赖（T5-09）：未以 `--features charabia`
/// 编译时，`--analyzer charabia` 直接报错并提示重编，而非静默回退（回退会
/// 在索引/查询两侧用不同分词器，是 R4 正确性事故）。
fn build_analyzer(args: &BenchArgs) -> Result<Box<dyn Analyzer>> {
    match args.analyzer.as_str() {
        "mixed" => Ok(Box::new(MixedAnalyzer::new())),
        #[cfg(feature = "charabia")]
        "charabia" => Ok(Box::new(CharabiaAnalyzer::new())),
        #[cfg(not(feature = "charabia"))]
        "charabia" => bail!(
            "--analyzer charabia 需要以 `cargo build --features charabia` 编译 helix（T5-09 对照实验，feature 隔离）"
        ),
        other => bail!(
            "不支持的 --analyzer {other:?}（支持 mixed{}）",
            if cfg!(feature = "charabia") { " / charabia" } else { "" }
        ),
    }
}

fn load_setup(args: &BenchArgs, need_vector: bool) -> Result<Setup> {
    let mut graph_src: Option<GraphSource> = None;
    let (index, vectors, analyzer) = match (&args.index, &args.input) {
        (Some(path), None) => {
            let t = Instant::now();
            let (index, vectors, fp, body_crc) = storage::load_with_crc(path)
                .with_context(|| format!("加载快照失败: {}", path.display()))?;
            println!("[快照加载 耗时 {:?}（NFR-04 口径之一）]", t.elapsed());
            // 图 sidecar 的来源（V2 Step 2）：图随快照同目录，body_crc 是版本锚点
            graph_src = Some((path.clone(), body_crc, fp.dim));
            // P11（B1 的 bench 侧修复，评审 P11）：快照记录的 analyzer 指纹与请求的
            // --analyzer 必须一致，不一致报 ConfigMismatch——不再"打印警告并回退 mixed"
            // （回退会让索引/查询两侧分词器不一致，是 R4 正确性事故）。
            let analyzer = build_analyzer(args)?;
            if analyzer.id() != fp.analyzer_id {
                return Err(helix_core::error::Error::ConfigMismatch {
                    expected: format!("analyzer={}", fp.analyzer_id),
                    actual: format!("analyzer={}", analyzer.id()),
                }
                .into());
            }
            (index, vectors, analyzer)
        }
        (None, Some(path)) => {
            // 每段落强制单 chunk（p5-design 5.2 步骤 8）：T2Ranking 标注在段落级，
            // 多 chunk 段落会导致同一 passage 的多个 chunk 各占 Top-10 位次、双计相关性。
            // 200K 字符 > 语料最长段落（76,895），保证恒单 chunk。
            const SINGLE_CHUNK_CHARS: usize = 200_000;
            let analyzer = build_analyzer(args)?;
            let index = crate::build_index(
                path,
                &Chunker::new(SINGLE_CHUNK_CHARS, 0),
                analyzer.as_ref(),
            )?;
            println!(
                "[评测构建：每段落强制单 chunk 路径（chunk_chars={SINGLE_CHUNK_CHARS}，无重叠），analyzer={}]",
                args.analyzer
            );
            (index, Vec::new(), analyzer)
        }
        _ => bail!("--index 与 --input 必须二选一"),
    };

    // 快照口径校验：T2Ranking 标注在段落级，chunk:doc ≠ 1:1 时存在多 chunk
    // 双计风险（--index 路径无法重切，建议改用 --input 强制单 chunk 路径）
    if index.num_chunks() as usize != index.num_docs() {
        println!(
            "[⚠️ 快照 chunk:doc ≠ 1:1（{} chunks / {} docs）：段落级标注会被多 chunk 双计，\
             建议 --input 走强制单 chunk 路径]",
            index.num_chunks(),
            index.num_docs()
        );
    }

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
        Some(build_backend(&vectors, args, graph_src.as_ref())?)
    } else {
        None
    };

    Ok(Setup {
        index,
        analyzer,
        vectors,
        embedder,
        backend,
        vector_kind: args.vector_index.clone(),
    })
}

/// 可选的图 sidecar 来源：`(快照路径, 快照正文 CRC, dim)`。
///
/// 仅 `--index` 路径有值（图随快照同目录，P1-6）；`--input` 内存直建路径为 `None`
/// —— bench 关注检索延迟，冷启动已在 S2-T14 单独测，故内存直建维持重建并打印提示。
type GraphSource = (std::path::PathBuf, u32, u32);

fn build_backend(
    vectors: &[(ChunkId, Vec<f32>)],
    args: &BenchArgs,
    graph_src: Option<&GraphSource>,
) -> Result<VectorBackend> {
    match args.vector_index.as_str() {
        "hnsw" => {
            let ef = args.ef_search.unwrap_or(HnswRsIndex::default_ef_search());
            // V2 Step 2（D-S2-06）：优先从图 sidecar 加载，失败才重建。
            // 校验逻辑与门面层共用 `load_graph_checked`（禁止在此复制校验）。
            if let Some((base, body_crc, dim)) = graph_src {
                match helix_core::vector::load_graph_checked(
                    base,
                    *body_crc,
                    *dim,
                    vectors.len() as u64,
                    ef,
                ) {
                    Ok(idx) => {
                        eprintln!(
                            "[HNSW 图从 sidecar 加载成功（冷启动快路径；消 R-P5-13 图抖动）]"
                        );
                        return Ok(VectorBackend::Hnsw(idx));
                    }
                    Err(reason) => {
                        eprintln!("[HNSW 图 sidecar 不可用（{reason}），降级重建（冷启动会变慢）]")
                    }
                }
            } else {
                eprintln!(
                    "[HNSW 图：内存直建路径（--input）无快照可绑图，走重建；\
                     如需测持久化图的冷启动请用 --index]"
                );
            }
            let t = Instant::now();
            let mut idx = HnswRsIndex::with_capacity(vectors.len().max(1024)).with_ef_search(ef);
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
    rrf_weights: &[f32],
    mode: SearchMode,
) -> Result<QueryExecutor<'a>> {
    let mut s = QueryExecutor::new(&setup.index, setup.analyzer.as_ref()).with_bm25_params(params);
    if mode != SearchMode::Bm25 {
        let e = setup
            .embedder
            .as_ref()
            .context("vector/hybrid 模式需要 embedder")?;
        let vi = setup
            .backend
            .as_ref()
            .context("vector/hybrid 模式需要向量后端")?;
        // bench 一律覆盖 fusion（哪怕 hybrid 用默认 k）——保证 --rrf-k/--rrf-weights 生效
        s = s
            .with_vector(e, vi.as_index())
            .with_fusion(Box::new(RrfFusion::new(rrf_k, rrf_weights.to_vec())));
    }
    Ok(s)
}

// ---------------------------------------------------------------------------
// 阶段 A：效果
// ---------------------------------------------------------------------------

/// 执行一次检索（有过滤走 `search_filtered`，无过滤走 `search`）。
fn run_once(
    searcher: &QueryExecutor,
    query: &str,
    mode: SearchMode,
    k: usize,
    filter: Option<&Filter>,
) -> helix_core::error::Result<helix_core::query::SearchResponse> {
    match filter {
        Some(f) => searcher.search_filtered(query, mode, k, Some(f)),
        None => searcher.search(query, mode, k),
    }
}

/// **post-filter oracle**：无过滤取 Top (k×depth) 再按 doc 位图过滤，取前 k。
///
/// 这就是 **Step 1 之前的行为**（融合后 post-filter），用作「下推是否回退」的基线。
/// `depth` 过采样是为了消除截断误差：若只对无过滤 Top-k 做过滤，一旦前 k 条恰好
/// 大多不在 allowed 内，oracle 自己就残缺——那不是下推的锅，是基线造得不对。
fn oracle_top_k(
    searcher: &QueryExecutor,
    bits: &DocBits,
    query: &str,
    mode: SearchMode,
    k: usize,
    depth: usize,
) -> Vec<ChunkId> {
    let resp = searcher
        .search(query, mode, k * depth)
        .expect("oracle 检索失败");
    resp.hits
        .iter()
        .filter(|h| bits.contains(h.doc_id))
        .take(k)
        .map(|h| h.chunk_id)
        .collect()
}

/// 过滤档位的上下文（`--filter` 派生出的四样东西）。
///
/// 直接动机是 clippy 的 `too_many_arguments`（阈值 7）——但捆成一束也确实比
/// 四个散参更能表达「它们同生共死」：`filter` 为 `None` 时其余三个必为空/零，
/// 由 [`FilterCtx::none`] 一次性构造，调用方不可能漏掉其中一个。
#[derive(Clone, Copy)]
struct FilterCtx<'a> {
    /// 传给内核的过滤条件；`None` = 不过滤
    filter: Option<&'a Filter>,
    /// `filter` 求得的 doc 位图；`None` = 无过滤。带过滤时用于 oracle 与 `allowed`
    bits: Option<&'a DocBits>,
    /// `bits` 内的 chunk 数（`allowed_chunk_count`），用于「用户视角缺口」的分母
    allowed: usize,
    /// oracle 过采样深度的安全系数，见 [`oracle_depth_for`]
    oracle_depth: usize,
}

impl FilterCtx<'_> {
    /// 无过滤档位：位图与 `allowed` 皆空，过滤相关统计整段跳过。
    fn none() -> Self {
        Self {
            filter: None,
            bits: None,
            allowed: 0,
            oracle_depth: 1,
        }
    }
}

fn eval_effect(
    searcher: &QueryExecutor,
    index: &Index,
    judgments: &[Judgment],
    mode: SearchMode,
    k: usize,
    ctx: FilterCtx<'_>,
) -> ModeResult {
    // 过滤档位下的额外统计（无过滤时整段跳过，成本为零）
    let allowed = ctx.allowed;
    // oracle 深度按选择度自适应：低选择度下固定 10×K 根本凑不够 K 条
    // （1% 选择度 + K=10 需要 ≈1000 条候选才能捞到 10 条 allowed），
    // 固定深度会让 oracle 自己残缺，重合率因此虚高——这是首版实测踩到的坑。
    let depth_eff = oracle_depth_for(index, allowed, k, ctx.oracle_depth);
    let mut hits_sum = 0usize;
    let mut shortfall_sum = 0usize;
    let mut overlap_sum = 0usize;
    let mut oracle_sum = 0usize;

    let mut per_query = Vec::with_capacity(judgments.len());
    for j in judgments {
        let resp = run_once(searcher, &j.query, mode, k, ctx.filter)
            .unwrap_or_else(|e| panic!("检索失败（qid={}）: {e}", j.qid));
        let ranked: Vec<&str> = resp.hits.iter().map(|h| h.source.as_str()).collect();
        let grades = j.grades();
        let thr1 = bench::evaluate(&ranked, &grades, k, 1);
        let thr2 = bench::evaluate(&ranked, &grades, k, 2);

        if let Some(b) = ctx.bits {
            hits_sum += resp.hits.len();
            shortfall_sum += k.min(allowed).saturating_sub(resp.hits.len());
            let oracle = oracle_top_k(searcher, b, &j.query, mode, k, depth_eff);
            let got: HashSet<ChunkId> = resp.hits.iter().map(|h| h.chunk_id).collect();
            overlap_sum += oracle.iter().filter(|c| got.contains(c)).count();
            oracle_sum += oracle.len();
        }
        per_query.push(PerQuery {
            qid: j.qid.clone(),
            qtype: j.qtype.clone(),
            thr1,
            thr2,
        });
    }

    let n = judgments.len().max(1);
    let filter_quality = ctx.bits.map(|_| FilterQuality {
        recall_vs_oracle: overlap_sum as f64 / oracle_sum.max(1) as f64,
        mean_hits: hits_sum as f64 / n as f64,
        mean_shortfall: shortfall_sum as f64 / n as f64,
        oracle_mean_hits: oracle_sum as f64 / n as f64,
    });
    ModeResult {
        per_query,
        filter_quality,
    }
}

/// 见 [`oracle_depth_for`]；oracle 深度的绝对上限。
///
/// 低选择度下「捞够 K 条」需要的候选数会爆炸（0.1% × K=10 ⇒ 约 1 万条），
/// 而每次 oracle 都是一次完整检索（10 万级上 hybrid 单发就是几十毫秒），
/// 不设上限会让 T12 的选择度扫描从"分钟级"变成"小时级"。超限时宁可让
/// 基线残缺并由 `oracle条数` 列自证，也不能让扫描跑不完。
const ORACLE_MAX_DEPTH: usize = 5_000;

/// oracle 的过采样深度：按选择度自适应，目标是让 oracle 稳定凑够 K 条。
///
/// 直觉：选择度 s 下，要捞到 K 条 allowed，无过滤候选需约 `K / s` 条；
/// 再乘 `--oracle-depth` 作为安全系数（默认 10，吸收「Top 区并非均匀含
/// allowed」的偏差）。上界是 [`ORACLE_MAX_DEPTH`] 与全库条数中的较小值，
/// 下界是 `K × depth`（高选择度时 `K/s` 反而小于它，不该缩水）。
///
/// ⚠️ 下界必须先被 [`ORACLE_MAX_DEPTH`] 夹住再传给 `clamp`：`clamp` 在
/// `min > max` 时**无条件 panic**（issue #9），而 `k × depth` 随用户参数
/// 无界增长（`--oracle-depth 600` × k=10 = 6000 > 5000）。两分支必须保持
/// 同一对齐方式——早退分支用 `.min(...)` 天然安全，主分支也如此。
fn oracle_depth_for(index: &Index, allowed: usize, k: usize, oracle_depth: usize) -> usize {
    let total = index.num_chunks().max(1) as f64;
    let sel = allowed as f64 / total;
    let cap = ORACLE_MAX_DEPTH.max(k);
    // saturating_mul：oracle_depth 是乘性安全系数，极端值饱和即可；
    // 乘法溢出 panic 会把「深度过大」变成「bench 崩溃」（issue #9 的相邻路径）
    let floor = k.saturating_mul(oracle_depth).max(k).min(cap);
    if sel <= 0.0 {
        return floor;
    }
    // saturating_mul：两处乘法都可能溢出（k×depth、need×depth），
    // 极端参数下饱和即可；乘法溢出 panic 会把「深度过大」变成
    // 「bench 崩溃」（issue #9 的相邻路径，测试里 usize::MAX 直接踩中）
    let need = ((k as f64 / sel).ceil() as usize).saturating_mul(oracle_depth);
    need.clamp(floor, cap).min(total as usize)
}

/// 过滤质量表：T12 三元数据的「召回」轴 + T14 的判据。
fn print_filter_quality(results: &HashMap<String, ModeResult>, k: usize, allowed: usize) {
    println!("\n== 过滤质量（vs post-filter oracle，K={k}，allowed={allowed}）==");
    println!(
        "{:<8} {:>10} {:>10} {:>12} {:>10}",
        "mode", "平均条数", "平均缺口", "重合率", "oracle条数"
    );
    for name in ["bm25", "vector", "hybrid"] {
        if let Some(q) = results.get(name).and_then(|r| r.filter_quality.as_ref()) {
            // 两种告警语义不同，别混为一谈：
            //   - 重合率 <0.99 → **下推回退**（本次实现的锅，必须查）
            //   - oracle < K   → 基线本身没凑够 K（allowed 内匹配 query 的文档
            //                    就这么少，是数据约束不是缺陷），此时重合率的
            //                    鉴别力有限——分母小，两边同样少就容易"全中"
            let flag = if q.recall_vs_oracle < 0.99 {
                "  ⚠️ 下推回退"
            } else if q.oracle_mean_hits < k as f64 * 0.9 {
                "  （基线<K，鉴别力有限）"
            } else {
                ""
            };
            println!(
                "{:<8} {:>10.2} {:>10.2} {:>12.4} {:>10.2}{}",
                name, q.mean_hits, q.mean_shortfall, q.recall_vs_oracle, q.oracle_mean_hits, flag
            );
        }
    }
    println!(
        "  oracle条数 = post-filter 基线自身的平均条数（<K 时重合率的分母偏小，\n  \
         只能证明「下推没比 post-filter 更差」，不能证明「召回足够」）。"
    );
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
    /// 平均返回条数（无过滤时恒为 K，除非语料不足）
    mean_hits: f64,
    /// 平均用户视角缺口 `min(K, allowed) − len`（无过滤时 `allowed=0`，恒为 0）
    mean_shortfall: f64,
}

fn eval_latency(
    searcher: &QueryExecutor,
    judgments: &[Judgment],
    mode: SearchMode,
    k: usize,
    warmup: usize,
    reps: usize,
    ctx: FilterCtx<'_>,
) -> LatencyResult {
    let allowed = ctx.allowed;
    let mut samples: Vec<f64> = Vec::with_capacity(judgments.len() * reps);
    let mut hits_sum = 0usize;
    let mut shortfall_sum = 0usize;
    let mut n_resp = 0usize;
    for j in judgments {
        for _ in 0..warmup {
            let _ = run_once(searcher, &j.query, mode, k, ctx.filter);
        }
        for _ in 0..reps {
            let t = Instant::now();
            let r = run_once(searcher, &j.query, mode, k, ctx.filter);
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
            if let Ok(resp) = r {
                hits_sum += resp.hits.len();
                shortfall_sum += k.min(allowed).saturating_sub(resp.hits.len());
                n_resp += 1;
            }
        }
    }
    samples.sort_by(|a, b| a.partial_cmp(b).expect("延迟样本无 NaN"));
    let n = samples.len();
    let denom = n_resp.max(1) as f64;
    LatencyResult {
        p50_ms: bench::percentile(&samples, 50.0),
        p99_ms: bench::percentile(&samples, 99.0),
        n,
        mean_hits: hits_sum as f64 / denom,
        mean_shortfall: shortfall_sum as f64 / denom,
    }
}

// ---------------------------------------------------------------------------
// 阶段 D：过滤求值耗时对照（T13）
// ---------------------------------------------------------------------------

/// 对照三条路径的**求值本身**耗时（不含检索）
///
/// 1. `allowed_chunks` —— Step 1 之前的实现：展开成 `HashSet<ChunkId>`，O(N)
/// 2. `doc_bits` + `ChunkFilter` —— Step 1 之后：doc 位图 + 惰性谓词，O(匹配文档数)
/// 3. `doc_bits_scan` —— 全扫兜底：**降级字段实际走的就是它**（评审 P2-2 主场景）
///
/// 顺带做一次一致性校验：路径 1 与路径 2 的允许集大小必须相等
/// （单测里的等价性证明，在真实 fixture 上再验一次）。
fn eval_filter_cost(index: &Index, filter: &Filter, warmup: usize, reps: usize) -> Result<()> {
    use std::hint::black_box;

    macro_rules! time_it {
        ($samples:expr, $e:expr) => {{
            let t = Instant::now();
            let v = $e;
            $samples.push(t.elapsed().as_secs_f64() * 1000.0);
            black_box(v)
        }};
    }

    let mut old_ms = Vec::with_capacity(reps);
    let mut new_ms = Vec::with_capacity(reps);
    let mut scan_ms = Vec::with_capacity(reps);

    for _ in 0..warmup {
        black_box(allowed_chunks(filter, index));
        black_box(doc_bits(filter, index));
        black_box(doc_bits_scan(filter, index));
    }
    let (mut old_n, mut new_n) = (0usize, 0usize);
    for _ in 0..reps {
        let s = time_it!(old_ms, allowed_chunks(filter, index));
        old_n = s.len();
        // 新路径 = doc_bits（字段索引位图）+ ChunkFilter::new（O(匹配文档数) 计数）
        let cf = time_it!(new_ms, ChunkFilter::new(doc_bits(filter, index), index));
        new_n = cf.allowed_count();
        time_it!(scan_ms, doc_bits_scan(filter, index));
    }

    let mean = |v: &Vec<f64>| v.iter().sum::<f64>() / v.len().max(1) as f64;
    let p99 = |v: &Vec<f64>| {
        let mut s = v.clone();
        s.sort_by(|a, b| a.partial_cmp(b).expect("无 NaN"));
        bench::percentile(&s, 99.0)
    };
    println!("\n== 过滤求值耗时（T13：warmup {warmup} + {reps} reps，仅求值，不含检索）==");
    println!("{:<34} {:>12} {:>12}", "路径", "平均(ms)", "P99(ms)");
    println!(
        "{:<34} {:>12.4} {:>12.4}",
        "allowed_chunks（旧·全扫 HashSet）",
        mean(&old_ms),
        p99(&old_ms)
    );
    println!(
        "{:<34} {:>12.4} {:>12.4}",
        "doc_bits + 惰性谓词（新）",
        mean(&new_ms),
        p99(&new_ms)
    );
    println!(
        "{:<34} {:>12.4} {:>12.4}",
        "doc_bits_scan（降级字段实际路径）",
        mean(&scan_ms),
        p99(&scan_ms)
    );
    let speed = if mean(&new_ms) > 0.0 {
        mean(&old_ms) / mean(&new_ms)
    } else {
        f64::INFINITY
    };
    println!("加速比（旧/新，按平均）: {speed:.1}×");

    if old_n != new_n {
        // 这不是性能问题而是**正确性**问题：两条路径的允许集不一致
        // 说明字段索引与全扫语义已经错开（R12）。bench 不 panic（保留现场数据），
        // 但必须显眼——单测 T6/T8 覆盖的是随机 metadata，这里是真实 fixture。
        eprintln!(
            "⚠️⚠️  允许集大小不一致：allowed_chunks={old_n} vs doc_bits={new_n}\n\
             ⚠️⚠️  字段索引与全扫语义已错开（R12），请立即用 T6/T8 复现"
        );
    }

    // 降级状态直接暴露：`ts_ms` / `uuid` 这类高基数字段必然降级，
    // 该字段上的所有过滤查询都会退回 doc_bits_scan（Q-I1 对它收益为 0）
    let degraded: Vec<&str> = collect_fields(filter)
        .iter()
        .filter(|f| index.field_index().is_degraded(f))
        .copied()
        .collect();
    println!(
        "降级字段: {}",
        if degraded.is_empty() {
            "无（全部走字段索引）".to_string()
        } else {
            format!("{degraded:?} → 该字段退化为全扫（Q-I1 对它收益为 0）")
        }
    );
    Ok(())
}

/// 收集过滤条件里出现的所有字段名（用于降级诊断）。
fn collect_fields(filter: &Filter) -> Vec<&str> {
    match filter {
        Filter::Eq { field, .. } | Filter::Range { field, .. } => vec![field.as_str()],
        Filter::And(v) | Filter::Or(v) => v.iter().flat_map(collect_fields).collect(),
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
            let searcher = make_searcher(
                setup,
                Bm25Params { k1, b },
                rrf_k,
                &[1.0, 1.0],
                SearchMode::Bm25,
            )?;
            // 网格搜索只调参、不过滤：整束传 none
            let r = eval_effect(
                &searcher,
                &setup.index,
                judgments,
                SearchMode::Bm25,
                k,
                FilterCtx::none(),
            );
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
    // 过滤质量只在传了 --filter 时存在；写进 json 让脚本能汇总三元数据，
    // 否则「重合率」只活在 stdout 里，无法跨档位对账
    let fq = r.filter_quality.as_ref().map(|q| {
        serde_json::json!({
            "recall_vs_oracle": q.recall_vs_oracle,
            "mean_hits": q.mean_hits,
            "mean_shortfall": q.mean_shortfall,
            "oracle_mean_hits": q.oracle_mean_hits,
        })
    });
    let mut out = serde_json::json!({
        "thr1": {"recall": a1.recall, "mrr": a1.mrr, "ndcg": a1.ndcg, "n": a1.n},
        "thr2": {"recall": a2.recall, "mrr": a2.mrr, "n": a2.n},
        "buckets": buckets,
        "per_query": per_query,
    });
    if let Some(v) = fq {
        out["filter_quality"] = v;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// issue #9：`k × oracle_depth > ORACLE_MAX_DEPTH` 时 `clamp(min, max)`
    /// 的 min > max 无条件 panic。修复后下界先被上限夹住，任何参数组合都不 panic。
    #[test]
    fn oracle深度超限不panic且收敛到上限() {
        let index = Index::new(); // num_chunks = 0，total 走 .max(1) 兜底
        let k = 10usize;

        // 越过 panic 线与溢出线的代表点；早退分支（allowed=0）一并覆盖
        for depth in [501usize, 600, 5_000, usize::MAX / 2, usize::MAX] {
            let d = oracle_depth_for(&index, 1, k, depth);
            assert!(d <= ORACLE_MAX_DEPTH.max(k), "depth={depth}: {d} 超上限");
            let d0 = oracle_depth_for(&index, 0, k, depth);
            assert!(
                d0 <= ORACLE_MAX_DEPTH.max(k),
                "早退 depth={depth}: {d0} 超上限"
            );
        }

        // issue 给的精确断言：depth = ORACLE_MAX_DEPTH / k + 1 时结果恰为上限。
        // 注意空 index 的 total=1，最终还会被 .min(total) 截到 1，
        // 所以断言上限收敛要看 clamp 中间值——用「不小于 min(total, cap)」刻画
        let edge = ORACLE_MAX_DEPTH / k + 1; // 501
        let expect = (ORACLE_MAX_DEPTH.max(k)).min(1); // total=1
        assert_eq!(oracle_depth_for(&index, 1, k, edge), expect);
        assert_eq!(oracle_depth_for(&index, 1, k, 600), expect);
    }

    /// 正常参数下的语义回归：默认 depth=10 的行为与修复前完全一致。
    /// 空 index 的 total 被 .max(1) 兜底为 1，深度必被截到 1 —— 用满语料
    /// 的等价场景验证：sel=1 时 need = k×depth 正是下界本身。
    #[test]
    fn oracle深度默认值语义不变() {
        let index = Index::new();
        // sel=1（allowed=1/total=1）：need = ceil(10/1)×10 = 100，
        // floor = min(100, 5000) = 100，再 .min(total=1) → 1
        assert_eq!(oracle_depth_for(&index, 1, 10, 10), 1);
        // 早退分支（allowed=0）：不受 total 截断（floor 直接返回），保持 100
        assert_eq!(oracle_depth_for(&index, 0, 10, 10), 100);
        // 中选择度（sel=0.5，allowed=1/total=2）：need = ceil(10/0.5)×10 = 200，
        // floor = 100，clamp → 200，.min(total=2) → 2
        // —— total=2 需要真实语料，空 index 无法构造，此行留作行为文档
        assert_eq!(oracle_depth_for(&index, 0, 3, 7), 21); // 3×7 < cap，早退不截断
    }
}
