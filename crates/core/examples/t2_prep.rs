//! t2_prep —— T2Ranking → 评测集装配器（T5-01/02a，p5-design.md 第 5 章）。
//!
//! 运行（需先下载三文件到 data/t2ranking/）：
//! ```bash
//! cargo run --release -p index-core --example t2_prep -- \
//!     --t2ranking data/t2ranking --out data
//! ```
//!
//! # 确定性承诺（NFR-06 的数据侧延伸）
//!
//! **全程固定种子**：任何机器重跑产出逐字节一致。
//! - 候选池排序：xxh64(qid, SEED_QID)
//! - 负例抽样：xxh64(pid, SEED_NEG) 哈希秩——等价于固定种子下的均匀抽样
//! - 桶内排序：xxh64(qid, SEED_BUCKET)
//! - 输出文件行序固定（corpus 按 pid 升序、queries 按 qid 升序）
//!
//! # 负例抽样算法（精确性，评审⑫）
//!
//! **单遍流式扫描 + 容量 7,000 的最小堆**（按哈希秩）：
//! 扫描结束时堆中保留的就是全库哈希秩最小的 7,000 个非 qrels 段落——
//! 精确取秩，**不是 Bernoulli 抽样**（计数随机会破坏逐字节一致承诺）。
//!
//! # 池内零假负例（5.3）
//!
//! 入选 320 查询的全部 qrels 段落（含 grade 0 已判定负例）都进语料；
//! 随机负例段落理论上可能恰好是某 query 的未标注相关段落——概率低，
//! 在评测报告中如实披露。
//!
//! # 分桶（5.4：分布先行）
//!
//! 规则顺序固定：mixed（含 ASCII 字母/数字）→ natural（≥16 字符）→
//! exact/paraphrase（o* 词汇重合度阈值 0.5）。**o* 分布直方图随统计输出**，
//! 若区分度不足（大量堆积 0.4~0.6），调整阈值并把决策记录在案。

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use clap::Parser;

use index_core::analyze::{Analyzer, MixedAnalyzer};
use index_core::chunk::Chunker;

// ---- 固定种子（改动 = 评测集变更，须在报告中记录）----
const SEED_QID: u64 = 0x5432_7465; // 候选池排序
const SEED_NEG: u64 = 0x6e65_6731; // 负例哈希秩
const SEED_BUCKET: u64 = 0x6275_636b; // 桶内排序

// ---- 装配参数（p5-design.md 5.2）----
const N_CANDIDATE: usize = 2000; // 候选查询池
const N_NEGATIVE: usize = 7000; // 均匀负例段落
const PER_BUCKET: usize = 80; // 每桶查询数
const TARGET_CORPUS: usize = 12_000; // 目标语料规模
const QUERY_MIN_CHARS: usize = 4;
const QUERY_MAX_CHARS: usize = 40;
const OSTAR_THRESHOLD: f64 = 0.5; // exact/paraphrase 分界（分布先行复核）
const NATURAL_MIN_CHARS: usize = 16;

fn xxh64(s: &str, seed: u64) -> u64 {
    xxhash_rust::xxh64::xxh64(s.as_bytes(), seed)
}

#[derive(Parser)]
#[command(about = "T2Ranking → 评测集装配器（固定种子，可复现）")]
struct Args {
    /// T2Ranking 原始数据目录（queries.dev.tsv / qrels.dev.tsv / collection.tsv）
    #[arg(long, default_value = "data/t2ranking")]
    t2ranking: PathBuf,
    /// 输出目录（t2-corpus.jsonl / t2-queries.jsonl）
    #[arg(long, default_value = "data")]
    out: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let t0 = std::time::Instant::now();

    // ---- 1. 解析 queries + qrels ----
    let queries = load_queries(&args.t2ranking.join("queries.dev.tsv"))?;
    println!("queries.dev.tsv: {} 条（含表头）", queries.len());

    let mut qrels: HashMap<String, Vec<(String, u8)>> = HashMap::new();
    for (qid, pid, grade) in load_qrels(&args.t2ranking.join("qrels.dev.tsv"))? {
        qrels.entry(qid).or_default().push((pid, grade));
    }
    println!("qrels.dev.tsv: {} 个 query 有标注", qrels.len());

