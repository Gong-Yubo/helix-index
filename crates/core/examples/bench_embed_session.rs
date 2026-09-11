//! embed 并行 spike 载体（V2 Step 6 · T7-09 · S6-01）。
//!
//! 回答两个**收益方向未定**的问题，为 D-S6-01（T7-09「投 / 不投」）提供实测依据：
//!
//! | 组 | 配置 | 想回答的问题（Q1 / Q2） |
//! | --- | --- | --- |
//! | **E1** | `sessions = 1`，`intra_threads = None`（= 满核），EP = CPU | 基线，同时是**探针**（复现不出 51~62 条/s 就先排查环境） |
//! | **E2** | `sessions ∈ {2, 4}`，`intra_threads = max(1, 核数 / sessions)`，EP = CPU | 把同一批算力**切开重新分配**能不能更快（§2.3：`intra_threads` 本就吃满所有核） |
//! | **E3** | `sessions = 1`，EP = CoreML（`--features coreml` 才有） | macOS aarch64 上 CoreML EP 能否初始化、能否加速 |
//!
//! 本文件**不进内核**：会话池是 spike 内的临时代码，`Embedder` trait 与 `LocalEmbedder`
//! 一字不改。池化只在决策为「投」时才落地（D-S6-02 / S6-04）。
//!
//! 运行（release，否则吞吐无意义）：
//!
//! ```text
//! cargo run -p helix-core --release --example bench_embed_session -- --json /tmp/s6-embed.json
//! cargo run -p helix-core --release --features coreml --example bench_embed_session \
//!     -- --json /tmp/s6-embed-coreml.json
//! ```
//!
//! # 口径（设计 §4.2.1；三组必须同口径，否则数据不可横比）
//!
//! | 项 | 取值 |
//! | --- | --- |
//! | 语料 | `data/t2-corpus.jsonl` 前 **4000** 段（与 `bench_batch_size.rs` 一致 ⇒ 可与既有 51~62 条/s 对照） |
//! | 调用方式 | 按 **64** 切片循环，复刻 `SearchIndex::flush` 的真实切片 |
//! | 预热 | 每档位 1 轮完整语料，**丢弃计时** |
//! | 计时 | 每轮累计 wall time；**A/B/A/B 顺序交错**（消除时间漂移），取中位数 |
//! | 保序 | 按块分发 + **按块索引回填**（与完成顺序无关，§4.3.1） |
//!
//! ## ⚠️ 三个容易误判的点
//!
//! 1. **实际进 ONNX 的 batch 是 64，不是 256**。`fastembed` 内部的 `DEFAULT_BATCH_SIZE = 256`
//!    从不触发——调用方（`search/index.rs` 的 flush 阈值）已在 64 处切好（设计 §2.4 末行）。
//! 2. **峰值 RSS 不在本进程内测**：peak RSS 是进程级单调量，同进程跑多档位无法分档归因；
//!    且读 `getrusage(2)` 需要 `unsafe`（本项目守门要求新代码无 `unsafe`）。
//!    ⇒ 由 `scripts/eval_embed_session.sh` 用**外置 `/usr/bin/time -l`** 逐档位起独立进程采集，
//!    与本项目 NFR-05 的既有口径一致（见 `scripts/eval_perf.sh`）。
//! 3. **交错只在「轮」这一层**：同一轮内档位仍是顺序执行的，所以控制组（E1）的轮间漂移
//!    必须打印出来；漂移 > 10% 时跨档位比较要先按控制组归一（`perf-ab-calibration` 的教训）。

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

/// 复刻 `SearchIndex::flush` 的实际切片大小（`search/config.rs` 的 `DEFAULT_BATCH_SIZE = 64`）。
///
/// ⚠️ 不是 `fastembed` 内部的 256：那一层切分在我们的调用链上**从不触发**。
const CALLER_BATCH: usize = 64;

/// 默认语料规模（与 `bench_batch_size.rs` 同口径）。
const DEFAULT_TEXTS: usize = 4000;

/// 执行后端。CoreML 只在 `--features coreml` 构建下可用。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ep {
    Cpu,
    #[allow(dead_code)] // 未开 coreml feature 时该变体仍参与默认档位表的构造逻辑
    CoreMl,
}

