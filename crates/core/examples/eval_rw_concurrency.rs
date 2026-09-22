//! **V2 Step 8 / `S8-09` 的标定载体**：合并期读延迟（`S8-T11`）、热路径四档（`S8-T12`）、
//! `commit()` 写延迟（`S8-T15`）。
//!
//! 由 `scripts/eval_rw_concurrency.sh` 逐档位**独立进程**驱动，输出机器可读行（前缀 `RWC `）。
//!
//! # 三个 mode
//!
//! | mode | 测什么 | 对应 |
//! | --- | --- | --- |
//! | `latency` | **A1（静止期）→ B（合并期）→ A2（合并后静止期）** 三臂的 `took` 分位数 | `S8-T11` / NFR-14 ②a·②b |
//! | `hotpath` | 四档 **BM25 lane** 耗时（无 delta / 有 delta 无墓碑 / 有跨段墓碑 / 墓碑已合并） | `S8-T12` / R52 |
//! | `write-latency` | `commit()` 的 **wall time** 分位数（真实本地 embedder） | `S8-T15` / NFR-11 |
//!
//! # 🔑 为什么 `latency` 用**合成 embedder**（而不是真实模型）
//!
//! 判据是「**合并期 ÷ 静止期**」的**比值**。若 `took` 里含一段**两臂都要付**的恒定成本
//! （查询侧编码 ≈10~15ms），比值会被**稀释**：设真实增量 Δ、恒定项 c ⇒ 实测比值
//! `(A+Δ+c)/(A+c) < (A+Δ)/A` ⇒ 恒定项越大越容易"通过"。
//! ⇒ 去掉它得到的是**更严**的判据；且与 `NFR-10` ② 收窄后的适用范围（**不含查询侧编码的读路径**）
//! 口径一致。合成 embedder（LCG，dim=512）另外保证**确定性**：向量侧的读成本不含模型调度抖动。
//!
//! ⚠️ **代价（如实登记）**：本档**不**覆盖「真实模型下查询编码与合并的交互」（`S8-T15` 覆盖写侧）。
//!
//! # 🔑 A/B 纪律：为什么是「同一进程内的 A1 → B → A2」而不是两个进程
//!
//! 设计 §4.13.1 要求「同一张冻结图 + 同一段布局内容」。跨进程做时：
//! ① `save` 前**必须 `merge_all()`**（`D-S8-01`）⇒ 落盘快照里**没有 delta** ⇒ 只能重建 delta，
//! 而每个 delta 段**各自建 Hnsw 图**（`OsRng`，无种子）⇒ 两臂的图**不是同一张**；
//! ② 同一进程内 A1/B/A2 全程共用**同一份内存图** ⇒ 「同图」是**字面成立**的。
//! ⇒ 以 A1（**与 B 的起始布局逐位相同**）为主基线；A2 给出「合并之后」的基线
//! （它同时是 R52 的可逆性信号：合并后热路径应回到零谓词）。
//!
//! 运行（`release`，否则读数无意义）：
//! ```text
//! cargo run --release -p helix-core --example eval_rw_concurrency -- \
//!     latency --corpus data/t2-corpus.jsonl --queries data/t2-queries.jsonl \
//!     --main 2000 --delta 200 --deltas 8 --q 300 --search-mode hybrid
//! ```
//!
//! ⚠️ **本文件是标定载体、不是生产代码**，但**入库**（同 `spike_s8s1.rs` / `spike_s8s2.rs` 的先例）
//! —— 否则读数不可复跑。参数一律经**环境变量**传（`RWC_*`），避免"参数写错跑成空进程"的形态
//! （Step 7 踩过：一次"测量"实际是 9 MiB 的空进程）。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use helix_core::chunk::Chunker;
use helix_core::document::Document;
use helix_core::embed::Embedder;
use helix_core::query::SearchMode;
use helix_core::search::{SearchIndex, SearchIndexBuilder};
use helix_core::types::DocId;

/// 机器可读行的前缀（脚本按它取数）。
const TAG: &str = "RWC";

// ─────────────────────────── 合成 embedder（与 churn_bench 同思路）───────────────────────────

/// 确定性合成 Embedder（LCG + L2 归一化，`dim = 512`，`id = "synth-512"`）。
///
/// 只用于 `latency` / `hotpath` 两个 mode：它们量的是**索引侧**读成本，
/// 与向量语义无关（理由见文件头）。
struct SynthEmbedder {
    dim: usize,
}

