//! 向量单路召回。
//!
//! 流程：`embed_query`（**加前缀**）→ 向量索引检索 → 距离转相似度 → Top-K。
//!
//! # 边界（架构文档 4.3 硬约束）
//!
//! 本文件**不得** import `super::bm25`，反之亦然——两条 lane 互不引用。

use std::cmp::Ordering;

use crate::embed::Embedder;
use crate::error::Result;
use crate::vector::{NormalizedVector, VectorIndex};

use super::{Retriever, Scored};

/// 向量检索器。持有 embedder 与向量索引的引用，不拥有它们。
pub struct VectorRetriever<'a> {
    embedder: &'a dyn Embedder,
    index: &'a dyn VectorIndex,
}

impl<'a> VectorRetriever<'a> {
    pub fn new(embedder: &'a dyn Embedder, index: &'a dyn VectorIndex) -> Self {
        Self { embedder, index }
    }
}

impl Retriever for VectorRetriever<'_> {
    fn search(&self, query: &str, k: usize) -> Result<Vec<Scored>> {
        if k == 0 || query.trim().is_empty() || self.index.is_empty() {
            return Ok(Vec::new());
        }

        let q = NormalizedVector::new(self.embedder.embed_query(query)?);
        let raw = self.index.search(&q, k)?;

        // distance（平方欧氏）→ 相似度 score = 1 - d²/2 = cos（单位向量）
        let mut out: Vec<Scored> = raw
            .into_iter()
            .map(|(chunk_id, d)| Scored {
                chunk_id,
                score: 1.0 - d / 2.0,
            })
            .collect();

        // 相似度降序；同分按 chunk_id 升序（确定性 NFR-06）
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::BruteForceIndex;

    /// 测试用假 Embedder：把文本映射到固定向量（不依赖模型）。
    struct FakeEmbedder;
    impl Embedder for FakeEmbedder {
        fn dim(&self) -> usize {
            2
        }
        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| vec![t.len() as f32, 1.0]).collect())
        }
        fn embed_query(&self, _text: &str) -> Result<Vec<f32>> {
            // 查询向量恒为 (1,0) → 最近的是 (1,1) 归一化后的向量
            Ok(vec![1.0, 0.0])
        }
    }

    #[test]
    fn topk按相似度降序() {
        let mut idx = BruteForceIndex::new();
        // 三个向量：0≈(1,0) 1≈(0,1) 2≈(1,1)/√2
        idx.add(0, NormalizedVector::new(vec![1.0, 0.0])).unwrap();
        idx.add(1, NormalizedVector::new(vec![0.0, 1.0])).unwrap();
        idx.add(2, NormalizedVector::new(vec![1.0, 1.0])).unwrap();

        let e = FakeEmbedder;
        let r = VectorRetriever::new(&e, &idx);

        // 查询向量 (1,0)：最近 0（cos=1），其次 2（cos≈0.707），最远 1（cos=0）
        let got = r.search("任意", 10).unwrap();
        let ids: Vec<u32> = got.iter().map(|s| s.chunk_id).collect();
        assert_eq!(ids, vec![0, 2, 1]);
        // 分数 = cos，递减
        assert!(got[0].score > got[1].score && got[1].score > got[2].score);
        assert!((got[0].score - 1.0).abs() < 1e-5);
        assert!(got[1].score < 0.75);
    }

    #[test]
    fn 空查询空索引() {
        let e = FakeEmbedder;
        let idx = BruteForceIndex::new();
        let r = VectorRetriever::new(&e, &idx);
        assert!(r.search("", 10).unwrap().is_empty());
        assert!(r.search("x", 10).unwrap().is_empty());
    }
}
