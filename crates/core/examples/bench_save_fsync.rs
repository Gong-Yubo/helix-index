//! save（快照落盘）耗时基准——原子写 fsync 代价实测（S3-08 / D-S3-06）。
//!
//! 口径：12K T2Ranking 真实语料正文（BM25 索引）+ 合成 512 维归一化向量。
//! 向量用合成而非真实 embed（一次性 ~220s，不值得为落盘计时重复）——
//! fsync 代价只取决于**字节数**（12K×512×f32 ≈ 24.6MB，加正文 ≈ 45~50MB，
//! 与 eval-report 的 52MB 级同量级），与向量内容无关。
//!
//! 运行（release，否则数字无意义）：
//!   cargo run -p helix-core --release --example bench_save_fsync -- [n] [dim] [repeats]
//!   默认：n=12000 dim=512 repeats=3
//!
//! 同一示例在改造前（main，直写）/ 改造后（V2 Step 3，tmp+fsync+rename）各跑
//! 一轮，结果入 eval-report §8.7（给范围不给单点值，单次波动可达 ±15%）。
//!
//! ⚠️ 产物**仅供计时**：快照带合成向量但 fingerprint 是空 embedder（dim=0），
//! 任何真实装配都 load 不出它——不得当作兼容性样例文件使用。

use std::path::Path;
use std::time::Instant;

use helix_core::analyze::MixedAnalyzer;
use helix_core::document::{Chunk, DocRecord};
use helix_core::index::Index;
use helix_core::storage::{save_with_crc, ConfigFingerprint};
use helix_core::types::ChunkId;
use helix_core::vector::NormalizedVector;

/// xorshift64：确定性伪随机（与 bench_parallel_build 同口径）。
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

    fn unit_vec(&mut self, dim: usize) -> Vec<f32> {
        let raw: Vec<f32> = (0..dim)
            .map(|_| (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32 - 1.0)
            .collect();
        // 归一化不改变长度（512），只看字节数——与内容无关
        NormalizedVector::new(raw).as_slice().to_vec()
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(12000);
    let dim: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(512);
    let repeats: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);

    // 语料正文（真实文本 → 倒排 + 正排 + 原文）
    let corpus_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/t2-corpus.jsonl");
    let corpus = std::fs::read_to_string(&corpus_path)?;
    let texts: Vec<String> = corpus
        .lines()
        .take(n)
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l)?;
            Ok(v["text"].as_str().unwrap_or_default().to_string())
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    println!("语料 {n} 段，合成向量 dim={dim}，save {repeats} 轮");

    let analyzer = MixedAnalyzer::new();
    let mut index = Index::new();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let vectors: Vec<(ChunkId, Vec<f32>)> = texts
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let doc = DocRecord {
                doc_id: 0,
                source: format!("doc-{i}"),
                metadata: serde_json::json!({}),
                content_hash: i as u64,
            };
            let chunk = Chunk {
                chunk_id: 0,
                doc_id: 0,
                ordinal: 0,
                text: text.clone(),
                char_start: 0,
                char_end: text.chars().count(),
            };
            index.add(doc, vec![chunk], &analyzer).unwrap();
            (i as ChunkId, rng.unit_vec(dim))
        })
        .collect();
    let fingerprint = ConfigFingerprint {
        analyzer_id: "mixed".to_string(),
        embedder_id: String::new(),
        dim: 0,
        chunker: (512, 64),
    };

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("bench.idx");
    for r in 0..repeats {
        let t = Instant::now();
        let crc = save_with_crc(&path, &index, &vectors, &fingerprint)?;
        let elapsed = t.elapsed().as_secs_f64();
        let size = std::fs::metadata(&path)?.len();
        println!("轮 {r}: save {elapsed:.3}s ｜ {size} 字节 ｜ crc={crc:#010x}");
    }
    Ok(())
}
