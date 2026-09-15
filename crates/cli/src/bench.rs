//! `helix bench` —— 效果与性能评测（P5 / T5-03b，p5-design.md 第 7 章）。
//!
//! # 执行流程（7.4）
//!
//! 加载快照（或 --input 重建）→ 加载 judgments（source 校验）→
//! 阶段 A【效果】→ 阶段 B【延迟】→ 阶段 B2【并发吞吐，仅 `--threads` 含 >1 档时】
//! →（--grid）阶段 C【网格】→ 输出。
//!
//! - `--threads N[,N...]`：V2 Step 6 的 **T7-17 / NFR-10 采集点**。默认 `1` ⇒
//!   **不新增任何输出阶段**（与不传该参数逐字一致，S6-T12 的回归防护）；
//!   含 >1 的档位时才跑阶段 B2。
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
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
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
use helix_core::vector::{
    BruteForceIndex, HnswRsIndex, NormalizedVector, VectorIndex, VectorRoute,
};

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
    /// 精排窗口 R（V2 Step 7 / S7-03）。**不传 = 关闭精排**（NoOp，与现状逐字一致）。
    ///
    /// 传了则装载本地精排器（`bge-reranker-v2-m3`）并把窗口放开到 R：
    /// 融合后取 `min(max(k, R), 融合条数)` 条交给精排，再由它排序截回 k 条。
    ///
    /// ⚠️ 需要以 `--features local-rerank` 编译；模型 ≈2.19GB，首次运行会下载。
    /// ⚠️ 与 `--runs > 1` **互斥**（后者刻意重建图，图漂移会污染精排 A/B 的差值）。
    /// ⚠️ `R = 0` 无意义 ⇒ 报错（要关精排请**不传**本参数）。
    #[arg(long, value_name = "R")]
    pub rerank_window: Option<usize>,
    /// 精排 tokenizer 的截断长度（默认 512）。
    ///
    /// ⚠️ 它**烧进**模型 tokenizer（构造期生效）⇒ 必须与 `--rerank-window`
    /// 同时给出，单独给会报错（不静默忽略）。服务于 S7-04 的 max_length 对照档
    /// （512 vs 1024，R48）。
    #[arg(long, value_name = "N")]
    pub rerank_max_length: Option<usize>,
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
    /// 跳过**所有性能测量阶段**（延迟与并发吞吐），只跑效果（可与 `--grid` 同用）
    // 实现提示（2026-09-13 自查）：阶段 B2【并发吞吐】与延迟阶段同在 `if !args.no_latency`
    // 块内 ⇒ 任何「`--threads 1` 不该产出 B2」的断言**不能**配 `--no-latency` 跑
    // （那样必然通过 = 空转）。CI 里那两条断言已按此修正。
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
    /// **并发检索的线程数档位**（逗号分隔；V2 Step 6 · T7-17 / NFR-10 的采集点）。
    ///
    /// - 默认 `1` ⇒ **行为与不传该参数完全一致**（不新增输出阶段；S6-T12 的回归防护）
    /// - 含 >1 的档位（如 `--threads 1,2,4,8`）⇒ 额外跑**阶段 B2【并发吞吐】**：
    ///   每档 N 个 worker 用 `&` 共享同一个 searcher，各自跑**同一批 query 的同一顺序**
    ///   （`QueryExecutor: Sync`，**无需 `Arc`、无需改内核**）。
    /// - 输出：总 QPS、每线程 P50/P99、全局 P50/P99、相对**首档**的加速比；
    ///   同时给出 NFR-10 方案 A 的判定（首档=1 且含 4 时）。
    ///
    /// 口径（设计 §4.6.2 / D-S6-09）：**先跑一趟预热扫描并丢弃**，再测第二趟
    /// （= 预热丢弃 + 顺序交错）；QPS 用**进程内**墙钟（spawn→join），不含进程启动与快照加载。
    /// ⚠️ 并发下的 per-query 延迟**含排队**，不得与 NFR-02（单线程口径）横比。
    /// ⚠️ **正确性是前置条件**：所有档位、所有 worker 的 hits（`chunk_id` + `score`）
    /// 必须**逐位一致**，否则直接 `Err` —— 读取路径本应纯不可变，不一致即真 bug（S6-T11）。
    #[arg(long, default_value = "1", value_name = "N[,N...]")]
    pub threads: String,
    /// 精确兜底阈值（V2 Step 5 / D-S5-02 / S5-04）：**A/B 的唯一开关**。
    ///
    /// - 不传 ⇒ 用内核默认（`HnswRsIndex` 的 `BRUTE_FALLBACK_MAX_ALLOWED`）
    /// - `off` ⇒ `with_brute_fallback(None)`，**关闭**兜底（行为回到 Step 5 之前）
    /// - 数字 N ⇒ 覆盖阈值（`allowed ≤ N` 且谓词为 `Filtered` 时走精确扫描）
    ///
    /// ⚠️ 只对 `--vector-index hnsw` 有意义：`BruteForceIndex` 本来就精确
    /// （`prefers_exact` 恒 `true`），没有"关不关"这回事。
    #[arg(long, value_name = "N|off")]
    pub brute_fallback: Option<String>,
}

/// 评测环境：索引 + 分词器 + 可选向量后端。
struct Setup {
    index: Index,
    analyzer: Box<dyn Analyzer>,
    vectors: Vec<(ChunkId, Vec<f32>)>,
    embedder: Option<LocalEmbedder>,
    backend: Option<VectorBackend>,
    /// 精排器（`None` = 未开启，装配 `NoOpReranker`）。V2 Step 7 / S7-03。
    ///
    /// ⚠️ 放在 `Setup` 而不是每次 `make_searcher` 新建：bench 会**每 mode × 每 run**
    /// 各装配一次 searcher（4 个入口），而精排器可能持有 2.19GB 的常驻 ONNX 会话
    /// （架构 R44）⇒ 必须用 `Arc` **共享同一实例**（重复加载 = 秒级 × N 且内存峰值叠加）。
    /// 身份串随之携带（`dyn Reranker` 上取不到，见 `RerankerHandle`）。
    reranker: Option<crate::RerankerHandle>,
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

    // V2 Step 7 / S7-03：精排参数。**参数面校验（含四条守卫）全部排在资源加载之前**；
    // 唯一会加载模型的 `build_reranker` 排在**所有纯参数校验之后**（理由见下）。
    //
    // ⚠️ 顺序刻意如此：① `resolve` / `check` 是纯参数面（与是否编译 feature 无关）⇒ 必须最先，
    //    否则同一份错配置在默认构建与 `--features local-rerank` 构建下会报不同的错，
    //    而 CI 的 smoke 走默认构建 ⇒ 互斥守卫会变成「只有本地能测」；
    //    ② `build_reranker` 排在 `parse_modes` / `parse_thread_levels` 之后：后者也是纯参数
    //    校验却便宜得多（凭什么叫用户先白等一次 2.19GB 模型加载才看到 `--modes` 拼错？）。
    let rerank_spec = crate::resolve_rerank(args.rerank_window, args.rerank_max_length)?;
    crate::check_rerank_runs(rerank_spec, args.runs)?;