    // ---- 2. 查询过滤：≥1 个 grade≥2；长度 4~40 字符 ----
    let filtered: Vec<&String> = queries
        .keys()
        .filter(|qid| {
            let Some(rels) = qrels.get(*qid) else {
                return false;
            };
            if !rels.iter().any(|(_, g)| *g >= 2) {
                return false;
            }
            let n = queries[*qid].chars().count();
            (QUERY_MIN_CHARS..=QUERY_MAX_CHARS).contains(&n)
        })
        .collect();
    println!(
        "过滤后（grade≥2 存在 且 长度 4~40 字符）: {} 条",
        filtered.len()
    );

    // ---- 3. 候选池：xxh64(qid, SEED_QID) 排序取前 2000 ----
    let mut ranked: Vec<(u64, &String)> = filtered
        .into_iter()
        .map(|qid| (xxh64(qid, SEED_QID), qid))
        .collect();
    ranked.sort_unstable();
    let candidates: Vec<String> = ranked
        .into_iter()
        .take(N_CANDIDATE)
        .map(|(_, qid)| qid.clone())
        .collect();
    println!(
        "候选池（哈希秩前 {}）: {} 条",
        N_CANDIDATE,
        candidates.len()
    );

    // 候选查询的全部 qrels pid 全集（含 0 级）
    let mut qrels_pids: HashSet<String> = HashSet::new();
    for qid in &candidates {
        for (pid, _) in &qrels[qid] {
            qrels_pids.insert(pid.clone());
        }
    }
    println!("候选池 qrels 段落全集: {} 个 pid", qrels_pids.len());

    // ---- 4. 单遍流式扫描 collection.tsv ----
    // qrels 段落全收集；非 qrels 段落进 7K 最小堆（按哈希秩，精确取秩）
    println!("单遍流式扫描 collection.tsv（最小堆精确取秩，非 Bernoulli）...");
    let mut qrels_text: HashMap<String, String> = HashMap::new();
    // 最小堆：Reverse 保证堆顶是哈希秩最小；容量 N_NEGATIVE
    let mut neg_heap: BinaryHeap<Reverse<(u64, String, String)>> = BinaryHeap::new();
    let mut seen_qrels = 0usize;
    let mut total_lines = 0usize;

    let f = std::fs::File::open(args.t2ranking.join("collection.tsv"))?;
    for line in BufReader::new(f).lines() {
        let line = line?;
        total_lines += 1;
        let Some((pid, text)) = line.split_once('\t') else {
            continue; // 空行/坏行跳过（统计口径见输出）
        };
        if qrels_pids.contains(pid) {
            qrels_text.insert(pid.to_string(), text.to_string());
            seen_qrels += 1;
        } else {
            let rank = xxh64(pid, SEED_NEG);
            let text = text.to_string();
            if neg_heap.len() < N_NEGATIVE {
                neg_heap.push(Reverse((rank, pid.to_string(), text)));
            } else if let Some(Reverse(top)) = neg_heap.peek() {
                if rank > top.0 {
                    // 新秩更大 → 挤掉堆顶（堆顶是当前最小秩）
                    let _ = neg_heap.pop();
                    neg_heap.push(Reverse((rank, pid.to_string(), text)));
                }
            }
        }
    }
    let missing = qrels_pids.len() - seen_qrels;
    println!(
        "扫描完成: {} 行；qrels 段落命中 {} / {}（缺失 {}，应为 0）",
        total_lines,
        seen_qrels,
        qrels_pids.len(),
        missing
    );
    if missing > 0 {
        anyhow::bail!("qrels pid 在 collection 缺失 {missing} 个——pid 映射断裂，中止");
    }

    let negatives: Vec<(String, String)> = neg_heap
        .into_iter()
        .map(|Reverse((_, pid, text))| (pid, text))
        .collect();

    // ---- 5. 分桶（MixedAnalyzer，o* 词汇重合度）----
    println!("分桶计算（jieba 分词 + o* 重合度）...");
    let analyzer = MixedAnalyzer::new();
    let mut buckets: HashMap<&str, Vec<(u64, String)>> = HashMap::new();
    let mut ostar_hist = [0usize; 10]; // 0.0~1.0 每 0.1 一格（natural/mixed 桶除外）
    let mut ostar_all: Vec<f64> = Vec::new();

