//! 统计量维护：`total_len` 与 `num_chunks`。
//!
//! 这两个量是 BM25 的 `avgdl` 分母来源，必须**增量维护**且在删除时回滚，
//! 且"增量维护的结果"要能通过单测与"全量重建"逐项相等（风险 R5）。
//! 注意 `df` 不在此维护——它由 `postings.len()` 直接推导（见 `inverted.rs`），
//! 不存在第二份需要同步的状态。

/// 索引级统计量
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// 所有活分片的 term 总数（Σ dl）
    pub total_len: u64,
    /// 活分片数量（BM25 中的 N）
    pub num_chunks: u32,
}

impl Stats {
    /// 平均分片长度（term 数）。`num_chunks == 0` 时返回 0，避免除零。
    pub fn avgdl(&self) -> f32 {
        if self.num_chunks == 0 {
            0.0
        } else {
            self.total_len as f32 / self.num_chunks as f32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avgdl_除零保护() {
        let s = Stats::default();
        assert_eq!(s.avgdl(), 0.0);
    }

    #[test]
    fn avgdl_normal() {
        let s = Stats {
            total_len: 13,
            num_chunks: 5,
        };
        assert!((s.avgdl() - 2.6).abs() < 1e-6);
    }
}