    let modes = parse_modes(&args.modes)?;
    // `--threads 1`（默认）⇒ None ⇒ **不跑阶段 B2**，输出与不传该参数逐字一致（S6-T12）
    let thread_levels = parse_thread_levels(&args.threads)?;
    // ⚠️ 唯一会加载模型的入口（≈2.19GB）⇒ 排在所有纯参数校验之后、`load_setup` 之前。
    let reranker = crate::build_reranker(rerank_spec)?;
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
    let mut setup = load_setup(&args, need_vector, reranker)?;
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
    // V2 Step 7 / S7-03：精排档位必须**显式打印**（A/B 的唯一开关）。
    // 理由同 `--brute-fallback`：标定时看错一行，会把「精排关」的结果当成「精排无效」。
    // 身份串含模型 + max_length + 窗口 ⇒ 一次打印即自证跑的是哪一档（D-S7-08/10）。
    println!(
        "精排: {}",
        match &setup.reranker {
            Some(h) => format!("开启（{}）", h.identity),
            None => "关（NoOp；用 --rerank-window R 开启）".to_string(),
        }
    );
    // S5-04：A/B 开关的生效值必须显式打印——「没传」与「off」语义不同，
    // 标定时看错一行就会把"全部退化成 ANN 基线"当成"兜底无效"。
    println!(
        "精确兜底: {}",
        match parse_brute_fallback(args.brute_fallback.as_deref())? {
            None => format!(
                "内核默认（阈值 {}；用 --brute-fallback off 关闭做 A/B 对照）",
                helix_core::vector::BRUTE_FALLBACK_MAX_ALLOWED
            ),
            Some(None) => "关闭（off）—— 行为回到 Step 5 之前".to_string(),
            Some(Some(n)) => format!("阈值 {n}"),
        }
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
    // S5-04 的 A/B 唯一开关必须进 JSON —— 否则两次跑出的文件无法自证跑的是哪一档
    json["brute_fallback"] = serde_json::json!(match parse_brute_fallback(
        args.brute_fallback.as_deref()
    )? {
        None => "kernel-default".to_string(),
        Some(None) => "off".to_string(),
        Some(Some(n)) => n.to_string(),
    });
    // V2 Step 7 / S7-03：精排是**第二个** A/B 唯一开关，同样必须进 JSON ——
    // 否则两次跑出的文件无法自证跑的是哪一档（`id` 含 max_length 与窗口，
    // 见 `reranker_identity`；D-S7-08 / D-S7-10 要求它可见）。
    json["rerank"] = match (&setup.reranker, rerank_spec) {
        (Some(h), crate::RerankSpec::On { window, .. }) => serde_json::json!({
            "enabled": true,
            "id": h.identity,
            "window": window,
        }),
        _ => serde_json::json!({"enabled": false}),
    };

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
                "{:<8} {:>10} {:>10} {:>10} {:>12} {:>12} {:>10}",
                "mode", "P50(ms)", "P99(ms)", "平均条数", "缺口(bench)", "缺口(内核)", "精确占比"
            );
        } else {
            println!("{:<8} {:>10} {:>10}", "mode", "P50(ms)", "P99(ms)");
        }
        let mut lat_rows: Vec<(SearchMode, LatencyResult)> = Vec::new();
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
                // 「缺口(bench)」与「缺口(内核)」并列（S5-07）：差异本身即
                // `min(K, allowed)` 与 `min(candidate_k, allowed)` 之别，不是重复列。
                // 「精确占比」= `vector_route == Exact` 的响应占比——T7-22 的 A/B 判据。
                println!(
                    "{:<8} {:>10.2} {:>10.2} {:>10.2} {:>12.2} {:>12.2} {:>10.3}",
                    mode_name(mode),
                    lat.p50_ms,
                    lat.p99_ms,
                    lat.mean_hits,
                    lat.mean_shortfall,
                    lat.mean_shortfall_kernel,
                    lat.exact_ratio
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
                    // bench 口径（分母 K）与内核口径（分母 candidate_k + allowed）并列
                    "mean_shortfall": lat.mean_shortfall,
                    "vector_shortfall_kernel": lat.mean_shortfall_kernel,
                    "vector_route_exact_ratio": lat.exact_ratio,
                    "mean_filter_eval_us": lat.mean_filter_eval_us,
                    "mean_bm25_ms": lat.mean_bm25_ms,
                    "mean_vector_ms": lat.mean_vector_ms,
                    "n_metrics": lat.n_metrics,
                }),
            );
            lat_rows.push((mode, lat));
        }

        // D-S5-04：把「兜底没生效」与「过滤求值本身贵」明确分开。
        // 降级字段（如 ts_ms）档位的 `doc_bits_scan` 全扫本身就占 ~8ms，
        // 与向量路正交——没有这一节，那个档位会被误判成"兜底失败"。
        if filter.is_some() {
            println!(
                "\n内核耗时分解（per-lane；Hybrid 下两路走 rayon::join，**区间重叠不可相加**）："
            );
            println!(
                "{:<8} {:>16} {:>12} {:>12} {:>12}",
                "mode", "filter_eval(ms)", "bm25(ms)", "vector(ms)", "n(metrics)"
            );
            for (mode, lat) in &lat_rows {
                println!(
                    "{:<8} {:>16.4} {:>12.4} {:>12.4} {:>12}",
                    mode_name(*mode),
                    lat.mean_filter_eval_us / 1000.0,
                    lat.mean_bm25_ms,
                    lat.mean_vector_ms,
                    lat.n_metrics
                );
            }
            println!(
                "  精确占比 = `Metrics.vector_route == Exact` 的响应占比（0 = 一次没兜底，\
                 1 = 每次都兜底）；\n  缺口(内核) 与 缺口(bench) 的分母不同（候选池 vs K），\
                 两者并列是为了让差异可见；\n  ⚠️ 精确路径下 缺口(内核) 通常为 0，但那不是恒等式：\
                 \n  allowed 来自 Index、扫描枚举的是图中的点，图滞后于索引时该值仍 > 0\
                 \n  —— 那时它反过来是「图未覆盖全部 allowed」的诊断信号。两种读数都必须\
                 \n  连看「精确占比 / route」，不能单看缺口。"
            );
        }
        json["latency"] = latency.into();

        // ---- 3.2 阶段 B2：并发吞吐（T7-17 / NFR-10；仅 --threads 含 >1 档时）----
        if let Some(levels) = &thread_levels {
            json["threads"] = run_concurrent_stage(
                &setup,
                SearchAssembly {
                    bm25_params,
                    rrf_k,
                    rrf_weights: &rrf_weights,
                },
                &modes,
                &judgments,
                &args,
                levels,
                filter.as_ref(),
            )?;
        }
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

fn load_setup(
    args: &BenchArgs,
    need_vector: bool,
    reranker: Option<crate::RerankerHandle>,
) -> Result<Setup> {
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
        reranker,
        vector_kind: args.vector_index.clone(),
    })
}

