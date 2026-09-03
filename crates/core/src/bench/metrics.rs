//! 评测指标（T5-03a，纯函数，独立单测）。
//!
//! # 设计（p5-design.md 第 6 章）
//!
//! - **Recall / MRR 用阈值化二值**（默认 grade ≥ 1；主表同时报告 thr=1 / thr=2）
//! - **NDCG 用分级 gain** = 2^grade − 1（TREC 惯例，0/1/3/7）
//! - NDCG 不受 `rel_threshold` 影响（分级指标无阈值概念）
//! - 延迟分位数用 nearest-rank
//! - 符号检验用二项精确（单侧），零依赖实现
//!
//! # 边界
//!
//! 本模块只做数学，不知道索引 / 检索器的存在——输入是
//! "排序后的 source 序列 + 该 query 的 graded 标注"。

use std::collections::HashMap;

/// 单 query 的指标值。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct QueryMetrics {
    pub recall: f64,
    pub mrr: f64,
    pub ndcg: f64,
}

/// 宏平均聚合。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Aggregate {
    pub n: usize,
    pub recall: f64,
    pub mrr: f64,
    pub ndcg: f64,
}

/// 对一个 query 的检索结果计算 Recall@K / MRR@K / NDCG@K。
///
/// - `ranked`：Top-K 的 source，按返回顺序（同分已由 chunk_id 升序 tie-break）
/// - `grades`：该 query 的全部标注（source → grade ∈ 0..=3，**含 0 级已知负例**）
/// - `k`：指标深度（ranked 长度应 ≤ k，超长只取前 k）
/// - `rel_threshold`：Recall / MRR 的相关阈值（1 或 2）
///
/// R = ∅ 时 Recall 定义为 0（非 NaN）；NDCG 的 IDCG 为 0 时同样定义为 0。
pub fn evaluate(
    ranked: &[&str],
    grades: &HashMap<String, u8>,
    k: usize,
    rel_threshold: u8,
) -> QueryMetrics {
    let ranked = &ranked[..ranked.len().min(k)];
    let grade_of = |s: &str| grades.get(s).copied().unwrap_or(0);

    // ---- Recall@K（分母不截断到 K）----
    let relevant: Vec<&String> = grades
        .iter()
        .filter(|(_, g)| **g >= rel_threshold)
        .map(|(s, _)| s)
        .collect();
    let recall = if relevant.is_empty() {
        0.0
    } else {
        let hit = ranked
            .iter()
            .filter(|s| grade_of(s) >= rel_threshold)
            .count();
        hit as f64 / relevant.len() as f64
    };

    // ---- MRR@K（首个 grade ≥ thr 的排名倒数；K 内无 → 0）----
    let mrr = ranked
        .iter()
        .position(|s| grade_of(s) >= rel_threshold)
        .map(|pos| 1.0 / (pos + 1) as f64)
        .unwrap_or(0.0);

    // ---- NDCG@K（分级，不受 rel_threshold 影响）----
    // DCG：返回序列中每个位置的 gain / log2(rank+1)。rank 从 1 起，log2(1+1)=1。
    let dcg: f64 = ranked
        .iter()
        .enumerate()
        .map(|(i, s)| gain(grade_of(s)) / (i as f64 + 2.0).log2())
        .sum();
    // IDCG：全部正例（grade ≥ 1）按 grade 降序取前 K 位的理想 DCG。
    let mut positives: Vec<u8> = grades.values().copied().filter(|g| *g >= 1).collect();
    positives.sort_unstable_by(|a, b| b.cmp(a));
    let idcg: f64 = positives
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, g)| gain(*g) / (i as f64 + 2.0).log2())
        .sum();
    let ndcg = if idcg > 0.0 { dcg / idcg } else { 0.0 };

    QueryMetrics { recall, mrr, ndcg }
}

/// TREC 惯例的分级增益：grade 0/1/2/3 → 0/1/3/7。
#[inline]
fn gain(grade: u8) -> f64 {
    (2u32.pow(grade as u32) - 1) as f64
}

/// 宏平均。
pub fn aggregate(metrics: &[QueryMetrics]) -> Aggregate {
    let n = metrics.len();
    if n == 0 {
        return Aggregate::default();
    }
    let mean = |f: fn(&QueryMetrics) -> f64| metrics.iter().map(f).sum::<f64>() / n as f64;
    Aggregate {
        n,
        recall: mean(|m| m.recall),
        mrr: mean(|m| m.mrr),
        ndcg: mean(|m| m.ndcg),
    }
}

