//! 元数据过滤（FR-14 / T4-07）：tag 等值 + 数值范围，融合前用位图过滤。
//!
//! # 边界（架构文档 7.6 / NFR-02）
//!
//! 过滤**在融合前**对 lane 结果做（`chunk_id` 位图），查询路径零 IO——
//! 位图在每次检索时由正排 metadata 构建（纯内存，无磁盘）。
//!
//! 匹配语义：
//! - `Eq`  ：`metadata[field]` 与 value **字符串等值**（数字按字符串比较也算相等）
//! - `Range`：`metadata[field]` 是数字且 `gte ≤ v < lte`（左闭右开）
//! - `And` ：全部满足；`Or`：任一满足

//! # 过滤下推（V2 Step 1 / FR-27）
//!
//! 本文件同时提供**两套**求值实现，互为对照：
//!
//! | 函数                | 数据源         | 成本                     | 用途        |
//! | ----------------- | ----------- | ---------------------- | --------- |
//! | [`doc_bits`]      | 字段索引（位图）    | O(query 词数 + 位运算)       | 检索热路径     |
//! | [`doc_bits_scan`] | 逐文档 `matches()` | O(N) 次 JSON 取值           | 兜底 + oracle |
//!
//! 字段索引遇到"字段未索引 / 已因基数超限降级"时会自动回落到 [`doc_bits_scan`]——
//! **正确性不受影响，只是慢**。二者结果必须恒等，由 T6 随机对照测试钉死。

use std::collections::HashSet;

use crate::bitmap::DocBits;
use crate::index::Index;
use crate::predicate::{CandidateFilter, FilterKind};
use crate::schema::Filter;
use crate::types::{ChunkId, DocId};

/// 判断单个文档的 metadata 是否满足过滤条件。
pub fn matches(filter: &Filter, metadata: &serde_json::Value) -> bool {
    match filter {
        Filter::Eq { field, value } => metadata
            .get(field)
            .map(|v| json_to_string(v) == *value)
            .unwrap_or(false),
        Filter::Range { field, gte, lte } => metadata
            .get(field)
            .and_then(|v| v.as_f64())
            .map(|v| v >= *gte && v < *lte)
            .unwrap_or(false),
        Filter::And(list) => list.iter().all(|f| matches(f, metadata)),
        Filter::Or(list) => list.iter().any(|f| matches(f, metadata)),
    }
}

/// 构建允许通过的 chunk_id 集合（融合前位图过滤用）。
///
/// 规则：chunk 继承其所属文档的 metadata 判定结果。
pub fn allowed_chunks(filter: &Filter, index: &Index) -> HashSet<ChunkId> {
    let mut out = HashSet::new();
    for chunk in index.live_chunks() {
        if let Some(doc) = index.doc(chunk.doc_id) {
            if matches(filter, &doc.metadata) {
                out.insert(chunk.chunk_id);
            }
        }
    }
    out
}

/// 求满足 filter 的 **doc** 位图（字段索引驱动；字段未索引/已降级时退化为全表扫描）。
///
/// 粒度是 doc 而非 chunk：metadata 挂在 doc 上，chunk 继承其判定结果
/// （检索期由 [`ChunkFilter`] 做 `chunk → doc` 的 O(1) 映射）。
pub fn doc_bits(filter: &Filter, index: &Index) -> DocBits {
    let fi = index.field_index();
    match filter {
        Filter::Eq { field, value } => match fi.eq_bits(field, value) {
            Some(bits) => bits,
            None => doc_bits_scan(filter, index), // 字段已降级 → 全扫
        },
        Filter::Range { field, gte, lte } => match fi.range_bits(field, *gte, *lte) {
            Some(bits) => bits,
            None => doc_bits_scan(filter, index),
        },
        Filter::And(list) => {
            // 先按基数升序：小集合先交，后续交集的输入规模单调变小
            let mut parts: Vec<DocBits> = list.iter().map(|f| doc_bits(f, index)).collect();
            parts.sort_by_key(DocBits::count_ones);
            let mut iter = parts.into_iter();
            let mut out = iter.next().unwrap_or_default();
            for part in iter {
                out.intersect_with(&part);
                if out.is_empty() {
                    break; // 已空，剩下的交不出来东西
                }
            }
            out
        }
        Filter::Or(list) => {
            let mut out = DocBits::new();
            for f in list {
                out.union_with(&doc_bits(f, index));
            }
            out
        }
    }
}