/// 可选的图 sidecar 来源：`(快照路径, 快照正文 CRC, dim)`。
///
/// 仅 `--index` 路径有值（图随快照同目录，P1-6）；`--input` 内存直建路径为 `None`
/// —— bench 关注检索延迟，冷启动已在 S2-T14 单独测，故内存直建维持重建并打印提示。
type GraphSource = (std::path::PathBuf, u32, u32);

/// 解析 `--brute-fallback`：
///
/// - `None`（未传）⇒ `Ok(None)`：不覆盖，用内核默认阈值；
/// - `"off"` ⇒ `Ok(Some(None))`：显式关闭兜底（S5-T7 的回归对照档）；
/// - 数字 ⇒ `Ok(Some(Some(n)))`：覆盖阈值。
///
/// 三层 `Option` 是刻意的：**"没传" 与 "显式关闭" 必须可区分**——
/// 若把没传也当成关闭，`--filter` 标定档（S5-04）就会在毫不知情的情况下
/// 全部退化成 ANN 基线，标定结论直接反向。
fn parse_brute_fallback(raw: Option<&str>) -> Result<Option<Option<usize>>> {
    let Some(s) = raw else { return Ok(None) };
    match s.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => Ok(Some(None)),
        other => {
            let n: usize = other
                .parse()
                .with_context(|| format!("--brute-fallback 需要数字或 off，收到 {other:?}"))?;
            Ok(Some(Some(n)))
        }
    }
}

/// 把 `--brute-fallback` 施加到 HNSW 后端（两步：先解析、后覆盖）。
///
/// ⚠️ 图**从 sidecar 加载**的路径同样要施加：`from_loaded` 只写产品默认值
/// （D-S5-02），bench A/B 若只覆盖"重建路径"，`--index` 档位会静默用默认阈值、
/// 两轮跑出同一份数据（标定结论反向）。
fn apply_brute_fallback(idx: HnswRsIndex, raw: Option<&str>) -> Result<HnswRsIndex> {
    Ok(match parse_brute_fallback(raw)? {
        None => idx, // 未传：内核默认
        Some(v) => idx.with_brute_fallback(v),
    })
}

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
                // 图加载**单独计时**：NFR-04 的「完整冷启动」= 快照加载 + 图加载，
                // 两者都持久化后应远小于图重建。与 10.2「勿混口径」一致。
                let t = Instant::now();
                let loaded = helix_core::vector::load_graph_checked(
                    base,
                    *body_crc,
                    *dim,
                    vectors.len() as u64,
                    ef,
                    false, // bench 只读：加载后不继续写入，并行开关无意义
                );
                match loaded {
                    Ok(idx) => {
                        eprintln!(
                            "[HNSW 图从 sidecar 加载成功 耗时 {:?}（冷启动快路径；\
                             NFR-04 口径之二；消 R-P5-13 图抖动）]",
                            t.elapsed()
                        );
                        return Ok(VectorBackend::Hnsw(apply_brute_fallback(
                            idx,
                            args.brute_fallback.as_deref(),
                        )?));
                    }
                    Err(reason) => {
                        eprintln!(
                            "[HNSW 图 sidecar 不可用（{reason}），降级重建（冷启动会变慢）；\
                             校验耗时 {:?}]",
                            t.elapsed()
                        )
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
            Ok(VectorBackend::Hnsw(apply_brute_fallback(
                idx,
                args.brute_fallback.as_deref(),
            )?))
        }
        "brute" => {
            if args.brute_fallback.is_some() {
                eprintln!(
                    "⚠️  --brute-fallback 对 --vector-index brute 无意义：\
                     Brute 本来就精确（prefers_exact 恒 true），没有「关不关」这回事"
                );
            }
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
    // V2 Step 7 / S7-03：精排注入。`Arc::clone` ⇒ 每 mode × 每 run 装配的 4 个入口
    // （效果 / 延迟 / 并发 / 网格）共享**同一个**模型实例；未开启时保持默认 NoOp
    // ⇒ 与不传 `--rerank-window` 逐位一致（S7-02 的「零回归」承诺在 CLI 侧也成立）。
    if let Some(h) = &setup.reranker {
        s = s.with_reranker_arc(Arc::clone(&h.reranker));
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
    /// 平均**用户视角**缺口 `min(K, allowed) − len`（无过滤时 `allowed=0`，恒为 0）。
    ///
    /// ⚠️ 与 [`Self::mean_shortfall_kernel`] 口径不同：内核按**融合前**的候选池
    /// `min(candidate_k, allowed)` 算（`candidate_k = max(k×3, 10)`），bench 侧看不到
    /// 内部 `candidate_k`，这里只能用 `K` 代替。两者都叫"缺口"而分母不同，
    /// **不可混用**——两列并列正是为了让这个差异显式（S5-07 修完断链 4 后，
    /// 差异本身变成了"`candidate_k` 与 `k` 之别"的度量，不再是"拿不到"）。
    mean_shortfall: f64,
    /// 平均**内核口径**缺口（`SearchResponse.metrics.vector_shortfall`，S5-07 断链 4）。
    ///
    /// 它才是 V2.1「是否引入 prefilter」判据的口径——与 `Metrics::vector_shortfall`
    /// 逐位同源，不再需要 bench 侧另算一个"近似"。
    mean_shortfall_kernel: f64,
    /// 走**精确路径**的响应占比（`metrics.vector_route == Exact`）。
    ///
    /// T7-22 的 A/B 判据：0.0 = 一次都没兜底（阈值没生效），
    /// 1.0 = 每次都兜底（该档位选择度确实 ≤ 阈值）。
    exact_ratio: f64,
    /// 平均过滤求值耗时（µs，内核口径）——兜底没生效与"过滤求值本身贵"靠它区分
    /// （D-S5-04：降级字段档位上 `doc_bits_scan` 就要 ~8ms）。
    mean_filter_eval_us: f64,
    /// BM25 路平均耗时（ms，per-lane，D-S5-06）
    mean_bm25_ms: f64,
    /// 向量路平均耗时（ms，per-lane；**含精确扫描的 O(N) 遍历**）
    mean_vector_ms: f64,
    /// 成功取到 `metrics` 的响应数（**分母的自证**：0 表示内核没暴露 metrics，
    /// 那一列的数字就没有意义）
    n_metrics: usize,
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
    // ↓ V2 Step 5 / S5-07：内核口径采集（断链 4 的修复点）
    let mut shortfall_kernel_sum = 0usize;
    let mut exact_count = 0usize;
    let mut filter_eval_us_sum = 0f64;
    let mut bm25_ms_sum = 0f64;
    let mut vector_ms_sum = 0f64;
    let mut n_metrics = 0usize;
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

                let m = &resp.metrics;
                shortfall_kernel_sum += m.vector_shortfall;
                if m.vector_route == VectorRoute::Exact {
                    exact_count += 1;
                }
                filter_eval_us_sum += m.filter_eval.as_secs_f64() * 1e6;
                bm25_ms_sum += m.bm25_elapsed.as_secs_f64() * 1000.0;
                vector_ms_sum += m.vector_elapsed.as_secs_f64() * 1000.0;
                n_metrics += 1;
            }
        }
    }
    samples.sort_by(|a, b| a.partial_cmp(b).expect("延迟样本无 NaN"));
    let n = samples.len();
    let denom = n_resp.max(1) as f64;
    // 内核口径的分母是**观测到 metrics 的响应数**，与 bench 口径的 n_resp 分开记：
    // 若两者不等，说明有响应没带 metrics（内核侧漏填），而不是"指标为 0"
    let kdenom = n_metrics.max(1) as f64;
    LatencyResult {
        p50_ms: bench::percentile(&samples, 50.0),
        p99_ms: bench::percentile(&samples, 99.0),
        n,
        mean_hits: hits_sum as f64 / denom,
        mean_shortfall: shortfall_sum as f64 / denom,
        mean_shortfall_kernel: shortfall_kernel_sum as f64 / kdenom,
        exact_ratio: exact_count as f64 / kdenom,
        mean_filter_eval_us: filter_eval_us_sum / kdenom,
        mean_bm25_ms: bm25_ms_sum / kdenom,
        mean_vector_ms: vector_ms_sum / kdenom,
        n_metrics,
    }
}