impl Embedder for SynthEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_documents(&self, texts: &[String]) -> helix_core::error::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| hash_unit_vec(t, self.dim)).collect())
    }

    fn embed_query(&self, text: &str) -> helix_core::error::Result<Vec<f32>> {
        Ok(hash_unit_vec(text, self.dim))
    }

    fn is_normalized(&self) -> bool {
        true
    }

    fn id(&self) -> &'static str {
        "synth-512"
    }
}

/// 文本 → 512 维 L2 单位向量（LCG，跨进程确定性）。
fn hash_unit_vec(text: &str, dim: usize) -> Vec<f32> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    for b in text.as_bytes() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(u64::from(*b) | 1);
    }
    let mut v = Vec::with_capacity(dim);
    for _ in 0..dim {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        // 取高 24 位映射到 [-1, 1)
        let x = ((state >> 40) as f32) / 8_388_608.0 - 1.0;
        v.push(x);
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    for x in &mut v {
        *x /= norm;
    }
    v
}

// ─────────────────────────── 分位数（最近秩，无插值）───────────────────────────

/// 升序 `xs` 的 `p` 分位数（**最近秩**：`ceil(p·n) - 1`，与项目既有口径一致）。
fn pct(xs: &[u64], p: f64) -> u64 {
    if xs.is_empty() {
        return 0;
    }
    let idx = ((p * xs.len() as f64).ceil() as usize)
        .saturating_sub(1)
        .min(xs.len() - 1);
    xs[idx]
}

/// 把一组样本打成一行读数。
///
/// 🔑 **为什么同时报「第 4 大样本」与「超预算条数」**：各臂的 `n` **差一个量级**
/// （B 的窗口样本上千条、A 的固定 `q` 只有几百条）⇒ 两个 `P99` 的"极端程度"**不可直接比**
/// （n 越大，P99 越靠尾部）。⇒ 用**同深度**口径（第 `k` 大，`k = ceil(1% × 400) = 4`）对齐，
/// 并附「超过各档预算的**条数**」（计数对分布形状不敏感，是小样本下更稳的信号）。
fn summarize(label: &str, mut us: Vec<u64>) -> (u64, u64, u64) {
    let n = us.len();
    us.sort_unstable();
    let (p50, p99) = (pct(&us, 0.50), pct(&us, 0.99));
    let kth = |k: usize| -> u64 {
        if n == 0 {
            0
        } else {
            us[n.saturating_sub(k)]
        }
    };
    let over = |ms: u64| us.iter().filter(|x| **x > ms * 1000).count();
    // ⚠️ **均值是本文件里最稳的统计量**：`p99` 在小样本（A 臂 n≈400 ⇒ p99 ≈ 第 4 大）与
    //    大样本（B 臂 n 上万）之间**不可直接比**，而均值对 n 不敏感
    //    （项目纪律：百分位在同一轮内也不稳 ⇒ 归一化与判据优先看均值，
    //    见 `perf-ab-calibration` 的四条抗噪声规则）。
    let mean = if n == 0 {
        0
    } else {
        us.iter().sum::<u64>() / n as u64
    };
    println!(
        "{TAG} arm={label} n={n} mean_us={mean} p50_us={p50} p99_us={p99} max_us={} k4_us={} k1_us={} over5ms={} over10ms={} over20ms={}",
        us.last().copied().unwrap_or(0),
        kth(4),
        kth(1),
        over(5),
        over(10),
        over(20)
    );
    (p50, p99, n as u64)
}

// ─────────────────────────── 语料 ───────────────────────────

