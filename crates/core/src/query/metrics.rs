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
            "search"
        );
    }
}