// ---------------------------------------------------------------------------
// 阶段 B2：并发检索吞吐（V2 Step 6 · T7-17 / NFR-10 方案 A）
// ---------------------------------------------------------------------------

/// NFR-10（方案 A，评审 Q4 已拍板）的相对提升阈值：
/// `--threads 4` 的 QPS ≥ `--threads 1` 的 QPS × 本值（近线性，允许 40% 折损）。
///
/// ⚠️ 需求文档里这个数值仍标「**拟**」，待 S6-08 实测后定稿（同 Step 5 的 NFR-13 先例）。
/// ⚠️ **不得**把它当绝对 QPS 目标（评审明确反对方案 B：绝对 QPS 与机器强耦合）。
const NFR10_MIN_SPEEDUP: f64 = 2.5;

/// 档位上界：每档会**真的 `spawn` 这么多 OS 线程**，误传大数（如 `--threads 100000`）
/// 会直接耗尽进程资源。上界给得比任何真实机器都宽，只用来挡住明显的误输入（评审 P3-5）。
const MAX_THREAD_LEVEL: usize = 256;

/// 解析 `--threads` 的档位列表（逗号分隔，去重保序）。
///
/// 返回 `None` 表示「**只有一档、且为 1**」⇒ **不跑阶段 B2**，
/// `bench --threads 1` 的输出与不传该参数**逐字一致**（S6-T12 的回归防护）。
fn parse_thread_levels(spec: &str) -> Result<Option<Vec<usize>>> {
    let mut levels: Vec<usize> = Vec::new();
    for part in spec.split(',') {
        let t = part.trim().parse::<usize>().with_context(|| {
            format!("--threads 解析失败：`{part}` 不是非负整数（原串 `{spec}`）")
        })?;
        if t == 0 {
            bail!("--threads 至少为 1（收到 0；原串 `{spec}`）");
        }
        if t > MAX_THREAD_LEVEL {
            bail!(
                "--threads 档位 {t} 超过上限 {MAX_THREAD_LEVEL}（原串 `{spec}`）\
                 —— 每档会真的 spawn 同等数量的 OS 线程，请改用与核数同量级的档位"
            );
        }
        if !levels.contains(&t) {
            levels.push(t);
        }
    }
    if levels.len() == 1 && levels[0] == 1 {
        return Ok(None);
    }
    Ok(Some(levels))
}

/// 单个 worker 的产出。
///
/// **不跨线程共享可变状态**：每个 worker 自己攒样本、自己算签名 ⇒ 无需锁，
/// 也就不会有「锁顺序导致的非确定性」污染 NFR-10 的正确性前置条件。
struct WorkerOut {
    /// 每次检索的耗时（ms；**不含预热**）
    samples: Vec<f64>,
    /// hits 的**逐位签名**（`chunk_id` + `score.to_bits()`，按 query / reps 顺序；
    /// 命中条数也入签名，否则「少返回一条」不会改变签名）
    signature: u64,
    /// 本 worker **计时窗内**的墙钟（s）—— 从 barrier 放行起算，**不含预热**
    measured_secs: f64,
    /// 计时窗内**失败**的检索次数
    ///
    /// ⚠️ 必须显式计数：签名只吃成功响应，若把 `Err` 静默丢弃，「全部失败」时每个
    /// worker 的签名都会退化成同一个**空哈希常量**，于是「各档签名一致」这条前置
    /// 条件被**空满足**（评审 P1-1）。
    n_err: usize,
    /// 计时窗内成功检索返回的**命中总条数**（= 0 ⇒ 一条都没搜到，「一致」无信息量）
    n_hits: u64,
    /// 首个失败的原因（诊断用；只留第一条，避免刷屏）
    first_err: Option<String>,
}

/// 一档线程数的并发测量结果。
struct ThreadLevelReport {
    threads: usize,
    /// 计入 QPS 的检索总次数（`threads × queries × reps`，**不含预热**）
    n_total: usize,
    /// 墙钟（s）——**进程内**读数：`spawn → join`，不含进程启动与快照加载
    wall_secs: f64,
    /// 每 worker 的 `(P50, P99)`（ms）——「吞吐上去了但尾延迟炸了」靠它发现（D-S6-09 辅指标）
    per_thread: Vec<(f64, f64)>,
    /// 全样本合并后的 `(P50, P99)`（ms）
    global: (f64, f64),
    /// **每个 worker** 的 hits 签名 ⇒ 正确性前置条件的证据（集合大小必须为 1）
    worker_signatures: Vec<u64>,
    /// 本档位**失败**的检索次数（> 0 ⇒ 前置条件不成立，绝不出表）
    n_err: usize,
    /// 本档位成功检索返回的**命中总条数**（= 0 ⇒ 「一条都没搜到」）
    n_hits: u64,
    /// 首个失败原因（诊断用）
    first_err: Option<String>,
}

impl ThreadLevelReport {
    fn qps(&self) -> f64 {
        if self.wall_secs > 0.0 {
            self.n_total as f64 / self.wall_secs
        } else {
            f64::NAN
        }
    }
}

/// 一次并发批量的规格：**「同一批 query 的同一顺序」**这层语义的名字。
///
/// 收成结构体不是为了让签名好看：`--k/--reps/--warmup/--filter` 四者必须**在同一个档位的
/// 所有 worker 上完全一致**（否则各 worker 跑的就不是同一批），把它们绑在一起可以让
/// 「不一致」在类型层面写不出来。
#[derive(Clone, Copy)]
struct BatchSpec<'a> {
    mode: SearchMode,
    k: usize,
    warmup: usize,
    reps: usize,
    filter: Option<&'a Filter>,
}

/// 构造 searcher 所需的装配参数（与 [`make_searcher`] 一一对应）。
#[derive(Clone, Copy)]
struct SearchAssembly<'a> {
    bm25_params: Bm25Params,
    rrf_k: f32,
    rrf_weights: &'a [f32],
}

