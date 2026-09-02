//! 中英分段：把原始文本切成「中文段」与「拉丁段」。
//!
//! 用 `unicode-segmentation` 而不是手写 Unicode 属性判断——全角符号、假名、
//! emoji、组合字符的边界自己判极易出错，一个轻量纯 Rust 依赖换正确性（见 thirdparty.md 4.2）。

use unicode_segmentation::UnicodeSegmentation;

/// 片段类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    /// 中文/日文/韩文等表意文字段，交给 jieba 分词
    Cjk,
    /// 拉丁字母与数字段，按正则切词
    Latin,
}

/// 一个同构片段
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub kind: SegmentKind,
}

/// 判断字形簇是否属于表意文字（CJK 统一汉字 + 扩展区 + 兼容区 + 假名/谚文）
fn is_cjk(g: &str) -> bool {
    g.chars().next().is_some_and(|c| {
        // CJK 统一汉字及扩展 A-F
        ('\u{3400}'..='\u{4DBF}').contains(&c)
            || ('\u{4E00}'..='\u{9FFF}').contains(&c)
            || ('\u{F900}'..='\u{FAFF}').contains(&c)
            || ('\u{20000}'..='\u{2A6DF}').contains(&c)
            || ('\u{2A700}'..='\u{2EBEF}').contains(&c)
            // 平假名 / 片假名 / 谚文音节
            || ('\u{3040}'..='\u{30FF}').contains(&c)
            || ('\u{AC00}'..='\u{D7AF}').contains(&c)
    })
}

/// 判断字形簇是否为拉丁字母或数字（含希腊/西里尔，避免把俄文片段丢掉）
fn is_alnum(g: &str) -> bool {
    g.chars().next().is_some_and(|c| c.is_alphanumeric())
}

/// 把文本切成同构片段序列。**非字母非汉字的字符（标点、空白、符号）被丢弃**，
/// 只作为分段边界使用——它们对 BM25 无检索价值，留在索引里只会增大 df。
pub fn segment(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut kind: Option<SegmentKind> = None;

    for g in text.graphemes(true) {
        let gk = if is_cjk(g) {
            Some(SegmentKind::Cjk)
        } else if is_alnum(g) {
            Some(SegmentKind::Latin)
        } else {
            None
        };

        match (gk, kind) {
            (Some(k), Some(cur)) if k == cur => buf.push_str(g),
            (Some(k), _) => {
                if let Some(cur) = kind.take() {
                    if !buf.is_empty() {
                        out.push(Segment {
                            text: std::mem::take(&mut buf),
                            kind: cur,
                        });
                    }
                }
                kind = Some(k);
                buf.push_str(g);
            }
            (None, _) => {
                if let Some(cur) = kind.take() {
                    if !buf.is_empty() {
                        out.push(Segment {
                            text: std::mem::take(&mut buf),
                            kind: cur,
                        });
                    }
                }
            }
        }
    }

    if let Some(cur) = kind {
        if !buf.is_empty() {
            out.push(Segment {
                text: buf,
                kind: cur,
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 中英混排分段() {
        let segs = segment("Rust 实现 BM25 检索");
        let kinds: Vec<_> = segs.iter().map(|s| (s.text.as_str(), s.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                ("Rust", SegmentKind::Latin),
                ("实现", SegmentKind::Cjk),
                ("BM25", SegmentKind::Latin),
                ("检索", SegmentKind::Cjk),
            ]
        );
    }

    #[test]
    fn 标点与空白被丢弃() {
        let segs = segment("你好，世界！  Hello, World!!!");
        assert!(segs
            .iter()
            .all(|s| !s.text.contains(['，', '！', ',', '!', ' '])));
        assert_eq!(segs.len(), 4);
    }

    #[test]
    fn 全角符号不产生异常片段() {
        // 全角逗号、全角字母、emoji 都不得混入词项
        let segs = segment("向量（embedding）检索🚀 很常用");
        for s in &segs {
            assert!(!s.text.contains('（') && !s.text.contains('）'));
            assert!(!s.text.contains('🚀'));
        }
        // 至少应包含「向量」「检索」「很常用」三个中文段与一个 embedding 拉丁段
        assert!(segs
            .iter()
            .any(|s| s.text == "embedding" && s.kind == SegmentKind::Latin));
        assert!(segs.iter().any(|s| s.text == "向量"));
    }

    #[test]
    fn 空文本与纯符号() {
        assert!(segment("").is_empty());
        assert!(segment("，。！？？").is_empty());
    }
}
