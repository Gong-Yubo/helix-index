#![cfg(feature = "local-rerank")]

//! V2 Step 7 / S7-01：`LocalReranker` 的**真模型**用例（S7-T8 / S7-T10）。
//!
//! # 为什么全部 `#[ignore]`
//!
//! 模型合计 ≈2.19GB（`#23` 前置 ②）⇒ 沿用项目纪律：CI **不下载模型**、性能数字不进 CI。
//! 本地跑法：
//!
//! ```text
//! cargo test -p helix-core --features local-rerank -- --ignored --nocapture
//! ```
//!
//! ⚠️ **不得**把「`#[ignore]` 用例未跑」写成「已覆盖」——`eval-report.md` §8.14 必须
//! **显式记录**哪些 `#[ignore]` 用例真的跑过、输出是什么。
//!
//! # 这里只覆盖「精排器本身」
//!
//! 窗口**放开**（`candidate_k` 联动 / `take_n` / 截断 / `Metrics` 采集点）属 S7-02，
//! 用**假精排器**在 `query/searcher.rs` 的秒级单测里钉（不需要真模型）。本文件的
//! `R = 50` 档验的是「精排器拿到 50 条时会输出什么」——即**截断契约**，不是编排层
//! 会不会真的给它 50 条。

use helix_core::query::{Explain, Hit};
use helix_core::rerank::{LocalReranker, Reranker};

/// 固定 query（**跨进程用例要求输入逐字节可复现** ⇒ 不得用随机 / HashMap 迭代序）。
const QUERY: &str = "如何实现支持中文的 BM25 检索";

/// 候选正文（`(chunk_id, text)`）：混入相关与无关，让重排有实际影响。
const DOCS: &[(u32, &str)] = &[
    (
        1,
        "中文分词是检索的第一步，BM25 需要先对中文做切分才能计算词频",
    ),
    (
        2,
        "BM25 是一种基于词频与逆文档频率的排序函数，广泛用于全文检索",
    ),
    (3, "今天服务器机房温度偏高，需要检查空调的排水管是否堵塞"),
    (4, "向量检索通过近邻搜索处理语义相似，与词面匹配互补"),
    (5, "项目经理通知：本周五之前提交季度总结报告，逾期视为放弃"),
    (
        6,
        "支持中文的检索需要处理未登录词与同义词，分词粒度影响召回",
    ),
    (7, "把咖啡豆研磨成中细颗粒后，用 92 度的水缓慢注水萃取"),
    (8, "倒排索引把词映射到文档列表，是 BM25 检索的底层数据结构"),
    (9, "周末计划去爬山，需要提前查看天气并准备防滑的登山鞋"),
    (10, "混合检索把 BM25 与向量两路结果融合，兼顾词面与语义"),
];

const TOP_N: usize = 10;

fn mk_hit(id: u32, text: &str) -> Hit {
    Hit {
        chunk_id: id,
        doc_id: id,
        // 输入分数刻意设为「与相关性无关」的常数：若精排没生效，输出顺序就等于输入顺序，
        // 断言才有鉴别力（否则输入本身已经排好，看不出任何东西）。
        score: 1.0,
        text: text.to_string(),
        source: format!("doc-{id}.md"),
        metadata: serde_json::json!({}),
        explain: Explain::default(),
    }
}

/// `TOP_N` 条候选（= `R = k` 档的输入）。
fn candidates() -> Vec<Hit> {
    DOCS.iter().map(|(id, t)| mk_hit(*id, t)).collect()
}

/// 更宽的候选池（`R = 50` 档）：前 `DOCS.len()` 条与 [`candidates`] 一致，其余为填充。
fn wide_candidates() -> Vec<Hit> {
    let mut v = candidates();
    v.extend((11..=50).map(|i| mk_hit(i, "另一段与查询无关的填充文本，用于把候选池撑到 50 条")));
    v
}

/// 读数：`(chunk_id, score 的位模式)`。
///
/// ⚠️ 用 `to_bits()` 而不是 `==`：本用例要证的是**逐位**一致（NFR-06 的「完全相同结果」），
/// 而不是「近似相等」。
fn readings(r: &LocalReranker, hits: Vec<Hit>, top_n: usize) -> Vec<(u32, u32)> {
    let out = r.rerank(QUERY, hits, top_n).expect("精排不得失败");
    assert!(
        !out.is_empty(),
        "读数不得为空（否则下面的相等断言是空转通过）"
    );
    out.iter()
        .map(|h| (h.chunk_id, h.score.to_bits()))
        .collect()
}

/// **S7-T8 / P1**：同进程、同一 `R`、同一精排器（同一 ONNX 会话）连续 N 次 ⇒ 逐位一致。
#[test]
#[ignore = "需下载 ≈2.19GB 模型（local-rerank）"]
fn p1_同进程重复三次逐位一致() {
    let r = LocalReranker::new().expect("模型加载失败");
    let first = readings(&r, candidates(), TOP_N);
    assert_eq!(first.len(), TOP_N, "R = k 时输出条数不变");

    for round in 1..3 {
        let again = readings(&r, candidates(), TOP_N);
        assert_eq!(
            first, again,
            "第 {round} 次重复与首次不一致 ⇒ NFR-06 在精排上不成立（R47 / P1）"
        );
    }
}

