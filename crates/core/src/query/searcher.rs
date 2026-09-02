//! 编排：把两路召回、融合、精排、回捞串起来。
//!
//! # 边界（架构文档 4.2）
//!
//! 本模块**不自己实现算法**，只调用 `retriever` / `fusion` / `rerank`。
//! 正文回捞只在这里做一次（对 Top-K）。

use std::collections::HashMap;
use std::time::Instant;

use crate::analyze::Analyzer;
use crate::embed::Embedder;
use crate::error::{Error, Result};
use crate::fusion::{FusionStrategy, LaneResults, RrfFusion};
use crate::index::Index;
use crate::rerank::{NoOpReranker, Reranker};
use crate::retriever::{Bm25Retriever, Retriever, VectorRetriever};
use crate::types::{ChunkId, Score};
use crate::vector::VectorIndex;

use super::explain::{determine_empty_reason, matched_terms};
use super::metrics::Metrics;
use super::response::{Explain, Hit, SearchResponse};

/// 检索模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Bm25,
    Vector,
    Hybrid,
}

/// 检索编排器。持有各层的引用，不拥有它们。
pub struct Searcher<'a> {
    index: &'a Index,
    analyzer: &'a dyn Analyzer,
    embedder: Option<&'a dyn Embedder>,
    vector_index: Option<&'a dyn VectorIndex>,
    fusion: Box<dyn FusionStrategy>,
    reranker: Box<dyn Reranker>,
}

impl<'a> Searcher<'a> {
    /// 只含 BM25 路（向量路留空）。
    pub fn new(index: &'a Index, analyzer: &'a dyn Analyzer) -> Self {
        Self {
            index,
            analyzer,
            embedder: None,
            vector_index: None,
            fusion: Box::new(RrfFusion::default()),
            reranker: Box::new(NoOpReranker),
        }
    }

    pub fn with_vector(
        mut self,
        embedder: &'a dyn Embedder,
        vector_index: &'a dyn VectorIndex,
    ) -> Self {
        self.embedder = Some(embedder);
        self.vector_index = Some(vector_index);
        self
    }

    pub fn with_fusion(mut self, fusion: Box<dyn FusionStrategy>) -> Self {
        self.fusion = fusion;
        self
    }

    pub fn with_reranker(mut self, reranker: Box<dyn Reranker>) -> Self {
        self.reranker = reranker;
        self
    }

