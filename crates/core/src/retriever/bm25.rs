//! BM25 单路召回。
//!
//! - OR 语义（FR-04）：query 中未命中的 term 贡献 0 分
//! - TAAT（ADR-006）：逐 term 遍历 posting 累加，而非逐文档
//! - 确定性（NFR-06）：同分按 `chunk_id` 升序 tie-break

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::analyze::Analyzer;
use crate::document::Chunk;
use crate::error::Result;
use crate::index::Index;
use crate::predicate::CandidateFilter;
use crate::types::{ChunkId, DocId};

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

        // ⚠️ **刻意不去重** query term（`S8-03` 评审 P4-1 更正了旧注释的「去重」说法）：
        //    本循环与跨段版（`SegmentedBm25Retriever`）里的同类循环都是 `for token in tokens`
        //    直遍 ⇒ 重复 term 会累加两次 —— **两条路径同形**，这正是逐位一致的前提之一
        //    （若只有一边去重，跨段与单段的结果就会分叉）。
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

/// 一个段的**检索视图**（`S8-04`）：段内索引 + 它在全局 ID 空间里的基址。
///
/// ⚠️ 公开的理由：它是 [`SegmentSet`] 的元素，而 `SegmentSet` 是 `SearchParts`
/// （公开结构）的字段类型。它**不**暴露 `Segment`（那是 `pub(crate)`）——
/// 只借 `Index` 与两个基址，都是公开类型。
#[derive(Debug, Clone, Copy)]
pub struct SegmentRef<'a> {
    /// 段内索引（**本地** ID 从 0 起）
    pub index: &'a Index,
    /// 该段的全局 `doc_id` 基址（`全局 = base_doc + 本地`）
    pub base_doc: DocId,
    /// 该段的全局 `chunk_id` 基址
    pub base_chunk: ChunkId,
}

/// **跨段** BM25 的输入（`S8-04` / 设计 §4.5.1）：FIFO 顺序的段 + **全局整数统计量**。
///
/// 🔑 `n` / `total_len` 必须是**跨段精确和**（`I8-5`）—— 由调用方（`Searcher::parts`）
/// 用 `Σ` 算好，而不是让检索器自己猜。
#[derive(Debug, Clone)]
pub struct SegmentSet<'a> {
    /// **FIFO 顺序**：`main, deltas[0], …`。
    ///
    /// ⚠️ **顺序是正确性的一部分**（`I8-6`）：TAAT 的累加顺序必须与单段建库一致
    /// （段内 postings 是 push 序 ⇒ 全局 `chunk_id` 升序）。
    pub segments: Vec<SegmentRef<'a>>,
    /// 全局 `N`（= Σ 各段**活**分片数）
    pub n: u32,
    /// 全局 `total_len`（= Σ 各段词项总数）
    pub total_len: u64,
    /// 跨段墓碑条数（**针对既往段**的全局 doc 墓碑）。
    ///
    /// 🔑 它决定 BM25 能否走**零谓词**热路径（`R52` / 设计 §4.7.3）：
    /// 段内的删除总是**物理摘除**该段 postings ⇒ 活 postings 里不可能有本段已删的
    /// chunk；真正需要判活的只有**跨段墓碑**（目标 doc 在所属段里仍存活、postings 还在）。
    /// ⇒ `tombstones == 0` 时 BM25 可以传 `filter = None`（与单段同速）。
    pub tombstones: usize,
}

impl<'a> SegmentSet<'a> {
    /// 全局 `chunk_id` → `(段, 段内本地 id)`；不属于任何段 ⇒ `None`。
    pub fn locate(&self, global_chunk: ChunkId) -> Option<(SegmentRef<'a>, ChunkId)> {
        for seg in &self.segments {
            if let Some(local) = global_chunk.checked_sub(seg.base_chunk) {
                if (local as usize) < seg.index.total_chunks() {
                    return Some((*seg, local));
                }
            }
        }
        None
    }

