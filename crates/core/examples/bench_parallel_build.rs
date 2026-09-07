//! 并行建图收益基准（S2-11 / D-S2-05 的决策依据）。
//!
//! 目的：回答设计文档 §10 未决问题 1——`parallel_build` 的**加速比**与**质量代价**
//! 到底是多少，从而决定默认值是否该从「关」翻到「开」。
//!
//! 口径：**只测纯 HNSW 建图**，不含 embed（12K 建库耗时的大头是 embed，会把建图
//! 收益稀释到看不见）。预生成向量后，一次性 `add_batch` 灌入（≥ 阈值 1000 才走并行）。
//!
//! 运行（release，否则数字无意义）：
//!   cargo run -p helix-core --release --example bench_parallel_build -- [n] [dim] [repeats] [real]
//!   默认：n=12000 dim=512 repeats=3，合成随机向量
//!   第 4 参 `real`：改用 **T2Ranking 真实语料 + LocalEmbedder** 的向量（更贴近生产，
//!   但要先花 ~220s embed 12K 段）。
//!
//! ⚠️ **两种向量的差异**：合成随机向量在 512 维下几乎两两等距（维度灾难），是 HNSW
//! 的**对抗性输入**，绝对耗时远高于真实语料 ⇒ **合成模式只有加速比可比，绝对耗时
//! 不可与 eval-report 8.2 的「纯索引构建」横比**；要报绝对耗时请用 `real`。

use std::time::Instant;

use helix_core::embed::{Embedder, LocalEmbedder};
use helix_core::vector::{BruteForceIndex, HnswRsIndex, NormalizedVector, VectorIndex};

/// xorshift64：确定性伪随机，避免为基准引入额外依赖（也保证跨轮可比）。
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// [-1, 1) 均匀分布
    fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32 - 1.0
    }

    fn unit_vec(&mut self, dim: usize) -> NormalizedVector {
        let raw: Vec<f32> = (0..dim).map(|_| self.next_f32()).collect();
        NormalizedVector::new(raw)
    }
}

/// 与暴力 oracle 的平均 Top-10 重合率（质量代价的度量）。
fn recall(
    idx: &HnswRsIndex,
    oracle: &BruteForceIndex,
    items: &[(u32, NormalizedVector)],
    queries: usize,
) -> anyhow::Result<f64> {
    let mut sum = 0.0f64;
    for (_, q) in items.iter().take(queries) {
        let got: Vec<u32> = idx.search(q, 10)?.into_iter().map(|(id, _)| id).collect();
        let want: Vec<u32> = oracle
            .search(q, 10)?
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        let hit = got.iter().filter(|id| want.contains(id)).count();
        sum += hit as f64 / 10.0;
    }
    Ok(sum / queries as f64)
}

fn median(xs: &mut [f64]) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = xs.len() / 2;
    if xs.len() % 2 == 1 {
        xs[mid]
    } else {
        (xs[mid - 1] + xs[mid]) / 2.0
    }
}

/// 真实语料向量：`data/t2-corpus.jsonl` 前 n 段，经 `LocalEmbedder` 嵌入。
///
/// embed 是一次性的（不参与建图对比，故单独计时打印）。
fn real_corpus_items(n: usize) -> anyhow::Result<Vec<(u32, NormalizedVector)>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/t2-corpus.jsonl");
    let corpus = std::fs::read_to_string(&path)?;
    let texts: Vec<String> = corpus
        .lines()
        .take(n)
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["text"].as_str().unwrap().to_string()
        })
        .collect();

    let embedder = LocalEmbedder::new()?;
    println!(
        "真实语料 {} 段，模型已加载（dim={}）",
        texts.len(),
        embedder.dim()
    );
    let t = Instant::now();
    let mut vecs: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
    for chunk in texts.chunks(64) {
        vecs.extend(embedder.embed_documents(chunk)?);
    }
    println!(
        "embed 完成 {:.1}s（**不计入**下面的建图对比）\n",
        t.elapsed().as_secs_f64()
    );
    Ok(vecs
        .into_iter()
        .enumerate()
        .map(|(i, v)| (i as u32, NormalizedVector::new(v)))
        .collect())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(12000);
    let dim: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(512);
    let repeats: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
    let real = args.get(4).is_some_and(|s| s == "real");

    let items: Vec<(u32, NormalizedVector)> = if real {
        real_corpus_items(n)?
    } else {
        println!("n={n} dim={dim} 每模式 {repeats} 轮（**纯 HNSW 建图**，不含 embed）");
        println!("⚠️ 合成随机向量 = 512 维下几乎等距的**对抗性输入**，仅加速比可比\n");
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        (0..n as u32).map(|i| (i, rng.unit_vec(dim))).collect()
    };
    let oracle = BruteForceIndex::from_entries(items.clone());

    println!(
        "{:<6} {:<4} {:>12} {:>10} {:>12}",
        "模式", "轮次", "建图耗时(s)", "并行分派", "oracle重合率"
    );
    let mut t_seq: Vec<f64> = Vec::new();
    let mut t_par: Vec<f64> = Vec::new();
    let mut q_seq = 0.0f64;
    let mut q_par = 0.0f64;
    let mut dispatch_par = 0usize;

    for (name, parallel) in [("串行", false), ("并行", true)] {
        for r in 0..repeats {
            let mut idx = HnswRsIndex::with_capacity(n);
            if parallel {
                idx = idx.with_parallel_build(true);
            }
            let t = Instant::now();
            idx.add_batch(&items)?;
            let elapsed = t.elapsed().as_secs_f64();
            let dispatches = idx.parallel_inserts();
            let q = recall(&idx, &oracle, &items, 20)?;
            println!("{name:<6} {r:<4} {elapsed:>12.3} {dispatches:>10} {q:>12.4}");
            if parallel {
                t_par.push(elapsed);
                q_par = q;
                dispatch_par = dispatches;
            } else {
                t_seq.push(elapsed);
                q_seq = q;
            }
        }
    }

    let m_seq = median(&mut t_seq);
    let m_par = median(&mut t_par);
    println!("\n汇总（中位数）：");
    println!(
        "  串行建图 {m_seq:.3}s ｜ 并行建图 {m_par:.3}s ｜ 加速比 {:.2}×",
        m_seq / m_par
    );
    println!("  oracle Top-10 重合率：串行 {q_seq:.4} ｜ 并行 {q_par:.4}");
    println!("  并行分派次数 {dispatch_par}（0 表示**根本没走并行**，检查 batch/阈值耦合）");
    Ok(())
}