impl Ep {
    fn as_str(self) -> &'static str {
        match self {
            Ep::Cpu => "cpu",
            Ep::CoreMl => "coreml",
        }
    }
}

/// 一个待测档位。
#[derive(Clone, Debug)]
struct Cfg {
    name: &'static str,
    sessions: usize,
    intra_threads: Option<usize>,
    ep: Ep,
}

/// 默认档位表（E1 / E2-2 / E2-4，开 coreml feature 时追加 E3）。
fn default_cfgs(cores: usize) -> Vec<Cfg> {
    let mut v = vec![
        Cfg {
            name: "e1",
            sessions: 1,
            intra_threads: None, // None = 用满所有核（fastembed 默认）
            ep: Ep::Cpu,
        },
        Cfg {
            name: "e2-2",
            sessions: 2,
            intra_threads: Some(cores.div_ceil(2).max(1)),
            ep: Ep::Cpu,
        },
        Cfg {
            name: "e2-4",
            sessions: 4,
            intra_threads: Some(cores.div_ceil(4).max(1)),
            ep: Ep::Cpu,
        },
    ];
    if cfg!(feature = "coreml") {
        v.push(Cfg {
            name: "e3-coreml",
            sessions: 1,
            intra_threads: None,
            ep: Ep::CoreMl,
        });
    }
    v
}

/// 模型缓存目录（与 `embed/local.rs` 的 `default_cache_dir()` 保持同一处，避免二次下载）。
fn cache_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".cache")
        .join("helix-index")
        .join("models")
}

/// 造一个 session。E1/E2/E3 共用这条构造路径（设计 §2.4：三组实验只需换参数）。
///
/// `coreml_static_shapes` 只对 [`Ep::CoreMl`] 生效：打开 `RequireStaticInputShapes` + `MLProgram`
/// 格式。**默认关闭**，因为它是 EP 调优（不是 E3 的定义）；加 `--coreml-static-shapes` 才启用，
/// 用于回答"E3 慢是不是因为动态形状导致 CoreML 反复重编译"这个反诘。
fn new_session(
    ep: Ep,
    intra_threads: Option<usize>,
    coreml_static_shapes: bool,
) -> anyhow::Result<TextEmbedding> {
    // 未开 `coreml` feature 时下面的 CoreMl 分支会被整体编译掉 ⇒ 显式吞掉，避免 unused warning
    let _ = coreml_static_shapes;

    let mut opts = InitOptions::new(EmbeddingModel::BGESmallZHV15)
        .with_cache_dir(cache_dir())
        .with_show_download_progress(false);
    if let Some(t) = intra_threads {
        opts = opts.with_intra_threads(t);
    }
    match ep {
        Ep::Cpu => {}
        Ep::CoreMl => {
            #[cfg(feature = "coreml")]
            {
                // ⚠️ 真实类型是 `ort::ep::CoreML`，不是 `CoreMLExecutionProvider`（设计 §2.4）。
                // `fastembed` 只重导出 `ExecutionProviderDispatch` 类型，不透传 feature
                // ⇒ 必须由本 crate 直接依赖 `ort` 并开 `coreml`。
                let ep_builder = ort::ep::CoreML::default();
                let ep_builder = if coreml_static_shapes {
                    ep_builder
                        .with_static_input_shapes(true)
                        .with_model_format(ort::ep::coreml::ModelFormat::MLProgram)
                } else {
                    ep_builder
                };
                opts = opts.with_execution_providers(vec![ep_builder.build()]);
            }
            #[cfg(not(feature = "coreml"))]
            {
                anyhow::bail!("未启用 coreml feature：请用 --features coreml 构建本 example");
            }
        }
    }
    TextEmbedding::try_new(opts)
        .map_err(|e| anyhow::anyhow!("session 初始化失败（ep={}）: {e}", ep.as_str()))
}

/// 会话池：N 个 session + 一个 N 线程的 rayon 池。
struct SessionPool {
    cfg: Cfg,
    sessions: Vec<Mutex<TextEmbedding>>,
    pool: rayon::ThreadPool,
}

