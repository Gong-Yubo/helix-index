//! **spike S8-S2**（`S8-08` / `T7-26` / `S8-T16`）：查询侧**会话池**的「投 / 不投」决策门。
//!
//! 设计 `v2-step8-design.md` §4.11.3 给了**三条合取判据**（任一条不过 ⇒ 判「不投」）：
//!
//! | # | 判据 | 阈值 | 依据 |
//! | --- | --- | --- | --- |
//! | ① | **数值一致性**（`Q3′` 的前置） | 同 query，单会话 vs 池化 ⇒ 向量**逐位相同**（`to_bits()`） | 不满足 ⇒ **直接判「不投」**，后两条不必测 |
//! | ② | **吞吐增益** | `QPS(4)/QPS(1)` 的 **vector 路 ≥ 2.5×** | 沿用 NFR-10 的既有判据（不新造） |
//! | ③ | **峰值 RSS 增量** | **≤ 20%**（相对 `sessions = 1` 臂；查询侧口径 = **batch 1 × 短 query**） | 借用 D-S6-01 的门槛值，**口径必须重测** |
//!
//! # 🟠 `Q3′` 为什么必须**实测**而不能推断
//!
//! 原 `Q3` 写作「不同 `sessions` / `intra_threads` 是否改变向量数值」。fastembed **没有**
//! `sessions` 参数（`InitOptions` 只有 `intra_threads`）⇒ 设计把它**改述为 Q3′**：
//! 「不同 `intra_threads`（以及不同**实例数**）是否改变 `embed_query` 的**数值**」。
//! ⚠️ 不允许按「ONNX 是确定性的」推断 —— R47 的教训是「不同 batch 组成下的 logit 已实测
//! 相同」，但那是**精排**；embed 侧从未测过。
//!
//! 本 spike 把 `Q3′` 拆成**三个可分别证伪的对照**（判据①的三个子项）：
//!
//! | 子项 | 对照 | 走的是生产路径吗 |
//! | --- | --- | --- |
//! | ①a | **同一个池内**轮转 4 个槽（连取 4 次必然轮过 4 槽） | ✅ 是（`LocalEmbedder`） |
//! | ①b | `sessions = 1` vs `sessions = 4`（**不同实例数**） | ✅ 是（`LocalEmbedder`） |
//! | ①c | 裸 `fastembed` 下 `intra_threads = 核数` vs `ceil(核数/4)` | ❌ **不是** —— 生产池所有槽用**同一份** `InitOptions`，这一项测的是「**要不要**给每槽分化 `intra_threads`」 |
//!
//! ⚠️ ①c 单独标注为**非生产路径**：若它证实「`intra_threads` 改变数值」，那也**不**推翻池化
//! —— 只要池内所有槽 `intra_threads` 一致（本实现就是如此）。它改变的是**结论的措辞**：
//! 「池化必须保证槽同构」从「显然」变成「**实测支撑的硬要求**」。
//!
//! # 用法（⚠️ 必须 `--release`）
//!
//! ```text
//! # ① 先造一张**冻结图**（A/B 纪律：两臂必须搜同一份内容 —— HNSW 拓扑用 OsRng，不冻结就不可归因）
//! cargo run --release --example spike_s8s2 -- prepare 2000 /tmp/s8s2.idx
//! # ② 判据①（单进程内三组对照）
//! cargo run --release --example spike_s8s2 -- consistency
//! # ③ 判据②（vector 路 QPS）+ 判据③（RSS 由外部 time -l 采）
//! cargo run --release --example spike_s8s2 -- query  1 1 300
//! cargo run --release --example spike_s8s2 -- query  4 4 300
//! cargo run --release --example spike_s8s2 -- search 1 4 300 /tmp/s8s2.idx
//! ```
//!
//! **推荐入口是 `scripts/eval_s8s2.sh`**（逐档位独立进程 + 外置 `/usr/bin/time -l` 采 RSS +
//! 末尾输出**合取表**）。本 example 直接跑也能出数，但**不会**替你采进程峰值 RSS。
//!
//! # 输出约定
//!
//! 人类可读的读数走 `println!`；脚本解析走**一行机器可读**记录：
//! `SPIKE_S8S2 mode=<m> ... key=value ...`（`scripts/eval_s8s2.sh` 按这行 grep）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use helix_core::document::Document;
use helix_core::embed::{Embedder as _, LocalEmbedder};
use helix_core::query::SearchMode;
use helix_core::search::{SearchIndex, VectorBackend};

