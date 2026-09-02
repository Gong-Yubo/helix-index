//! 停用词表（中英文合并，**保守裁剪**）。
//!
//! 取舍原则：只去掉真正无检索价值的虚词。**单字词（云/网/库）一律保留**——
//! 它们在中文技术语境里常是关键术语。宁可多留几个词让 idf 自然抑制，
//! 也不要因为过度裁剪导致某些查询零召回（FR-04 的防空召回要求）。

/// 判断是否为停用词
pub fn is_stopword(term: &str) -> bool {
    if term.is_empty() {
        return true;
    }
    // 英文按小写比对，避免调用方漏做小写化时静默失效
    let lower = term.to_ascii_lowercase();
    if EN.contains(&lower.as_str()) {
        return true;
    }
    // 中文停用词多为 1~2 字，直接比对
    ZH.contains(&term)
}

/// 英文停用词（小写后比对）
const EN: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "has", "have", "how",
    "i", "in", "is", "it", "its", "of", "on", "or", "that", "the", "this", "to", "was", "were",
    "what", "when", "where", "which", "who", "why", "will", "with", "you", "your", "we", "our",
    "can", "could", "do", "does", "did", "not", "no", "yes", "if", "then", "than", "so", "such",
    "about", "into", "over", "under", "again", "more", "most", "some", "any", "all", "very",
];

/// 中文停用词（保守：只列高频虚词）
const ZH: &[&str] = &[
    "的",
    "了",
    "和",
    "与",
    "或",
    "在",
    "是",
    "有",
    "也",
    "就",
    "都",
    "而",
    "及",
    "等",
    "着",
    "吗",
    "呢",
    "吧",
    "啊",
    "把",
    "被",
    "给",
    "让",
    "对",
    "从",
    "到",
    "为",
    "以",
    "之",
    "其",
    "这",
    "那",
    "我",
    "你",
    "他",
    "她",
    "它",
    "们",
    "个",
    "上",
    "下",
    "中",
    "里",
    "后",
    "前",
    "时",
    "一个",
    "什么",
    "怎么",
    "如何",
    "为什么",
    "可以",
    "因为",
    "所以",
    "但是",
    "如果",
    "通过",
    "进行",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 虚词被过滤() {
        assert!(is_stopword("的"));
        assert!(is_stopword("the"));
        assert!(is_stopword("The")); // 调用方需先小写化
        assert!(is_stopword(""));
    }

    #[test]
    fn 单字术语保留() {
        // 「云 / 网 / 库」是中文技术语境的关键术语，绝不能过滤
        for t in ["云", "网", "库", "图", "栈", "树"] {
            assert!(!is_stopword(t), "'{t}' 不应是停用词");
        }
    }

    #[test]
    fn 实词保留() {
        for t in ["检索", "向量", "分词", "embedding", "bm25", "rust"] {
            assert!(!is_stopword(t), "'{t}' 不应是停用词");
        }
    }
}