/// P2 的**子进程入口**：由 [`p2_跨进程两次逐位一致`] 用 `--exact p2_child_reads --ignored`
/// 再跑一遍**同一个测试二进制**（= 独立进程、独立 ONNX 会话）。
///
/// ⚠️ 单独给它一个用例（而不是在父进程里用线程模拟「另一个进程」）：跨进程的差异源
/// （内存布局 / 线程池状态 / 会话初始化）只有真的换进程才会出现。
#[test]
#[ignore = "P2 的子进程入口，由 p2_跨进程两次逐位一致 调用"]
fn p2_child_reads() {
    let r = LocalReranker::new().expect("模型加载失败");
    for (id, bits) in readings(&r, candidates(), TOP_N) {
        // 父进程按此前缀解析；格式必须稳定。
        println!("READING {id} {bits}");
    }
}

/// **S7-T8 / P2**：两次**独立进程**、同一 `R`、同一输入 ⇒ 逐位一致。
#[test]
#[ignore = "需下载 ≈2.19GB 模型（local-rerank）"]
fn p2_跨进程两次逐位一致() {
    let exe = std::env::current_exe().expect("拿不到测试二进制路径");
    let child = std::process::Command::new(&exe)
        .args(["--exact", "p2_child_reads", "--ignored", "--nocapture"])
        .output()
        .expect("子进程必须能启动");
    let stdout = String::from_utf8_lossy(&child.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&child.stderr).into_owned();
    assert!(child.status.success(), "子进程失败：\n{stdout}\n{stderr}");

    // 解析子进程读数。⚠️ 先自证「真的解析到了」，否则「空 == 空」是空转通过。
    let child_reads: Vec<(u32, u32)> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("READING "))
        .map(|l| {
            let mut it = l.split_whitespace();
            let id = it
                .next()
                .expect("缺 chunk_id")
                .parse()
                .expect("chunk_id 非数字");
            let bits = it.next().expect("缺位模式").parse().expect("位模式非数字");
            (id, bits)
        })
        .collect();
    assert!(
        !child_reads.is_empty(),
        "子进程没有产出任何读数 ⇒ 下面的相等断言无意义\nstdout={stdout}"
    );

    let local = readings(
        &LocalReranker::new().expect("模型加载失败"),
        candidates(),
        TOP_N,
    );
    assert_eq!(local.len(), child_reads.len(), "两次进程的输出条数不同");
    assert_eq!(
        local, child_reads,
        "两次独立进程的结果不一致 ⇒ NFR-06 在精排上不成立（R47 / P2）"
    );
}

/// **S7-T10（真模型 smoke）**：`R = k` ⇒ 集合不变、仅顺序可能变；`R = 50` ⇒ 截断契约。
#[test]
#[ignore = "需下载 ≈2.19GB 模型（local-rerank）"]
fn t10_真模型smoke_重排与截断契约() {
    let r = LocalReranker::new().expect("模型加载失败").with_window(50);
    assert_eq!(r.candidate_window(10), 50, "窗口由 R 决定（不是 k）");
    assert!(
        r.id().contains("window=50") && r.id().contains("max_len=512"),
        "身份字符串必须体现运行期参数（D-S7-08）：{}",
        r.id()
    );

    // --- R = k：集合不变 ---
    let base = candidates();
    let out = r.rerank(QUERY, base.clone(), TOP_N).expect("精排不得失败");
    assert_eq!(out.len(), TOP_N, "R = k ⇒ 条数不变");
    let mut got: Vec<u32> = out.iter().map(|h| h.chunk_id).collect();
    let mut want: Vec<u32> = base.iter().map(|h| h.chunk_id).collect();
    got.sort_unstable();
    want.sort_unstable();
    assert_eq!(got, want, "R = k ⇒ 集合不变（只可能改顺序）");

    // --- R = 50：输入 50 条、输出截断到 k ---
    let wide = wide_candidates();
    assert_eq!(wide.len(), 50);
    let out = r.rerank(QUERY, wide.clone(), TOP_N).expect("精排不得失败");
    assert_eq!(out.len(), TOP_N, "必须截断到 top_n");
    let pool: std::collections::HashSet<u32> = wide.iter().map(|h| h.chunk_id).collect();
    assert!(
        out.iter().all(|h| pool.contains(&h.chunk_id)),
        "不得凭空造出候选（输出 ⊆ 输入）"
    );

    // --- 排序与分数契约 ---
    assert!(
        out.windows(2).all(|w| w[0].score >= w[1].score),
        "hits 必须按 score 严格不增"
    );
    assert!(
        out.iter().all(|h| h.score > 0.0 && h.score < 1.0),
        "σ(logit) 必须落在开区间 (0,1)"
    );
    for h in &out {
        assert!(h.score.is_finite(), "chunk {} 的分数非有限值", h.chunk_id);
    }

    // 观测（**不断言**：「增补」的幅度取决于模型实际打分，不是契约）：
    // 输出是否超出了输入前 k 条 ⇒ 这正是「放开窗口」想要的那部分收益。
    let top_k: std::collections::HashSet<u32> =
        wide.iter().take(TOP_N).map(|h| h.chunk_id).collect();
    let promoted = out.iter().filter(|h| !top_k.contains(&h.chunk_id)).count();
    println!("R = 50 时，落入最终 top-{TOP_N} 但不在输入前 {TOP_N} 条的候选数 = {promoted}");
}