/// 模型缓存目录（**刻意复刻** `LocalEmbedder` 用的那一个）。
///
/// ⚠️ 为什么不在 example 里直接调 `helix_core::embed::default_cache_dir()`：它是
/// `pub(crate)`（只给 crate 内的 `rerank::local` 复用，`PR #43` 评审 P3-5 收敛过公开面）。
/// ⇒ 这里按 `bench_embed_session.rs` 的**既有先例**复刻同一路径，并**断言它存在**
/// —— 路径一旦漂移就当场失败，而不是静默换成 fastembed 的默认目录（那会触发一次下载）。
fn cache_dir() -> PathBuf {
    let d = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".cache")
        .join("helix-index")
        .join("models");
    assert!(
        d.exists(),
        "模型缓存目录不存在：{} —— 本 spike 的 ①c 对照需要**已缓存**的 bge-small-zh-v1.5         （路径与 LocalEmbedder 用的一致；请先跑一次 `helix build --vectors` 或 `embed_smoke`）",
        d.display()
    );
    d
}

/// 短 query 语料（查询侧口径 = **batch 1 × 短 query**，见设计 §4.11.3 判据③的括号）。
///
/// ⚠️ 刻意**短**：`batch 1 × 短 query` 的激活张量才是「查询侧内存」，长文本会把
/// **建库侧**的内存（`R41`：batch 64 × 长文本 2~3 GB）混进来。
const QUERIES: [&str; 12] = [
    "并发检索",
    "可见性边界",
    "合并延迟",
    "倒排索引",
    "向量召回",
    "会话池",
    "墓碑回收",
    "段基址",
    "原子发布",
    "配置指纹",
    "读写并发",
    "增量写入",
];

/// 建库语料（`prepare` 用；短文档 ⇒ 建库本身不进查询侧读数）。
fn corpus(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("会话池标定语料 {i} 增量写入与合并期间读不阻塞"))
        .collect()
}

fn cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

fn q(i: usize) -> String {
    // 轮转取，保证 query 集与并发度无关（A/B 只允许**一个**变量）
    format!("{} {i}", QUERIES[i % QUERIES.len()])
}