fn read_jsonl_texts(path: &str, limit: usize) -> Vec<String> {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("读不到 {path}: {e}（cwd 应为 repo root）"));
    let mut out = Vec::with_capacity(limit);
    for line in raw.lines() {
        if out.len() >= limit {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        // 只取正文字段（不引 serde_json —— example 不额外拉依赖）：
        // 语料用 `"text"`（`t2-corpus.jsonl`）、查询用 `"query"`（`t2-queries.jsonl`）。
        // ⚠️ **必须是「键位置」的那个出现**：正文自身可能含 `\"text\":` 这样的**转义**副本
        // （语料是自然语言 JSON）⇒ 逐个候选检查**前一个非空字符是 `,` 或 `{`**。
        let hit = ["\"text\":", "\"query\":"]
            .iter()
            .filter_map(|k| {
                line.match_indices(k)
                    .find(|(i, _)| {
                        line[..*i]
                            .trim_end()
                            .chars()
                            .next_back()
                            .is_some_and(|c| c == ',' || c == '{')
                    })
                    .map(|(i, k)| i + k.len())
            })
            .min();
        if let Some(pos) = hit {
            let rest = line[pos..].trim_start();
            if let Some(body) = rest.strip_prefix('"') {
                let mut s = String::new();
                let mut esc = false;
                for ch in body.chars() {
                    if esc {
                        s.push(match ch {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            c => c,
                        });
                        esc = false;
                    } else if ch == '\\' {
                        esc = true;
                    } else if ch == '"' {
                        break;
                    } else {
                        s.push(ch);
                    }
                }
                out.push(s);
            }
        }
    }
    assert!(!out.is_empty(), "{path} 里没解析出 text/query 字段");
    out
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn arg(flag: &str, default: &str) -> String {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .position(|x| x == flag)
        .and_then(|i| a.get(i + 1).cloned())
        .unwrap_or_else(|| default.to_string())
}

// ─────────────────────────── 建库 ───────────────────────────

/// 建一个「主段 + `deltas` 个增量段」的布局，返回 `SearchIndex` 与主段各 doc 的 id。
///
/// ⚠️ **主段是「先 commit 再 merge」造出来的**：`SearchIndex` 的初始视图里主段是空的，
/// 内容必须经一次 `commit()`（封成 delta）再 `merge_all()` 才落进主段
/// ⇒ 这正是 `D-S8-01` 的「`save` 前先合并」在**内存内**的同一条路径。
fn build_layout(
    docs: &[String],
    main_n: usize,
    delta_n: usize,
    deltas: usize,
    embedder: Option<Arc<dyn Embedder>>,
    single_chunk: bool,
) -> (SearchIndex, Vec<DocId>) {
    let mut b = SearchIndexBuilder::default();
    if let Some(e) = embedder {
        b = b.embedder(Some(e));
    }
    if single_chunk {
        // 单 chunk：1 doc == 1 chunk == 1 向量点 ⇒ 段布局可预测
        b = b.chunker(Chunker::new(200_000, 0));
    }
    let mut idx = b.build();

    let mut main_ids = Vec::new();
    for (i, t) in docs.iter().take(main_n).enumerate() {
        let out = idx.add(Document::new(t.clone())).expect("add 主段");
        main_ids.push(out.doc_id);
        // 每 512 篇 commit 一次，避免单段过大
        if i % 512 == 511 {
            idx.commit().expect("commit 主段");
        }
    }
    idx.commit().expect("commit 主段（尾批）");
    let rep = idx.merge_all().expect("merge_all 造主段");
    assert!(rep.segments_merged > 0, "主段必须由合并造出");

    for d in 0..deltas {
        for k in 0..delta_n {
            let t = &docs[(main_n + d * delta_n + k) % docs.len()];
            idx.add(Document::new(format!("{t} #d{d}-{k}")))
                .expect("add delta");
        }
        idx.commit().expect("commit delta");
    }
    (idx, main_ids)
}

// ─────────────────────────── mode: latency ───────────────────────────

fn run_arm_queries(
    searcher: &helix_core::search::Searcher,
    qs: &[String],
    q: usize,
    mode: SearchMode,
) -> Vec<u64> {
    let mut out = Vec::with_capacity(q);
    for i in 0..q {
        let resp = searcher
            .search_with(qs[i % qs.len()].as_str())
            .mode(mode)
            .top_n(10)
            .exec()
            .expect("读端不得出错（FR-17）");
        out.push(resp.took.as_micros() as u64);
    }
    out
}

fn mode_latency() -> usize {
    let corpus = arg("--corpus", "data/t2-corpus.jsonl");
    let queries_path = arg("--queries", "data/t2-queries.jsonl");
    let main_n = env_usize("RWC_MAIN", 2000);
    let delta_n = env_usize("RWC_DELTA", 200);
    let deltas = env_usize("RWC_DELTAS", 8);
    let q = env_usize("RWC_Q", 300);
    let tombstones = env_usize("RWC_TOMB", 0);
    let mode_s = arg("--search-mode", "hybrid");
    let mode = SearchMode::parse(&mode_s).expect("mode 必须是 bm25 / vector / hybrid");

    let docs = read_jsonl_texts(&corpus, main_n + delta_n * deltas + 16);
    let qs = read_jsonl_texts(&queries_path, 64);

    let (mut idx, main_ids) = build_layout(
        &docs,
        main_n,
        delta_n,
        deltas,
        Some(Arc::new(SynthEmbedder { dim: 512 })),
        true,
    );

    // 跨段墓碑（可选）：删主段里的若干 doc ⇒ 合并时**必须**把它们物理化
    //
    // ⚠️ **`remove` 不会立刻改变视图**（`SearchIndex::remove` 的**分支②**只把 doc_id 写进
    // `builder.tombstones` 这个**待发布集合**）⇒ 必须**再 `commit()` 一次**才发布进
    // `View.tombstones`（这就是设计 §4.8.3 ③ 的「墓碑-only 段」形态）。
    // 少了这一次 `commit()`，合并期就不会有墓碑要物理化 —— 本臂会**静默测错对象**。
    for id in main_ids.iter().take(tombstones) {
        idx.remove(*id).expect("remove 主段 doc");
    }
    if tombstones > 0 {
        idx.commit().expect("commit 发布墓碑");
        let m = idx
            .searcher()
            .search_with("x")
            .mode(SearchMode::Bm25)
            .top_n(1)
            .exec()
            .unwrap();
        assert_eq!(
            m.metrics.tombstoned, tombstones,
            "前提：{tombstones} 条跨段墓碑必须真的发布进视图"
        );
        println!(
            "{TAG} note=tombstones_set n={tombstones} metrics_tombstoned={}",
            m.metrics.tombstoned
        );
    }

    let searcher = idx.searcher();
    println!(
        "{TAG} setup main={main_n} delta={delta_n} deltas={deltas} q={q} mode={mode_s} tombstones={tombstones}"
    );

    // 预热（丢弃首轮：NFR-10 的既有测量口径）
    let _ = run_arm_queries(&searcher, &qs, 20, mode);

    let a1 = run_arm_queries(&searcher, &qs, q, mode);

    // ── 臂 B：合并期 ──
    //
    // 🔴 **读端必须「跑到合并结束为止」，而不是跑固定 `q` 条**：若读端跑满 `q` 条而合并窗口
    //    只覆盖其中一段，B 的分位数就被**窗口外的静止期样本稀释** ⇒ 比值被拉向 1.0
    //    ⇒ 判据**偏松**、可能放过真实的尖刺。⇒ 读端持续查询直到写端置 `stop`，
    //    并把每条样本**带上全局序号**，事后按 `[start, end)` **切片**取出「窗口内」样本。
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicUsize::new(0));
    let s2 = searcher.clone();
    let qs2 = qs.clone();
    let stop2 = Arc::clone(&stop);
    let done2 = Arc::clone(&done);
    // 下限：窗口太小（例如 delta 极少）时也要有可读的 n
    let min_q = env_usize("RWC_MIN_Q", 30);
    let reader = thread::spawn(move || {
        let mut samples: Vec<(usize, u64)> = Vec::new();
        let mut i = 0usize;
        while !(stop2.load(Ordering::Relaxed) && i >= min_q) {
            let resp = s2
                .search_with(qs2[i % qs2.len()].as_str())
                .mode(mode)
                .top_n(10)
                .exec()
                .expect("读端不得出错（FR-17：合并期也不得拒绝查询）");
            samples.push((i, resp.took.as_micros() as u64));
            i += 1;
            done2.store(i, Ordering::Relaxed);
        }
        samples
    });

    // 等读端真的跑起来（至少 1 条），再开合并 —— 否则「合并期」可能整段落在读端启动之前。
    // 同时记录**开合并那一刻**的读端进度 = 窗口起点。
    while done.load(Ordering::Relaxed) == 0 {
        thread::sleep(Duration::from_micros(200));
    }
    let start_idx = done.load(Ordering::Relaxed);
    let t_merge = Instant::now();
    let mut merges = 0usize;
    loop {
        match idx.merge_pending() {
            Ok(Some(_)) => merges += 1,
            Ok(None) => break,
            Err(e) => panic!("单写端下 merge_pending 不应 Err: {e}"),
        }
        if merges > deltas {
            break; // 防御：不应发生
        }
    }
    let merge_us = t_merge.elapsed().as_micros() as u64;
    let end_idx = done.load(Ordering::Relaxed);
    stop.store(true, Ordering::Relaxed);
    let samples = reader.join().expect("读线程 join");
    println!(
        "{TAG} merge_window merges={merges} merge_total_us={merge_us} idx_start={start_idx} idx_end={end_idx} n_window={}",
        end_idx.saturating_sub(start_idx)
    );

    // ── 臂 A2：合并之后（同时是 R52 的可逆性信号）──
    let a2 = run_arm_queries(&searcher, &qs, q, mode);

    // B1 = **窗口内**样本（**严格**读法，也是设计 §4.13.1 的「合并进行中」字面口径）；
    // B2 = 全部样本（含窗口外 ⇒ 偏松，仅作对照）。
    let b1: Vec<u64> = samples
        .iter()
        .filter(|(i, _)| *i >= start_idx && *i < end_idx)
        .map(|(_, us)| *us)
        .collect();
    let b2: Vec<u64> = samples.iter().map(|(_, us)| *us).collect();

    let (_a1_50, a1_99, _) = summarize("A1_static_before", a1);
    let (_b1_50, b1_99, b1n) = summarize("B1_merge_window", b1);
    let (_b2_50, b2_99, _) = summarize("B2_all_samples", b2);
    let (_a2_50, a2_99, _) = summarize("A2_static_after", a2);
    assert!(
        b1n > 0,
        "前提：合并窗口内必须至少有一条查询样本（否则本臂什么都没测）"
    );
    assert!(merges > 0, "前提：至少发生过一次合并");

    let m = searcher
        .search_with("x")
        .mode(SearchMode::Bm25)
        .top_n(1)
        .exec()
        .unwrap();
    println!(
        "{TAG} ratio p99_b1_over_a1={:.4} p99_b2_over_a1={:.4} p99_b1_over_a2={:.4} \
a1_p99_us={a1_99} a2_p99_us={a2_99} b1_p99_us={b1_99} b2_p99_us={b2_99}",
        b1_99 as f64 / a1_99.max(1) as f64,
        b2_99 as f64 / a1_99.max(1) as f64,
        b1_99 as f64 / a2_99.max(1) as f64
    );
    println!(
        "{TAG} post_merge segments={} tombstones={}",
        m.metrics.segments, m.metrics.tombstoned
    );
    0
}

