//! 归一化向量：入库前强制 L2 归一化。
//!
//! # 距离约定（架构文档 7.3 / p2-design.md）
//!
//! 用**平方欧氏距离**（不开根号，单调性不变）：
//! `d² = Σ(aᵢ−bᵢ)² = 2 − 2·cos`（当两向量均为单位向量时）。
//! 因此 `cos = 1 − d²/2`，距离越小越近。

/// 已 L2 归一化的向量（内部保证范数 = 1）。
#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedVector(Vec<f32>);

impl NormalizedVector {
    /// 构造并**强制**归一化。幂等：输入已归一化时只多一次点积判断。
    pub fn new(raw: Vec<f32>) -> Self {
        let mut v = raw;
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 && (norm - 1.0).abs() > 1e-6 {
            for x in &mut v {
                *x /= norm;
            }
        }
        Self(v)
    }

    /// 向量维度
    pub fn dim(&self) -> usize {
        self.0.len()
    }

    /// 底层切片（用于喂给 ANN 索引与距离计算）。
    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    /// 余弦相似度（两者均为单位向量，等于点积）。
    pub fn cosine(&self, other: &Self) -> f32 {
        self.0.iter().zip(&other.0).map(|(a, b)| a * b).sum()
    }

    /// 平方欧氏距离 `d² = 2 − 2·cos`。
    pub fn distance_sq(&self, other: &Self) -> f32 {
        self.0
            .iter()
            .zip(&other.0)
            .map(|(a, b)| {
                let d = a - b;
                d * d
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 归一化幂等() {
        let a = NormalizedVector::new(vec![3.0, 4.0]); // 长度 5 → (0.6, 0.8)
        assert!((a.cosine(&NormalizedVector::new(vec![0.6, 0.8])) - 1.0).abs() < 1e-6);
        let norm: f32 = a.as_slice().iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn 正交向量余弦为零() {
        let a = NormalizedVector::new(vec![1.0, 0.0]);
        let b = NormalizedVector::new(vec![0.0, 1.0]);
        assert!(a.cosine(&b).abs() < 1e-6);
        // d² = 2 → cos = 1 - d²/2 = 0
        assert!((a.distance_sq(&b) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn 距离与余弦换算() {
        // 随机归一化向量对，验证 cos = 1 - d²/2 与直接点积误差 < 1e-5
        let mut seed: u64 = 42;
        let mut rng = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as u32 as f32 / u32::MAX as f32
        };
        for _ in 0..100 {
            let va: Vec<f32> = (0..512).map(|_| rng() * 2.0 - 1.0).collect();
            let vb: Vec<f32> = (0..512).map(|_| rng() * 2.0 - 1.0).collect();
            let a = NormalizedVector::new(va);
            let b = NormalizedVector::new(vb);
            let cos_direct = a.cosine(&b);
            let cos_from_d = 1.0 - a.distance_sq(&b) / 2.0;
            assert!(
                (cos_direct - cos_from_d).abs() < 1e-5,
                "cos 直接点积 {cos_direct} vs 1-d²/2 {cos_from_d}"
            );
        }
    }

    #[test]
    fn 零向量不除零() {
        let a = NormalizedVector::new(vec![0.0, 0.0]);
        assert_eq!(a.as_slice(), &[0.0, 0.0]);
    }
}
