//! batch_size 校准基准（I-13 / D-I7）：测 `embed_documents` 在不同 batch 下的吞吐。
//!
//! 目的：为门面层写缓冲的 `batch_size` 默认值提供实测依据（不盲信经验值）。
//! 运行（需 local-embed，release）：
//!   cargo run -p helix-core --release --example bench_batch_size
//!
//! 口径：T2Ranking 12K 段落，每档 batch 重复 embed 全文，测条/秒吞吐。

use std::time::Instant;

use helix_core::embed::{Embedder, LocalEmbedder};

fn main() -> anyhow::Result<()> {
    // 读 T2Ranking 语料，取子集（避免整库 OOM；batch 吞吐与语料规模无关）
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/t2-corpus.jsonl");
    let corpus = std::fs::read_to_string(&path)?;
    let texts: Vec<String> = corpus
        .lines()
        .take(4000)
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["text"].as_str().unwrap().to_string()
        })
        .collect();
    println!(
        "语料 {} 段，总字数 {}",
        texts.len(),
        texts.iter().map(|t| t.chars().count()).sum::<usize>()
    );

    let embedder = LocalEmbedder::new()?;
    println!("模型已加载（dim={}）\n", embedder.dim());

    println!(
        "{:<8} {:>10} {:>12} {:>10}",
        "batch", "耗时(s)", "吞吐(条/s)", "相对64"
    );
    let mut baseline = 0.0f64;
    for batch in [32usize, 64, 128, 256] {
        let t = Instant::now();
        for chunk in texts.chunks(batch) {
            embedder.embed_documents(chunk)?;
        }
        let elapsed = t.elapsed().as_secs_f64();
        let throughput = texts.len() as f64 / elapsed;
        if batch == 64 {
            baseline = throughput;
        }
        let rel = throughput / baseline;
        println!("{batch:<8} {elapsed:>10.2} {throughput:>12.1} {rel:>9.2}x");
    }
    Ok(())
}
