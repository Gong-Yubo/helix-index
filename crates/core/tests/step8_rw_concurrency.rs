//! **V2 Step 8 / S8-01（T7-25）**：R34（`points_by_layer` 的**递归读**）的并发判据。
//!
//! # 为什么是 std-only 复刻体，而不是端到端测试
//!
//! `hnsw_rs` 的 `Hnsw::insert` 要求 `&mut self`，本仓门面层的 `HnswRsIndex::add` 同样要求
//! `&mut self`（V2.0 的单写者语义）⇒ **在安全 Rust 下不可能**让「一次精确扫描」与「一次插入」
//! 在同一个 `Hnsw` 上重叠。R34 因此在今天**不可达**，却会在 Step 8 读写并发一开时**立即变活**
//! （`plan-v2.md` T7-25 之所以是**硬前置**，正是为此）。⇒ 用一个**不含 `hnsw_rs`** 的
//! 最小复刻体把**机制**锁住：
//!
//! | 本文件的形状 | 对应的依赖源码 |
//! | --- | --- |
//! | 旧：取读锁 → **持锁期间再取一次** | `IterPoint::new` 取 `points_by_layer.read()` 并全程持有（`hnsw.rs:633`）+ `IterPoint::next` 在**层切换**时**再取一次同一把锁**（`:661`；相邻的 `:660` 是另一把 `entry_point` 锁，**不构成**递归） |
//! | 新：**取读锁 → 迭代 → 释放**，下一层重新取 | `IterPointLayer::new` 只取一次（`:701`）+ `IterPointLayer::next` **只索引** `pi_guard[self.layer]`、**不再取锁**（`:715-723`） |
//!
//! 生产侧的实际写法在 `hnsw_rs_index.rs` 的 `search_exact_filtered`（`0..=get_max_level_observed()`
//! 的逐层循环），其正确性由同文件的单测（覆盖等价 / 定向用例 / 与 `BruteForceIndex` 逐位一致）守。
//!
//! # 平台前提（**已实测**，不是推断）
//!
//! 本文件依赖「**写者优先**」：写者一旦在等待，**新读者会被拒**（含 `try_read`）。
//! 用 [`等到写者排队`] 把它从「假设」变成**已观测事实**。两端实测（各 5 轮，全数命中）：
//! macOS（`pthread_rwlock_tryrdlock`）与 **Linux / `rust:1-slim`（= CI 的 `ubuntu-latest`）**。
//! 若某平台不满足，本文件的断言会**指名道姓地**说明「本平台不具备 R34 的前提」——
//! 那意味着 `architecture-design.md` §14.3 R34 需按该平台重估，而不是「测试坏了」。
//!
//! # ⚠️ 本文件**不覆盖**什么（覆盖边界，务必先读）
//!
//! 1. **不覆盖「真实 `Hnsw` 上的读写并发」** —— 见上，那在本项目里**不可达**（无 `unsafe`）。
//! 2. **「改回 `IntoIterator`」只被**部分**覆盖（2026-09-18 第 1 轮评审后就地更正）**：
//!    在**非空图**上改回去后**覆盖仍然正确**（`IntoIterator` 的语义就是全量）⇒ 那些行为断言
//!    **抓不住它**；但 `hnsw_rs_index.rs` 的 **`空图精确扫描返回空且不panic`** 在**空图**这一条
//!    输入上**能抓住**（旧写法 panic 于 `hnsw.rs:662` 的 `entry_point_ref.as_ref().unwrap()`；
//!    **M4 变异实测**：该用例红、其余 10 条全绿）。
//!    ⚠️ **仍抓不住**「改回 `IntoIterator` **且同时**补空图早退」的写法 —— 那种写法仍带 R34 的
//!    递归读 ⇒ 只能靠本文件的存在 + 评审红线（同族的「不变式靠评审守」见架构 §14.6）。
//!    ⇒ 这也是本文件的主要价值：把「为什么不能用 `IntoIterator`」变成**可执行的证据**。
//! 3. **不覆盖 `get_max_level()` 与 `get_max_level_observed()` 的混用** —— 那条由生产侧
//!    `全量遍历基数等于点数且零重复` 守（层号只写 0 时经生产路径的**覆盖等价断言**立刻红；
//!    「layer 0 不是全量」由其 `upper > 0` + 并集等式**蕴含**）。

