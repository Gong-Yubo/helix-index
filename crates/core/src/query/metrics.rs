//! 可观测性：检索过程的轻量指标（NFR-07）。
//!
//! # ⚠️ 如何观测（当前限制）
//!
//! `Metrics` **只在 `search_parts` 内部聚合，经 [`Metrics::log`] 以 `tracing::info!`
//! 输出**（事件名 `search`）——它既不在 `SearchResponse` 里，也不进 bench 的
//! `bench::QueryMetrics`。由此有三个后果：
//!
//! - 宿主程序必须挂 `tracing` subscriber 才能拿到这些数字，否则静默丢弃
//! - **无法对其做单元测试**（外部拿不到实例）
//! - **无法被 bench 聚合**——而 `vector_shortfall` 的定位正是「V2.1 是否引入
//!   prefilter 的判据」。真要用它做决策，得先把 `Metrics` 暴露进响应或 bench 采集链路
//!
//! 这个缺口是已知且未修的（见 issue #7），先让口径正确（按 `allowed` 归一），
//! 再让口径可观测。

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
    /// 向量路「本该拿到 `min(candidate_k, allowed)` 条、实得 n 条」的缺口。
    ///
    /// 归一到 `allowed`（通过过滤的候选总数）而非裸 `candidate_k`：候选池本身不足
    /// `candidate_k` 时（小语料、高选择度过滤）`candidate_k - len` 恒为正，会让
    /// 「filtered-ANN 是否降级」这个信号彻底失去鉴别力。
    ///
    /// > 0 表示低选择度下 ANN 没凑够候选——不是错误，但会让召回静默下降，
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
