//! 文本分析：中英混合分词、Token 过滤器链、停用词。
//!
//! 禁止：不知道索引的存在。Analyzer trait 的 analyze_query 默认转发 analyze_doc，
//! 强制索引侧与查询侧一致（风险 R4）。
//!
//! 实现阶段：P1 T1-04~T1-07
