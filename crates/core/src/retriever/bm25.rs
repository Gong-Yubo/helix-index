//! BM25 单路召回。
//!
//! - OR 语义（FR-04）：query 中未命中的 term 贡献 0 分
//! - TAAT（ADR-006）：逐 term 遍历 posting 累加，而非逐文档
//! - 确定性（NFR-06）：同分按 `chunk_id` 升序 tie-break

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::analyze::Analyzer;
use crate::error::Result;
use crate::index::Index;
use crate::predicate::CandidateFilter;
use crate::types::ChunkId;

use super::{Retriever, Scored};

/// BM25 可调参数（默认值经 P5 网格搜索定稿：T2Ranking 320 query × 12K 段落，
/// 16 格 k1×b 网格 NDCG@10 选优 k1=1.5/b=0.75，Recall/MRR 相对 Lucene 起点
/// 1.2/0.75 均不退化；详见 docs/devel/p5-design.md 8.x 与 eval-report，风险 R6）
#[derive(Debug, Clone, Copy)]
pub struct Bm25Params {
    /// 词频饱和参数（P5 实测定稿 1.5，非 Lucene 默认 1.2）
    pub k1: f32,
    /// 长度归一化强度（0~1，定稿 0.75）
    pub b: f32,
}

impl Default for Bm25Params {
    fn default() -> Self {
        Self { k1: 1.5, b: 0.75 }
    }
}

/// BM25 检索器。持有索引与分词器的引用，不拥有它们。
pub struct Bm25Retriever<'a> {
    index: &'a Index,
    analyzer: &'a dyn Analyzer,
    params: Bm25Params,
}

impl<'a> Bm25Retriever<'a> {
    /// 构造 BM25 检索器（用 `Bm25Params::default()`，即 P5 定稿值）。
    pub fn new(index: &'a Index, analyzer: &'a dyn Analyzer) -> Self {
        Self {
            index,
            analyzer,
            params: Bm25Params::default(),
        }
    }

    /// 覆盖 BM25 参数（网格搜索/调参用）。
    pub fn with_params(mut self, params: Bm25Params) -> Self {
        self.params = params;
        self
    }

    /// 计算一个 term 的 idf（与 tantivy 源码 `bm25.rs::idf` 一致）。
    fn idf(&self, df: u32) -> f32 {
        let n = self.index.num_chunks() as f32;
        let x = (n - df as f32 + 0.5) / (df as f32 + 0.5);
        (1.0 + x).ln()
    }
}

