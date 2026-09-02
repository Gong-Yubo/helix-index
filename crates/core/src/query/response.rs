//! 结构化返回类型：`Hit` / `Explain` / `SearchResponse` / `EmptyReason`。
//!
//! 这是架构文档 5.8 的唯一定义源落地，语义务必严格对齐（见各字段注释）。

use std::time::Duration;

use crate::types::{ChunkId, DocId, Score};

/// 一条最终命中结果（已回捞正文与元数据）。
#[derive(Debug, Clone)]
pub struct Hit {
    pub chunk_id: ChunkId,
    pub doc_id: DocId,
    /// 融合后分数（Hybrid）/ 单路分数（单路模式）
    pub score: Score,
    pub text: String,
    pub source: String,
    pub metadata: serde_json::Value,
    pub explain: Explain,
}

impl Hit {
    /// 直接产出可拼进 prompt 的上下文块，带出处标注（FR-12）。
    pub fn to_context_block(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("[来源: {}]\n", self.source));
        s.push_str(&self.text);
        if !self.explain.matched_terms.is_empty() {
            s.push_str(&format!(
                "\n(命中词: {})",
                self.explain.matched_terms.join(", ")
            ));
        }
        s
    }
}

/// 命中解释：告诉 Agent "为什么召回这条"（FR-13 / 架构 4.5）。
#[derive(Debug, Clone, Default)]
pub struct Explain {
    /// 查询分词中真正命中该分片的词（BM25 才有意义，向量路为空）
    pub matched_terms: Vec<String>,
    /// `None` = 该 lane 未召回此文档（诊断信号，勿用 0 或 usize::MAX）
    pub bm25_score: Option<Score>,
    pub bm25_rank: Option<u32>,
    pub vector_score: Option<Score>,
    pub vector_rank: Option<u32>,
    pub fused_score: Score,
}

/// 检索响应。
#[derive(Debug, Clone)]
pub struct SearchResponse {
    pub hits: Vec<Hit>,
    /// 融合阶段看到的候选总数（bm25 候选 + vector 候选去重后）
    pub total_candidates: usize,
    /// 空结果时说明原因，供 Agent 决策（FR-13）
    pub empty_reason: Option<EmptyReason>,
    pub took: Duration,
}

/// 空结果原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyReason {
    /// 索引为空
    NoDocuments,
    /// 词全部未命中 —— 提示 query 可能含幻觉词
    AllTermsUnmatched,
    /// 有候选但被 filter 全部过滤（P4 过滤落地后启用）
    FilteredOut,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_block含出处与正文() {
        let hit = Hit {
            chunk_id: 0,
            doc_id: 0,
            score: 1.0,
            text: "中文分词是检索的第一步".into(),
            source: "chinese-tokenization.md".into(),
            metadata: serde_json::json!({}),
            explain: Explain {
                matched_terms: vec!["分词".into(), "检索".into()],
                ..Default::default()
            },
        };
        let block = hit.to_context_block();
        assert!(block.contains("[来源: chinese-tokenization.md]"));
        assert!(block.contains("中文分词是检索的第一步"));
        assert!(block.contains("分词"));
    }
}