    for qid in &candidates {
        let query = &queries[qid];
        let qtype = if query.chars().any(|c| c.is_ascii_alphanumeric()) {
            "mixed"
        } else if query.chars().count() >= NATURAL_MIN_CHARS {
            "natural"
        } else {
            // o* = max over grade≥1 段落的 |query 词项 ∩ 段落词项| / |query 词项|
            let q_terms: HashSet<String> = analyzer
                .analyze_query(query)
                .into_iter()
                .map(|t| t.term.to_string())
                .collect();
            let mut ostar = 0.0f64;
            if !q_terms.is_empty() {
                for (pid, grade) in &qrels[qid] {
                    if *grade < 1 {
                        continue;
                    }
                    let Some(text) = qrels_text.get(pid) else {
                        continue;
                    };
                    let p_terms: HashSet<String> = analyzer
                        .analyze_doc(text)
                        .into_iter()
                        .map(|t| t.term.to_string())
                        .collect();
                    let overlap = q_terms.intersection(&p_terms).count() as f64;
                    ostar = ostar.max(overlap / q_terms.len() as f64);
                }
            }
            // o* 恰为 1.0 时 (1.0*10.0) as usize == 10 会越界，钳制到末格
            ostar_hist[((ostar * 10.0) as usize).min(9)] += 1;
            ostar_all.push(ostar);
            if ostar >= OSTAR_THRESHOLD {
                "exact"
            } else {
                "paraphrase"
            }
        };
        buckets
            .entry(qtype)
            .or_default()
            .push((xxh64(qid, SEED_BUCKET), qid.clone()));
    }