/// 跑**一档**线程数：N 个 worker 用 `&` 共享 `searcher`，各自跑**同一批 query 的同一顺序**。
///
/// 共享可行性已由 `tests::queryexecutor可跨线程共享` 在编译期钉住（`QueryExecutor: Sync`）
/// ⇒ **不需要 `Arc`、不需要改内核**（设计 §4.6.1）。
fn concurrent_batch(
    searcher: &QueryExecutor,
    judgments: &[Judgment],
    spec: BatchSpec<'_>,
    threads: usize,
) -> Result<ThreadLevelReport> {
    let BatchSpec {
        mode,
        k,
        warmup,
        reps,
        filter,
    } = spec;
    // 所有 worker 的热身都做完再一起放行 —— 否则「先热完的 worker」的计时窗会被
    // 「后热完的 worker」的预热流量污染，各 worker 的窗也不同步（评审 P2-1）。
    let barrier = std::sync::Barrier::new(threads);
    let outs: Vec<WorkerOut> = thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    // ① 预热：所有 query 各跑 `warmup` 次后**丢弃**。
                    //    ⚠️ 这段**必须**落在计时窗之外：旧实现把它套在 `t0..t0+wall` 里，
                    //    而 `n_total` 只算 reps ⇒ 分子分母口径不一致，QPS 被系统性低估
                    //    `reps/(warmup+reps)`（默认 20/23 ≈ 13%；评审 P2-1）。
                    for j in judgments {
                        for _ in 0..warmup {
                            let _ = run_once(searcher, &j.query, mode, k, filter);
                        }
                    }
                    // ② 同步点：N 个 worker 全部热完，计时窗从此刻开始
                    barrier.wait();
                    let t0 = Instant::now();
                    let mut samples = Vec::with_capacity(judgments.len() * reps);
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    let mut n_err = 0usize;
                    let mut n_hits = 0u64;
                    let mut first_err: Option<String> = None;
                    for j in judgments {
                        for _ in 0..reps {
                            let t = Instant::now();
                            let r = run_once(searcher, &j.query, mode, k, filter);
                            samples.push(t.elapsed().as_secs_f64() * 1000.0);
                            // 正确性签名：只吃**成功的**响应、按 query/reps 顺序
                            // ⇒ 与线程数无关，可跨档、跨 worker 逐位比对（S6-T11）
                            match r {
                                Ok(resp) => {
                                    n_hits += resp.hits.len() as u64;
                                    resp.hits.len().hash(&mut hasher);
                                    for h in &resp.hits {
                                        h.chunk_id.hash(&mut hasher);
                                        h.score.to_bits().hash(&mut hasher);
                                    }
                                }
                                // ⚠️ 失败**不能**静默丢弃（评审 P1-1）：全部失败时签名会
                                // 退化成同一个空哈希常量 ⇒ 前置条件被**空满足**。这里
                                // 计数 + 留因，由 check_concurrency_precondition 统一拒绝。
                                Err(e) => {
                                    n_err += 1;
                                    if first_err.is_none() {
                                        first_err = Some(e.to_string());
                                    }
                                }
                            }
                        }
                    }
                    WorkerOut {
                        samples,
                        signature: hasher.finish(),
                        measured_secs: t0.elapsed().as_secs_f64(),
                        n_err,
                        n_hits,
                        first_err,
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("worker 只做只读检索，不应 panic"))
            .collect()
    });
    // 并发窗口 = 各 worker 计时窗的**最大值**（它们都从 barrier 放行起算）
    let wall_secs = outs.iter().map(|o| o.measured_secs).fold(0.0_f64, f64::max);

    let mut per_thread = Vec::with_capacity(threads);
    let mut worker_signatures = Vec::with_capacity(threads);
    let mut all: Vec<f64> = Vec::with_capacity(threads * judgments.len() * reps);
    let mut n_err = 0usize;
    let mut n_hits = 0u64;
    let mut first_err: Option<String> = None;
    for mut o in outs {
        o.samples
            .sort_by(|a, b| a.partial_cmp(b).expect("延迟样本无 NaN"));
        per_thread.push((
            bench::percentile(&o.samples, 50.0),
            bench::percentile(&o.samples, 99.0),
        ));
        worker_signatures.push(o.signature);
        n_err += o.n_err;
        n_hits += o.n_hits;
        first_err = first_err.or(o.first_err);
        all.extend(o.samples);
    }
    all.sort_by(|a, b| a.partial_cmp(b).expect("延迟样本无 NaN"));
    Ok(ThreadLevelReport {
        threads,
        n_total: threads * judgments.len() * reps,
        wall_secs,
        per_thread,
        global: (bench::percentile(&all, 50.0), bench::percentile(&all, 99.0)),
        worker_signatures,
        n_err,
        n_hits,
        first_err,
    })
}

/// 并发正确性**前置条件**的统一判据（S6-T11；评审 P1-1 收口）。
///
/// 三条缺一不可 —— 少了任何一条，这组检查都能被「什么都没测到」**空满足**：
///
/// 1. **每一次检索都必须成功**：签名只吃成功响应 ⇒ 若把 `Err` 静默丢弃，「全部
///    失败」时每个 worker 的签名都等于同一个**空哈希常量**，第 3 条会被空满足；
/// 2. **至少要有命中**：query 与快照完全对不上（`--queries` / `--filter` / 模式
///    不匹配）时「各档结果一致」毫无信息量 —— 全空同样一致；
/// 3. **所有档位、所有 worker 的签名完全相同**：读路径纯不可变（设计 §2.6），
///    这条**应当**成立；不成立即真 bug（浮点环境变量 / 锁顺序 / 分配器非确定性）。
///
/// 抽成独立函数（而不是留在 `run_concurrent_stage` 里）是为了让三条判据都能被
/// **合成数据直接单测** —— 否则「全失败 / 全空」这两条只能靠造真实故障来覆盖。
fn check_concurrency_precondition(mode: SearchMode, rows: &[ThreadLevelReport]) -> Result<()> {
    let n_err: usize = rows.iter().map(|r| r.n_err).sum();
    if n_err > 0 {
        let detail = rows
            .iter()
            .find_map(|r| r.first_err.clone())
            .unwrap_or_else(|| "（无错误详情）".into());
        bail!(
            "并发测量出现**检索失败**：mode={} 共 {n_err} 次失败。失败的检索不可计入 QPS \
             （它既没有结果、耗时也不代表检索成本）⇒ 本次吞吐数字不予采信。首个错误：{detail}",
            mode_name(mode)
        );
    }
    let n_hits: u64 = rows.iter().map(|r| r.n_hits).sum();
    if n_hits == 0 {
        bail!(
            "并发测量**未取到任何命中**（mode={}，共 {} 次检索全部返回 0 条）。\
             此时「各档 hits 逐位一致」不构成任何正确性证据（全空也一致）⇒ 前置条件被空满足。\
             请检查 --queries / --filter / --modes 是否与快照匹配。",
            mode_name(mode),
            rows.iter().map(|r| r.n_total).sum::<usize>()
        );
    }
    let sigs: HashSet<u64> = rows
        .iter()
        .flat_map(|r| r.worker_signatures.iter().copied())
        .collect();
    if sigs.len() != 1 {
        let detail: Vec<String> = rows
            .iter()
            .map(|r| format!("{} 线程 → {:?}", r.threads, r.worker_signatures))
            .collect();
        bail!(
            "并发正确性前置条件不成立（S6-T11）：mode={} 的 hits 签名不一致 \
             —— 读路径本应纯不可变，出现差异即真 bug（吞吐数字不予采信）。各档签名：{}",
            mode_name(mode),
            detail.join(" ｜ ")
        );
    }
    Ok(())
}