    pub fn search(&self, query: &str, mode: SearchMode, k: usize) -> Result<SearchResponse> {
        let started = Instant::now();
        let mut metrics = Metrics::default();

        let index_is_empty = self.index.num_chunks() == 0;
        let query_tokens = self.analyzer.analyze_query(query);
        let query_is_empty = query_tokens.is_empty();

        if index_is_empty {
            return Ok(empty_response(
                Some(super::response::EmptyReason::NoDocuments),
                started,
            ));
        }

        // 候选预算：融合时多看几倍，给精排留余地
        let candidate_k = k.saturating_mul(3).max(10);

        // 1. 两路召回（Hybrid 并行，单路只跑一路）
        let (bm25_lane, vector_lane) = match mode {
            SearchMode::Bm25 => {
                let bm25 = Bm25Retriever::new(self.index, self.analyzer);
                (Some(to_lane(bm25.search(query, candidate_k)?)), None)
            }
            SearchMode::Vector => {
                let (e, vi) = self.require_vector()?;
                let vec = VectorRetriever::new(e, vi);
                (None, Some(to_lane(vec.search(query, candidate_k)?)))
            }
            SearchMode::Hybrid => {
                let (e, vi) = self.require_vector()?;
                let bm25 = Bm25Retriever::new(self.index, self.analyzer);
                let vec = VectorRetriever::new(e, vi);
                // 并行执行；返回 (bm25, vector)，融合前按固定 lane 顺序收集
                let (r1, r2) = rayon::join(
                    || bm25.search(query, candidate_k),
                    || vec.search(query, candidate_k),
                );
                (Some(to_lane(r1?)), Some(to_lane(r2?)))
            }
        };

        metrics.bm25 = bm25_lane.as_ref().map_or(0, |l| l.len());
        metrics.vector = vector_lane.as_ref().map_or(0, |l| l.len());

        // 2. 融合（或单路直通）
        let mut lanes: Vec<LaneResults> = Vec::new();
        if let Some(l) = &bm25_lane {
            lanes.push(l.clone());
        }
        if let Some(l) = &vector_lane {
            lanes.push(l.clone());
        }

        // 单路模式：直接用该路结果作为"融合"输出（分数即单路分数）
        let fused: Vec<(ChunkId, Score)> = if mode == SearchMode::Hybrid {
            self.fusion.fuse(&lanes, candidate_k)
        } else {
            lanes.into_iter().flatten().collect()
        };

        metrics.candidates = fused.len();

        if fused.is_empty() {
            let reason = determine_empty_reason(false, query_is_empty, 0);
            return Ok(empty_response(reason, started));
        }

        // 3. 对 Top-K 做一次正排回捞 + 组装 explain
        let lane_rank = |lane: &Option<LaneResults>| -> HashMap<ChunkId, (u32, Score)> {
            lane.as_ref()
                .map(|l| {
                    l.iter()
                        .enumerate()
                        .map(|(i, (id, s))| (*id, (i as u32 + 1, *s)))
                        .collect()
                })
                .unwrap_or_default()
        };
        let bm25_rank = lane_rank(&bm25_lane);
        let vector_rank = lane_rank(&vector_lane);

        let mut hits = Vec::with_capacity(fused.len().min(k));
        for (chunk_id, fused_score) in fused.into_iter().take(k) {
            let Some(chunk) = self.index.chunk(chunk_id) else {
                continue;
            };
            let doc = self
                .index
                .doc(chunk.doc_id)
                .ok_or(Error::ChunkNotFound(chunk_id))?;

            let explain = Explain {
                matched_terms: matched_terms(self.analyzer, query, &chunk.text),
                bm25_score: bm25_rank.get(&chunk_id).map(|(_, s)| *s),
                bm25_rank: bm25_rank.get(&chunk_id).map(|(r, _)| *r),
                vector_score: vector_rank.get(&chunk_id).map(|(_, s)| *s),
                vector_rank: vector_rank.get(&chunk_id).map(|(r, _)| *r),
                fused_score,
            };

            hits.push(Hit {
                chunk_id,
                doc_id: chunk.doc_id,
                score: fused_score,
                text: chunk.text.clone(),
                source: doc.source.clone(),
                metadata: doc.metadata.clone(),
                explain,
            });
        }

        // 4. 精排（NoOp 留位）
        let hits = self.reranker.rerank(query, hits, k)?;

        metrics.fused = hits.len();
        metrics.took = started.elapsed();
        metrics.log(query);

        Ok(SearchResponse {
            hits,
            total_candidates: metrics.candidates,
            empty_reason: None,
            took: metrics.took,
        })
    }

    fn require_vector(&self) -> Result<(&'a dyn Embedder, &'a dyn VectorIndex)> {
        match (self.embedder, self.vector_index) {
            (Some(e), Some(vi)) => Ok((e, vi)),
            _ => Err(Error::NoEmbedder),
        }
    }
}

fn empty_response(
    reason: Option<super::response::EmptyReason>,
    started: Instant,
) -> SearchResponse {
    SearchResponse {
        hits: Vec::new(),
        total_candidates: 0,
        empty_reason: reason,
        took: started.elapsed(),
    }
}

