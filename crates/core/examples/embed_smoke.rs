//! T0-05：ONNX 模型下载 + 推理验证（P0 最高风险项）。
//!
//! 运行：
//!   cargo run -p helix-core --example embed_smoke
//!   HF_ENDPOINT=https://hf-mirror.com cargo run -p helix-core --example embed_smoke
//!
//! 验证内容：
//!   1. 模型能下载（Qdrant/bge-small-zh-v1.5，约 91 MB）
//!   2. 维度为 512
//!   3. 输出向量 L2 范数 ≈ 1.0
//!   4. 记录**模型已缓存后**的单条推理耗时（NFR-02 第一批实测基线）

use std::time::Instant;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

fn main() -> anyhow::Result<()> {
    println!("==> 初始化 TextEmbedding（首次运行会下载模型）...");
    let t_init = Instant::now();
    // ⚠️ 变体名是 BGESmallZHV15，不是 BGESmallZH
    let mut model = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::BGESmallZHV15).with_show_download_progress(true),
    )?;
    println!("    初始化耗时: {:?}", t_init.elapsed());

    // ---- 预热：首次 embed 可能触发余下的懒加载 ----
    let warm = model.embed(vec!["预热"], None)?;
    println!("==> 预热完成，dim = {}", warm[0].len());

    // ---- 正式验证：两条语义相近的中文问句 ----
    let texts = vec![
        "Rust 里怎么实现支持中文的 BM25 检索？",
        "向量检索与关键词检索的区别是什么？",
    ];

    let t0 = Instant::now();
    let embeddings = model.embed(&texts, None)?;
    println!("==> embed 2 条耗时: {:?}", t0.elapsed());

    let dim = embeddings[0].len();
    let norm: f32 = embeddings[0].iter().map(|x| x * x).sum::<f32>().sqrt();
    let cos: f32 = embeddings[0]
        .iter()
        .zip(embeddings[1].iter())
        .map(|(a, b)| a * b)
        .sum();

    println!("dim              = {}", dim);
    println!("first 4 dims     = {:?}", &embeddings[0][..4]);
    println!("l2 norm          = {:.6}", norm);
    println!("cos(text0,text1) = {:.4}", cos);

    // ---- 单条推理耗时（模型已缓存，取 5 次平均）----
    let n = 5;
    let t1 = Instant::now();
    for _ in 0..n {
        let _ = model.embed(vec!["单条推理耗时测量"], None)?;
    }
    println!(
        "单条推理平均耗时  = {:?}（{} 次平均，模型已缓存）",
        t1.elapsed() / n,
        n
    );

    // ---- 断言 ----
    assert_eq!(dim, 512, "bge-small-zh-v1.5 应为 512 维，实际 {}", dim);
    assert!(
        (norm - 1.0).abs() < 1e-3,
        "fastembed 输出应为单位向量，实际 norm = {norm}"
    );
    println!("\n✅ T0-05 通过：模型可下载、可推理，维度与归一化符合预期");
    Ok(())
}
