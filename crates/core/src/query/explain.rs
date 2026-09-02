//! 可解释性辅助：matched_terms 计算与空结果原因判定。

use std::collections::HashSet;

use crate::analyze::Analyzer;

use super::response::EmptyReason;

/// 计算 query 分词中**真正命中该分片**的词（BM25 的 matched_terms）。
///
/// 向量路无词项，`matched_terms` 恒为空；该函数只服务 BM25 路与 Hybrid 的 explain。
pub fn matched_terms(analyzer: &dyn Analyzer, query: &str, chunk_text: &str) -> Vec<String> {
    let chunk_terms: HashSet<String> = analyzer
        .analyze_doc(chunk_text)
        .into_iter()
        .map(|t| t.term.to_string())
        .collect();

    let mut out: Vec<String> = analyzer
        .analyze_query(query)
        .into_iter()
        .filter(|t| chunk_terms.contains(t.term.as_str()))
        .map(|t| t.term.to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 判定空结果原因（FR-13）。
///
/// - `index_is_empty`：索引无分片 → `NoDocuments`
/// - `query_is_empty`：查询分词后为空（全停用词）→ `AllTermsUnmatched`
/// - `candidates == 0`：两路都无候选 → `AllTermsUnmatched`
/// - 否则 `None`（有结果）
pub fn determine_empty_reason(
    index_is_empty: bool,
    query_is_empty: bool,
    candidates: usize,
) -> Option<EmptyReason> {
    if index_is_empty {
        Some(EmptyReason::NoDocuments)
    } else if query_is_empty || candidates == 0 {
        Some(EmptyReason::AllTermsUnmatched)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::MixedAnalyzer;

    #[test]
    fn 空结果原因判定() {
        assert_eq!(
            determine_empty_reason(true, false, 0),
            Some(EmptyReason::NoDocuments)
        );
        assert_eq!(
            determine_empty_reason(false, true, 0),
            Some(EmptyReason::AllTermsUnmatched)
        );
        assert_eq!(
            determine_empty_reason(false, false, 0),
            Some(EmptyReason::AllTermsUnmatched)
        );
        assert_eq!(determine_empty_reason(false, false, 3), None);
    }

    #[test]
    fn matched_terms只含命中词() {
        let a = MixedAnalyzer::new();
        let got = matched_terms(&a, "BM25 参数", "BM25 是一种检索算法，参数 k1 控制词频");
        assert!(got.contains(&"bm25".to_string()));
        assert!(got.contains(&"参数".to_string()));
        assert!(!got.contains(&"检索".to_string())); // query 里没有"检索"
    }
}