/// 把 `Vec<Scored>` 转成 `LaneResults`。
fn to_lane(v: Vec<crate::retriever::Scored>) -> LaneResults {
    v.into_iter().map(|s| (s.chunk_id, s.score)).collect()
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)] // 中文测试名含英文缩写（NoDocuments / AllTermsUnmatched）
    use super::*;
    use crate::analyze::MixedAnalyzer;
    use crate::chunk::Chunker;
    use crate::document::Document;
    use crate::vector::BruteForceIndex;

    /// 确定性假 Embedder：FNV-1a 哈希 → 8 维归一化向量（同一文本恒同一向量）。
    struct FakeEmbedder;
    impl Embedder for FakeEmbedder {
        fn dim(&self) -> usize {
            8
        }
        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| fake_vec(t)).collect())
        }
        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            Ok(fake_vec(text))
        }
    }

    fn fake_vec(text: &str) -> Vec<f32> {
        // FNV-1a（确定，跨进程稳定）
        let mut h: u64 = 0xcbf29ce484222325;
        for b in text.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let mut v: Vec<f32> = (0..8)
            .map(|i| {
                let byte = (h >> (i * 8)) as u8;
                byte as f32 / 255.0 * 2.0 - 1.0
            })
            .collect();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }

    fn build_index(texts: &[&str]) -> (Index, MixedAnalyzer) {
        let analyzer = MixedAnalyzer::new();
        let chunker = Chunker::default();
        let mut index = Index::new();
        for (i, text) in texts.iter().enumerate() {
            let doc = Document {
                doc_id: 0,
                source: format!("doc-{i}"),
                metadata: serde_json::json!({}),
                content_hash: 0,
            };
            index.add(doc, chunker.chunk(0, text), &analyzer).unwrap();
        }
        (index, analyzer)
    }

    #[test]
    fn bm25模式不碰向量() {
        let (index, analyzer) = build_index(&["BM25 是检索算法", "向量检索用余弦相似度"]);
        // 只构造 Searcher::new（无向量），BM25 模式应正常工作
        let searcher = Searcher::new(&index, &analyzer);
        let resp = searcher.search("BM25", SearchMode::Bm25, 10).unwrap();
        assert!(!resp.hits.is_empty());
        assert_eq!(resp.empty_reason, None);
    }

    #[test]
    fn 三模式各有结果() {
        let (index, analyzer) = build_index(&[
            "BM25 是经典关键词检索算法，参数 k1 控制词频饱和",
            "向量检索把文本编码成向量计算余弦相似度",
            "混合检索融合关键词和向量两路结果",
        ]);

        let e = FakeEmbedder;
        let entries: Vec<(u32, crate::vector::NormalizedVector)> = index
            .live_chunks()
            .map(|c| {
                let v = fake_vec(&c.text);
                (c.chunk_id, crate::vector::NormalizedVector::new(v))
            })
            .collect();
        let mut vi = BruteForceIndex::new();
        for (id, v) in entries {
            vi.add(id, v).unwrap();
        }

        let searcher = Searcher::new(&index, &analyzer).with_vector(&e, &vi);

        let bm25 = searcher.search("检索", SearchMode::Bm25, 10).unwrap();
        let vec = searcher.search("检索", SearchMode::Vector, 10).unwrap();
        let hybrid = searcher.search("检索", SearchMode::Hybrid, 10).unwrap();

        assert!(!bm25.hits.is_empty());
        assert!(!vec.hits.is_empty());
        assert!(!hybrid.hits.is_empty());
        // Hybrid 的 explain 两路 rank 至少有一路非 None
        assert!(hybrid
            .hits
            .iter()
            .any(|h| h.explain.bm25_rank.is_some() || h.explain.vector_rank.is_some()));
    }

    #[test]
    fn 空索引返回NoDocuments() {
        let analyzer = MixedAnalyzer::new();
        let index = Index::new();
        let searcher = Searcher::new(&index, &analyzer);
        let resp = searcher.search("x", SearchMode::Bm25, 10).unwrap();
        assert_eq!(
            resp.empty_reason,
            Some(crate::query::response::EmptyReason::NoDocuments)
        );
    }

    #[test]
    fn 全停用词返回AllTermsUnmatched() {
        let (index, analyzer) = build_index(&["BM25 检索算法"]);
        let searcher = Searcher::new(&index, &analyzer);
        // "的 了 在" 全是停用词
        let resp = searcher.search("的 了 在", SearchMode::Bm25, 10).unwrap();
        assert_eq!(
            resp.empty_reason,
            Some(crate::query::response::EmptyReason::AllTermsUnmatched)
        );
    }

    #[test]
    fn 连续100次结果一致() {
        let (index, analyzer) = build_index(&[
            "BM25 是经典关键词检索算法",
            "向量检索计算余弦相似度",
            "混合检索融合两路结果",
            "倒排索引维护词项到文档的映射",
        ]);
        let e = FakeEmbedder;
        let entries: Vec<(u32, crate::vector::NormalizedVector)> = index
            .live_chunks()
            .map(|c| {
                (
                    c.chunk_id,
                    crate::vector::NormalizedVector::new(fake_vec(&c.text)),
                )
            })
            .collect();
        let mut vi = BruteForceIndex::new();
        for (id, v) in entries {
            vi.add(id, v).unwrap();
        }
        let searcher = Searcher::new(&index, &analyzer).with_vector(&e, &vi);

        let first: Vec<u32> = searcher
            .search("检索", SearchMode::Hybrid, 10)
            .unwrap()
            .hits
            .iter()
            .map(|h| h.chunk_id)
            .collect();
        for _ in 0..100 {
            let cur: Vec<u32> = searcher
                .search("检索", SearchMode::Hybrid, 10)
                .unwrap()
                .hits
                .iter()
                .map(|h| h.chunk_id)
                .collect();
            assert_eq!(first, cur, "第 N 次结果与首次不一致");
        }
    }
}
