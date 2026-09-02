//! Token 过滤器链。
//!
//! 顺序固定为：NFKC 归一化 → 小写化 → 停用词过滤 → 长度过滤。
//! 归一化与小写化在**分词前**对原始文本做（见 `MixedAnalyzer`），
//! 停用词与长度过滤在**分词后**逐 token 做。

use unicode_normalization::UnicodeNormalization;

use super::stopwords::is_stopword;

/// token 长度上限（字符数）。超过的丢弃，防异常长词污染索引。
const MAX_TERM_CHARS: usize = 32;

/// 分词后是否保留该 token。
///
/// 返回 `None` 表示被过滤（停用词 / 超长 / 空串），
/// `Some(term)` 表示保留并已做小写化。
pub fn filter_token(raw: &str) -> Option<String> {
    let term = raw.to_lowercase();
    if term.is_empty() || is_stopword(&term) {
        return None;
    }
    if term.chars().count() > MAX_TERM_CHARS {
        return None;
    }
    Some(term)
}

/// 对整段文本做 NFKC 归一化。
///
/// NFKC 会把全角字母/数字、全角标点兼容字符折叠为等价形式，
/// 例如全角 "Ａ" → "A"、全角 "，" → 半角。这能保证索引侧与查询侧
/// 对"看起来不同、实质相同"的文本得到同一 term（架构文档 4.2 / R4）。
pub fn normalize(text: &str) -> String {
    text.nfkc().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 停用词与超长被过滤() {
        assert_eq!(filter_token("的"), None);
        assert_eq!(filter_token("the"), None);
        assert_eq!(filter_token(""), None);
        assert_eq!(filter_token(&"长".repeat(40)), None);
    }

    #[test]
    fn 正常词保留并小写() {
        assert_eq!(filter_token("Rust"), Some("rust".to_string()));
        assert_eq!(filter_token("BM25"), Some("bm25".to_string()));
        assert_eq!(filter_token("检索"), Some("检索".to_string()));
    }

    #[test]
    fn nfkc_折叠全角() {
        assert_eq!(normalize("ＡＢＣ"), "ABC");
        assert_eq!(normalize("１２３"), "123");
    }
}