// ─────────────────────────── mode: hotpath ───────────────────────────

fn hot_measure(idx: &SearchIndex, qs: &[String], q: usize) -> (u64, u64, usize, usize) {
    let searcher = idx.searcher();
    // 预热
    for i in 0..10 {
        let _ = searcher
            .search_with(qs[i % qs.len()].as_str())
            .mode(SearchMode::Bm25)
            .top_n(10)
            .exec()
            .unwrap();
    }
    let mut us = Vec::with_capacity(q);
    let mut tomb = 0;
    let mut seg = 0;
    for i in 0..q {
        let r = searcher
            .search_with(qs[i % qs.len()].as_str())
            .mode(SearchMode::Bm25)
            .top_n(10)
            .exec()
            .unwrap();
        us.push(r.metrics.bm25_elapsed.as_micros() as u64);
        tomb = r.metrics.tombstoned;
        seg = r.metrics.segments;
    }
    us.sort_unstable();
    (pct(&us, 0.50), pct(&us, 0.99), tomb, seg)
}

fn mode_hotpath() -> usize {
    let corpus = arg("--corpus", "data/t2-corpus.jsonl");
    let queries_path = arg("--queries", "data/t2-queries.jsonl");
    let main_n = env_usize("RWC_MAIN", 2000);
    let delta_n = env_usize("RWC_DELTA", 200);
    let q = env_usize("RWC_Q", 300);
    let tomb_n = env_usize("RWC_TOMB", 20);

    let docs = read_jsonl_texts(&corpus, main_n + delta_n * 8 + 16);
    let qs = read_jsonl_texts(&queries_path, 64);

    // 档 1：只有主段（无 delta、无墓碑）
    let (mut idx, main_ids) = build_layout(
        &docs,
        main_n,
        0,
        0,
        Some(Arc::new(SynthEmbedder { dim: 512 })),
        false,
    );
    let (p50, p99, t, s) = hot_measure(&idx, &qs, q);
    println!(
        "{TAG} hot stage=1 desc=main_only p50_us={p50} p99_us={p99} tombstones={t} segments={s}"
    );
    let s1_p50 = p50;

    // 档 2：有 delta、无墓碑
    for k in 0..delta_n {
        let tx = format!("{} #hot-{k}", docs[(main_n + k) % docs.len()]);
        idx.add(Document::new(tx)).expect("add delta");
    }
    idx.commit().expect("commit delta");
    let (p50, p99, t, s) = hot_measure(&idx, &qs, q);
    println!("{TAG} hot stage=2 desc=delta_no_tomb p50_us={p50} p99_us={p99} tombstones={t} segments={s}");

    // 档 3：有跨段墓碑（删主段里的 doc ⇒ 墓碑目标在**已合并**的主段里）
    // ⚠️ 同 `latency` 的注释：`remove` 只写 `builder.tombstones`，**必须再 `commit()`** 才发布。
    for id in main_ids.iter().take(tomb_n) {
        idx.remove(*id).expect("remove 主段 doc");
    }
    idx.commit().expect("commit 发布墓碑");
    let (p50, p99, t, s) = hot_measure(&idx, &qs, q);
    println!("{TAG} hot stage=3 desc=cross_segment_tomb p50_us={p50} p99_us={p99} tombstones={t} segments={s}");
    assert!(t > 0, "档 3 的前提：必须真的存在跨段墓碑");

    // 档 4：墓碑已 `merge_all()` 物理化 ⇒ 必须回到零谓词
    idx.merge_all().expect("merge_all 物理化墓碑");
    let (p50, p99, t, s) = hot_measure(&idx, &qs, q);
    println!("{TAG} hot stage=4 desc=after_merge_all p50_us={p50} p99_us={p99} tombstones={t} segments={s}");
    assert_eq!(t, 0, "档 4 的前提：合并后墓碑必须清零（墓碑已物理化）");
    println!(
        "{TAG} hot_ratio stage4_over_stage1={:.4} stage1_p50_us={s1_p50}",
        p50 as f64 / s1_p50.max(1) as f64
    );
    0
}