/// 逐位比较两个向量（`f32::to_bits()`）——「逐位一致」是本 spike 判据①的**全部内容**。
fn bitwise_eq(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// 打印机器可读行（脚本按 `SPIKE_S8S2 ` 前缀 grep）。
fn emit(s: &str) {
    println!("SPIKE_S8S2 {s}");
}

// ---------------------------------------------------------------------------
// mode = consistency（判据①，三组对照）
// ---------------------------------------------------------------------------

fn mode_consistency() {
    println!("=== spike S8-S2 / 判据①：数值一致性（`Q3′` 的前置）===");
    println!(
        "环境：核数 = {}；query 条数 = {}；⚠️ 这一条**不测时间**（只判「值」）",
        cores(),
        QUERIES.len()
    );

    // ①a 同一个池内轮转 4 槽
    let four = LocalEmbedder::new_with_sessions(4).expect("构造 4 会话池");
    let slots: Vec<Vec<f32>> = (0..4)
        .map(|_| four.embed_query(QUERIES[0]).expect("embed_query"))
        .collect();
    let slot_ok = slots.iter().all(|v| bitwise_eq(&slots[0], v));
    println!(
        "①a 同池轮转 4 槽逐位一致          = {}（槽长 {}，dim {}）",
        slot_ok,
        four.sessions(),
        four.dim()
    );

    // ①b sessions = 1 vs 4（不同实例数）
    let one = LocalEmbedder::new_with_sessions(1).expect("构造单会话");
    let mut q_same = true;
    for i in 0..QUERIES.len() {
        let text = q(i);
        let a = one.embed_query(&text).expect("embed_query(1)");
        let b = four.embed_query(&text).expect("embed_query(4)");
        if !bitwise_eq(&a, &b) {
            q_same = false;
            println!("  🔴 不一致：query = {text:?}");
        }
    }
    let texts: Vec<String> = (0..QUERIES.len()).map(q).collect();
    let da = one.embed_documents(&texts).expect("embed_documents(1)");
    let db = four.embed_documents(&texts).expect("embed_documents(4)");
    let doc_same = da.iter().zip(&db).all(|(x, y)| bitwise_eq(x, y));
    println!("①b sessions 1 vs 4（query 侧 / 入库侧）= {q_same} / {doc_same}");

    // ①c 裸 fastembed：intra_threads 对照（**非生产路径**）
    let n = cores();
    let mk = |it: usize| -> TextEmbedding {
        TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallZHV15)
                .with_cache_dir(cache_dir())
                .with_show_download_progress(false)
                .with_intra_threads(it),
        )
        .expect("构造裸 TextEmbedding")
    };
    let mut full = mk(n);
    let mut quarter = mk((n / 4).max(1));
    let probe: Vec<String> = QUERIES.iter().map(|s| (*s).to_string()).collect();
    let vf = full.embed(probe.clone(), None).expect("embed(intra=核数)");
    let vq = quarter
        .embed(probe.clone(), None)
        .expect("embed(intra=核数/4)");
    let intra_same = vf.iter().zip(&vq).all(|(x, y)| bitwise_eq(x, y));
    println!(
        "①c intra_threads {n} vs {}（**非生产路径**）= {}",
        (n / 4).max(1),
        intra_same
    );

    let pass = slot_ok && q_same && doc_same;
    println!();
    println!(
        "判据①（**只看生产路径**：①a + ①b）= **{}**{}",
        if pass { "PASS" } else { "FAIL" },
        if intra_same {
            ""
        } else {
            "；⚠️ ①c 显示 intra_threads **会**改变数值 ⇒ 结论措辞必须写明「池内所有槽必须同构」"
        }
    );
    emit(&format!(
        "mode=consistency cores={n} slot_same={slot_ok} sessions_same_query={q_same} \
         sessions_same_docs={doc_same} intra_threads_same={intra_same} pass={pass} \
         dim={} sessions_pool={}",
        four.dim(),
        four.sessions()
    ));
}

// ---------------------------------------------------------------------------
// mode = prepare（造一张冻结图）
// ---------------------------------------------------------------------------

fn mode_prepare(n_docs: usize, index: &str) {
    println!("=== spike S8-S2 / prepare：造冻结图（{n_docs} 篇 ⇒ {index}）===");
    let emb: Arc<dyn helix_core::embed::Embedder> =
        Arc::new(LocalEmbedder::new_with_sessions(1).expect("构造 embedder"));
    let mut idx = SearchIndex::builder()
        .embedder(Some(emb))
        .vector_backend(VectorBackend::Hnsw)
        .build();
    for t in corpus(n_docs) {
        idx.add(Document::new(t)).expect("add");
    }
    idx.commit().expect("commit");
    let p = PathBuf::from(index);
    idx.save(&p).expect("save");
    println!("已落盘：{}", p.display());
    emit(&format!("mode=prepare docs={n_docs} path={index} ok=true"));
}

// ---------------------------------------------------------------------------
// mode = query（判据②的机理 + 判据③的 RSS 臂；**只跑 encode**）
// ---------------------------------------------------------------------------

