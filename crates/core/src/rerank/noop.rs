//! 无操作精排：原样返回，不改变顺序（FR-18 的占位实现）。

use crate::error::Result;
use crate::query::response::Hit;

use super::Reranker;

/// 空实现精排器。
#[derive(Debug, Default)]
pub struct NoOpReranker;

impl Reranker for NoOpReranker {
    fn rerank(&self, _query: &str, hits: Vec<Hit>, top_n: usize) -> Result<Vec<Hit>> {
        Ok(hits.into_iter().take(top_n).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::response::{Explain, Hit};

    fn dummy_hit(id: u32) -> Hit {
        Hit {
            chunk_id: id,
            doc_id: id,
            score: 1.0,
            text: "t".into(),
            source: "s".into(),
            metadata: serde_json::json!({}),
            explain: Explain {
                matched_terms: vec![],
                bm25_score: None,
                bm25_rank: None,
                vector_score: None,
                vector_rank: None,
                fused_score: 1.0,
            },
        }
    }

    #[test]
    fn noop不改变顺序() {
        let hits = vec![dummy_hit(3), dummy_hit(1), dummy_hit(2)];
        let r = NoOpReranker;
        let out = r.rerank("q", hits, 10).unwrap();
        let ids: Vec<u32> = out.iter().map(|h| h.chunk_id).collect();
        assert_eq!(ids, vec![3, 1, 2]);
    }

    #[test]
    fn noop截断top_n() {
        let hits = vec![dummy_hit(0), dummy_hit(1), dummy_hit(2)];
        let r = NoOpReranker;
        let out = r.rerank("q", hits, 2).unwrap();
        assert_eq!(out.len(), 2);
    }
}