#![allow(non_snake_case)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use helix_core::chunk::Chunker;
use helix_core::document::Document;
use helix_core::error::{Error, Result};
use helix_core::query::SearchMode;
use helix_core::search::{MergeReport, SearchIndex, VectorBackend, VectorMergeStrategy};
use helix_core::types::ChunkId;

/// 观测上界：**只在断言失败时**才会接近它（正常路径不应有任何可见等待）。
const PATIENCE: Duration = Duration::from_secs(10);

/// 「旧形状的第二次读锁会被拒」的**有界观测窗口**。
///
/// 取值只需「远大于一次锁交接」—— 它是**判据的下界**（超时即判阻塞），不是性能指标。
const R34_BLOCK_WINDOW: Duration = Duration::from_millis(300);

/// 生成一个**受闸的写者**：它必须先收到 `go` 才会去取写锁。
///
/// ⚠️ **闸不是可有可无的**：若直接 `spawn(|| lock.write())`，写者可能在读线程拿到读锁
/// **之前**就取到并释放写锁而**结束** ⇒ 「写者正在等待」这一前提**永不成立** ⇒
/// [`等到写者排队`] 会一直自旋到超时。**实测**：无闸时 10s 超时失败；加闸后立即通过。
/// 闸的时序 = 「读线程持锁」→ `armed` → 主线程 `go` → 写者此刻**必然只能排队**。
fn 受闸写者(lock: Arc<RwLock<usize>>) -> (mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let handle = thread::spawn(move || {
        let _ = go_rx.recv();
        let _g = lock.write().unwrap();
    });
    (go_tx, handle)
}

/// 反向自证的**前提观测**：在「本线程持有读锁 + 另有写者在等」时，`try_read()` 必须失败。
///
/// **这不是计时猜测**：读锁之间是**共享**的 ⇒ 没有写者等待时，持读锁者再 `try_read()`
/// **必然成功**；只有「有写者排队」才能让它失败。⇒ 本函数返回即意味着
/// 「**写者已排队、且新读者会被拒**」这条准入判据**已被观测**，而 R34 正是它的推论
/// （阻塞版 `read()` 与 `try_read()` 共享同一套准入判据）。
///
/// 用自旋而不是 `sleep`：前者在**观测到事实**时立即返回，后者只是**假设**写者已就位。
fn 等到写者排队(lock: &RwLock<usize>) {
    let t0 = Instant::now();
    loop {
        if lock.try_read().is_err() {
            return;
        }
        assert!(
            t0.elapsed() < PATIENCE,
            "10s 内未观测到「写者排队 ⇒ 新读者被拒」：本平台不具备 R34 赖以成立的前提\
             （写者优先）。见 architecture-design.md §14.3 R34 —— 换平台需据此重估该风险。"
        );
        thread::yield_now();
    }
}

