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

    /// 平方欧氏距离 `d² = Σ(aᵢ−bᵢ)²`。
    ///
    /// 实现**转发** [`Self::distance_to_slice`]，全 crate 只有那一份求和循环——
    /// 见那里的说明（I4 的"逐位一致"必须是结构，而不是两份循环恰好同形）。
    pub fn distance_sq(&self, other: &Self) -> f32 {
        self.distance_to_slice(&other.0)
    }

    /// 与**已归一化**的切片算平方欧氏距离（`Σ(aᵢ−bᵢ)²`）。
    ///
    /// 供精确扫描逐点调用：候选向量来自图/存储的切片视图，走本函数**不分配、
    /// 不重算范数**（若改走 `NormalizedVector::new(v.to_vec())` 再 `distance_sq`，
    /// 每个候选都要一次堆分配 + 一次 512 维归约，`O(N)` 遍历的常数会被放大到
    /// 不可接受）。
    ///
    /// # 为什么 `distance_sq` 必须转发到这里（而不是并排放第二份循环）
    ///
    /// "精确路径与 [`crate::vector::BruteForceIndex`] 逐位一致"（I4 / S5-T3）
    /// 若靠**两份各自编写的求和循环**，就只依赖"编译器恰好把两者归约成同一串浮点
    /// 运算"——没有任何东西强制，是一句会腐烂的断言。单一实现 + 转发把它变成结构：
    /// `BruteForceIndex` 走 `query.distance_sq(v)` ⇒ 与精确路径天然同源。
    ///
    /// # 前置条件
    ///
    /// `other` 必须已 L2 归一化（图内数据来自 `NormalizedVector`，天然满足）。
    pub fn distance_to_slice(&self, other: &[f32]) -> f32 {
        self.0
            .iter()
            .zip(other)
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

    /// **S5-01 数值回归**：`distance_sq` 转发 `distance_to_slice` 之后必须**逐位**
    /// 等于重构前的那份求和循环。
    ///
    /// 断言用 `f32::to_bits` 而不是 `abs() < eps`：本 Step 把 I4（精确路径与
    /// `BruteForceIndex` 逐位一致）从"巧合"降级成"结构"，前提正是**这条转发没有
    /// 顺手改掉数值**。用 epsilon 会放过"少加一项 / 换了归约顺序"这类改动——
    /// 而那恰好会让 I4 变成 flaky。S5-T3 的逐位比对因此有了底。
    #[test]
    fn 距离转发后与重构前逐位相同() {
        let mut seed: u64 = 2026;
        let mut rng = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as u32 as f32 / u32::MAX as f32 * 2.0 - 1.0
        };
        for _ in 0..200 {
            let a = NormalizedVector::new((0..512).map(|_| rng()).collect());
            let b = NormalizedVector::new((0..512).map(|_| rng()).collect());

            // 重构前的实现原样复刻（唯一求和来源变更的对照）
            let legacy: f32 = a
                .as_slice()
                .iter()
                .zip(b.as_slice())
                .map(|(x, y)| {
                    let d = x - y;
                    d * d
                })
                .sum();

            assert_eq!(
                a.distance_sq(&b).to_bits(),
                legacy.to_bits(),
                "distance_sq 与重构前不逐位相同（I4 的结构性前提被破坏）"
            );
            assert_eq!(
                a.distance_to_slice(b.as_slice()).to_bits(),
                legacy.to_bits(),
                "distance_to_slice 与重构前不逐位相同"
            );
        }
    }
}