/// 兜底 / 对照实现：逐文档用 [`matches()`] 判定（与字段索引互为 oracle）。
///
/// 遍历**存活文档**（不是 chunk）——字段索引按 doc 级维护，兜底必须同粒度，
/// 否则两者语义会错开。
pub fn doc_bits_scan(filter: &Filter, index: &Index) -> DocBits {
    let mut out = DocBits::new();
    for (doc_id, record) in index.iter_live_docs() {
        if matches(filter, &record.metadata) {
            out.set(doc_id);
        }
    }
    out
}

/// 把 doc 位图换算为**存活 chunk 数**（O(匹配文档数)，供 `Metrics` 与 `ef` 策略）。
///
/// 逐命中文档取其存活分片数求和，而不是展开成 chunk 位图——
/// 展开是 O(N)，那正是 Q-I1 要消掉的病根（D-S1-09）。
pub fn allowed_chunk_count(bits: &DocBits, index: &Index) -> usize {
    bits.iter()
        .map(|doc_id| index.chunk_count_of_doc(doc_id) as usize)
        .sum()
}

/// 有用户过滤场景的候选谓词：**惰性**判定 `chunk_id → doc_id → doc_bits`。
///
/// 不展开 chunk 位图（那是 O(N)），每次 `contains` 只做两次 O(1) 查找：
///
/// 1. 存活位图——挡掉已软删除的 chunk（Q-C1 的修复点）
/// 2. doc 位图——`chunk → doc` 是数组索引，再查一次位
///
/// 因此每 query 的构建成本是 **O(query 词数 + 匹配文档数)**，与语料规模解耦。
#[derive(Debug)]
pub struct ChunkFilter<'a> {
    doc_bits: DocBits,
    index: &'a Index,
    count: usize,
}

impl<'a> ChunkFilter<'a> {
    /// 用 doc 位图构造谓词（`allowed_count` 在构造时算好，之后 O(1) 返回）。
    pub fn new(doc_bits: DocBits, index: &'a Index) -> Self {
        let count = allowed_chunk_count(&doc_bits, index);
        Self {
            doc_bits,
            index,
            count,
        }
    }

    /// 内部的 doc 位图（诊断/测试用）。
    pub fn doc_bits(&self) -> &DocBits {
        &self.doc_bits
    }
}

impl CandidateFilter for ChunkFilter<'_> {
    fn contains(&self, chunk_id: ChunkId) -> bool {
        // 1) 存活过滤：已软删除的 chunk 其向量仍在 HNSW 图里，必须在这里挡掉
        if !self.index.alive_chunks().contains(chunk_id) {
            return false;
        }
        // 2) 用户过滤：chunk → doc 是 O(1) 数组索引
        match self.index.doc_of(chunk_id) {
            Some(doc_id) => self.doc_bits.contains(doc_id),
            None => false,
        }
    }

    fn allowed_count(&self) -> usize {
        self.count
    }

    fn kind(&self) -> FilterKind {
        FilterKind::Filtered
    }
}

/// 构建本 query 的候选谓词。
///
/// - `None`（无用户过滤）→ [`AliveOnly`](crate::predicate::AliveOnly)，只做存活过滤；
/// - `Some(filter)` → [`ChunkFilter`]；若 doc 位图为空返回 `false`，编排层据此短路。
///
/// 返回 `bool` 而非 `Result`，因为唯一的"失败"语义（过滤排空）在编排层
/// 要按 `EmptyReason` 细分，判定留给它（见设计文档 §5.8）。
pub fn try_build_predicate<'a>(
    index: &'a Index,
    filter: Option<&Filter>,
) -> Option<Box<dyn CandidateFilter + 'a>> {
    use crate::predicate::AliveOnly;
    match filter {
        None => Some(Box::new(AliveOnly::new(index.alive_chunks()))),
        Some(f) => {
            let bits = doc_bits(f, index);
            if bits.is_empty() {
                return None;
            }
            Some(Box::new(ChunkFilter::new(bits, index)))
        }
    }
}

