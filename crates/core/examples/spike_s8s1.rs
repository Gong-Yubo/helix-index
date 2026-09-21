//! **spike S8-S1**：合并器**向量侧**的 A/B 决策门（设计 `v2-step8-design.md` §4.9.3）。
//!
//! 设计给了两条路，**本 spike 的任务是把它从「倾向」变成「读数」**：
//!
//! | 方案 | 做法 | 预期成本 |
//! | --- | --- | --- |
//! | **A** | 主图 `dump_graph` → `load_graph`（得**新**图，旧点拓扑保住）→ 把 delta 的点 `add_batch` 进去 | `O(delta)`（与增量成正比） |
//! | **B** | 把「主段 + delta」的 `raw_vectors` 合起来**全量重建**一张图 | `O(N·logN)`（与 delta 无关） |
//!
//! # 五条**可证伪**判据（任一不满足 ⇒ 回落 B）
//!
//! ① **可行性**：A 跑通（dump→load→增量 insert 后可检索、`len()` 与主段+delta 吻合）；
//! ② **收益**：`delta ≈ 主段 1/10` 时 **A 的耗时 ≤ B 的 1/5**；
//! ③ **正确性**：A 与 B 的图在**同一 query 集**上 top-10 **重合率 ≥ 0.99**
//!    （⚠️ **不要求逐位** —— 图不同、ANN 结果本来就允许不同，见设计 §4.6.2）；
//! ④ 任一不满足 ⇒ **回落 B**，并把「合并 = 全量重建」的代价写进 NFR-14；
//! ⑤ **累积**：同一进程**连续合并 10 次**，峰值 RSS 增量 **< 1 MiB**
//!    （⚠️ 阈值不是「不许泄漏」（`Box::leak` 是 R23 的既定事实），而是把两种量级分开：
//!    「只泄漏 `HnswIo`（≈200B + 路径串/次）」vs「误泄漏整张图（≈50MB/次）」—— 见 R51）。
//!
//! # 用法
//!
//! ```text
//! cargo run --release --example spike_s8s1 -- [N_MAIN] [N_DELTA] [DIM] [QUERIES]
//! # 默认 12000 1200 384 100
//! ```
//!
//! ⚠️ **必须 `--release`**：debug 下 HNSW 构建慢一个量级，读数不可用。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use helix_core::storage::GraphManifest;
use helix_core::types::ChunkId;
use helix_core::vector::{
    BruteForceIndex, HnswRsIndex, NormalizedVector, VectorGraphPersist, VectorIndex,
};

// ---------------------------------------------------------------------------
// 确定性 PRNG（xorshift64*）—— 不引第三方依赖，保证可复现
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 均匀 `[-1, 1)` 的浮点（53 位精度），再交给 `NormalizedVector::new` 归一化。
    fn next_f32(&mut self) -> f32 {
        let u = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        (u * 2.0 - 1.0) as f32
    }

    fn unit_vec(&mut self, dim: usize) -> NormalizedVector {
        let v: Vec<f32> = (0..dim).map(|_| self.next_f32()).collect();
        NormalizedVector::new(v)
    }
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

