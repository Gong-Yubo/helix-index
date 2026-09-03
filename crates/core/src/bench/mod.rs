//! 评测（P5 / T5-03a）。
//!
//! # 边界（架构文档 4.2）
//!
//! 本模块是**纯数学 + 数据装载**：指标计算只消费
//! "排序后的 source 序列 + graded 标注"，不知道索引 / 检索器的内部；
//! judgments 装载被 tantivy 基线测试（ADR-007）复用。
//!
//! # 指标口径（p5-design.md 第 6 章）
//!
//! - Recall / MRR：阈值化二值（`rel_threshold`，主表 thr=1 / thr=2 双列）
//! - NDCG：分级 gain = 2^grade − 1（不受阈值影响）
//! - 聚合：宏平均
//! - 延迟：nearest-rank P50 / P99
//! - 显著性：二项精确符号检验（单侧）

pub mod judgments;
pub mod metrics;

pub use judgments::{load_judgments, validate_sources, Judgment, RelEntry};
pub use metrics::{aggregate, evaluate, percentile, sign_test, Aggregate, QueryMetrics};