/// 档位给不出 NFR-10 判定时的说明文案（评审 P3-6；**复审 P3-11 修正**）。
///
/// 三种情形**必须分开说** —— 旧实现把三条路径都写成「档位不含 t=1 ⇒ 缺单线程锚点」，
/// 而 `--threads 1,2` 明明有 t=1 的**数据行**在表里、缺的是**分子** `QPS(4)`；
/// 把同一句「缺锚点」用在缺口完全不同的场景上属**诊断失实**。
fn no_verdict_notice(levels: &[usize]) -> &'static str {
    match (levels.contains(&1), levels.contains(&4)) {
        (true, false) => {
            "\n  ⚠️ 档位不含 t=4 ⇒ **无 NFR-10 判定**（缺分子 `QPS(4)`）；\
             单线程锚点 t=1 在场，签名一致仍可作 S6-T11 的证据。"
        }
        (false, true) => {
            "\n  ⚠️ 档位不含 t=1 ⇒ **缺单线程锚点**：以上签名一致只能证明各并发档之间一致，\
             **不能**替代「与 1 线程逐位一致」（S6-T11）；同时无 NFR-10 判定（缺分母 `QPS(1)`）。"
        }
        (false, false) => {
            "\n  ⚠️ 档位既不含 t=1 也不含 t=4 ⇒ **无 NFR-10 判定**（分子与分母都不存在），\
             且**缺单线程锚点**：签名一致只能证明各并发档之间一致，\
             **不能**替代「与 1 线程逐位一致」（S6-T11）。"
        }
        // 同时含 1 与 4 时调用点走的是 `if let` 分支、不会到这里；这里留空串而非 `unreachable!`，
        // 避免把一个「不该发生」变成一个能把整段评测打断的路径。
        (true, true) => "",
    }
}