impl Retriever for Bm25Retriever<'_> {
    /// BM25 召回（TAAT 累加 + 末端 truncate）。
    ///
    /// # 过滤下推（§5.6）
    ///
    /// 判定加在 TAAT 累加循环内：被跳过的候选**不参与累加**，其余候选的分数不受影响。
    /// 因此这是**真下推**——召回不损失，且累加顺序不变（NFR-06 确定性保持）。
    ///
    /// 与 `allowed_chunks` 的融合前 post-filter 相比，下推后的 top-k 取自
    /// 「allowed 内的 top-k」而非「全局 top-k ∩ allowed」，是后者的**超集**（评审 P1-3）。
    fn search_filtered(
        &self,
        query: &str,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<Scored>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let tokens = self.analyzer.analyze_query(query);
        if tokens.is_empty() || self.index.num_chunks() == 0 {
            return Ok(Vec::new());
        }

        let avgdl = self.index.avgdl();
        let k1 = self.params.k1;
        let b = self.params.b;

        // TAAT 累加：chunk_id → 累计分
        let mut acc: HashMap<ChunkId, f32> = HashMap::new();

        // 去重 query term，但保留顺序无关紧要（累加）
        for token in tokens {
            let term = token.term.as_str();
            let Some(term_id) = self.index.term_id(term) else {
                continue; // df = 0，OR 语义下不贡献
            };
            let df = self.index.doc_freq(term);
            if df == 0 {
                continue;
            }
            let idf = self.idf(df);

            for posting in self.index.postings_by_id(term_id) {
                // 下推：评分期就跳过不通过的候选（其余候选分数不受影响）
                if let Some(f) = filter {
                    if !f.contains(posting.chunk_id) {
                        continue;
                    }
                }
                let dl = self.index.chunk_len(posting.chunk_id) as f32;
                let tf = posting.tf as f32;
                let norm = k1 * (1.0 - b + b * dl / avgdl);
                let partial = idf * (tf * (k1 + 1.0) / (tf + norm));
                *acc.entry(posting.chunk_id).or_insert(0.0) += partial;
            }
        }

        // 收集 → 排序（score 降序，chunk_id 升序 tie-break）
        let mut out: Vec<Scored> = acc
            .into_iter()
            .map(|(chunk_id, score)| Scored { chunk_id, score })
            .collect();
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        out.truncate(k);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::MixedAnalyzer;
    use crate::document::{Chunk, DocRecord};

    fn doc(source: &str, _text: &str) -> DocRecord {
        DocRecord {
            doc_id: 0,
            source: source.to_string(),
            metadata: serde_json::json!({}),
            content_hash: 0,
        }
    }

    fn chunk(doc_id: u32, text: &str) -> Chunk {
        Chunk {
            chunk_id: 0,
            doc_id,
            ordinal: 0,
            text: text.to_string(),
            char_start: 0,
            char_end: 0,
        }
    }

    /// 直接用固定 term 序列构造索引（绕过分词），验证 BM25 打分与手算值一致。
    ///
    /// 注：`add` 接口走 analyzer 分词，而这里要精确控制 term 序列，
    /// 因此用了一个能按固定序列出词的 analyzer。
    #[test]
    fn 手算值比对() {
        // 手算样例（见 p1-design.md 附录 A）：
        // docs: 0=[rust,rust,bm25] 1=[rust,bm25,vector] 2=[vector,vector,index] 3=[index,rust] 4=[bm25,index]
        // 用一个 analyzer 直接返回固定 term 序列，绕过分词不确定性。
        struct SeqAnalyzer;
        impl Analyzer for SeqAnalyzer {
            fn analyze_doc(&self, text: &str) -> Vec<crate::analyze::Token> {
                let seq: Vec<&str> = match text {
                    "rust rust bm25" => vec!["rust", "rust", "bm25"],
                    "rust bm25 vector" => vec!["rust", "bm25", "vector"],
                    "vector vector index" => vec!["vector", "vector", "index"],
                    "index rust" => vec!["index", "rust"],
                    "bm25 index" => vec!["bm25", "index"],
                    "rust bm25" => vec!["rust", "bm25"],
                    "vector" => vec!["vector"],
                    "index nonexist" => vec!["index", "nonexist"],
                    _ => vec![],
                };
                seq.into_iter()
                    .enumerate()
                    .map(|(i, t)| crate::analyze::Token {
                        term: t.into(),
                        position: i as u32,
                        byte_start: 0,
                        byte_end: 0,
                    })
                    .collect()
            }
        }

        let analyzer = SeqAnalyzer;
        let mut index = Index::new();
        for (d, text) in [
            (0u32, "rust rust bm25"),
            (1, "rust bm25 vector"),
            (2, "vector vector index"),
            (3, "index rust"),
            (4, "bm25 index"),
        ] {
            index
                .add(doc("s", text), vec![chunk(d, text)], &analyzer)
                .unwrap();
        }

        let r = Bm25Retriever::new(&index, &analyzer);

        let check = |q: &str, expected: &[u32]| {
            let got: Vec<u32> = r
                .search(q, 10)
                .unwrap()
                .into_iter()
                .map(|s| s.chunk_id)
                .collect();
            assert_eq!(got, expected, "query={q}");
        };

        // 期望排序来自 p1-design.md 附录 A。
        // 注意：分数为 0 的文档（不含任何 query term）**不返回**，所以只断言非零结果。
        // tie-break：chunk 3 与 4 同分时按 chunk_id 升序 → 3 在 4 前。
        check("rust bm25", &[0, 1, 3, 4]);
        check("vector", &[2, 1]);
        check("index nonexist", &[3, 4, 2]);
    }

    #[test]
    fn 空query与空索引() {
        let analyzer = MixedAnalyzer::new();
        let index = Index::new();
        let r = Bm25Retriever::new(&index, &analyzer);
        assert!(r.search("", 10).unwrap().is_empty());
        assert!(r.search("rust", 10).unwrap().is_empty());
    }

    #[test]
    fn 全停用词query返回空() {
        let analyzer = MixedAnalyzer::new();
        let mut index = Index::new();
        index
            .add(
                doc("s", "rust 检索"),
                vec![chunk(0, "rust 检索")],
                &analyzer,
            )
            .unwrap();
        let r = Bm25Retriever::new(&index, &analyzer);
        // "的 了 在" 全是停用词
        assert!(r.search("的 了 在", 10).unwrap().is_empty());
    }

    #[test]
    fn tie_break按chunk_id升序() {
        struct DummyAnalyzer;
        impl Analyzer for DummyAnalyzer {
            fn analyze_doc(&self, text: &str) -> Vec<crate::analyze::Token> {
                text.split_whitespace()
                    .enumerate()
                    .map(|(i, t)| crate::analyze::Token {
                        term: t.into(),
                        position: i as u32,
                        byte_start: 0,
                        byte_end: 0,
                    })
                    .collect()
            }
        }
        let analyzer = DummyAnalyzer;
        let mut index = Index::new();
        // 两个 chunk 都只含 "a"，同分 → 应返回 [0, 1]
        index
            .add(doc("s", "a"), vec![chunk(0, "a")], &analyzer)
            .unwrap();
        index
            .add(doc("s", "a"), vec![chunk(1, "a")], &analyzer)
            .unwrap();
        let r = Bm25Retriever::new(&index, &analyzer);
        let got: Vec<u32> = r
            .search("a", 10)
            .unwrap()
            .into_iter()
            .map(|s| s.chunk_id)
            .collect();
        assert_eq!(got, vec![0, 1]);
    }
}