/// **S8-T1 正向臂**：**逐层形状**在「有写者等待」时**必须仍然完成**。
///
/// 断言的是**完成性（活性）**，不是「完全不被阻塞」：逐层形状在层与层之间**释放**读锁，
/// 所以下一层的 `read()` 可能被排队中的写者挡一下 —— 但它**不持锁等待** ⇒ 写者能推进、
/// 读者随后也能推进 ⇒ **必然终止**（读者至多 `L+1` 次取锁，无活锁）。
///
/// ⚠️ **鉴别力来自 `drop(g)`**：把 `drop(g)` 去掉（退化成旧形状「持锁期间再取一次」），
/// 读者在第二次取锁时会撞上**已排队的写者** ⇒ 永久阻塞 ⇒ 本用例**卡到 `PATIENCE` 超时后变红**。
/// ⇒ `drop(g)` 不是「顺手清理」，它是被断言的行为本身；而写者**必须已被排队**才有鉴别力，
/// 这就是下面要先 `等到写者排队` 再放行的原因。
#[test]
fn S8_T1_逐层形状在写者等待时仍完成() {
    // 多轮：单轮可能「写者来得太晚」，多轮把「一次都没赶上」的概率压到可忽略。
    for round in 0..8 {
        let lock = Arc::new(RwLock::new(0usize));
        let (go_tx, writer) = 受闸写者(Arc::clone(&lock));
        let (armed_tx, armed_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<()>();

        let reader = {
            let lock = Arc::clone(&lock);
            thread::spawn(move || {
                // 第 0 层：持锁 → 就绪 → **确认写者已排队** → 释放（这才是被测形状的第一步）
                let g0 = lock.read().unwrap();
                let _ = armed_tx.send(());
                等到写者排队(&lock);
                std::hint::black_box(*g0);
                drop(g0);

                // 第 1..=3 层：每层「取锁 → 迭代 → **释放**」——层间 guard 不重叠（= 无 R34 递归读）
                for _layer in 1..=3usize {
                    let g = lock.read().unwrap();
                    std::hint::black_box(*g);
                    drop(g); // ⚠️ 关键：**下一层之前必须释放**
                }
                let _ = done_tx.send(());
            })
        };

        armed_rx.recv_timeout(PATIENCE).expect("读线程未就绪");
        go_tx.send(()).expect("放行写者");
        assert!(
            done_rx.recv_timeout(PATIENCE).is_ok(),
            "第 {round} 轮：逐层形状未能在 {PATIENCE:?} 内完成 —— \
             疑似退化成「持读锁期间再取锁」（R34 / hnsw_rs `hnsw.rs:633` + `:661`）"
        );
        reader.join().unwrap();
        writer.join().unwrap();
    }
}

/// **S8-T1 反向自证**：**旧形状**（持读锁期间**再取一次**）在「有写者等待」时**必须被拒**。
///
/// 两条臂：
/// - **非阻塞臂**（`try_read`）：与阻塞版 `read()` 共享准入判据 ⇒ 精确，且**不会把线程挂死**；
/// - **阻塞臂**（`read()`）：R34 的本体 —— 有界观测（[`R34_BLOCK_WINDOW`]），超时即判「阻塞」。
///
/// 收尾：`drop(g1)` 之后**所有线程都能推进**（写者拿到写锁并释放，之后探针拿到读锁）
/// ⇒ 两条臂都可以 `join`，**无需抛弃线程**。
#[test]
fn S8_T1_反向自证_旧形状的第二次读锁会被拒() {
    let lock = Arc::new(RwLock::new(0usize));
    let (go_tx, writer) = 受闸写者(Arc::clone(&lock));
    let (armed_tx, armed_rx) = mpsc::channel::<()>();

    let reader = {
        let lock = Arc::clone(&lock);
        thread::spawn(move || {
            // 等价 IterPoint::new（`hnsw.rs:633`）—— 迭代全程持有
            let g1 = lock.read().unwrap();
            let _ = armed_tx.send(());
            等到写者排队(&lock);

            // 臂 ①：非阻塞 —— 与阻塞版共享同一套准入判据
            let denied_try = lock.try_read().is_err();

            // 臂 ②：真·阻塞（R34 本体）—— 等价 IterPoint::next 的层切换（`hnsw.rs:661`）
            let (done_tx, done_rx) = mpsc::channel::<()>();
            let probe = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let g2 = lock.read().unwrap();
                    drop(g2);
                    let _ = done_tx.send(());
                })
            };
            let denied_block = done_rx.recv_timeout(R34_BLOCK_WINDOW).is_err();

            // 释放 ⇒ 写者与探针依次推进（两条臂都因此可 join）
            drop(g1);
            probe.join().unwrap();
            (denied_try, denied_block)
        })
    };

    armed_rx.recv_timeout(PATIENCE).expect("读线程未就绪");
    go_tx.send(()).expect("放行写者");
    let (denied_try, denied_block) = reader.join().unwrap();
    writer.join().unwrap();

    assert!(
        denied_try,
        "旧形状的第二次读锁（`try_read` 臂）被放行 ⇒ 本平台无「写者优先」、\
         R34 不成立，architecture-design.md §14.3 的前提需据此重估"
    );
    assert!(
        denied_block,
        "旧形状的第二次读锁（阻塞臂）在 {R34_BLOCK_WINDOW:?} 内被放行 ⇒ 同上；\
         若本臂为红而 `try_read` 臂为绿，说明两支的准入判据不一致（值得单独立项）"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// `S8-T10`：写 / 合并 / 读**并发**（验收标准 5「读不阻塞写」的**唯一**判据）
// ══════════════════════════════════════════════════════════════════════════

/// 从命中文本尾部取出编号（本文件构造的语料形如 `「并发探针 文档 12」`）。
/// 非本文件语料（尾串不是数字）⇒ `None`，调用方按「不参与编号断言」处理。
fn 编号(text: &str) -> Option<usize> {
    text.rsplit(char::is_whitespace)
        .next()
        .and_then(|s| s.parse().ok())
}

fn 纯_bm25_库() -> SearchIndex {
    SearchIndex::builder()
        .embedder(None)
        .vector_backend(VectorBackend::Brute)
        .chunker(Chunker::new(200_000, 0))
        .batch_size(1024)
        .build()
}

/// **`S8-T10`（主臂）**：写 / 合并 / 读**并发**。
///
/// # ⚠️ 与设计原文的偏差（已单列报评审）：为什么是**两线程**而不是三线程
///
/// `D-S8-05` 明确规定 **`SearchIndex` 保持 `!Sync`**（`add(&mut self)` 不改），
/// 而 `merge_pending()` **同样**要 `&mut self` ⇒ 「写」与「合并」**必须共用同一个写端**
/// （同一时刻只有一个能持有它）。⇒ 「写线程 + 合并线程 + 读线程**三者同时跑**」在安全 Rust 下
/// **不可达**（要 `unsafe` 才能绕过 `!Sync`），取形 = 「写 + 合并」一个线程 + 「读」一个线程
/// （读端是 `Searcher`，它 `Send + Sync`）。
/// **被测语义没有缩水**：读端仍与「写/合并」**重叠**进行 —— 这正是 `FR-17` 要钉的东西。
///
/// # 判据
///
/// ① **读端零 `Err`**（`FR-17`：不得以「重建索引中」为由拒绝查询）；
/// ② **读端只看到已提交前缀**（不得出现「编号 ≥ 已提交上界」的内容）；
/// ③ 写端 `merge_pending()` **零 `Err`**（单写端下属 `I8-7` 的合法路径）；
/// ④ 终态**内容完整**（合并**不丢内容**）。
#[test]
fn S8_T10_写与合并与读的并发探针() {
    let mut idx = 纯_bm25_库();
    let searcher = idx.searcher();
    // 已提交的**编号上界**（读端靠它判「看到的是不是未提交内容」）
    let committed = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let s2 = searcher.clone();
    let c2 = Arc::clone(&committed);
    let st2 = Arc::clone(&stop);
    let reader = thread::spawn(move || {
        let mut rounds = 0usize;
        while !st2.load(Ordering::Relaxed) {
            let resp = s2
                .search_with("并发探针")
                .mode(SearchMode::Bm25)
                .top_n(10)
                .exec()
                .expect("① 读端不得出错（FR-17：合并与写期间都不得拒绝查询）");
            let up = c2.load(Ordering::Relaxed);
            for h in &resp.hits {
                if let Some(n) = 编号(&h.text) {
                    assert!(
                        n < up,
                        "② 读端看到了**未提交**的内容（编号 {n} ≥ 已提交上界 {up}）\
                         —— 可见性边界应当是 `commit()` 之后（NFR-14 ①）"
                    );
                }
            }
            rounds += 1;
            thread::yield_now();
        }
        rounds
    });

    // 写 + 合并（**同一个写端**，交替驱动；见上方偏差说明）
    let mut merges = 0usize;
    for i in 0..60 {
        idx.add(Document::new(format!("并发探针 文档 {i}")))
            .unwrap();
        idx.commit().unwrap();
        committed.store(i + 1, Ordering::Relaxed);
        if i % 5 == 4 {
            let rep: Option<MergeReport> = idx
                .merge_pending()
                .expect("③ 单写端下 merge_pending 不应 Err");
            if let Some(r) = rep {
                assert_eq!(r.segments_merged, 1, "merge_pending 一次只并一段");
                merges += 1;
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    let rounds = reader.join().unwrap();
    assert!(
        rounds > 0,
        "前提：读端至少完整跑完一轮（否则本用例什么都没测）"
    );
    assert!(
        merges > 0,
        "前提：至少发生过一次合并（否则没测到「合并期并发读」）"
    );

    let all = idx
        .searcher()
        .search_with("并发探针")
        .mode(SearchMode::Bm25)
        .top_n(200)
        .exec()
        .unwrap();
    assert_eq!(
        all.hits.len(),
        60,
        "④ 合并**不丢内容**：写完的 60 篇必须全部可检索"
    );
}

/// **`S8-T10`（双写端臂）**：两个写端（共享同一个 `Arc<Shared>`）**交替**提交 ⇒
/// **段 ID 空间不得重叠**（`I8-7`）。
///
/// # 这条在钉什么
///
/// 两个写端各自有**独立的 `SegmentBuilder`**，而「段基址 = 创建时刻的已发布长度」。
/// ⇒ 若两个写端都按自己的基址发布，**两段的 ID 空间会重叠**（同一 `chunk_id` 指向两个分片）
/// ⇒ 检索结果错乱、`remove` 误伤。=>
/// 后提交者的基址已过期 ⇒ **必须被拒**（`Err(Busy)`，`S8-03` 评审 P2-1 的那条对账）。
///
/// ⚠️ 两个写端**不同时**操作（`!Sync`）：「交替」正是这条对账存在的前提。
/// ⚠️ 被拒后按 `error.rs` 的恢复指引**重做 `add`**（被拒时 builder 已被换成对齐当前基址的新的）。
#[test]
fn S8_T10_双写端交替提交不得让段ID空间重叠() {
    let mut a = 纯_bm25_库();
    a.add(Document::new("A 端文档 0")).unwrap();
    a.commit().unwrap();

    // 第二个写端：与 `a` 共享同一个 `Arc<Shared>`（`into_index()` 总是成功）
    let mut b = a.searcher().into_index().unwrap();

    // 🔑 **交替提交下，双方的基址都会轮流过期**（谁后提交谁成功）—— 这正是被测语义：
    //    「过期基址的提交必须被拒」。⇒ 两侧都要**按 `error.rs` 的恢复指引重做**
    //    （被拒时内容已丢弃；`commit()` 末尾已把 builder 换成与当前基址对齐的新的）。
    let mut rejected = 0usize;
    fn try_commit(idx: &mut SearchIndex, text: String, rejected: &mut usize) {
        idx.add(Document::new(text.clone())).unwrap();
        match idx.commit() {
            Ok(()) => {}
            Err(Error::Busy(_)) => {
                *rejected += 1;
                idx.add(Document::new(text)).unwrap();
                idx.commit()
                    .expect("重做 `add` 后必须能提交（`error.rs` 的恢复指引）");
            }
            Err(e) => panic!("只应出现 Busy（暂态并发），实际是 {e}"),
        }
    }
    for i in 1..8 {
        try_commit(&mut a, format!("A 端文档 {i}"), &mut rejected);
        try_commit(&mut b, format!("B 端文档 {i}"), &mut rejected);
    }
    assert!(
        rejected > 0,
        "基址对账**必须**在这一交替序列里生效过（否则两段的 ID 空间早就重叠了）"
    );

    // 终态：两端的全部内容都可检索，且**全局 chunk_id 无重复**（I8-7）
    let resp = a
        .searcher()
        .search_with("端文档")
        .mode(SearchMode::Bm25)
        .top_n(200)
        .exec()
        .unwrap();
    let ids: Vec<ChunkId> = resp.hits.iter().map(|h| h.chunk_id).collect();
    let uniq: std::collections::HashSet<ChunkId> = ids.iter().copied().collect();
    assert_eq!(
        ids.len(),
        uniq.len(),
        "全局 chunk_id 不得重复 —— 重复即「段 ID 空间重叠」（I8-7 被破坏）；实测 {ids:?}"
    );
    assert_eq!(
        resp.hits.len(),
        1 + 7 + 7,
        "终态应有 1（A 首批）+ 7（A 后续）+ 7（B 重做后提交）= 15 篇"
    );
}

/// **`S8-06` 的落地取证**：合并器原语面对**纯 BM25 装配**时的报告口径。
///
/// ⚠️ 这条刻意**不**用 `#[ignore]`：它把「`MergeReport.vector_strategy` 的三个取值」
/// 中**最容易写错的那一个**（`None` vs `Rebuild`）钉在 CI 里 —— 纯 BM25 库**没有**向量侧
/// 工作，记成 `Rebuild` 是「把无说成有」（`NFR-07`：不撒谎）。
#[test]
fn S8_06_纯BM25库的合并报告应为None策略() {
    let mut idx = 纯_bm25_库();
    for i in 0..3 {
        idx.add(Document::new(format!("报告口径 文档 {i}")))
            .unwrap();
        idx.commit().unwrap();
    }
    let rep: MergeReport = idx.merge_all().unwrap();
    assert_eq!(rep.segments_merged, 3);
    assert_eq!(
        rep.vector_strategy,
        VectorMergeStrategy::None,
        "纯 BM25 装配**没有**向量侧工作 ⇒ 必须记 None（记成 Rebuild 就是「把无说成有」）"
    );
    assert_eq!(rep.vector_merge_ms, 0, "没有向量侧工作 ⇒ 耗时口径应为 0");
    let _: Result<()> = Ok(()); // 保持 `Result` 在作用域（与上面两个用例的 import 一致）
}