    /// 按全局 `chunk_id` 取分片正文（回捞用）。
    pub fn chunk(&self, global_chunk: ChunkId) -> Option<&'a Chunk> {
        let (seg, local) = self.locate(global_chunk)?;
        seg.index.chunk(local)
    }

    /// 按全局 `chunk_id` 取**全局** `doc_id`（回捞用）。
    pub fn global_doc_id(&self, global_chunk: ChunkId) -> Option<DocId> {
        let (seg, local) = self.locate(global_chunk)?;
        seg.index.doc_of(local).map(|d| seg.base_doc + d)
    }

    /// 该**全局** `doc_id` 是否落在某个段里（合并/墓碑判定用）。
    pub fn owns_doc(&self, global_doc: DocId) -> bool {
        self.segments.iter().any(|seg| {
            global_doc >= seg.base_doc
                && ((global_doc - seg.base_doc) as usize) < seg.index.total_docs()
        })
    }

    /// 词典探针：query 的任一 term 在**任何段**里有 posting 吗？
    ///
    /// 与单段的 `query_has_hits` **同口径**（去重 query term、判 `df > 0`），
    /// 只是把「单个索引」换成「所有段」。用于空结果时区分
    /// 「query 侧无命中」与「过滤太窄」（§5.8.1 的 query 侧优先）。
    pub fn any_term_hits(&self, analyzer: &dyn Analyzer, query: &str) -> bool {
        let mut seen = std::collections::HashSet::new();
        analyzer
            .analyze_query(query)
            .iter()
            .filter(|t| seen.insert(t.term.as_str()))
            .any(|t| {
                self.segments
                    .iter()
                    .any(|s| s.index.doc_freq(t.term.as_str()) > 0)
            })
    }
}

/// **跨段** BM25 检索器（`S8-04` / 设计 §4.5.2）。
///
/// 与单段的 [`Bm25Retriever`] 的差别只有「统计量取全局和」与「遍历是 term 外层、
/// **段**内层」两点 —— 而这两点恰好是 `S8-T4`「分段布局与单段布局逐位一致」的**全部**条件
/// （设计 §2.5 的可加性分析）。
pub struct SegmentedBm25Retriever<'a> {
    set: &'a SegmentSet<'a>,
    analyzer: &'a dyn Analyzer,
    params: Bm25Params,
}

impl<'a> SegmentedBm25Retriever<'a> {
    /// 构造（默认 `Bm25Params`，即 P5 定稿值）。
    pub fn new(set: &'a SegmentSet<'a>, analyzer: &'a dyn Analyzer) -> Self {
        Self {
            set,
            analyzer,
            params: Bm25Params::default(),
        }
    }

    /// 覆盖 BM25 参数（与单段版同签名，网格搜索/调参用）。
    pub fn with_params(mut self, params: Bm25Params) -> Self {
        self.params = params;
        self
    }
}

impl Retriever for SegmentedBm25Retriever<'_> {
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
        if tokens.is_empty() || self.set.n == 0 {
            return Ok(Vec::new());
        }

        let n = self.set.n as f32;
        // ⚠️ `avgdl` 用**全局** `total_len / N` 算**一次**（`I8-5`）：各段各算再平均会引入
        //    两次浮点舍入 ⇒ 分数只在分段布局下漂移。
        let avgdl = self.set.total_len as f32 / self.set.n as f32;
        let k1 = self.params.k1;
        let b = self.params.b;

        let mut acc: HashMap<ChunkId, f32> = HashMap::new();

        for token in tokens {
            let term = token.term.as_str();
            // `df` = **全局精确整数和**（`I8-5`）—— 不是最大值、不是平均
            let df: u32 = self
                .set
                .segments
                .iter()
                .map(|s| s.index.doc_freq(term))
                .sum();
            if df == 0 {
                continue; // OR 语义：该 term 不贡献
            }
            let x = (n - df as f32 + 0.5) / (df as f32 + 0.5);
            let idf = (1.0 + x).ln();

            // ⚠️ `I8-6`：**外层 term、内层段**（FIFO）。段内 postings 是 push 序 ⇒
            //    全局 `chunk_id` 升序 ⇒ 与单段建库的累加顺序**逐位一致**。
            for seg in &self.set.segments {
                let Some(term_id) = seg.index.term_id(term) else {
                    continue; // 该段没有这个词
                };
                for posting in seg.index.postings_by_id(term_id) {
                    let global = seg.base_chunk + posting.chunk_id;
                    if let Some(f) = filter {
                        if !f.contains(global) {
                            continue;
                        }
                    }
                    let dl = seg.index.chunk_len(posting.chunk_id) as f32;
                    let tf = posting.tf as f32;
                    let norm = k1 * (1.0 - b + b * dl / avgdl);
                    let partial = idf * (tf * (k1 + 1.0) / (tf + norm));
                    *acc.entry(global).or_insert(0.0) += partial;
                }
            }
        }

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