// ─────────────────────────── mode: write-latency ───────────────────────────

/// **`S8-T15`**：`commit()` 的 wall time 分位数（**真实本地 embedder**）。
///
/// 口径（设计 §4.13.2）：`commit()` 调用的端到端 wall time，**含** `flush`（embed + 归一化 +
/// 灌向量）+ 封段 + 发布，**不含**首次模型下载与 `try_new` 加载（构造在计时之外）。
#[cfg(feature = "local-embed")]
fn mode_write_latency() -> usize {
    {
        let corpus = arg("--corpus", "data/t2-corpus.jsonl");
        let batches = env_usize("RWC_BATCHES", 40);
        let batch_size = env_usize("RWC_BATCH_SIZE", 64);
        let docs = read_jsonl_texts(&corpus, batches * batch_size + 8);

        // ⚠️ 真实本地 embedder（NFR-11 的口径就是「含 embed 的 `commit()` 端到端」）
        let emb = helix_core::embed::LocalEmbedder::new()
            .expect("本地 embedder 构造失败（模型未缓存？）");
        let mut idx = SearchIndexBuilder::default()
            .embedder(Some(Arc::new(emb) as Arc<dyn Embedder>))
            .build();
        println!(
            "{TAG} write_setup batches={batches} batch_size={batch_size} engine=local embedder=bge-small-zh-v1.5"
        );

        let mut us = Vec::with_capacity(batches);
        for b in 0..batches {
            for k in 0..batch_size {
                let t = format!("{} #w-{b}-{k}", docs[b * batch_size + k]);
                idx.add(Document::new(t)).expect("add");
            }
            let t0 = Instant::now();
            idx.commit().expect("commit");
            us.push(t0.elapsed().as_micros() as u64);
        }
        let (p50, p99, n) = summarize("commit_wall", us);
        println!(
            "{TAG} write_ratio p50_s={:.3} p99_s={:.3} n={n}",
            p50 as f64 / 1e6,
            p99 as f64 / 1e6
        );
    }
    0
}

/// 未启用 `local-embed` 时本 mode 不可用（**显式缺席**，不静默跳过 —— 同 `spike_s8s2` 的纪律）。
#[cfg(not(feature = "local-embed"))]
fn mode_write_latency() -> usize {
    println!("{TAG} write_latency skipped=no_local_embed_feature");
    0
}

fn main() {
    // 位置参数 = 本次要跑的 mode（`latency` / `hotpath` / `write-latency`）
    let mode = std::env::args().nth(1).unwrap_or_default();
    let code = match mode.as_str() {
        "latency" => mode_latency(),
        "hotpath" => mode_hotpath(),
        "write-latency" => mode_write_latency(),
        other => {
            eprintln!("未知 mode: {other:?}（可用：latency / hotpath / write-latency）");
            2
        }
    };
    std::process::exit(code as i32);
}
