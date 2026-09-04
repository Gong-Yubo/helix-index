//! 编排：把两路召回、融合、精排、回捞串起来。
//!
//! # 边界（架构文档 4.2）
//!
//! 本模块**不自己实现算法**，只调用 `retriever` / `fusion` / `rerank`。
//! 正文回捞只在这里做一次（对 Top-K）。
//!
//! # 结构（P6 / I-01）
//!
//! 编排逻辑的唯一实现是自由函数 [`search_parts`]，依赖打包在 [`SearchParts`]。
//! [`QueryExecutor`]（旧名 `Searcher`）是它的薄壳：**对外签名一字不改**，
//! 供需要逐 lane 自定义组装的调用方使用（逃生舱，见 p6-design 7.3 最后一行）。
//! 门面层的 owned `Searcher`（I-05）同样复用 `search_parts`，不复制编排逻辑。

use std::collections::HashMap;
use std::time::Instant;

use crate::analyze::Analyzer;
use crate::embed::Embedder;
use crate::error::{Error, Result};
use crate::fusion::{FusionStrategy, LaneResults, RrfFusion};
use crate::index::Index;
use crate::rerank::{NoOpReranker, Reranker};
use crate::retriever::{Bm25Params, Bm25Retriever, Retriever, VectorRetriever};
use crate::types::{ChunkId, Score};
use crate::vector::VectorIndex;

use super::explain::{determine_empty_reason, matched_terms};
use super::metrics::Metrics;
use super::response::{Explain, Hit, SearchResponse};

/// 检索模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// 仅关键词路（BM25）
    Bm25,
    /// 仅语义路（向量近邻检索）
    Vector,
    /// 两路召回后融合（RRF，推荐默认）
    Hybrid,
}

/// `search_parts` 的全部依赖（借用型）。
///
/// 两个入口（`QueryExecutor` 与门面层 owned `Searcher`）都把自己的状态
/// 投影成这个结构再调用同一个函数，保证编排逻辑只有一份实现。
pub struct SearchParts<'a> {
    /// 倒排 + 正排 + 统计量
    pub index: &'a Index,
    /// 查询侧分词器（必须与建库侧同款，R4）
    pub analyzer: &'a dyn Analyzer,
    /// 向量化（`None` = 纯 BM25）
    pub embedder: Option<&'a dyn Embedder>,
    /// 向量索引（`None` = 纯 BM25）
    pub vector_index: Option<&'a dyn VectorIndex>,
    /// 融合策略
    pub fusion: &'a dyn FusionStrategy,
    /// 重排策略
    pub reranker: &'a dyn Reranker,
    /// BM25 参数（P5 定稿 k1=1.5 / b=0.75）
    pub bm25_params: Bm25Params,
}

