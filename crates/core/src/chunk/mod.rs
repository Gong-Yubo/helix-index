//! 文本分块：固定长度 + 重叠，优先按段落边界切。
//!
//! # 偏移约定（p1-design.md D1）
//!
//! - **长度按字符（char）计数**，符合"512 字"的直觉
//! - **偏移记录为字节（byte）下标**，`&text[start..end]` 可 O(1) 切片
//!
//! 两个单位不同是刻意的：分块决策用 char（对人直观），
//! 切片用 byte（对机器高效且不会在 UTF-8 边界上 panic）。

use crate::document::Chunk;
use crate::types::DocId;

/// 分块器
#[derive(Debug, Clone, Copy)]
pub struct Chunker {
    /// 每块的最大字符数（按 char 计）
    chunk_chars: usize,
    /// 相邻块重叠的字符数（按 char 计）
    overlap_chars: usize,
}

impl Chunker {
    /// `chunk_chars` 必须大于 `overlap_chars`。
    pub fn new(chunk_chars: usize, overlap_chars: usize) -> Self {
        assert!(
            chunk_chars > overlap_chars,
            "chunk_chars 必须大于 overlap_chars"
        );
        Self {
            chunk_chars,
            overlap_chars,
        }
    }

    /// 每块最大字符数（配置指纹用，p6-design 8.2）。
    pub fn chunk_chars(&self) -> usize {
        self.chunk_chars
    }

    /// 相邻块重叠字符数（配置指纹用，p6-design 8.2）。
    pub fn overlap_chars(&self) -> usize {
        self.overlap_chars
    }

    /// 把一段文本切成若干 `Chunk`。文本过短时退化为单块。
    pub fn chunk(&self, doc_id: DocId, text: &str) -> Vec<Chunk> {
        // 预计算每个 char 下标对应的 byte 偏移，O(1) 转 byte
        let byte_of: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .collect();
        let nchars = byte_of.len() - 1;
        let to_byte = |c: usize| byte_of[c.min(nchars)];

        // 段落边界：每个 "\n\n"（容忍 \r）之后的 char 下标
        let para_starts = paragraph_starts(text);

        let mut out = Vec::new();
        let mut ordinal = 0u32;
        let mut start_char = 0usize;

        while start_char < nchars {
            let desired_end = (start_char + self.chunk_chars).min(nchars);

            // 若尚未到文本末尾，尽量把切点回退到最近的段落边界，
            // 避免把一段话拦腰截断。取 (start_char, desired_end] 内**最靠后**的边界。
            let mut cut = desired_end;
            if desired_end < nchars {
                for &p in para_starts.iter().rev() {
                    if p > start_char && p <= desired_end {
                        cut = p;
                        break;
                    }
                }
            }

            let (bs, be) = (to_byte(start_char), to_byte(cut));
            let chunk_text = &text[bs..be];

            out.push(Chunk {
                chunk_id: 0, // 由 Index::add 回填
                doc_id,
                ordinal,
                text: chunk_text.to_string(),
                char_start: bs,
                char_end: be,
            });
            ordinal += 1;

            if cut >= nchars {
                break;
            }

            // 下一块起点：回退 overlap，且保证严格前进，防止死循环
            let next = cut.saturating_sub(self.overlap_chars);
            start_char = if next <= start_char { cut } else { next };
        }

        out
    }
}

impl Default for Chunker {
    fn default() -> Self {
        Self::new(512, 64)
    }
}

/// 返回每个段落首字符的 char 下标（在 "\n\n" 或 "\r\n\r\n" 之后）。
fn paragraph_starts(text: &str) -> Vec<usize> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let is_nl = c == '\n' || c == '\r';
        if is_nl {
            // 跳过连续的换行（\r\n 或 \n\n）
            let mut j = i;
            while j < chars.len() && (chars[j] == '\n' || chars[j] == '\r') {
                j += 1;
            }
            // 若跨越了至少两个换行符，则 j 处是段落起点
            if j - i >= 2 {
                out.push(j);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 短文本单块() {
        let c = Chunker::default();
        let chunks = c.chunk(0, "Hello 世界");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Hello 世界");
        assert_eq!(
            &"Hello 世界"[chunks[0].char_start..chunks[0].char_end],
            chunks[0].text
        );
    }

    #[test]
    fn 偏移可还原原文() {
        let c = Chunker::new(8, 2);
        let text = "甲乙丙丁戊己庚辛壬癸";
        let chunks = c.chunk(0, text);
        assert!(chunks.len() > 1, "应切成多块，实际 {}", chunks.len());
        for ch in &chunks {
            assert_eq!(&text[ch.char_start..ch.char_end], ch.text, "偏移错位");
        }
    }

    #[test]
    fn 重叠且不丢字() {
        let c = Chunker::new(8, 2);
        let text = "一二三四五六七八九十甲乙丙丁戊己庚辛壬癸";
        let chunks = c.chunk(0, text);
        assert!(chunks.len() > 2);
        // 首块开头 = 全文开头，末块结尾 = 全文结尾
        assert!(chunks[0].text.starts_with('一'));
        assert!(chunks.last().unwrap().text.ends_with('癸'));
        // 相邻块存在 overlap 字符
        for pair in chunks.windows(2) {
            let a: Vec<char> = pair[0].text.chars().collect();
            let b: Vec<char> = pair[1].text.chars().collect();
            let n = 2usize.min(a.len()).min(b.len());
            assert_eq!(&a[a.len() - n..], &b[..n], "相邻块重叠部分不一致");
        }
    }

    #[test]
    fn 段落边界优先不丢字() {
        let c = Chunker::new(20, 2);
        let text = "第一段内容。\n\n第二段内容。";
        let chunks = c.chunk(0, text);
        let joined: String = chunks.iter().map(|c| c.text.as_str()).collect();
        assert!(joined.contains("第一段"));
        assert!(joined.contains("第二段"));
        // 理想情况：段落边界成为切点，第二段不被截断
        assert!(chunks.iter().any(|c| c.text.contains("第二段内容。")));
    }

    #[test]
    fn 含emoji不panic且偏移正确() {
        let c = Chunker::new(5, 1);
        let text = "中文🚀测试😀内容";
        let chunks = c.chunk(0, text);
        assert!(!chunks.is_empty());
        for ch in &chunks {
            assert_eq!(&text[ch.char_start..ch.char_end], ch.text);
        }
    }

    #[test]
    fn 空文本() {
        let c = Chunker::default();
        assert!(c.chunk(0, "").is_empty());
    }
}
