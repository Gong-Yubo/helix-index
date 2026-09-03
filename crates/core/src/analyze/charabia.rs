//! T5-09 分词对照实验：charabia 0.10（chinese 裁剪版）Analyzer 实现。
//!
//! 定位（p5-design.md 第 9 章）：charabia 中文分词底层即 jieba-rs，
//! 本对照的实质是「charabia 规范化管道（ NFC + 分类 + 归一化链）vs
//! 自研过滤链（normalize + segment + jieba + filter_token）」。
//! 三步验证（依赖树无 lindera / MSRV 1.90 / cargo-deny 全绿）已通过，
//! 以 feature `charabia` 隔离，不进默认特性。
//!
//! 对照实验必须**索引/查询两侧同时切换**（R4：索引与查询侧 Analyzer
//! 不一致是最严重的正确性事故源）。

use crate::analyze::{Analyzer, Token};
use charabia::TokenKind;
use smol_str::SmolStr;

/// charabia 分词器封装。`TokenizerBuilder` 生产的 `Tokenizer` 静态期
/// 初始化代价高（加载 jieba 词典），用 `OnceLock` 全局单例。
pub struct CharabiaAnalyzer;

static TOKENIZER: std::sync::OnceLock<charabia::Tokenizer<'static>> = std::sync::OnceLock::new();

fn tokenizer() -> &'static charabia::Tokenizer<'static> {
    TOKENIZER.get_or_init(|| {
        // chinese 裁剪 feature 下 build：jieba 分段 + 默认归一化链
        //（小写化、NFC 等）。不配 stop_words / 自定义 separators，
        // 与 MixedAnalyzer 的对照才是"纯管道差异"。
        charabia::TokenizerBuilder::default().into_tokenizer()
    })
}

impl CharabiaAnalyzer {
    pub fn new() -> Self {
        // 提前触发词典加载，避免首次查询计入延迟
        let _ = tokenizer();
        Self
    }
}

impl Default for CharabiaAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for CharabiaAnalyzer {
    fn analyze_doc(&self, text: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut position = 0u32;

        for tok in tokenizer().tokenize(text) {
            // 只保留 Word 类 token：Separator（含中英间空白/标点）不进倒排。
            // 与自研链一致（segment 后标点天然不在段内，filter_token 再滤一道）。
            if !matches!(tok.kind, TokenKind::Word) {
                continue;
            }
            let term = tok.lemma();
            // 与自研 filter_token 同口径的兜底过滤：空白、单字符 CJK、
            // 过长词。charabia 归一化链不做停用词与长度过滤（那是
            // Meilisearch 在上层做的），为公平对照由本侧补齐。
            let term = term.trim();
            if term.is_empty() {
                continue;
            }
            if term.chars().count() == 1 && is_cjk(term) {
                continue;
            }
            if term.chars().count() > 32 {
                continue;
            }
            tokens.push(Token {
                term: SmolStr::new(term),
                position,
                byte_start: tok.byte_start,
                byte_end: tok.byte_end,
            });
            position += 1;
        }
        tokens
    }
}

fn is_cjk(s: &str) -> bool {
    s.chars().all(|c| {
        matches!(c,
            '\u{4E00}'..='\u{9FFF}'
                | '\u{3400}'..='\u{4DBF}'
                | '\u{F900}'..='\u{FAFF}'
                | '\u{3040}'..='\u{30FF}')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 中文基本分词() {
        let a = CharabiaAnalyzer::new();
        let toks = a.analyze_doc("小明硕士毕业于中国科学院计算所");
        let terms: Vec<&str> = toks.iter().map(|t| t.term.as_ref()).collect();
        // charabia kvariants 归一化：简体 → 繁体变体（碩士/計算），
        // 单字 CJK 保留（小/明）。索引/查询两侧同变换，检索语义不受影响。
        assert!(
            terms.contains(&"碩士") && terms.contains(&"計算"),
            "terms = {terms:?}"
        );
    }

    #[test]
    fn 中英混排() {
        let a = CharabiaAnalyzer::new();
        let toks = a.analyze_doc("使用 Rust 编写检索引擎");
        let terms: Vec<&str> = toks.iter().map(|t| t.term.as_ref()).collect();
        assert!(terms.contains(&"rust"), "应小写化: {terms:?}");
        assert!(terms.contains(&"檢索"), "kvariants 变体: {terms:?}");
    }

    #[test]
    fn 两侧一致() {
        let a = CharabiaAnalyzer::new();
        let d = a.analyze_doc("向量检索与 BM25 融合");
        let q = a.analyze_query("向量检索与 BM25 融合");
        assert_eq!(d.len(), q.len());
    }
}