/// 编排的唯一实现：两路召回 → 融合前过滤 → 融合 → 回捞 → 精排。
///
/// 所有检索入口（`QueryExecutor::search` / 门面 `Searcher::search`）最终都到这里，
/// 不存在第二份编排逻辑。
pub fn search_parts(
    parts: &SearchParts<'_>,
    query: &str,
    mode: SearchMode,
    k: usize,
    filter: Option<&crate::schema::Filter>,
) -> Result<SearchResponse> {
    let started = Instant::now();
    let mut metrics = Metrics::default();

    let index_is_empty = parts.index.num_chunks() == 0;
    let query_tokens = parts.analyzer.analyze_query(query);
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
            let bm25 =
                Bm25Retriever::new(parts.index, parts.analyzer).with_params(parts.bm25_params);
            (Some(to_lane(bm25.search(query, candidate_k)?)), None)
        }
        SearchMode::Vector => {
            let (e, vi) = require_vector(parts.embedder, parts.vector_index)?;
            let vec = VectorRetriever::new(e, vi);
            (None, Some(to_lane(vec.search(query, candidate_k)?)))
        }
        SearchMode::Hybrid => {
            let (e, vi) = require_vector(parts.embedder, parts.vector_index)?;
            let bm25 =
                Bm25Retriever::new(parts.index, parts.analyzer).with_params(parts.bm25_params);
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

    // 1.5 融合前位图过滤（FR-14 / NFR-02：查询路径零 IO）
    let pre_filter_candidates = metrics.bm25 + metrics.vector;
    let mut bm25_lane = bm25_lane;
    let mut vector_lane = vector_lane;
    if let Some(f) = filter {
        let allowed = super::filter::allowed_chunks(f, parts.index);
        let retain = |lane: &mut Option<LaneResults>| {
            if let Some(l) = lane.as_mut() {
                l.retain(|(id, _)| allowed.contains(id));
            }
        };
        retain(&mut bm25_lane);
        retain(&mut vector_lane);
    }

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
        parts.fusion.fuse(&lanes, candidate_k)
    } else {
        lanes.into_iter().flatten().collect()
    };

    metrics.candidates = fused.len();

    if fused.is_empty() {
        // 有候选但被过滤光 → FilteredOut；否则按"无召回/空 query"判定
        let reason = if pre_filter_candidates > 0 && filter.is_some() {
            Some(super::response::EmptyReason::FilteredOut)
        } else {
            determine_empty_reason(false, query_is_empty, 0)
        };
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
        let Some(chunk) = parts.index.chunk(chunk_id) else {
            continue;
        };
        let doc = parts
            .index
            .doc(chunk.doc_id)
            .ok_or(Error::ChunkNotFound(chunk_id))?;

        let explain = Explain {
            matched_terms: matched_terms(parts.analyzer, query, &chunk.text),
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
    let hits = parts.reranker.rerank(query, hits, k)?;

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

/// 向量路依赖检查：embedder 与 vector_index 必须成对出现。
fn require_vector<'a>(
    embedder: Option<&'a dyn Embedder>,
    vector_index: Option<&'a dyn VectorIndex>,
) -> Result<(&'a dyn Embedder, &'a dyn VectorIndex)> {
    match (embedder, vector_index) {
        (Some(e), Some(vi)) => Ok((e, vi)),
        _ => Err(Error::NoEmbedder),
    }
}

/// 检索编排器（旧名 `Searcher`，P6/I-01 改名）。持有各层的引用，不拥有它们。
///
/// 适合需要逐 lane 自定义组装的调用方（逃生舱，p6-design 7.3 最后一行）；
/// 一般用途请用门面层的 owned `Searcher`（I-05）。
pub struct QueryExecutor<'a> {
    index: &'a Index,
    analyzer: &'a dyn Analyzer,
    embedder: Option<&'a dyn Embedder>,
    vector_index: Option<&'a dyn VectorIndex>,
    fusion: Box<dyn FusionStrategy>,
    reranker: Box<dyn Reranker>,
    /// BM25 参数（P5 网格搜索从外部注入；默认 Bm25Params::default()）
    bm25_params: Bm25Params,
}

impl<'a> QueryExecutor<'a> {
    /// 只含 BM25 路（向量路留空）。
    pub fn new(index: &'a Index, analyzer: &'a dyn Analyzer) -> Self {
        Self {
            index,
            analyzer,
            embedder: None,
            vector_index: None,
            fusion: Box::new(RrfFusion::default()),
            reranker: Box::new(NoOpReranker),
            bm25_params: Bm25Params::default(),
        }
    }

    /// 覆盖 BM25 参数（T5-05 网格搜索注入口；现状内部用默认参数）。
    pub fn with_bm25_params(mut self, params: Bm25Params) -> Self {
        self.bm25_params = params;
        self
    }

    /// 接入向量路：embedder 负责查询侧向量化，vector_index 负责近邻检索。
    pub fn with_vector(
        mut self,
        embedder: &'a dyn Embedder,
        vector_index: &'a dyn VectorIndex,
    ) -> Self {
        self.embedder = Some(embedder);
        self.vector_index = Some(vector_index);
        self
    }

