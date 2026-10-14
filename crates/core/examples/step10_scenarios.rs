//! V2 Step 10 场景机制端到端示例：**命名空间过滤 / MMR 去重 / token budget 裁剪**。
//!
//! 运行（需 `local-embed`，首次会下载 bge-small-zh-v1.5 约 91MB）：
//!
//! ```bash
//! cargo run -p helix-core --example step10_scenarios
//! ```
//!
//! 这三个机制**默认关**（不启用时全链路与引入前**逐位一致**）⇒ 本示例逐段**显式启用**，
//! 并打印「启用前 / 启用后」的差异（次序 / 长度 / 可观测信号），好让读者一眼看出各自改了什么。
//!
//! ⚠️ 后处理阶段只有**一个槽位**（`D-S10-01`）⇒ 三种后处理器**后设置者生效**，不能叠加。
//! 所以本示例为每一段**各建一个索引**（代价：每段都要初始化一次 embedder）。
//!
//! 设计见 `docs/devel/v2-step10-design.md`（`S10-08` 文档段 / 测试计划 `S10-T18`）。

use std::sync::Arc;

use helix_core::prelude::*;
use helix_core::schema::Filter;
use helix_core::search::{SearchIndex, SearchIndexBuilder};

/// 命名空间字段名由**调用方约定**（内核不保留魔法字段名）。
const NS_FIELD: &str = "ns";
/// 时间字段名同理（内核不猜 `ts_ms` / `created_at`）。
const TS_FIELD: &str = "ts_ms";
/// 「现在」= 2026-10-01T12:00:00Z（示例固定它 ⇒ 读数**可复现**）。
const NOW_MS: i64 = 1_790_856_000_000;
const DAY_MS: i64 = 86_400_000;

/// 六篇小语料：A 组三篇**高度冗余**（词面几乎重合，都会被「HNSW / 向量检索」召回），
/// B 组三篇各讲一件事（互不冗余）。每篇带 `ns` 与 `ts_ms` 元数据。
///
/// `(source, ns, text, ts_ms)`
const DOCS: &[(&str, &str, &str, i64)] = &[
    (
        "hnsw-1.md",
        "tenant-a",
        "HNSW 是一种近似最近邻搜索的图结构算法，用多层跳表加速向量检索。",
        NOW_MS - 400 * DAY_MS,
    ),
    (
        "hnsw-2.md",
        "tenant-a",
        "HNSW 用多层跳表做近似最近邻搜索，是加速向量检索的图结构算法。",
        NOW_MS - 300 * DAY_MS,
    ),
    (
        "hnsw-3.md",
        "tenant-a",
        "近似最近邻搜索可用 HNSW 图结构加速，其多层跳表提升向量检索速度。",
        NOW_MS - DAY_MS,
    ),
    (
        "bm25.md",
        "tenant-b",
        "BM25 依据词频与逆文档频率给倒排索引的命中打分，是经典稀疏检索算法。",
        NOW_MS - 200 * DAY_MS,
    ),
    (
        "filter.md",
        "tenant-b",
        "元数据过滤把谓词下推到字段索引，避免对全量文档逐条求值。",
        NOW_MS - 100 * DAY_MS,
    ),
    (
        "rerank.md",
        "tenant-b",
        "交叉编码器精排把 query 与候选正文一起送进模型，逐对打分后重排。",
        NOW_MS - 2 * DAY_MS,
    ),
];

const QUERY: &str = "近似最近邻搜索怎么加速向量检索";

/// 按给定装配建库 + 摄入 + `commit()`（写端可见性由 `commit()` 决定，S8-02）。
fn build(builder: SearchIndexBuilder) -> anyhow::Result<SearchIndex> {
    let mut index = builder.build();
    for (source, ns, text, ts_ms) in DOCS {
        index.add(
            Document::new(*text)
                .with_source(*source)
                .with_metadata(serde_json::json!({ NS_FIELD: *ns, TS_FIELD: *ts_ms })),
        )?;
    }
    index.commit()?;
    Ok(index)
}

/// 打印一行「名次 / 出处 / 分数 / 机制信号」。
fn print_hits(title: &str, searcher: &helix_core::search::Searcher, filter: Option<&Filter>) {
    let mut req = searcher.search_with(QUERY).top_n(6);
    if let Some(f) = filter {
        req = req.filter(f);
    }
    let resp = req.exec().expect("检索失败");
    println!("\n--- {title}（{} 条）---", resp.hits.len());
    for (i, h) in resp.hits.iter().enumerate() {
        println!(
            "  {}. {:<12} score={:.6} mmr={:?} units={:?} decay={:?}",
            i + 1,
            h.source,
            h.score,
            h.explain.mmr_selected,
            h.explain.budget_units,
            h.explain.decay_factor,
        );
    }
    println!(
        "  [metrics] filter_degraded={} filter_eval={:?} budget_units={:?} empty_reason={:?}",
        resp.metrics.filter_degraded,
        resp.metrics.filter_eval,
        resp.metrics.budget_units,
        resp.empty_reason,
    );
}

