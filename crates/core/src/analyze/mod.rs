//! 文本分析：中英混合分词、Token 过滤器链、停用词。
//!
//! # 边界（架构文档 4.2）
//!
//! 本模块**不知道索引的存在**——它只把文本变成 `Vec<Token>`，
//! 不关心这些 token 之后去向何方。
//!
//! # 一致性（风险 R4）
//!
//! `Analyzer::analyze_query` **默认转发** `analyze_doc`，从 API 层面杜绝
//! "索引侧和查询侧分词不一致"这个隐蔽 bug。未来若查询侧需要差异化
//! （同义词扩展、保留停用词），再单独 override `analyze_query`。

mod filter;
mod segment;
mod stopwords;

#[cfg(feature = "charabia")]
mod charabia;

#[cfg(feature = "charabia")]
pub use charabia::CharabiaAnalyzer;

pub use filter::{filter_token, normalize};
pub use segment::{segment, Segment, SegmentKind};
pub use stopwords::is_stopword;

use smol_str::SmolStr;

/// 归一化后的索引词
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// 词项本体（已小写化、去停用词、长度过滤后）
    pub term: SmolStr,
    /// 词序，供未来短语查询使用
    pub position: u32,
    /// 原文中的字节偏移（byte，非 char 索引），供精确引用与高亮（FR-12，P3 实现）
    pub byte_start: usize,
    /// 同上，结束偏移（不含）
    pub byte_end: usize,
}

/// 文本分析器抽象。
///
/// 索引侧与查询侧**必须共用同一实现**。
pub trait Analyzer: Send + Sync {
    /// 入库侧分词
    fn analyze_doc(&self, text: &str) -> Vec<Token>;
    /// 查询侧分词。默认转发 `analyze_doc`，强制两侧一致。
    fn analyze_query(&self, text: &str) -> Vec<Token> {
        self.analyze_doc(text)
    }
}

/// 中英混合分析器：NFKC 归一化 → 中英分段 → 中文 jieba / 拉丁切词 → 过滤链。
pub struct MixedAnalyzer {
    jieba: jieba_rs::Jieba,
}

impl MixedAnalyzer {
    pub fn new() -> Self {
        Self {
            jieba: jieba_rs::Jieba::new(),
        }
    }

    /// 核心分词逻辑：`analyze_doc` 与 `analyze_query` 共用。
    ///
    /// 说明：`byte_start` / `byte_end` 当前置 0——BM25 路径不消费它们，
    /// 精确的原文偏移留到 P3（FR-12 的 explain / 高亮）再实现，届时需要
    /// 分段时携带"归一化文本 → 原文"的映射。该取舍见 `p1-design.md` D1。
    fn analyze(&self, text: &str) -> Vec<Token> {
        let normalized = normalize(text);

        let mut tokens = Vec::new();
        let mut position = 0u32;

        for seg in segment(&normalized) {
            match seg.kind {
                SegmentKind::Cjk => {
                    for tok in self.jieba.cut(&seg.text, false) {
                        let w = tok.word.trim();
                        if let Some(term) = filter_token(w) {
                            tokens.push(Token {
                                term: SmolStr::new(term),
                                position,
                                byte_start: 0,
                                byte_end: 0,
                            });
                            position += 1;
                        }
                    }
                }
                SegmentKind::Latin => {
                    // 拉丁段内部按非字母数字切词（已由 segment 保证段内同构，
                    // 但数字与字母之间、下划线等仍需再切，这里兜底）
                    for word in seg.text.split(|c: char| !c.is_alphanumeric()) {
                        if word.is_empty() {
                            continue;
                        }
                        if let Some(term) = filter_token(word) {
                            tokens.push(Token {
                                term: SmolStr::new(term),
                                position,
                                byte_start: 0,
                                byte_end: 0,
                            });
                            position += 1;
                        }
                    }
                }
            }
        }

        tokens
    }
}

impl Default for MixedAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for MixedAnalyzer {
    fn analyze_doc(&self, text: &str) -> Vec<Token> {
        self.analyze(text)
    }
    // analyze_query 走默认实现，与 analyze_doc 完全一致。
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(a: &dyn Analyzer, t: &str) -> Vec<String> {
        a.analyze_doc(t)
            .into_iter()
            .map(|x| x.term.to_string())
            .collect()
    }

    #[test]
    fn 中英混排分词() {
        let a = MixedAnalyzer::new();
        let got = terms(&a, "Rust 实现 BM25 检索");
        assert_eq!(got, vec!["rust", "实现", "bm25", "检索"]);
    }

    #[test]
    fn 索引侧与查询侧一致() {
        let a = MixedAnalyzer::new();
        for text in ["Rust 实现 BM25", "向量检索", "Hello World 你好"] {
            let doc = a.analyze_doc(text);
            let query = a.analyze_query(text);
            assert_eq!(doc, query, "两侧分词不一致: {text}");
        }
    }

    #[test]
    fn 停用词被过滤() {
        let a = MixedAnalyzer::new();
        let got = terms(&a, "如何在 Rust 中实现 BM25");
        assert!(!got.contains(&"在".to_string()));
        assert!(!got.contains(&"的".to_string()));
        assert!(!got.contains(&"如何".to_string()));
    }

    #[test]
    fn 全角折叠() {
        let a = MixedAnalyzer::new();
        assert_eq!(terms(&a, "ＡＢＣ１２３"), vec!["abc123".to_string()]);
    }

    #[test]
    fn 空与纯符号() {
        let a = MixedAnalyzer::new();
        assert!(terms(&a, "").is_empty());
        assert!(terms(&a, "，。！？").is_empty());
    }
}