fn mode_query(sessions: usize, threads: usize, queries: usize) {
    let emb = Arc::new(LocalEmbedder::new_with_sessions(sessions).expect("构造池"));
    println!(
        "=== spike S8-S2 / query：encode-only QPS（sessions={} threads={} queries={}）===",
        emb.sessions(),
        threads,
        queries
    );

    // 预热（丢弃）：让会话各自的图优化 / 内存池先就位
    for i in 0..8 {
        let _ = emb.embed_query(&q(i)).expect("warmup");
    }

    let done = Arc::new(AtomicUsize::new(0));
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for t in 0..threads {
            let emb = Arc::clone(&emb);
            let done = Arc::clone(&done);
            s.spawn(move || {
                // 固定切分（每线程 [start, end)）⇒ 与并发度无关的**同一批** query
                let start = t * queries / threads;
                let end = (t + 1) * queries / threads;
                for i in start..end {
                    let _ = emb.embed_query(&q(i)).expect("embed_query");
                    done.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let n = done.load(Ordering::Relaxed);
    let qps = n as f64 / (ms / 1000.0);
    println!("完成 {n} 条 encode，耗时 {ms:.1} ms ⇒ QPS = {qps:.1}");
    emit(&format!(
        "mode=query sessions={sessions} threads={threads} queries={n} elapsed_ms={ms:.1} \
         qps={qps:.1}"
    ));
}

// ---------------------------------------------------------------------------
// mode = search（判据②：**vector 路** QPS；读一张冻结图 ⇒ A/B 只差 sessions）
// ---------------------------------------------------------------------------

fn mode_search(sessions: usize, threads: usize, queries: usize, index: &str) {
    let p = PathBuf::from(index);
    assert!(
        p.exists(),
        "冻结图不存在：{index} —— 请先跑 `prepare`（A/B 纪律：两臂必须搜同一份内容）"
    );
    let emb: Arc<dyn helix_core::embed::Embedder> =
        Arc::new(LocalEmbedder::new_with_sessions(sessions).expect("构造池"));
    let idx = SearchIndex::builder()
        .embedder(Some(emb))
        .vector_backend(VectorBackend::Hnsw)
        .load(&p)
        .expect("load 冻结图（配置指纹不含会话数，故 1 / N 都能读同一份快照）");
    let searcher = idx.searcher();
    println!(
        "=== spike S8-S2 / search：vector 路 QPS（sessions={sessions} threads={threads} \
         queries={queries}，图 = {index}）==="
    );

    for i in 0..8 {
        let _ = searcher
            .search_with(&q(i))
            .mode(SearchMode::Vector)
            .top_n(10)
            .exec()
            .expect("warmup");
    }

    let done = Arc::new(AtomicUsize::new(0));
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for t in 0..threads {
            let searcher = searcher.clone();
            let done = Arc::clone(&done);
            s.spawn(move || {
                let start = t * queries / threads;
                let end = (t + 1) * queries / threads;
                for i in start..end {
                    let _ = searcher
                        .search_with(&q(i))
                        .mode(SearchMode::Vector)
                        .top_n(10)
                        .exec()
                        .expect("vector search");
                    done.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let n = done.load(Ordering::Relaxed);
    let qps = n as f64 / (ms / 1000.0);
    println!("完成 {n} 条 vector 检索，耗时 {ms:.1} ms ⇒ QPS = {qps:.1}");
    emit(&format!(
        "mode=search sessions={sessions} threads={threads} queries={n} elapsed_ms={ms:.1} \
         qps={qps:.1} index={index}"
    ));
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mode = a.first().map(String::as_str).unwrap_or("consistency");
    let num = |i: usize, d: usize| -> usize { a.get(i).and_then(|s| s.parse().ok()).unwrap_or(d) };
    match mode {
        "consistency" => mode_consistency(),
        "prepare" => {
            let docs = num(1, 2000);
            let path = a
                .get(2)
                .cloned()
                .unwrap_or_else(|| "/tmp/s8s2.idx".to_string());
            mode_prepare(docs, &path);
        }
        "query" => mode_query(num(1, 1), num(2, 1), num(3, 300)),
        "search" => {
            let path = a
                .get(4)
                .cloned()
                .unwrap_or_else(|| "/tmp/s8s2.idx".to_string());
            mode_search(num(1, 1), num(2, 1), num(3, 300), &path);
        }
        other => {
            eprintln!(
                "未知 mode：{other}（可用：prepare / consistency / query / search）\n\
                 用法：spike_s8s2 <mode> [sessions] [threads] [queries] [index]"
            );
            std::process::exit(2);
        }
    }
}
