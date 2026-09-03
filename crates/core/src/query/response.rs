//! 结构化返回类型：`Hit` / `Explain` / `SearchResponse` / `EmptyReason`。
//!
//! 这是架构文档 5.8 的唯一定义源落地，语义务必严格对齐（见各字段注释）。

use std::time::Duration;

use crate::types::{ChunkId, DocId, Score};

/// 一条最终命中结果（已回捞正文与元数据）。
#[derive(Debug, Clone)]
pub struct Hit {
    /// 命中的分片 ID（检索的最小单位）
    pub chunk_id: ChunkId,
    /// 所属文档 ID（一个文档可能含多个分片）
    pub doc_id: DocId,
    /// 融合后分数（Hybrid）/ 单路分数（单路模式）
    pub score: Score,
    /// 分片正文
    pub text: String,
    /// 出处：文件路径 / URL / 标题——溯源用（FR-12）
    pub source: String,
    /// 业务自定义元数据（过滤用，FR-14）
    pub metadata: serde_json::Value,
    /// 命中解释：为什么召回这条（FR-13）
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
    /// BM25 路分数；`None` = 该 lane 未召回此文档（诊断信号，勿用 0 或 usize::MAX）
    pub bm25_score: Option<Score>,
    /// BM25 路排名（从 1 起）；`None` 同 `bm25_score`
    pub bm25_rank: Option<u32>,
    /// 向量路余弦相似度；`None` = 未召回
    pub vector_score: Option<Score>,
    /// 向量路排名（从 1 起）；`None` 同 `vector_score`
    pub vector_rank: Option<u32>,
    /// 融合后的最终分数
    pub fused_score: Score,
}

/// 检索响应。
#[derive(Debug, Clone)]
pub struct SearchResponse {
    /// 最终命中列表（按分数降序）
    pub hits: Vec<Hit>,
    /// 融合阶段看到的候选总数（bm25 候选 + vector 候选去重后）
    pub total_candidates: usize,
    /// 空结果时说明原因，供 Agent 决策（FR-13）
    pub empty_reason: Option<EmptyReason>,
    /// 本次检索耗时（可观测性，NFR-07）
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