/// 跑「并发吞吐」阶段，返回可并入 `--json` 的对象。
///
/// 顺序：每档**先跑一趟预热扫描并丢弃**，再按 `levels` 顺序测第二趟
/// —— 这同时满足评审要求的「预热丢弃首轮」与「顺序交错」两条口径（§4.6.2 / D-S6-08）。
///
/// ⚠️ 各 mode 的表**先攒进 `report` 再统一吐**（评审 P3-9）：任一模态的前置条件不成立
/// 就直接 `Err`，此时 stdout 上**不留**前序 mode「看起来正常」的 QPS 表。
fn run_concurrent_stage(
    setup: &Setup,
    asm: SearchAssembly<'_>,
    modes: &[SearchMode],
    judgments: &[Judgment],
    args: &BenchArgs,
    levels: &[usize],
    filter: Option<&Filter>,
) -> Result<serde_json::Value> {
    use std::fmt::Write as _;

    println!("\n== 并发吞吐（V2 Step 6 · T7-17 / NFR-10 方案 A）==");
    println!(
        "  档位 {levels:?}（加速比相对**首档 {}**）；每档 N 个 worker 共享同一 searcher，\
         各自跑**同一批 query 的同一顺序**（{} 条 × {} reps）。",
        levels[0],
        judgments.len(),
        args.reps
    );
    println!(
        "  口径：先跑一趟预热扫描并丢弃，再测第二趟；QPS 用**进程内**墙钟，每个 worker 在自己的\
         **预热完成同步点**之后起算、到 join 为止 ⇒ **不含**进程启动、快照加载与预热。"
    );
    println!("  ⚠️ 并发下 per-query 延迟**含排队**，不得与 NFR-02（单线程口径）横比。");

    // 各 mode 的表先攒后吐（理由见函数 doc）
    let mut report = String::new();
    let mut out = serde_json::Map::new();
    for &mode in modes {
        let searcher = make_searcher(setup, asm.bm25_params, asm.rrf_k, asm.rrf_weights, mode)?;
        let spec = BatchSpec {
            mode,
            k: args.k,
            warmup: args.warmup,
            reps: args.reps,
            filter,
        };
        // 预热扫描（丢弃）
        for &t in levels {
            let _ = concurrent_batch(&searcher, judgments, spec, t)?;
        }
        let mut rows = Vec::with_capacity(levels.len());
        for &t in levels {
            rows.push(concurrent_batch(&searcher, judgments, spec, t)?);
        }

        // ⚠️ 正确性是**前置条件**（先正确、后吞吐；评审对 D-S6-08 的补充 ①），判据见
        //    `check_concurrency_precondition` —— 「无失败 / 有命中 / 签名一致」三条缺一
        //    不可，否则会被「什么都没测到」**空满足**（评审 P1-1）。
        check_concurrency_precondition(mode, &rows)?;

        let _ = writeln!(report, "\n  mode={}", mode_name(mode));
        let _ = writeln!(
            report,
            "  {:<8} {:>12} {:>10} {:>22} {:>13} {:>13}",
            "threads", "QPS", "加速比", "每线程P50(ms)范围", "全局P50(ms)", "全局P99(ms)"
        );
        let base_qps = rows[0].qps();
        let mut mode_json = serde_json::Map::new();
        for r in &rows {
            let q = r.qps();
            let speedup = if base_qps > 0.0 {
                q / base_qps
            } else {
                f64::NAN
            };
            let lo = r
                .per_thread
                .iter()
                .map(|x| x.0)
                .fold(f64::INFINITY, f64::min);
            let hi = r
                .per_thread
                .iter()
                .map(|x| x.0)
                .fold(f64::NEG_INFINITY, f64::max);
            let _ = writeln!(
                report,
                "  {:<8} {:>12.1} {:>9.2}× {:>22} {:>13.3} {:>13.3}",
                r.threads,
                q,
                speedup,
                format!("[{lo:.3}, {hi:.3}]"),
                r.global.0,
                r.global.1
            );
            let per_thread: Vec<String> = r
                .per_thread
                .iter()
                .enumerate()
                .map(|(i, (p50, p99))| format!("w{i}: P50={p50:.3} P99={p99:.3}"))
                .collect();
            let _ = writeln!(report, "      {}", per_thread.join(" ｜ "));
            mode_json.insert(
                r.threads.to_string(),
                serde_json::json!({
                    "threads": r.threads,
                    "qps": q,
                    "speedup_vs_first_level": speedup,
                    "wall_secs": r.wall_secs,
                    "n_total": r.n_total,
                    "per_thread_p50_ms": r.per_thread.iter().map(|x| x.0).collect::<Vec<_>>(),
                    "per_thread_p99_ms": r.per_thread.iter().map(|x| x.1).collect::<Vec<_>>(),
                    "global_p50_ms": r.global.0,
                    "global_p99_ms": r.global.1,
                    "hits_signature": r.worker_signatures.first().copied().unwrap_or(0),
                }),
            );
        }

        // NFR-10（方案 A）判定：只在**同时测到 1 与 4** 时给（否则分母不存在）
        let q1 = rows.iter().find(|r| r.threads == 1).map(|r| r.qps());
        let q4 = rows.iter().find(|r| r.threads == 4).map(|r| r.qps());
        if let (Some(q1), Some(q4)) = (q1, q4) {
            let ratio = q4 / q1;
            let pass = ratio >= NFR10_MIN_SPEEDUP;
            let _ = writeln!(
                report,
                "\n  NFR-10（方案 A / D-S6-08）：QPS(4)/QPS(1) = {ratio:.2}，阈值 {NFR10_MIN_SPEEDUP:.1} ⇒ {}",
                if pass { "✅ PASS" } else { "❌ FAIL" }
            );
            mode_json.insert(
                "nfr10".into(),
                serde_json::json!({
                    "qps_threads_1": q1,
                    "qps_threads_4": q4,
                    "ratio": ratio,
                    "threshold": NFR10_MIN_SPEEDUP,
                    "verdict": if pass { "pass" } else { "fail" },
                }),
            );
        } else {
            // 给不出判定（缺 t=1 或 t=4）。三种情形**分开说**（复审 P3-11：旧版把
            // `--threads 1,2` 也说成「缺单线程锚点」，而该场景 t=1 的数据行就在表里）。
            // 不断言失败（`--threads 2,4` 的探索性用法仍可用），但必须把缺口说准。
            debug_assert!(
                !(levels.contains(&1) && levels.contains(&4)),
                "if-let 的 else 分支不可能同时含 1 与 4"
            );
            let _ = writeln!(report, "{}", no_verdict_notice(levels));
        }
        mode_json.insert("baseline_threads".into(), serde_json::json!(levels[0]));
        out.insert(mode_name(mode).to_string(), mode_json.into());
    }
    // 所有 mode 的前置条件都过了 ⇒ 一次性吐出（见函数 doc 的「先攒后吐」）
    print!("{report}");
    Ok(out.into())
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
    use helix_core::document::DocRecord;

    /// S6-T12 + 边界：`--threads` 的档位解析。
    ///
    /// 🔑 最关键的是第一条 `"1" -> None`：**只有一档且为 1 时不跑阶段 B2** ——
    /// 这正是「`bench --threads 1` 的输出与不传该参数**逐字一致**」的结构性保证
    /// （端到端逐字比对另见 CI 的 `bench --threads 1 与默认一致` 一步）。
    #[test]
    fn 并发档位解析() {
        assert_eq!(
            parse_thread_levels("1").unwrap(),
            None,
            "单档 1 ⇒ 不跑并发阶段"
        );
        assert_eq!(parse_thread_levels("4").unwrap(), Some(vec![4]));
        assert_eq!(
            parse_thread_levels("1,2,4,8").unwrap(),
            Some(vec![1, 2, 4, 8])
        );
        // 去重**保序**（顺序即「交错扫描」的顺序，不可重排）
        assert_eq!(parse_thread_levels("4,2,4").unwrap(), Some(vec![4, 2]));
        // 宽容空白
        assert_eq!(parse_thread_levels(" 2 , 2 ").unwrap(), Some(vec![2]));
        // 非法：0 / 非数字 / 空串
        for bad in ["0", "1,0", "a", "1,a", ""] {
            assert!(parse_thread_levels(bad).is_err(), "`{bad}` 应被拒绝");
        }
        // 非法：超过档位上界（评审 P3-5）—— 误传大数会真的 spawn 这么多 OS 线程
        for bad in ["257", "1,100000"] {
            let e = parse_thread_levels(bad).expect_err("超过上界必须被拒绝");
            assert!(
                e.to_string().contains("超过上限"),
                "`{bad}` 的错误信息应点明上限，实得：{e}"
            );
        }
        // 边界：恰好等于上界应被接受（上界是「含」）
        assert_eq!(
            parse_thread_levels(&MAX_THREAD_LEVEL.to_string()).unwrap(),
            Some(vec![MAX_THREAD_LEVEL])
        );
    }

    /// 合成一个档位报告（供 [`并发前置条件三条判据都有牙齿`] 直接构造判据输入）。
    fn 合成档位(
        threads: usize,
        n_err: usize,
        n_hits: u64,
        signatures: Vec<u64>,
    ) -> ThreadLevelReport {
        ThreadLevelReport {
            threads,
            n_total: threads * 6 * 3,
            wall_secs: 1.0,
            per_thread: vec![(1.0, 1.0); threads],
            global: (1.0, 1.0),
            worker_signatures: signatures,
            n_err,
            n_hits,
            first_err: if n_err > 0 {
                Some("合成错误：检索失败".into())
            } else {
                None
            },
        }
    }

    /// 🔴 评审 **P1-1**：并发前置条件的三条判据**都**必须有牙齿。
    ///
    /// 每条判据都各自能被「什么都没测到」**空满足**，所以本测试为每种空满足各造一个
    /// 合成样本、逐条要求 `Err`，再用一个健康样本要求 `Ok`（否则「永远报错」也会变绿）。
    /// 与被测实现同构的只有 [`check_concurrency_precondition`] 本身，**不含**并发调度
    /// ⇒ 秒级、无模型、可进 CI。
    #[test]
    fn 并发前置条件三条判据都有牙齿() {
        use std::collections::hash_map::DefaultHasher;

        // 「全失败」时每个 worker 的签名 = `DefaultHasher` 无输入时的 `finish()`。
        // 现算而非常量：换工具链也不会让本测试失效，且**把机制写在测试里**。
        let empty_sig = DefaultHasher::new().finish();

        // ① 健康样本：无失败 + 有命中 + 签名一致 ⇒ 必须 Ok
        let healthy = vec![
            合成档位(1, 0, 60, vec![7]),
            合成档位(4, 0, 60, vec![7, 7, 7, 7]),
        ];
        assert!(
            check_concurrency_precondition(SearchMode::Bm25, &healthy).is_ok(),
            "健康样本必须通过 —— 否则下面的「都必须 Err」只是「永远 Err」"
        );

        // ② 全部检索失败：**这正是旧实现被空满足的场景**。先反证「只看签名集合大小
        //    会判通过」，再要求新判据把它拦下。
        let all_err = vec![
            合成档位(1, 0, 60, vec![empty_sig]),
            合成档位(4, 36, 0, vec![empty_sig; 4]),
        ];
        let sigs: HashSet<u64> = all_err
            .iter()
            .flat_map(|r| r.worker_signatures.iter().copied())
            .collect();
        assert_eq!(
            sigs.len(),
            1,
            "空满足场景下「签名集合大小为 1」确实成立 —— 只靠这一条判据会被骗（P1-1 的机理）"
        );
        let e = check_concurrency_precondition(SearchMode::Vector, &all_err)
            .expect_err("存在失败检索时必须拒绝");
        assert!(
            e.to_string().contains("检索失败") && e.to_string().contains("合成错误"),
            "必须报出「检索失败」并带首个错误原因，实得：{e}"
        );

        // ③ 全部成功但**一条命中都没有** ⇒ 「各档一致」毫无信息量，同样必须拒绝
        let no_hits = vec![合成档位(1, 0, 0, vec![7]), 合成档位(4, 0, 0, vec![7; 4])];
        let e = check_concurrency_precondition(SearchMode::Bm25, &no_hits)
            .expect_err("零命中时必须拒绝（前置条件被空满足）");
        assert!(
            e.to_string().contains("未取到任何命中"),
            "必须报出「未取到任何命中」，实得：{e}"
        );

        // ④ 签名跨档不一致 ⇒ 真 bug，必须拒绝
        let mismatch = vec![
            合成档位(1, 0, 60, vec![7]),
            合成档位(4, 0, 60, vec![7, 7, 8, 7]),
        ];
        let e = check_concurrency_precondition(SearchMode::Hybrid, &mismatch)
            .expect_err("签名不一致时必须拒绝");
        assert!(
            e.to_string().contains("签名不一致"),
            "必须报出「签名不一致」，实得：{e}"
        );
    }

    /// S6-T11：**同一批 query 在 1 / 4 线程下的 `hits` 必须逐位一致**。
    ///
    /// 三个断言缺一不可，否则「两档都调用了同一个错函数」也能让它变绿：
    /// 1. 同一档内 N 个 worker 的签名互相一致；
    /// 2. 4 线程的签名 == 1 线程的签名（跨档，S6-T11 的原文判据）；
    /// 3. 两者都 == **手写朴素单线程循环**的签名 —— 这一条是本测试的**独立性来源**
    ///    （变异验证正是打它：把签名的算法改错，只有第 3 条会红）。
    ///
    /// 复审 P3-11：**档位缺口的说明必须分三种情形**，不能共用一句话。
    ///
    /// 旧实现把 `--threads 1,2`（t=1 在场、缺的是分子 `QPS(4)`）也说成
    /// 「档位不含 t=1 ⇒ 缺单线程锚点」，与表里真实存在的 t=1 数据行矛盾 ⇒ 诊断失实。
    /// 变异点：把 `(true, false)` 分支改成与 `(false, true)` 相同文案 ⇒ 第 ① 组断言报红。
    #[test]
    fn 无判定档位的说明分三种情形() {
        // ① 有 t=1、无 t=4 ⇒ 只说「缺分子」，**不得**出现「缺单线程锚点」
        let a = no_verdict_notice(&[1, 2]);
        assert!(a.contains("不含 t=4") && a.contains("缺分子"), "{a}");
        assert!(
            !a.contains("缺单线程锚点"),
            "t=1 数据行在场时不应报「缺锚点」，否则与表内容矛盾：{a}"
        );
        // ② 有 t=4、无 t=1 ⇒ 必须同时点明「缺锚点」与「缺分母」
        let b = no_verdict_notice(&[2, 4]);
        assert!(
            b.contains("不含 t=1") && b.contains("缺单线程锚点") && b.contains("缺分母"),
            "{b}"
        );
        // ③ 两者皆无 ⇒ 锚点缺失与判定缺失都要说
        let c = no_verdict_notice(&[2]);
        assert!(
            c.contains("既不含 t=1 也不含 t=4") && c.contains("缺单线程锚点"),
            "{c}"
        );
        // ④ 1 与 4 都在 ⇒ 调用点走 if 分支、不会到这里；返回空串而非 panic（不制造打断点）
        assert!(no_verdict_notice(&[1, 2, 4]).is_empty());
    }

    /// 走 BM25（**不需要模型**）⇒ 秒级、可进 CI（设计 §7 对 S6-T11 的「✅（小语料）」）。
    #[test]
    fn 并发检索结果逐位一致() {
        let analyzer = MixedAnalyzer::new();
        let chunker = Chunker::default();
        let mut index = Index::new();
        for i in 0..40 {
            let doc = DocRecord {
                doc_id: 0,
                source: format!("doc-{i}.md"),
                metadata: serde_json::json!({}),
                content_hash: 0,
            };
            // 文本单调递增 ⇒ 各 query 的命中集不同，签名才有区分度
            let text = format!("检索 文档编号 {i} 并发 词条{i}");
            index.add(doc, chunker.chunk(0, &text), &analyzer).unwrap();
        }
        let searcher = QueryExecutor::new(&index, &analyzer);
        let judgments: Vec<Judgment> = (0..6)
            .map(|i| Judgment {
                qid: format!("q{i}"),
                query: format!("词条{i}"),
                qtype: "exact".into(),
                relevance: Vec::new(),
            })
            .collect();

        let sigs = |threads: usize| {
            concurrent_batch(
                &searcher,
                &judgments,
                BatchSpec {
                    mode: SearchMode::Bm25,
                    k: 10,
                    warmup: 1,
                    reps: 3,
                    filter: None,
                },
                threads,
            )
            .unwrap()
            .worker_signatures
        };
        let s1 = sigs(1);
        let s4 = sigs(4);
        assert_eq!(s1.len(), 1, "1 档应有 1 个 worker");
        assert_eq!(s4.len(), 4, "4 档应有 4 个 worker");
        assert!(
            s4.iter().all(|x| *x == s4[0]),
            "同一档内 4 个 worker 的结果必须一致：{s4:?}"
        );
        assert_eq!(
            s1[0], s4[0],
            "4 线程的 hits 必须与 1 线程逐位一致（S6-T11）"
        );

        // 3) 手写朴素单线程循环（非并发路径、非被测函数）算同一签名
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for j in &judgments {
            for _ in 0..3 {
                let r = run_once(&searcher, &j.query, SearchMode::Bm25, 10, None).unwrap();
                r.hits.len().hash(&mut hasher);
                for h in &r.hits {
                    h.chunk_id.hash(&mut hasher);
                    h.score.to_bits().hash(&mut hasher);
                }
            }
        }
        assert_eq!(
            hasher.finish(),
            s1[0],
            "并发签名的算法必须与「朴素单线程循环」一致（否则 1、2 两条断言可能是同错互证）"
        );
    }

    /// T7-17 的前提：`QueryExecutor` 必须 `Sync`，N 个 worker 才能用 `&` 共享它
    /// （**无需改内核**、也无须 `Arc`）。
    ///
    /// 门面层的 owned `Searcher` 已在 `search/searcher.rs` 断言
    /// `Clone + Send + Sync + 'static`；但 bench 实际共享的是这个**借用的逃生舱类型**
    /// （`QueryExecutor<'a>`，为了注入 `--k1/--b/--rrf-k/--ef-search/--brute-fallback`），
    /// 它此前**没有任何断言** ⇒ 一旦某个字段引入非 `Sync` 的 `Box<dyn ...>`，
    /// 并发阶段会在编译期以外的地方静默退化。这里把它钉住。
    #[test]
    fn queryexecutor可跨线程共享() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<QueryExecutor<'static>>();
    }

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