    // ---- 6. 每桶取前 80 ----
    let mut selected: Vec<(String, String, &'static str)> = Vec::new(); // (qid, query, type)
    println!("\n== 分桶分布 ==");
    for name in ["mixed", "natural", "exact", "paraphrase"] {
        let mut list = buckets.remove(name).unwrap_or_default();
        list.sort_unstable();
        let take = list.len().min(PER_BUCKET);
        println!(
            "  {name:<10}: 候选 {} 条 → 取 {} 条{}",
            list.len(),
            take,
            if take < 30 {
                "  ⚠️ <30 条（该桶结论将标\"探索性\"）"
            } else {
                ""
            }
        );
        for (_, qid) in list.into_iter().take(PER_BUCKET) {
            selected.push((qid.clone(), queries[&qid].clone(), name));
        }
    }
    let n_queries = selected.len();

    // ---- 7. 语料装配：选中查询的全部 qrels 段落 + 负例补足至 TARGET ----
    let mut corpus_pids: HashSet<String> = HashSet::new();
    let mut grade_dist = [0usize; 4];
    for (qid, _, _) in &selected {
        for (pid, grade) in &qrels[qid] {
            if corpus_pids.insert(pid.clone()) {
                grade_dist[*grade as usize] += 1;
            }
        }
    }
    let n_qrels_selected = corpus_pids.len();
    for (pid, _) in &negatives {
        if corpus_pids.len() >= TARGET_CORPUS {
            break;
        }
        if corpus_pids.insert(pid.clone()) {
            grade_dist[0] += 1; // 未标注负例，记 0 级口径
        }
    }
    // 负例文本并入文本表（后续 chunk 断言与 corpus 输出使用；
    // negatives 此后不再使用，直接 move）
    for (pid, text) in negatives {
        qrels_text.insert(pid, text);
    }
    println!(
        "\n语料装配: qrels 段落 {} + 负例 {} = {} 段落（目标 ~{}）",
        n_qrels_selected,
        corpus_pids.len() - n_qrels_selected,
        corpus_pids.len(),
        TARGET_CORPUS
    );

    // ---- 8. 段落→chunk 断言（默认 Chunker 下每段落应为 1 chunk）----
    let chunker = Chunker::default();
    let mut multi_chunk = 0usize;
    let mut lens: Vec<usize> = Vec::with_capacity(corpus_pids.len());
    for pid in &corpus_pids {
        let text = &qrels_text[pid];
        lens.push(text.chars().count());
        if chunker.chunk(0, text).len() > 1 {
            multi_chunk += 1;
        }
    }
    lens.sort_unstable();
    let pct = |q: f64| lens[((q * lens.len() as f64).ceil() as usize).clamp(1, lens.len()) - 1];
    println!(
        "段落→chunk 断言: 多 chunk 段落 {} 个（应为 0；>0 时评测构建需走\"每段落强制单 chunk\"路径）",
        multi_chunk
    );
    println!(
        "语料长度（字符）: P50 = {}, P95 = {}, Max = {}",
        pct(0.5),
        pct(0.95),
        lens.last().unwrap()
    );

    // ---- o* 直方图（5.4 分布先行：0.5 阈值的定稿依据）----
    println!("\n== o* 分布直方图（exact/paraphrase 候选查询）==");
    for (i, n) in ostar_hist.iter().enumerate() {
        let bar = "█".repeat((*n).min(60));
        println!(
            "  {:.1}~{:.1}: {:>4} {}",
            i as f64 / 10.0,
            (i + 1) as f64 / 10.0,
            n,
            bar
        );
    }

    // ---- 9. 输出 ----
    std::fs::create_dir_all(&args.out)?;
    let corpus_path = args.out.join("t2-corpus.jsonl");
    {
        let mut pids: Vec<&String> = corpus_pids.iter().collect();
        pids.sort();
        // corpus 行序 = pid 字典序（数字位数一致时字典序 = 数值序，仍固定）
        let mut w = std::io::BufWriter::new(std::fs::File::create(&corpus_path)?);
        for pid in pids {
            writeln!(
                w,
                "{}",
                serde_json::json!({
                    "source": pid,
                    "text": qrels_text[pid],
                    "metadata": {"origin": "t2ranking"},
                })
            )?;
        }
        w.flush()?;
    }

    let queries_path = args.out.join("t2-queries.jsonl");
    {
        // queries 行序 = qid 字典序；每行的 relevance 按 source 字典序（确定性）
        let mut sel = selected.clone();
        sel.sort_by(|a, b| a.0.cmp(&b.0));
        let mut w = std::io::BufWriter::new(std::fs::File::create(&queries_path)?);
        for (qid, query, qtype) in &sel {
            let mut rels = qrels[qid].clone();
            rels.sort_by(|a, b| a.0.cmp(&b.0));
            let relevance: Vec<serde_json::Value> = rels
                .into_iter()
                .map(|(source, grade)| serde_json::json!({"source": source, "grade": grade}))
                .collect();
            writeln!(
                w,
                "{}",
                serde_json::json!({
                    "qid": qid,
                    "query": query,
                    "type": qtype,
                    "relevance": relevance,
                })
            )?;
        }
        w.flush()?;
    }

    // ---- 10. 汇总统计 ----
    println!("\n== 装配汇总 ==");
    println!("  查询数      : {}", n_queries);
    println!("  语料段落数  : {}", corpus_pids.len());
    println!("  分级分布    : {:?}", grade_dist);
    println!("  查询长度中位: {}", {
        let mut ql: Vec<usize> = selected.iter().map(|(_, q, _)| q.chars().count()).collect();
        ql.sort_unstable();
        ql[ql.len() / 2]
    });
    let corpus_size = std::fs::metadata(&corpus_path)?.len();
    let queries_size = std::fs::metadata(&queries_path)?.len();
    println!(
        "  产出        : {}（{:.1} MB）、{}（{:.1} KB）",
        corpus_path.display(),
        corpus_size as f64 / 1e6,
        queries_path.display(),
        queries_size as f64 / 1e3
    );
    println!("  耗时        : {:?}", t0.elapsed());
    println!("\n✅ 装配完成。种子与阈值见源码常量（改动 = 评测集变更，须记录）");
    Ok(())
}

/// queries.dev.tsv（qid \t query，带表头）
fn load_queries(path: &Path) -> anyhow::Result<HashMap<String, String>> {
    let f = std::fs::File::open(path)?;
    let mut out = HashMap::new();
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line?;
        if i == 0 && line.starts_with("qid") {
            continue; // 表头
        }
        let Some((qid, query)) = line.split_once('\t') else {
            continue;
        };
        out.insert(qid.to_string(), query.to_string());
    }
    Ok(out)
}

/// qrels.dev.tsv（qid \t iter \t pid \t grade，带表头；iter 列忽略）
fn load_qrels(path: &Path) -> anyhow::Result<Vec<(String, String, u8)>> {
    let f = std::fs::File::open(path)?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line?;
        if i == 0 && line.starts_with("qid") {
            continue; // 表头
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() != 4 {
            anyhow::bail!("qrels 第 {} 行列数 {}（应为 4）: {line}", i + 1, cols.len());
        }
        let grade: u8 = cols[3]
            .parse()
            .map_err(|e| anyhow::anyhow!("qrels 第 {} 行 grade 解析失败（{e}）: {line}", i + 1))?;
        out.push((cols[0].to_string(), cols[2].to_string(), grade));
    }
    Ok(out)
}
