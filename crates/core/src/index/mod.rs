//! 倒排索引、正排存储与统计量维护。
//!
//! # 边界（架构文档 4.2）
//!
//! 本模块**不碰融合与排序**，只负责"把词项映射到分片"这件事。
//! 统计量（`total_len` / `num_chunks`）必须增量维护，且与全量重建一致（风险 R5）。
//!
//! # 设计取舍
//!
//! - `Index` **不持有 `Analyzer`**：它是纯数据结构；`add` / `remove` 由调用方
//!   传入 `&dyn Analyzer`，分词策略与索引解耦。
//! - `df` 不单独存，由 `postings.len()` 推导（见 `inverted.rs`），杜绝漂移。

mod forward;
mod inverted;
mod posting;
mod stats;

pub use forward::ForwardStore;
pub use inverted::InvertedIndex;
pub use posting::Posting;
pub use stats::Stats;

use crate::analyze::{Analyzer, Token};
use crate::document::{Chunk, Document};
use crate::error::Result;
use crate::types::{ChunkId, DocId};

/// 内存索引：倒排 + 正排 + 统计量。
#[derive(Debug, Default)]
pub struct Index {
    inverted: InvertedIndex,
    forward: ForwardStore,
    stats: Stats,
    /// 每个分片的 term 数（dl），按 `ChunkId` 索引；删除后保留（ID 不复用）
    chunk_lens: Vec<u32>,
}

impl Index {
    pub fn new() -> Self {
        Self::default()
    }

    /// 摄入一个文档及其分片，返回 `(doc_id, chunk_ids)`。
    ///
    /// - 幂等去重（content_hash）是 **P4 / T4-04** 的职责，此处不做
    /// - 每个分片独立分词、独立维护 postings 与统计量
    pub fn add(
        &mut self,
        doc: Document,
        chunks: Vec<Chunk>,
        analyzer: &dyn Analyzer,
    ) -> Result<(DocId, Vec<ChunkId>)> {
        let doc_id = self.forward.insert_doc(doc);
        let mut chunk_ids = Vec::with_capacity(chunks.len());

        for mut chunk in chunks {
            chunk.doc_id = doc_id;
            let tokens = analyzer.analyze_doc(&chunk.text);

            // 聚合词频 + 位置
            let mut tf: Vec<(&str, u32, Vec<u32>)> = Vec::new();
            for tok in &tokens {
                if let Some(entry) = tf.iter_mut().find(|e| e.0 == tok.term.as_str()) {
                    entry.1 += 1;
                    entry.2.push(tok.position);
                } else {
                    tf.push((tok.term.as_str(), 1, vec![tok.position]));
                }
            }

            let chunk_id = self.forward.insert_chunk(chunk);
            for (term, tfn, positions) in &tf {
                self.inverted.add(term, chunk_id, *tfn, positions);
            }

            self.chunk_lens.push(tokens.len() as u32);
            self.stats.total_len += tokens.len() as u64;
            self.stats.num_chunks += 1;
            chunk_ids.push(chunk_id);
        }

        Ok((doc_id, chunk_ids))
    }

    /// 删除一个文档（及其全部分片），含统计量回滚。
    ///
    /// 回滚方式是**重新分析分片原文**（与全量重建用同一 analyzer），
    /// 而非按存储值反推——这是防漂移的关键（架构文档 12.1 的"最关键测试"）。
    pub fn remove(&mut self, doc_id: DocId, analyzer: &dyn Analyzer) -> Result<()> {
        // 先取走该文档的分片 ID 列表（chunk_ids_of_doc 依赖 forward 中的存活分片）
        let chunk_ids = self.forward.chunk_ids_of_doc(doc_id);

        for chunk_id in chunk_ids {
            if let Some(chunk) = self.forward.chunk(chunk_id) {
                // 需要持有 text 的副本，因为随后会墓碑化 forward 里的 chunk
                let text = chunk.text.clone();
                let tokens = analyzer.analyze_doc(&text);

                // 回滚 postings
                let mut tf: Vec<(&str, u32)> = Vec::new();
                for tok in &tokens {
                    if let Some(e) = tf.iter_mut().find(|e| e.0 == tok.term.as_str()) {
                        e.1 += 1;
                    } else {
                        tf.push((tok.term.as_str(), 1));
                    }
                }
                for (term, _) in &tf {
                    self.inverted.remove(term, chunk_id);
                }

                // 回滚统计量
                self.stats.total_len -= tokens.len() as u64;
                self.stats.num_chunks -= 1;
            }
            self.forward.tombstone_chunk(chunk_id);
        }

        self.forward.tombstone_doc(doc_id);
        Ok(())
    }

    // ---- 查询接口（供 Retriever / 测试）----

    pub fn num_chunks(&self) -> u32 {
        self.stats.num_chunks
    }

    pub fn total_len(&self) -> u64 {
        self.stats.total_len
    }

    pub fn avgdl(&self) -> f32 {
        self.stats.avgdl()
    }

    pub fn doc_freq(&self, term: &str) -> u32 {
        self.inverted.doc_freq(term)
    }

    pub fn term_id(&self, term: &str) -> Option<crate::types::TermId> {
        self.inverted.term_id(term)
    }

    pub fn postings_by_id(&self, id: crate::types::TermId) -> &[Posting] {
        self.inverted.postings_by_id(id)
    }

    /// 某分片是否存活（未被墓碑化）。
    pub fn is_live_chunk(&self, chunk_id: ChunkId) -> bool {
        self.forward.chunk(chunk_id).is_some()
    }

    pub fn chunk(&self, chunk_id: ChunkId) -> Option<&Chunk> {
        self.forward.chunk(chunk_id)
    }

    pub fn doc(&self, doc_id: DocId) -> Option<&Document> {
        self.forward.doc(doc_id)
    }

    /// 某分片的 term 数（dl）。已删除分片返回 0（但调用方通常先判活）。
    pub fn chunk_len(&self, chunk_id: ChunkId) -> u32 {
        self.chunk_lens.get(chunk_id as usize).copied().unwrap_or(0)
    }

    /// 迭代所有活分片（确定性顺序，用于"全量重建"对照测试）。
    pub fn live_chunks(&self) -> impl Iterator<Item = &Chunk> {
        self.forward.iter_live_chunks()
    }
}

/// 把 token 序列聚合为 (term, tf, positions) 序列——供测试与 add 内部复用。
#[allow(dead_code)]
fn aggregate(tokens: &[Token]) -> Vec<(String, u32, Vec<u32>)> {
    let mut out: Vec<(String, u32, Vec<u32>)> = Vec::new();
    for tok in tokens {
        if let Some(e) = out.iter_mut().find(|e| e.0 == tok.term.as_str()) {
            e.1 += 1;
            e.2.push(tok.position);
        } else {
            out.push((tok.term.to_string(), 1, vec![tok.position]));
        }
    }
    out
}
