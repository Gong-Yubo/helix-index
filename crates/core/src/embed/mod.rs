//! 文本向量化：Embedder trait + 本地 fastembed / 远程 HTTP 两个实现。
//!
//! 禁止：不知道索引的存在；embed_query 与 embed_documents 必须分开
//! （BGE 查询侧需加 instruction 前缀，风险 R2）。
//!
//! 实现阶段：P2 T2-01~T2-08
