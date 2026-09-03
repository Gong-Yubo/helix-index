//! T4-06a：向量索引 A/B —— `hnsw_rs`（原生增量）vs `instant-distance`（一次性构建）。
//!
//! 运行方式（debug 下两者都极慢，必须 release）：
//!
//! ```bash
//! cargo test --release -p index-core --test vector_ab -- --ignored --nocapture
//! ```
//!
//! 对比项（p4-design.md 7.1）：
//! 1. **增量写入延迟**：hnsw_rs 单条 insert 的 P50/P99；
//!    instant-distance 无 insert，其"增量成本"= 定期全量重建，测 1 万条 build 耗时
//! 2. **召回一致性**：两者与暴力 Top-10 的重合率（判据 ≥ 95%）
//! 3. **结论输出**：达标则采用 hnsw_rs 原生增量、删除 delta 区设计

#![allow(non_snake_case)] // 中文测试名

use std::time::Instant;

use index_core::vector::{BruteForceIndex, HnswIndex, HnswRsIndex, NormalizedVector, VectorIndex};

fn random_vec(seed: &mut u64) -> Vec<f32> {
    let mut next = || {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*seed >> 33) as u32 as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    let v: Vec<f32> = (0..512).map(|_| next()).collect();
    // 归一化
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.into_iter().map(|x| x / norm).collect()
}

fn topk_overlap(
    a: &dyn VectorIndex,
    b: &dyn VectorIndex,
    queries: &[NormalizedVector],
    k: usize,
) -> f32 {
    let mut total = 0.0;
    for q in queries {
        let av: Vec<u32> = a.search(q, k).unwrap().iter().map(|(i, _)| *i).collect();
        let bv: Vec<u32> = b.search(q, k).unwrap().iter().map(|(i, _)| *i).collect();
        let aset: std::collections::HashSet<u32> = av.iter().copied().collect();
        total += bv.iter().filter(|x| aset.contains(x)).count() as f32 / k as f32;
    }
    total / queries.len() as f32
}

#[test]
#[ignore]
fn 向量索引AB对比() {
    let n = 10_000usize;
    let mut seed = 7u64;
    let entries: Vec<(u32, NormalizedVector)> = (0..n)
        .map(|i| (i as u32, NormalizedVector::new(random_vec(&mut seed))))
        .collect();
    let queries: Vec<NormalizedVector> = (0..10)
        .map(|_| NormalizedVector::new(random_vec(&mut seed)))
        .collect();
    let brute = BruteForceIndex::from_entries(entries.clone());

    println!("\n========== 向量索引 A/B（{n} 条 × 512 维，release） ==========\n");

    // ---- A：hnsw_rs（原生增量 insert）----
    let mut h = HnswRsIndex::with_capacity(n);
    let mut insert_times: Vec<u128> = Vec::with_capacity(n);
    let t_all = Instant::now();
    for (id, v) in &entries {
        let t = Instant::now();
        h.add(*id, v.clone()).unwrap();
        insert_times.push(t.elapsed().as_micros());
    }
    let h_total_ms = t_all.elapsed().as_millis();
    insert_times.sort_unstable();
    let p50 = insert_times[n / 2];
    let p99 = insert_times[(n as f32 * 0.99) as usize];
    let h_overlap = topk_overlap(&h, &brute, &queries, 10);

    println!("[A] hnsw_rs（原生增量）");
    println!("    逐条插入总耗时 = {h_total_ms} ms（含 {n} 次 insert）");
    println!("    单条 insert P50 = {p50} µs, P99 = {p99} µs");
    println!("    Top-10 vs 暴力重合率 = {h_overlap:.3}");

    // ---- B：instant-distance（一次性构建）----
    let t = Instant::now();
    let idist = HnswIndex::build(entries.clone());
    let b_build_ms = t.elapsed().as_millis();
    let b_overlap = topk_overlap(&idist, &brute, &queries, 10);

    println!("\n[B] instant-distance（一次性构建，无增量 insert）");
    println!("    全量 build 耗时 = {b_build_ms} ms（delta 方案的重建成本）");
    println!("    Top-10 vs 暴力重合率 = {b_overlap:.3}");

    // ---- 结论 ----
    println!("\n---------- 对比结论 ----------");
    println!("增量能力:  A 支持（P99 {p99}µs/条）；B 不支持（重建 {b_build_ms}ms/万条）");
    println!("召回重合:  A = {h_overlap:.3}, B = {b_overlap:.3}（判据 ≥ 0.95）");
    let a_ok = h_overlap >= 0.95;
    println!(
        "判定:      {}",
        if a_ok {
            "A（hnsw_rs）达标 → 采用原生增量，删除 delta 区设计"
        } else {
            "A 未达标 → 回落「instant-distance 主索引 + delta 暴力区」"
        }
    );
    println!();

    // 断言判据（A 的召回必须达标才算 A/B 有结论）
    assert!(h_overlap >= 0.95, "hnsw_rs 召回未达标: {h_overlap}");
}