impl SessionPool {
    fn build(cfg: &Cfg, coreml_static_shapes: bool) -> anyhow::Result<Self> {
        let mut sessions = Vec::with_capacity(cfg.sessions);
        for _ in 0..cfg.sessions {
            sessions.push(Mutex::new(new_session(
                cfg.ep,
                cfg.intra_threads,
                coreml_static_shapes,
            )?));
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(cfg.sessions)
            .thread_name(|i| format!("embed-spike-{i}"))
            .build()?;
        Ok(Self {
            cfg: cfg.clone(),
            sessions,
            pool,
        })
    }

    /// 跑一整轮：按 [`CALLER_BATCH`] 切块 → 轮转分发到 session → **按块索引回填**。
    ///
    /// 正确性不依赖 rayon 的 `collect` 语义：`out.par_chunks_mut` 与 `texts.chunks` 位置一一对应，
    /// 回填时用块索引显式搬迁（设计 §4.3.1 的硬要求）。
    fn run_pass(&self, texts: &[String]) -> anyhow::Result<Duration> {
        let n = texts.len();
        let mut out: Vec<Option<Vec<f32>>> = vec![None; n];
        let err: Mutex<Option<String>> = Mutex::new(None);

        let t0 = Instant::now();
        self.pool.install(|| {
            use rayon::prelude::*;
            out.par_chunks_mut(CALLER_BATCH)
                .enumerate()
                .for_each(|(bi, slot)| {
                    // 任一块失败 ⇒ 其余块直接放弃（错误在收尾统一上报）
                    if err.lock().map(|g| g.is_some()).unwrap_or(true) {
                        return;
                    }
                    let lo = bi * CALLER_BATCH;
                    // ⚠️ 必须夹紧右界：末块不足 BATCH 时 `lo + CALLER_BATCH` 会越界（设计 §4.3.1）
                    let hi = (lo + CALLER_BATCH).min(n);
                    let idx = bi % self.sessions.len();
                    let mut s = self.sessions[idx].lock().expect("session 锁已中毒");
                    match s.embed(&texts[lo..hi], None) {
                        Ok(mut v) => {
                            for (dst, src) in slot.iter_mut().zip(v.drain(..)) {
                                *dst = Some(src);
                            }
                        }
                        Err(e) => {
                            *err.lock().expect("err 锁已中毒") = Some(e.to_string());
                        }
                    }
                });
        });
        let elapsed = t0.elapsed();

        if let Some(m) = err.lock().expect("err 锁已中毒").take() {
            anyhow::bail!("embed 失败：{m}");
        }
        // 完整性自检：少回填会静默少算吞吐，必须在这里炸出来
        anyhow::ensure!(
            out.iter().all(|o| o.is_some()),
            "有块未回填（保序回填逻辑有缺陷）"
        );
        Ok(elapsed)
    }
}

/// 一轮结果。
struct Round {
    cfg: Cfg,
    secs: Vec<f64>,
}

impl Round {
    fn median(&self) -> f64 {
        let mut v = self.secs.clone();
        v.sort_by(|a, b| a.partial_cmp(b).expect("耗时不应为 NaN"));
        let m = v.len() / 2;
        if v.len().is_multiple_of(2) {
            (v[m - 1] + v[m]) / 2.0
        } else {
            v[m]
        }
    }
}

fn chip_name() -> String {
    if let Ok(o) = std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
    {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    if let Ok(s) = std::fs::read_to_string("/proc/cpuinfo") {
        if let Some(l) = s.lines().find(|l| l.starts_with("model name")) {
            if let Some((_, v)) = l.split_once(':') {
                return v.trim().to_string();
            }
        }
    }
    "unknown".to_string()
}

fn load_texts(path: &std::path::Path, limit: usize) -> anyhow::Result<Vec<String>> {
    let raw = std::fs::read_to_string(path)?;
    let texts: Vec<String> = raw
        .lines()
        .take(limit)
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("语料每行应为 JSON");
            v["text"].as_str().expect("语料缺 text 字段").to_string()
        })
        .collect();
    Ok(texts)
}