fn main() -> anyhow::Result<()> {
    // ================== ① 命名空间（检索级隔离）==================
    // 承载形态 = 既有 `Filter::Eq` 用在**约定字段名**上（内核不加第二套过滤路径）。
    println!("=== ① 命名空间：tenant-a / tenant-b 各自只看自己的语料 ===");
    let index_a = build(SearchIndex::builder())?;
    let searcher_a = index_a.searcher();
    let ns_a = Filter::namespace(NS_FIELD, "tenant-a");
    print_hits("未过滤", &searcher_a, None);
    print_hits("ns = tenant-a", &searcher_a, Some(&ns_a));
    // ⚠️ `metrics.filter_degraded == true` ⇒ 该字段已**降级**、本次过滤走了 O(N) 全扫。
    //    高基数（每会话一个值）的字段**必然降级**且**不可逆**（除重建）——
    //    代价与判据见 `data/eval/t7-27/README.md`。

    // ================== ② MMR（结果去重）==================
    // `λ = 0.5`、候选池 `pool = 10`（⚠️ 建议 `pool >= 4 * k`，否则退化为「只重排」）。
    // ⚠️ 本段 `print_hits` 用 `top_n = 6` = 语料规模 ⇒ MMR **没有可丢的余量** ⇒ 演示的是
    //    「**冗余项被压到末位**」而非「丢弃」；要演示丢弃，把 `top_n` 调到 < 语料规模（如 3）。
    println!("\n=== ② MMR：A 组三篇冗余 → 冗余项被压到末位（top_n = 语料规模，故不丢条目）===");
    let index_b = build(SearchIndex::builder().mmr(0.5, 10))?;
    let searcher_b = index_b.searcher();
    print_hits("MMR 关闭（对照：按融合分取前 k）", &searcher_a, None);
    print_hits("MMR 开启 λ=0.5", &searcher_b, None);
    // ⚠️ 开启后 `hits` **不再按 `score` 降序**（改按 MMR 的选择序）——
    //    判据 = `explain.mmr_selected.is_some()`（架构 `R62`）。

    // ================== ③ token budget（按预算裁剪）==================
    // ⚠️ 默认计数器数的是**字符数**，不是 token 数（内核没有目标模型的 tokenizer）。
    println!("\n=== ③ token budget：预算 40 字符 ⇒ 顺序截断 ===");
    let index_c = build(SearchIndex::builder().token_budget(40))?;
    let searcher_c = index_c.searcher();
    print_hits("无预算（对照）", &searcher_a, None);
    print_hits("budget = 40", &searcher_c, None);
    // 累加至**不超过预算**为止，超出的条目**及其后全部丢弃**（顺序截断）。
    // 逐条读数 = `explain.budget_units`；合计 = `metrics.budget_units`；
    // 若被截到 0 条 ⇒ `empty_reason = Some(EmptyReason::PostEmptied)`。

    // ================== ④ 时间衰减（附赠：同一语料上的第三个机制）==================
    // 作用分 = **融合分这一档**：`score *= exp(-λ · age)`；`age` 单位是**秒**，岁数越大的条目掉得越多。
    // 🔴 **λ 必须与你的时间尺度配对**（λ 的量纲 = 1/秒 ⇒ 特征时间 = 1/λ）：
    //    本示例的时间戳是**天级**（`NOW_MS − 400 / 300 / 1 / 200 / 100 / 2 天`）⇒ 取 **λ = 1e-7**
    //    （400 天 ⇒ `exp(−3.46) ≈ 0.031`，六条的分级一眼可见）。
    //    ⚠️ 反例：写 `λ = 1e-3`（特征时间 ≈ 17 分钟）会让**大于 1.2034 天**的条目在 **f32 下溢为 0**
    //    —— 实测 6 条里 5 条 `score = 0.000000`，且最老的那条因同分后按 `chunk_id` 兜底**反而排第 2**，
    //    与「新条目上浮」**相反**（该阈值 = `f32` 下 `exp(−λ·age)` 归零的临界年龄，实测算得 1.2034 天）。
    //    分钟级时间戳才该用 `1e-3` 量级。
    println!("\n=== ④ 时间衰减 λ=1e-7（1/秒，配天级时间戳）：新条目上浮 ===");
    let clock = Arc::new(|| NOW_MS);
    let index_d = build(SearchIndex::builder().time_decay(1e-7, TS_FIELD, clock))?;
    let searcher_d = index_d.searcher();
    print_hits("衰减关闭（对照）", &searcher_a, None);
    print_hits("衰减开启", &searcher_d, None);
    // ⚠️ 时间字段**缺失 / 非整数** ⇒ `Err(InvalidInput)`（不静默跳过，NFR-07）：
    //    错误消息带字段名与 `chunk_id`，便于区分「字段名写错」与「个别文档缺数据」。

    println!("\n完成。四个机制都**默认关**，未启用时全链路与引入前逐位一致。");
    Ok(())
}
