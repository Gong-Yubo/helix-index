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

use std::collections::HashSet;

use crate::index::Index;
use crate::schema::Filter;
use crate::types::ChunkId;

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

fn json_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

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
}