/// 极简 `--key value` 解析（与本项目 other examples 同风格，不引 clap）。
struct Args {
    texts: usize,
    rounds: usize,
    warmup: usize,
    configs: Option<Vec<String>>,
    json: Option<PathBuf>,
    /// 仅对 `e3-coreml` 生效：EP 调优（静态输入形状 + MLProgram）。
    coreml_static_shapes: bool,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut a = Args {
        texts: DEFAULT_TEXTS,
        rounds: 3,
        warmup: 1,
        configs: None,
        json: None,
        coreml_static_shapes: false,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let need = |i: usize| -> anyhow::Result<&String> {
            argv.get(i + 1)
                .ok_or_else(|| anyhow::anyhow!("{} 缺少取值", argv[i]))
        };
        match argv[i].as_str() {
            "--texts" => {
                a.texts = need(i)?.parse()?;
                i += 2;
            }
            "--rounds" => {
                a.rounds = need(i)?.parse()?;
                i += 2;
            }
            "--warmup" => {
                a.warmup = need(i)?.parse()?;
                i += 2;
            }
            "--configs" => {
                a.configs = Some(need(i)?.split(',').map(|s| s.trim().to_string()).collect());
                i += 2;
            }
            "--json" => {
                a.json = Some(PathBuf::from(need(i)?));
                i += 2;
            }
            "--coreml-static-shapes" => {
                a.coreml_static_shapes = true;
                i += 1;
            }
            "-h" | "--help" => {
                println!(
                    "用法: bench_embed_session [--texts N] [--rounds N] [--warmup N] \
                     [--configs e1,e2-2,e2-4(,e3-coreml)] [--json PATH] \
                     [--coreml-static-shapes]"
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("未知参数：{other}"),
        }
    }
    Ok(a)
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let corpus =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/t2-corpus.jsonl");
    let texts = load_texts(&corpus, args.texts)?;
    let chars: usize = texts.iter().map(|t| t.chars().count()).sum();

    println!("== V2 Step 6 · T7-09 embed 并行 spike（S6-01）==");
    println!(
        "机器：{} / {} 核；语料 {} 段（{} 字）；切片 {}；warmup {} 轮 + 计时 {} 轮；交错 A/B/…",
        chip_name(),
        cores,
        texts.len(),
        chars,
        CALLER_BATCH,
        args.warmup,
        args.rounds
    );
    if !cfg!(feature = "coreml") {
        println!("⚠️ 未启用 coreml feature ⇒ E3 档位缺席（加 --features coreml 可测）");
    }

    let all = default_cfgs(cores);
    let cfgs: Vec<Cfg> = match &args.configs {
        None => all,
        Some(names) => {
            let mut v = Vec::new();
            for n in names {
                let c = all.iter().find(|c| c.name == n).ok_or_else(|| {
                    anyhow::anyhow!(
                        "未知档位 {n}；可用：{}",
                        all.iter().map(|c| c.name).collect::<Vec<_>>().join(", ")
                    )
                })?;
                v.push(c.clone());
            }
            v
        }
    };
    anyhow::ensure!(!cfgs.is_empty(), "档位表为空");

    // ---- 建池（模型加载与 ONNX 图优化不计入任何计时）----
    println!(
        "加载 session ...（共 {} 个：{}）",
        cfgs.iter().map(|c| c.sessions).sum::<usize>(),
        cfgs.iter()
            .map(|c| format!("{}×{}", c.name, c.sessions))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let pools: Vec<SessionPool> = cfgs
        .iter()
        .map(|c| SessionPool::build(c, args.coreml_static_shapes))
        .collect::<anyhow::Result<Vec<_>>>()?;
    println!("加载完成");

    // ---- 预热（丢弃）----
    for _ in 0..args.warmup {
        for p in &pools {
            let d = p.run_pass(&texts)?;
            println!(
                "  [warmup] {:<10} {:>7.2}s（丢弃）",
                p.cfg.name,
                d.as_secs_f64()
            );
        }
    }

    // ---- 计时：按轮交错 A/B/… ----
    let mut rounds: Vec<Round> = pools
        .iter()
        .map(|p| Round {
            cfg: p.cfg.clone(),
            secs: Vec::new(),
        })
        .collect();
    for r in 1..=args.rounds {
        for (pi, p) in pools.iter().enumerate() {
            let d = p.run_pass(&texts)?;
            rounds[pi].secs.push(d.as_secs_f64());
            println!(
                "  [round {r}/{n}] {:<10} {:>7.2}s",
                p.cfg.name,
                d.as_secs_f64(),
                n = args.rounds
            );
        }
    }

    // ---- 控制组（E1 = 第一个档位）的轮间漂移 ----
    let ctrl = &rounds[0];
    let (drift_pct, ctrl_spread) = control_drift(ctrl);

    // ---- 汇总表 ----
    let base_med = ctrl.median();
    println!();
    println!(
        "{:<10} {:>8} {:>11} {:>7} {:>26} {:>10} {:>11} {:>9}",
        "档位", "sessions", "intra", "ep", "每轮耗时(s)", "中位数(s)", "吞吐(条/s)", "相对E1"
    );
    for rd in &rounds {
        let med = rd.median();
        let thr = texts.len() as f64 / med;
        let rel = base_med / med;
        println!(
            "{:<10} {:>8} {:>11} {:>7} {:>26} {:>10.2} {:>11.1} {:>8.2}x",
            rd.cfg.name,
            rd.cfg.sessions,
            rd.cfg
                .intra_threads
                .map(|t| t.to_string())
                .unwrap_or_else(|| "None".into()),
            rd.cfg.ep.as_str(),
            rd.secs
                .iter()
                .map(|s| format!("{s:.1}"))
                .collect::<Vec<_>>()
                .join("/"),
            med,
            thr,
            rel
        );
    }
    println!();
    println!(
        "控制组（{}）轮间漂移：{drift_pct:+.1}%（min {:.2}s / max {:.2}s，带宽 {:.1}%）",
        ctrl.cfg.name,
        ctrl_spread.0,
        ctrl_spread.1,
        (ctrl_spread.1 - ctrl_spread.0) / ctrl_spread.0 * 100.0
    );
    if drift_pct.abs() > 10.0 {
        println!("⚠️ 漂移 > 10% ⇒ 跨档位比较请先按控制组归一（perf-ab-calibration 纪律）");
    }

    // ---- 决策门（D-S6-01，写死在输出里）----
    if let Some(e1) = rounds.iter().find(|r| r.cfg.name == "e1") {
        for rd in rounds.iter().filter(|r| r.cfg.name != "e1") {
            let gain = (e1.median() / rd.median() - 1.0) * 100.0;
            println!(
                "决策门：{} 相对 e1 吞吐增益 {:+.1}%（门槛 ≥ +30%；RSS 增量由脚本单独测，门槛 ≤ +20%）⇒ 吞吐口径{}",
                rd.cfg.name,
                gain,
                if gain >= 30.0 { "过" } else { "不过" }
            );
        }
    }
    println!(
        "⚠️ 峰值 RSS 不在本进程内测：见 scripts/eval_embed_session.sh（外置 /usr/bin/time -l）"
    );

    // ---- JSON 落盘 ----
    if let Some(path) = &args.json {
        let doc = serde_json::json!({
            "meta": {
                "chip": chip_name(),
                "cores": cores,
                "texts": texts.len(),
                "chars": chars,
                "caller_batch": CALLER_BATCH,
                "warmup": args.warmup,
                "rounds": args.rounds,
                "coreml_feature": cfg!(feature = "coreml"),
                "coreml_static_shapes": args.coreml_static_shapes,
            },
            "control": {
                "name": ctrl.cfg.name,
                "rounds_secs": ctrl.secs,
                "drift_pct": drift_pct,
            },
            "configs": rounds.iter().map(|rd| serde_json::json!({
                "name": rd.cfg.name,
                "sessions": rd.cfg.sessions,
                "intra_threads": rd.cfg.intra_threads,
                "ep": rd.cfg.ep.as_str(),
                "rounds_secs": rd.secs,
                "median_secs": rd.median(),
                "throughput_per_sec": texts.len() as f64 / rd.median(),
                "speedup_vs_e1": base_med / rd.median(),
            })).collect::<Vec<_>>(),
        });
        std::fs::write(path, serde_json::to_string_pretty(&doc)?)?;
        println!("JSON 已写入 {}", path.display());
    }
    Ok(())
}

/// 控制组漂移：`(末轮 / 首轮 - 1) * 100`，以及 `(min, max)`。
fn control_drift(rd: &Round) -> (f64, (f64, f64)) {
    let first = rd.secs.first().copied().unwrap_or(f64::NAN);
    let last = rd.secs.last().copied().unwrap_or(f64::NAN);
    let drift = (last / first - 1.0) * 100.0;
    let min = rd.secs.iter().copied().fold(f64::INFINITY, f64::min);
    let max = rd.secs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (drift, (min, max))
}