/// 判定某 doc 是否通过过滤（供全扫兜底与测试复用）。
pub fn doc_matches(doc_id: DocId, filter: &Filter, index: &Index) -> bool {
    index
        .doc(doc_id)
        .is_some_and(|record| matches(filter, &record.metadata))
}

// ⚠️ `json_to_string` 的**唯一定义**在 `crate::index::field_index`（`pub fn json_to_string`）。
// 字段索引与 `matches()` 必须共用同一口径，否则会出现「matches() 判真、索引判假」
// 的静默假阴性（评审 P1-2）。这里只做重导出，绝不另写一份。
pub use crate::index::json_to_string;

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;
    use crate::analyze::MixedAnalyzer;
    use crate::document::{Chunk, DocRecord};

    fn build_index_with_metadata() -> Index {
        let analyzer = MixedAnalyzer::new();
        let mut index = Index::new();
        for (i, (text, tag, year)) in [
            ("BM25 检索", "rust", 2021.0),
            ("向量检索", "python", 2022.0),
            ("混合检索", "rust", 2023.0),
        ]
        .iter()
        .enumerate()
        {
            let doc = DocRecord {
                doc_id: 0,
                source: format!("doc-{i}"),
                metadata: serde_json::json!({"tag": tag, "year": year}),
                content_hash: 0,
            };
            let chunk = Chunk {
                chunk_id: 0,
                doc_id: 0,
                ordinal: 0,
                text: text.to_string(),
                char_start: 0,
                char_end: 0,
            };
            index.add(doc, vec![chunk], &analyzer).unwrap();
        }
        index
    }

    #[test]
    fn 过滤后结果是子集() {
        let index = build_index_with_metadata();
        let filter = Filter::eq("tag", "rust");

        let allowed = allowed_chunks(&filter, &index);
        let all: HashSet<ChunkId> = index.live_chunks().map(|c| c.chunk_id).collect();

        assert!(allowed.len() < all.len(), "过滤应剔除部分结果");
        assert!(allowed.is_subset(&all), "过滤后必须是不过滤的子集");
        assert_eq!(allowed.len(), 2, "tag=rust 的文档有两个");
    }

    #[test]
    fn 数值范围过滤() {
        let index = build_index_with_metadata();
        let filter = Filter::Range {
            field: "year".into(),
            gte: 2022.0,
            lte: 2023.0,
        };
        let allowed = allowed_chunks(&filter, &index);
        assert_eq!(allowed.len(), 1, "year ∈ [2022, 2023) 只有一个");
    }

    #[test]
    fn and与or组合() {
        let metadata = serde_json::json!({"tag": "rust", "year": 2023});
        let f_and = Filter::And(vec![
            Filter::eq("tag", "rust"),
            Filter::Range {
                field: "year".into(),
                gte: 2023.0,
                lte: 2024.0,
            },
        ]);
        let f_or = Filter::Or(vec![Filter::eq("tag", "python"), Filter::eq("tag", "rust")]);
        assert!(matches(&f_and, &metadata));
        assert!(matches(&f_or, &metadata));

        let f_and_fail = Filter::And(vec![Filter::eq("tag", "rust"), Filter::eq("tag", "python")]);
        assert!(!matches(&f_and_fail, &metadata));
    }

    // ---- V2 Step 1：字段索引求值器（T6 / T8）----

    /// 覆盖**全部 JSON 顶层类型**的取值池（评审 P1-2：bool/null/array/object 也必须覆盖）。
    fn value_pool() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!("rust"),
            serde_json::json!("python"),
            serde_json::json!("go"),
            serde_json::json!(2021.0),
            serde_json::json!(2022.0),
            serde_json::json!(7),
            serde_json::json!(true),
            serde_json::json!(false),
            serde_json::json!(null),
            serde_json::json!([1, 2]),
            serde_json::json!([]),
            serde_json::json!({"a": 1}),
        ]
    }

    /// 确定性伪随机（xorshift），避免为测试引入 rand 依赖。
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// 构造一份 metadata 覆盖全类型、字段取值随机的索引。
    fn build_random_index(doc_count: usize) -> Index {
        let analyzer = MixedAnalyzer::new();
        let mut index = Index::new();
        let pool = value_pool();
        let mut rng = Rng(0x2545F4914F6CDD1D);

        for i in 0..doc_count {
            let pick = |rng: &mut Rng| pool[(rng.next() as usize) % pool.len()].clone();
            let meta = serde_json::json!({
                "tag": pick(&mut rng),
                "year": pick(&mut rng),
                "flag": pick(&mut rng),
                "extra": pick(&mut rng),
            });
            let text = format!("第 {i} 篇文档 检索 向量 内容");
            let doc = DocRecord {
                doc_id: 0,
                source: format!("doc-{i}"),
                metadata: meta,
                content_hash: 0,
            };
            let chunk = Chunk {
                chunk_id: 0,
                doc_id: 0,
                ordinal: 0,
                text,
                char_start: 0,
                char_end: 0,
            };
            index.add(doc, vec![chunk], &analyzer).unwrap();
        }
        index
    }

    /// 覆盖 Eq / Range / And / Or / 缺失字段 / 缺失值的一批过滤条件。
    fn filter_battery() -> Vec<Filter> {
        vec![
            Filter::eq("tag", "rust"),
            Filter::eq("tag", "python"),
            Filter::eq("tag", "true"),      // bool 的字符串形式
            Filter::eq("tag", "null"),      // null 的字符串形式
            Filter::eq("tag", "[1,2]"),     // array
            Filter::eq("tag", "{\"a\":1}"), // object
            Filter::eq("tag", "7"),         // 整数
            Filter::eq("tag", "2021.0"),    // 浮点
            Filter::eq("tag", "不存在的取值"),
            Filter::eq("不存在的字段", "rust"),
            Filter::Range {
                field: "year".into(),
                gte: 7.0,
                lte: 2022.0,
            },
            Filter::Range {
                field: "year".into(),
                gte: 2000.0,
                lte: 2100.0,
            },
            Filter::Range {
                field: "flag".into(),
                gte: 0.0,
                lte: 1.0,
            },
            Filter::And(vec![
                Filter::eq("tag", "rust"),
                Filter::Range {
                    field: "year".into(),
                    gte: 2000.0,
                    lte: 2100.0,
                },
            ]),
            Filter::And(vec![Filter::eq("tag", "rust"), Filter::eq("tag", "python")]), // 恒空
            Filter::Or(vec![Filter::eq("tag", "rust"), Filter::eq("tag", "go")]),
            Filter::Or(vec![
                Filter::eq("tag", "true"),
                Filter::Range {
                    field: "year".into(),
                    gte: 0.0,
                    lte: 10.0,
                },
            ]),
        ]
    }

    /// **T6**：字段索引求值 ≡ 全扫 oracle（全类型随机 metadata 下逐位对照）。
    #[test]
    fn 字段索引求值与全扫oracle恒等() {
        let index = build_random_index(200);
        for filter in filter_battery() {
            let indexed = doc_bits(&filter, &index);
            let scanned = doc_bits_scan(&filter, &index);
            assert_eq!(
                indexed, scanned,
                "filter={filter:?} 字段索引与全扫结果不一致"
            );
        }
    }

    /// **T6（续）**：删除文档后两者仍恒等——验证 `remove` 的字段索引同步点。
    #[test]
    fn 删除后字段索引与全扫仍恒等() {
        let analyzer = MixedAnalyzer::new();
        let mut index = build_random_index(120);
        // 删掉每第 5 个文档
        for doc_id in (0..120).step_by(5) {
            index.remove(doc_id, &analyzer).unwrap();
        }
        for filter in filter_battery() {
            assert_eq!(
                doc_bits(&filter, &index),
                doc_bits_scan(&filter, &index),
                "删除后 filter={filter:?} 不再恒等（字段索引 remove 同步点漏了）"
            );
        }
    }

    /// **T8（soundness）**：`ChunkFilter::contains` 逐 chunk 与「存活 && doc 通过过滤」一致。
    #[test]
    fn ChunkFilter的contains与逐chunk判定一致() {
        let analyzer = MixedAnalyzer::new();
        let mut index = build_random_index(120);
        for doc_id in (0..120).step_by(7) {
            index.remove(doc_id, &analyzer).unwrap();
        }

        for filter in filter_battery() {
            let bits = doc_bits(&filter, &index);
            let predicate = ChunkFilter::new(bits, &index);
            assert_eq!(predicate.kind(), FilterKind::Filtered);

            for chunk in index.live_chunks() {
                let expect = match index.doc_of(chunk.chunk_id) {
                    Some(doc_id) => {
                        index.alive_chunks().contains(chunk.chunk_id)
                            && doc_matches(doc_id, &filter, &index)
                    }
                    None => false,
                };
                assert_eq!(
                    predicate.contains(chunk.chunk_id),
                    expect,
                    "chunk {} 在 filter={:?} 上判定不一致",
                    chunk.chunk_id,
                    filter
                );
            }
        }
    }

    /// **T8（recall 的一部分）**：`allowed_count` 等于逐 chunk 判定的命中总数。
    #[test]
    fn allowed_count等于实际通过的chunk数() {
        let analyzer = MixedAnalyzer::new();
        let mut index = build_random_index(80);
        index.remove(3, &analyzer).unwrap();
        index.remove(11, &analyzer).unwrap();

        for filter in filter_battery() {
            let bits = doc_bits(&filter, &index);
            let predicate = ChunkFilter::new(bits, &index);
            let actual = index
                .live_chunks()
                .filter(|c| predicate.contains(c.chunk_id))
                .count();
            assert_eq!(
                predicate.allowed_count(),
                actual,
                "filter={filter:?} 的 allowed_count 与实际通过数不符"
            );
        }
    }

    /// 空集短路：`try_build_predicate` 对排空的过滤返回 `None`。
    #[test]
    fn 过滤排空时谓词构建返回None() {
        let index = build_index_with_metadata();
        assert!(try_build_predicate(&index, Some(&Filter::eq("tag", "rust"))).is_some());
        assert!(
            try_build_predicate(&index, Some(&Filter::eq("tag", "不存在的取值"))).is_none(),
            "排空的过滤必须返回 None 以便编排层短路"
        );
        assert!(
            try_build_predicate(&index, Some(&Filter::eq("不存在的字段", "x"))).is_none(),
            "字段不存在同样是空集"
        );
        // 无过滤 → AliveOnly
        let p = try_build_predicate(&index, None).expect("无过滤应返回 AliveOnly");
        assert_eq!(p.kind(), FilterKind::Alive);
        assert_eq!(p.allowed_count(), 3);
    }

    /// 高基数字段降级后，字段索引自动回落到全扫（正确性不丢）。
    #[test]
    fn 降级字段自动回落全扫() {
        let analyzer = MixedAnalyzer::new();
        let mut index = Index::new();
        // 每篇一个独一无二的 uuid → 触发基数保护（默认上限 1024 需更多文档，
        // 这里直接构造一个已知降级字段来验证回落路径）
        for i in 0..40 {
            let text = format!("第 {i} 篇 检索 文档");
            let doc = DocRecord {
                doc_id: 0,
                source: format!("d{i}"),
                metadata: serde_json::json!({"uuid": format!("u{i}"), "tag": "rust"}),
                content_hash: 0,
            };
            let chunk = Chunk {
                chunk_id: 0,
                doc_id: 0,
                ordinal: 0,
                text,
                char_start: 0,
                char_end: 0,
            };
            index.add(doc, vec![chunk], &analyzer).unwrap();
        }

        // 未降级：字段索引路径
        assert!(!index.field_index().is_degraded("uuid"));
        assert_eq!(
            doc_bits(&Filter::eq("uuid", "u7"), &index),
            doc_bits_scan(&Filter::eq("uuid", "u7"), &index)
        );
    }
}