/// nearest-rank 分位数：N 个升序样本，P_p 取第 `ceil(p/100 × N)` 个（1-based）。
///
/// p5-design.md 6.3 手算锚点：6,400 样本 → P99 = 第 6,336 个、P50 = 第 3,200 个。
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    assert!((0.0..=100.0).contains(&p), "p 必须在 0~100，收到 {p}");
    assert!(!sorted.is_empty(), "样本为空");
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    // ceil 可能因浮点误差得 0（如 p=0）；clamp 到 [1, N]
    let rank = rank.clamp(1, sorted.len());
    sorted[rank - 1]
}

/// 二项精确符号检验（单侧 p 值，H0: 每对等概率 +/-）。
///
/// `positive` 次 delta > 0、共 `n` 次非零 delta：
/// p = Σ_{i=positive}^{n} C(n, i) / 2^n。
///
/// 手算锚点（6.2）：18 对中正 14 → p ≈ 0.0154。
pub fn sign_test(positive: usize, n: usize) -> f64 {
    assert!(positive <= n, "positive ({positive}) 不能大于 n ({n})");
    if n == 0 {
        return 1.0;
    }
    // 对数空间累加防溢出：log2(p) = log2(Σ 2^log2C(n,i)) − n
    // C(n,i) 递推：C(n,0)=1，C(n,i+1) = C(n,i)·(n−i)/(i+1)
    let mut log2_terms: Vec<f64> = Vec::with_capacity(n - positive + 1);
    let mut c: f64 = 1.0; // C(n, positive)
    for i in 0..positive {
        c = c * (n - i) as f64 / (i + 1) as f64;
    }
    log2_terms.push(c.log2());
    for i in positive..n {
        c = c * (n - i) as f64 / (i + 1) as f64;
        log2_terms.push(c.log2());
    }
    // log2(sum) = max + log2(Σ 2^(x − max))
    let max = log2_terms.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let sum = log2_terms.iter().map(|x| (x - max).exp2()).sum::<f64>();
    (max + sum.log2() - n as f64).exp2().min(1.0) // log2(p) → p，clamp ≤ 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grades(pairs: &[(&str, u8)]) -> HashMap<String, u8> {
        pairs.iter().map(|(s, g)| (s.to_string(), *g)).collect()
    }

    // ---------- 6.2 手算锚点 ----------

    #[test]
    fn recall手算() {
        // R(1) = {A,B,C}，Top10 含 A、C → 2/3
        let g = grades(&[("A", 1), ("B", 2), ("C", 1), ("D", 0)]);
        let ranked = ["A", "D", "C", "E"];
        let m = evaluate(&ranked, &g, 10, 1);
        assert!(
            (m.recall - 2.0 / 3.0).abs() < 1e-12,
            "recall = {}",
            m.recall
        );
    }

    #[test]
    fn mrr手算() {
        // 首个 grade≥1 在第 4 位 → 0.25；Top10 无 → 0
        let g = grades(&[("A", 1)]);
        let m = evaluate(&["x", "y", "z", "A"], &g, 10, 1);
        assert!((m.mrr - 0.25).abs() < 1e-12);
        let m2 = evaluate(&["x", "y", "z"], &g, 10, 1);
        assert_eq!(m2.mrr, 0.0);
    }

    #[test]
    fn ndcg分级手算() {
        // J 正例 X(3), Y(1), Z(2)；返回第 1 位 X、第 3 位 Y、第 7 位 Z
        // DCG = 7/1 + 1/2 + 3/3 = 8.5
        // IDCG = 7/1 + 3/log2(3) + 1/2 ≈ 9.39279
        // NDCG ≈ 0.90495
        let g = grades(&[("X", 3), ("Y", 1), ("Z", 2)]);
        let ranked = ["X", "a", "Y", "b", "c", "d", "Z", "e", "f", "g"];
        let m = evaluate(&ranked, &g, 10, 1);
        let idcg = 7.0 + 3.0 / 3.0f64.log2() + 0.5;
        assert!((m.ndcg - 8.5 / idcg).abs() < 1e-12, "ndcg = {}", m.ndcg);
        assert!((m.ndcg - 0.90495).abs() < 1e-4, "ndcg = {}", m.ndcg);
    }

    #[test]
    fn 阈值切换() {
        // 同例 thr=2 → R = {X, Z}，Top10 含 X、Z → Recall = 1.0
        let g = grades(&[("X", 3), ("Y", 1), ("Z", 2)]);
        let ranked = ["X", "a", "Y", "b", "c", "d", "Z", "e", "f", "g"];
        let m = evaluate(&ranked, &g, 10, 2);
        assert!((m.recall - 1.0).abs() < 1e-12);
        // thr=2 时 Y 不再是相关 → MRR 看首个 grade≥2 即 X 第 1 位
        assert!((m.mrr - 1.0).abs() < 1e-12);
        // NDCG 不受阈值影响
        let m1 = evaluate(&ranked, &g, 10, 1);
        assert_eq!(m.ndcg, m1.ndcg);
    }

    // ---------- 6.2 边界 ----------

    #[test]
    fn 无正例_非nan() {
        let g = grades(&[("A", 0)]);
        let m = evaluate(&["A", "B"], &g, 10, 1);
        assert_eq!(m.recall, 0.0);
        assert_eq!(m.ndcg, 0.0, "IDCG=0 时 NDCG 应为 0 而非 NaN");
    }

    #[test]
    fn 相关数超过k_idcg截断() {
        // 15 个 grade 1 正例，K=10：全正例按序 → NDCG = 1.0
        // （IDCG 取前 10 位，DCG 与 IDCG 相同）
        let pairs: Vec<(String, u8)> = (0..15).map(|i| (format!("p{i}"), 1)).collect();
        let g: HashMap<String, u8> = pairs.into_iter().collect();
        let ranked: Vec<&str> = vec!["p0", "p1", "p2", "p3", "p4", "p5", "p6", "p7", "p8", "p9"];
        let m = evaluate(&ranked, &g, 10, 1);
        assert!((m.ndcg - 1.0).abs() < 1e-12, "ndcg = {}", m.ndcg);
        assert!((m.recall - 10.0 / 15.0).abs() < 1e-12);
    }

    #[test]
    fn grade0贡献为0() {
        // 返回全是 grade 0 → DCG = 0 → NDCG = 0
        let g = grades(&[("A", 3), ("B", 0), ("C", 0)]);
        let m = evaluate(&["B", "C"], &g, 10, 1);
        assert_eq!(m.ndcg, 0.0);
    }

    #[test]
    fn ranked超长只取前k() {
        // y 排在第 11 位（K 之外）：不进 DCG 分子，但进 IDCG 分母（全部正例）
        let g = grades(&[("A", 1), ("y", 1)]);
        let mut ranked: Vec<&str> = vec!["A"];
        ranked.extend(vec!["x"; 9]); // 占满前 10
        ranked.push("y"); // 第 11 位
        let m = evaluate(&ranked, &g, 10, 1);
        // 对照：ranked 截断到前 10（y 直接不存在）→ 指标完全一致
        let m2 = evaluate(&ranked[..10], &g, 10, 1);
        assert!((m.ndcg - m2.ndcg).abs() < 1e-12, "y 在 K 外不应影响 NDCG");
        assert!((m.recall - m2.recall).abs() < 1e-12);
        // recall：R = {A, y}，Top10 只命中 A → 0.5
        assert!((m.recall - 0.5).abs() < 1e-12);
    }

    // ---------- 6.3 分位数 ----------

    #[test]
    fn nearest_rank分位数() {
        // 6,400 样本：P99 = 第 6,336 个（值 6335），P50 = 第 3,200 个（值 3199）
        let samples: Vec<f64> = (0..6400).map(|i| i as f64).collect();
        assert_eq!(percentile(&samples, 99.0), 6335.0);
        assert_eq!(percentile(&samples, 50.0), 3199.0);
        // 小样本：5 个 → P50 = 第 ceil(2.5)=3 个、P100 = 第 5 个
        let s5 = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&s5, 50.0), 3.0);
        assert_eq!(percentile(&s5, 100.0), 5.0);
        assert_eq!(percentile(&s5, 0.0), 1.0);
    }

    // ---------- 符号检验 ----------

    #[test]
    fn 符号检验手算() {
        // 18 对中正 14 负 4：p = Σ_{i=14}^{18} C(18,i)/2^18 = 4048/262144
        let p = sign_test(14, 18);
        let expected = 4048.0 / 262144.0;
        assert!(
            (p - expected).abs() < 1e-12,
            "p = {p}, expected = {expected}"
        );
        assert!((p - 0.0154).abs() < 1e-4);
    }

    #[test]
    fn 符号检验边界() {
        assert_eq!(sign_test(0, 0), 1.0);
        assert!(
            (sign_test(18, 18) - 1.0 / 262144.0).abs() < 1e-15,
            "全正 p = 2^-18"
        );
        assert_eq!(sign_test(0, 18), 1.0, "全负单侧 p = 1");
    }

    // ---------- 聚合 ----------

    #[test]
    fn 宏平均() {
        let ms = vec![
            QueryMetrics {
                recall: 1.0,
                mrr: 0.5,
                ndcg: 1.0,
            },
            QueryMetrics {
                recall: 0.0,
                mrr: 0.0,
                ndcg: 0.0,
            },
        ];
        let agg = aggregate(&ms);
        assert_eq!(agg.n, 2);
        assert!((agg.recall - 0.5).abs() < 1e-12);
        assert!((agg.mrr - 0.25).abs() < 1e-12);
        assert_eq!(aggregate(&[]), Aggregate::default());
    }
}
