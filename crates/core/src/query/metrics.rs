//! 可观测性：检索过程的轻量指标（NFR-07）。

use std::time::Duration;

/// 一次检索的指标快照。
#[derive(Debug, Clone, Copy, Default)]
pub struct Metrics {
    /// 总耗时（含两路召回 + 融合 + 回捞）
    pub took: Duration,
    /// BM25 路候选数
    pub bm25: usize,
    /// 向量路候选数
    pub vector: usize,
    /// 融合去重后候选总数
    pub candidates: usize,
    /// 最终输出条数
    pub fused: usize,
    /// 过滤求值 + 谓词构建耗时（O(query 词数 + 匹配文档数)，不再是 O(N) 全扫）
    pub filter_eval: Duration,
    /// 通过过滤的候选总数（`CandidateFilter::allowed_count`；无过滤时为存活 chunk 数）
    pub allowed: usize,
    /// 向量路"想取 `candidate_k` 条、实得 n 条"的缺口（暴露 filtered-ANN 的降级）
    ///
    /// > 0 表示低选择度下 ANN 凑不够候选——不是错误，但会让召回静默下降，
    /// > 必须可观测（V2.1 是否引入 prefilter 结构的判据）。
    pub vector_shortfall: usize,
}

impl Metrics {
    /// 用 tracing 输出一行结构化日志。
    pub fn log(&self, query: &str) {
        tracing::info!(
            query = query,
            took_ms = self.took.as_millis() as u64,
            bm25 = self.bm25,
            vector = self.vector,
            candidates = self.candidates,
            fused = self.fused,
            filter_eval_us = self.filter_eval.as_micros() as u64,
            allowed = self.allowed,
            vector_shortfall = self.vector_shortfall,
            "search"
        );
    }
}