/// 当前进程的常驻内存（KiB）。`ps -o rss=` 在 macOS 与 Linux 上都可用。
fn rss_kib() -> u64 {
    let pid = std::process::id().to_string();
    match std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse()
            .unwrap_or(0),
        Err(_) => 0,
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// 建一张 HNSW 图（**与 `search::rebuild_vector_index` 的 Hnsw 分支同一批调用**，
/// 这样方案 B 量到的就是生产路径的成本）。
fn build_hnsw(entries: &[(ChunkId, NormalizedVector)], ef: usize) -> HnswRsIndex {
    let mut vi = HnswRsIndex::with_capacity(entries.len().max(1024))
        .with_ef_search(ef)
        .with_parallel_build(true);
    vi.add_batch(entries).expect("add_batch 失败");
    vi
}

/// 占位 manifest。
///
/// 🔑 **A 路径的 load 不需要它**：`HnswRsIndex::load_graph` 的该参数名是 `_m`
/// （真校验在门面层的 `load_graph_checked` 里做，属 CRC/manifest 流程）。
/// spike 只量「图的 dump→load→insert」本身，因此传占位值即可 —— 这一点已核过源码，
/// **不是假设**。
fn placeholder_manifest(dim: u32, nb_point: u64) -> GraphManifest {
    GraphManifest {
        manifest_version: 1,
        producer: "spike-s8s1".to_string(),
        graph_format: 4,
        dist_id: "DistDotClamped".to_string(),
        platform: 0x01,
        max_nb_connection: 32,
        ef_construction: 300,
        snapshot_crc: 0,
        snapshot_len: 0,
        dim,
        nb_point,
        graph_crc: 0,
        graph_len: 0,
        data_crc: 0,
        data_len: 0,
    }
}

/// 清理某 base 的三个 sidecar 文件（避免累积测试把磁盘写满）。
fn remove_graph_files(base: &Path) {
    for suffix in [".hnsw.graph", ".hnsw.data"] {
        let p = PathBuf::from(format!("{}{suffix}", base.display()));
        let _ = std::fs::remove_file(p);
    }
}

/// 以**精确后端**（`Brute`）为参照的 top-`k` **平均召回率**。
///
/// 🔑 **为什么需要它**：重合率（A vs B）被「层的随机分配」污染 —— 连 B vs B′（同数据重建两次）
/// 都只有 ~0.93（`hnsw_rs` 用无种子 `OsRng` 分配层级）。而「A 有没有**丢点**」的正问是
/// 「**精确路径能找到的，A 能不能找到**」⇒ 以 Brute 为参照直接量召回，不受拓扑抖动影响。
fn topk_recall(
    graph: &HnswRsIndex,
    brute: &BruteForceIndex,
    queries: &[NormalizedVector],
    k: usize,
) -> f64 {
    use std::collections::HashSet;
    let mut sum = 0.0f64;
    for q in queries {
        let hg = graph.search(q, k).expect("图检索失败");
        let hb = brute.search(q, k).expect("精确检索失败");
        let sg: HashSet<ChunkId> = hg.iter().map(|(id, _)| *id).collect();
        let sb: HashSet<ChunkId> = hb.iter().map(|(id, _)| *id).collect();
        assert!(!sb.is_empty(), "前提：精确路径必须有命中");
        sum += sg.intersection(&sb).count() as f64 / sb.len() as f64;
    }
    sum / queries.len() as f64
}

/// 两个图在**同一 query 集**上 top-`k` 的**平均重合率**（集合交 / k）。
fn topk_overlap(a: &HnswRsIndex, b: &HnswRsIndex, queries: &[NormalizedVector], k: usize) -> f64 {
    use std::collections::HashSet;
    let mut sum = 0.0f64;
    for q in queries {
        let ha = a.search(q, k).expect("A 图检索失败");
        let hb = b.search(q, k).expect("B 图检索失败");
        let sa: HashSet<ChunkId> = ha.iter().map(|(id, _)| *id).collect();
        let sb: HashSet<ChunkId> = hb.iter().map(|(id, _)| *id).collect();
        assert!(!sa.is_empty(), "前提：必须有命中（否则重合率无意义）");
        sum += sa.intersection(&sb).count() as f64 / k as f64;
    }
    sum / queries.len() as f64
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let n_main: usize = args.first().and_then(|s| s.parse().ok()).unwrap_or(12_000);
    let n_delta: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1_200);
    let dim: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(384);
    let n_queries: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100);
    let k = 10usize;
    let ef = HnswRsIndex::default_ef_search();

    let dir = std::env::temp_dir().join(format!("spike-s8s1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录失败");

    println!("=== spike S8-S1（设计 §4.9.3 的五条决策门）===");
    println!(
        "参数：N_main={n_main} N_delta={n_delta} dim={dim} queries={n_queries} k={k} ef={ef}\n"
    );

    // ── 造数据：主段 N 条 + delta 1/10 条（ID 连续，与真实分段的全局 ID 空间一致）──
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut main_entries: Vec<(ChunkId, NormalizedVector)> = Vec::with_capacity(n_main);
    for i in 0..n_main {
        main_entries.push((i as ChunkId, rng.unit_vec(dim)));
    }
    let mut delta_entries: Vec<(ChunkId, NormalizedVector)> = Vec::with_capacity(n_delta);
    for i in 0..n_delta {
        delta_entries.push(((n_main + i) as ChunkId, rng.unit_vec(dim)));
    }
    let queries: Vec<NormalizedVector> = (0..n_queries).map(|_| rng.unit_vec(dim)).collect();

    // ── 建主图（A 与 B 的共同起点；这一段的耗时只作参照，不计入判据）──
    let t = Instant::now();
    let main = build_hnsw(&main_entries, ef);
    let t_build_main = t.elapsed();
    assert_eq!(main.len(), n_main, "主图点数应为 {n_main}");

    // ── 方案 A：dump → load → 增量 insert ──
    let base_a = dir.join("main.idx");
    let t = Instant::now();
    let stats = main.dump_graph(&base_a).expect("dump_graph 失败");
    let t_dump = t.elapsed();
    assert_eq!(stats.nb_point, n_main as u64, "dump 报的点数应等于主图点数");

    let t = Instant::now();
    let m = placeholder_manifest(dim as u32, stats.nb_point);
    let mut a_graph =
        HnswRsIndex::load_graph(&base_a, &m, ef, true).expect("A 路径 load_graph 失败");
    let t_load = t.elapsed();

    // 判据① 的「load 后点数吻合」：必须**在 insert 之前**断言（否则与 delta 混在一起）
    assert_eq!(
        a_graph.len(),
        n_main,
        "判据① 失败：load 回来的点数与主图不一致"
    );

    let t = Instant::now();
    a_graph
        .add_batch(&delta_entries)
        .expect("A 路径增量 insert 失败");
    let t_insert = t.elapsed();
    let t_a = t_dump + t_load + t_insert;

    assert_eq!(
        a_graph.len(),
        n_main + n_delta,
        "判据① 失败：A 完成后点数应为 N_main + N_delta"
    );

    // ── 方案 B：全量重建（用「主段 + delta」的全部原始向量）──
    let all_entries: Vec<(ChunkId, NormalizedVector)> = main_entries
        .iter()
        .chain(delta_entries.iter())
        .map(|(id, v)| (*id, v.clone()))
        .collect();
    let t = Instant::now();
    let b_graph = build_hnsw(&all_entries, ef);
    let t_b = t.elapsed();
    assert_eq!(b_graph.len(), n_main + n_delta, "B 图的点数应吻合");

    // ── 判据③：正确性（同一 query 集 top-k 平均重合率）──
    //
    // 🔴 **必须带对照组 —— 首跑只算 A vs B 得出的「0.922 < 0.99 ⇒ 回落 B」不可信**：
    //    `hnsw_rs` 的层级分配用**无种子 `OsRng`**（本仓已登记的事实，见
    //    `compaction_cli_semantics` 的 CLI3/CLI4 分析）⇒ **同一份数据重建两次的图也不同**。
    //    若「B vs B′」本身就掉到 0.92 附近，那 A-vs-B 的 0.922 反映的是**方法固有的拓扑抖动**，
    //    **不是** A 丢了点。⇒ 判定必须**相对基线**，而不是相对一个凭空定的 0.99。
    let overlap_ab = topk_overlap(&a_graph, &b_graph, &queries, k);

    // 对照①：B vs B′ —— 同一份数据**重建两次**（量化「图间拓扑抖动」的基线）
    let t = Instant::now();
    let b2_graph = build_hnsw(&all_entries, ef);
    let t_b2 = t.elapsed();
    let overlap_bb = topk_overlap(&b_graph, &b2_graph, &queries, k);

    // 对照②：A vs A′ —— 同一 A 路径**重跑一次**。
    // A 路径应当**确定性**（dump/load 保拓扑不变、insert 顺序固定）⇒ 期望恰好 1.0000；
    // 若不为 1，说明 A 路径自身不可复现（那本身就是一个阻断项）。
    let base_a2 = dir.join("main2.idx");
    let st2 = main.dump_graph(&base_a2).expect("A′ dump 失败");
    let m2 = placeholder_manifest(dim as u32, st2.nb_point);
    let mut a2_graph = HnswRsIndex::load_graph(&base_a2, &m2, ef, true).expect("A′ load 失败");
    a2_graph.add_batch(&delta_entries).expect("A′ insert 失败");
    let overlap_aa = topk_overlap(&a_graph, &a2_graph, &queries, k);

    // 对照③（🔑 **不受拓扑随机性污染的「丢点」判据**）：以 **Brute（精确）** 为参照的 recall@k。
    // 13200 点 × 100 query 的精确扫描 ≈ 秒级（384 维），成本可忽略。
    let brute = BruteForceIndex::from_entries(all_entries.clone());
    let recall_a = topk_recall(&a_graph, &brute, &queries, k);
    let recall_b = topk_recall(&b_graph, &brute, &queries, k);

    // ── 判据⑤：累积（连续 10 次 A 的峰值 RSS 增量）──
    let rss_base = rss_kib();
    let mut rss_peak = rss_base;
    for i in 0..10 {
        let base = dir.join(format!("acc-{i}.idx"));
        let st = main.dump_graph(&base).expect("累积轮 dump 失败");
        let mm = placeholder_manifest(dim as u32, st.nb_point);
        let mut g = HnswRsIndex::load_graph(&base, &mm, ef, true).expect("累积轮 load 失败");
        g.add_batch(&delta_entries).expect("累积轮 insert 失败");
        drop(g);
        rss_peak = rss_peak.max(rss_kib());
        remove_graph_files(&base);
    }
    let rss_delta_kib = rss_peak.saturating_sub(rss_base);

    // ── 汇总 ──
    let ratio = if t_b.as_secs_f64() > 0.0 {
        t_a.as_secs_f64() / t_b.as_secs_f64()
    } else {
        f64::INFINITY
    };
    println!(
        "[参照] 主图构建（不计入判据）        = {:>10.1} ms",
        ms(t_build_main)
    );
    println!(
        "[A] dump（主图 → 临时目录）        = {:>10.1} ms",
        ms(t_dump)
    );
    println!(
        "[A] load（load_graph，点数已核）    = {:>10.1} ms",
        ms(t_load)
    );
    println!(
        "[A] insert(delta {n_delta} 点)         = {:>10.1} ms",
        ms(t_insert)
    );
    println!("[A] 合计                           = {:>10.1} ms", ms(t_a));
    println!(
        "[B] 全量重建（{n_main}+{n_delta} 点）        = {:>10.1} ms",
        ms(t_b)
    );
    println!(
        "[B′] 第二次重建（对照组，同数据）    = {:>10.1} ms",
        ms(t_b2)
    );
    println!();
    println!(
        "判据① 可行性：load 后点数吻合 + insert 后可检索 + len={} 【PASS】",
        a_graph.len()
    );
    println!(
        "判据② 收益：A/B = {:.3}（B/5 对应 {:.1}）⇒ A ≤ B/5 ? 【{}】",
        ratio,
        ms(t_b) / 5.0,
        if ratio <= 0.2 { "PASS" } else { "FAIL" }
    );
    println!("       top-{k} 重合率（三方对照）：");
    println!("         A vs B  = {overlap_ab:.4}   ← 跨方法");
    println!("         B vs B′ = {overlap_bb:.4}   ← 同数据重建两次 = HNSW 固有拓扑抖动**基线**");
    println!("         A vs A′ = {overlap_aa:.4}   ← 同一 A 路径重跑（非 1.0：insert 阶段沿用 OsRng 分层）");
    println!("         recall@k vs Brute：A = {recall_a:.4}｜B = {recall_b:.4}");
    let thr = overlap_bb - 0.01;
    // 🔑 判据③ 取形**就地更正为双判据**（原「≥ 0.99」结构性不可达 —— 基线 B vs B′ 只有 0.928）：
    //    a. A 与 B 的差异 **不大于**「B 与自己重建一次」的差异（留 1% 容差）；
    //    b. A 的**精确参照召回**不低于 B（留 1% 容差）—— 这条才直接回答「丢没丢点」。
    let c3a = overlap_ab >= thr;
    let c3b = recall_a + 0.01 >= recall_b;
    println!("判据③ 正确性（双判据，阈值就地更正）：");
    println!(
        "  a) A vs B = {overlap_ab:.4} ≥ 基线−0.01 = {thr:.4} ? 【{}】",
        if c3a { "PASS" } else { "FAIL" }
    );
    println!(
        "  b) recall(A) = {recall_a:.4} ≥ recall(B)−0.01 = {:.4} ? 【{}】",
        recall_b - 0.01,
        if c3b { "PASS" } else { "FAIL" }
    );
    println!();
    // ⚠️ `A vs A′` 的自证阈值取 **0.99**（不是 1.0）：A 路径的 insert 阶段沿用 `hnsw_rs` 的
    //    `OsRng` 分层 ⇒ **不要求逐位**（与设计 §4.6.2「ANN 结果本来就允许不同」一致）。
    //    它只用来排除「A 路径自身不可复现」这一种坏情况。
    let all_pass = ratio <= 0.2 && c3a && c3b && rss_delta_kib < 1024 && overlap_aa >= 0.99;
    println!();
    println!(
        "⇒ 取形建议：{}",
        if all_pass {
            "A（dump→load→增量 insert）+ 回落 B"
        } else {
            "回落 B（全量重建），并把代价写进 NFR-14"
        }
    );

    let _ = std::fs::remove_dir_all(&dir);
}