    /// 覆盖融合策略（默认 `RrfFusion`）。
    pub fn with_fusion(mut self, fusion: Box<dyn FusionStrategy>) -> Self {
        self.fusion = fusion;
        self
    }

    /// 覆盖重排策略（默认 `NoOpReranker`，P7 再接真实 rerank）。
    pub fn with_reranker(mut self, reranker: Box<dyn Reranker>) -> Self {
        self.reranker = reranker;
        self
    }

    /// 把自身状态投影成借用型 `SearchParts`（编排内核的输入）。
    fn parts(&self) -> SearchParts<'_> {
        SearchParts {
            index: self.index,
            analyzer: self.analyzer,
            embedder: self.embedder,
            vector_index: self.vector_index,
            fusion: self.fusion.as_ref(),
            reranker: self.reranker.as_ref(),
            bm25_params: self.bm25_params,
        }
    }

    /// 执行一次检索，返回结构化响应（含 hits / explain / took）。
    pub fn search(&self, query: &str, mode: SearchMode, k: usize) -> Result<SearchResponse> {
        self.search_filtered(query, mode, k, None)
    }

    /// 带元数据过滤的检索（FR-14 / T4-07）。
    ///
    /// 过滤在**融合前**对 lane 结果做（chunk_id 位图），查询路径零 IO。
    /// 有候选但被过滤条件全部排除时，`empty_reason = FilteredOut`。
    pub fn search_filtered(
        &self,
        query: &str,
        mode: SearchMode,
        k: usize,
        filter: Option<&crate::schema::Filter>,
    ) -> Result<SearchResponse> {
        let parts = self.parts();
        search_parts(&parts, query, mode, k, filter)
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
        // 只构造 QueryExecutor::new（无向量），BM25 模式应正常工作
        let searcher = QueryExecutor::new(&index, &analyzer);
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

        let searcher = QueryExecutor::new(&index, &analyzer).with_vector(&e, &vi);

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
        let searcher = QueryExecutor::new(&index, &analyzer);
        let resp = searcher.search("x", SearchMode::Bm25, 10).unwrap();
        assert_eq!(
            resp.empty_reason,
            Some(crate::query::response::EmptyReason::NoDocuments)
        );
    }

    #[test]
    fn 全停用词返回AllTermsUnmatched() {
        let (index, analyzer) = build_index(&["BM25 检索算法"]);
        let searcher = QueryExecutor::new(&index, &analyzer);
        // "的 了 在" 全是停用词
        let resp = searcher.search("的 了 在", SearchMode::Bm25, 10).unwrap();
        assert_eq!(
            resp.empty_reason,
            Some(crate::query::response::EmptyReason::AllTermsUnmatched)
        );
    }

    #[test]
    fn 过滤后是子集且全过滤触发FilteredOut() {
        use crate::schema::Filter;
        let (index, analyzer) = build_index(&[
            "BM25 是经典检索算法",
            "向量检索计算余弦相似度",
            "混合检索融合两路",
        ]);
        let searcher = QueryExecutor::new(&index, &analyzer);

        // 不过滤：应召回多条
        let all = searcher.search("检索", SearchMode::Bm25, 10).unwrap();
        assert!(all.hits.len() >= 2);

        // 过滤：构造一个只匹配部分文档的 metadata 条件——本测试语料无 metadata，
        // 因此用"永不匹配"的条件验证 FilteredOut
        let impossible = Filter::eq("__no_such_field__", "__no_such_value__");
        let filtered = searcher
            .search_filtered("检索", SearchMode::Bm25, 10, Some(&impossible))
            .unwrap();
        assert!(filtered.hits.is_empty());
        assert_eq!(
            filtered.empty_reason,
            Some(crate::query::response::EmptyReason::FilteredOut)
        );

        // 不过滤时有结果 → 证明 FilteredOut 不是因为无召回
        assert!(!all.hits.is_empty());
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
        let searcher = QueryExecutor::new(&index, &analyzer).with_vector(&e, &vi);

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
