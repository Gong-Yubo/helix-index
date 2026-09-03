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

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::analyze::{Analyzer, Token};
use crate::document::{Chunk, Document};
use crate::error::Result;
use crate::types::{ChunkId, DocId, TermId};

/// 内存索引：倒排 + 正排 + 统计量。
#[derive(Debug, Default)]
pub struct Index {
    inverted: InvertedIndex,
    forward: ForwardStore,
    stats: Stats,
    /// 每个分片的 term 数（dl），按 `ChunkId` 索引；删除后保留（ID 不复用）
    chunk_lens: Vec<u32>,
    /// content_hash → doc_id，幂等 upsert 去重用（FR-15）
    content_hashes: HashMap<u64, DocId>,
}

/// 快照的索引部分（storage 序列化的数据源）。
///
/// `term_dict` **按 TermId 升序**、`content_hashes` **按 hash 升序**导出，
/// 保证快照字节流确定（NFR-06）且 round-trip 后 TermId / ID 映射稳定。
///
/// ⚠️ `Document.metadata` 是 `serde_json::Value`，其序列化走 `serialize_any`，
/// **bincode 2 不支持**（非自描述格式）——因此快照里用 `DocumentDto`
/// 把 metadata 存成 JSON 字符串，导入时再解析回来。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotSections {
    pub term_dict: Vec<(String, TermId)>,
    pub postings: Vec<Vec<Posting>>,
    pub docs: Vec<Option<DocumentDto>>,
    pub chunks: Vec<Option<Chunk>>,
    pub chunk_lens: Vec<u32>,
    pub stats: Stats,
    pub content_hashes: Vec<(u64, DocId)>,
}

/// 快照专用的文档 DTO（metadata 序列化为 JSON 字符串）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentDto {
    pub doc_id: DocId,
    pub source: String,
    pub metadata_json: String,
    pub content_hash: u64,
}

impl From<&Document> for DocumentDto {
    fn from(d: &Document) -> Self {
        Self {
            doc_id: d.doc_id,
            source: d.source.clone(),
            metadata_json: d.metadata.to_string(),
            content_hash: d.content_hash,
        }
    }
}

impl From<DocumentDto> for Document {
    fn from(d: DocumentDto) -> Self {
        Self {
            doc_id: d.doc_id,
            source: d.source,
            metadata: serde_json::from_str(&d.metadata_json).unwrap_or(serde_json::json!({})),
            content_hash: d.content_hash,
        }
    }
}

impl Index {
    pub fn new() -> Self {
        Self::default()
    }

    /// 摄入一个文档及其分片，返回 `(doc_id, chunk_ids)`。
    ///
    /// - **幂等 upsert（FR-15）**：`content_hash != 0` 且已存在同 hash 的存活文档
    ///   时直接跳过（返回既有 doc_id 与空 chunk 列表），不产生重复分片
    /// - 每个分片独立分词、独立维护 postings 与统计量
    pub fn add(
        &mut self,
        doc: Document,
        chunks: Vec<Chunk>,
        analyzer: &dyn Analyzer,
    ) -> Result<(DocId, Vec<ChunkId>)> {
        let content_hash = doc.content_hash;

        // 幂等去重：同 hash 的存活文档 → 跳过
        if content_hash != 0 {
            if let Some(&existing) = self.content_hashes.get(&content_hash) {
                if self.forward.doc(existing).is_some() {
                    return Ok((existing, Vec::new()));
                }
            }
        }

        let doc_id = self.forward.insert_doc(doc);
        if content_hash != 0 {
            self.content_hashes.insert(content_hash, doc_id);
        }
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

        // 回滚 content_hash 映射（幂等 upsert 的状态）
        if let Some(doc) = self.forward.doc(doc_id) {
            if doc.content_hash != 0 {
                self.content_hashes.remove(&doc.content_hash);
            }
        }

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

    /// 活文档数。
    pub fn num_docs(&self) -> usize {
        self.forward.live_docs()
    }

    // ---- 快照导出 / 导入（T4-02，供 storage 模块序列化）----

    pub fn export(&self) -> SnapshotSections {
        let (term_dict, postings) = self.inverted.export();
        let (docs, chunks) = self.forward.export();
        let mut content_hashes: Vec<(u64, DocId)> =
            self.content_hashes.iter().map(|(h, d)| (*h, *d)).collect();
        content_hashes.sort_unstable();
        SnapshotSections {
            term_dict,
            postings,
            docs: docs
                .iter()
                .map(|d| d.as_ref().map(DocumentDto::from))
                .collect(),
            chunks: chunks.to_vec(),
            chunk_lens: self.chunk_lens.clone(),
            stats: self.stats,
            content_hashes,
        }
    }

    /// 从快照恢复。**不重建向量索引**——向量由 storage 层按原始向量重建（D1）。
    pub fn import(sections: SnapshotSections) -> Self {
        let inverted = InvertedIndex::import(sections.term_dict, sections.postings);
        let docs: Vec<Option<Document>> = sections
            .docs
            .into_iter()
            .map(|d| d.map(Document::from))
            .collect();
        let forward = ForwardStore::import(docs, sections.chunks);
        let content_hashes: HashMap<u64, DocId> = sections.content_hashes.into_iter().collect();
        Self {
            inverted,
            forward,
            stats: sections.stats,
            chunk_lens: sections.chunk_lens,
            content_hashes,
        }
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

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)] // 中文测试名
    use super::*;
    use crate::analyze::MixedAnalyzer;
    use crate::chunk::Chunker;
    use crate::document::content_hash;

    fn make_doc(source: &str, text: &str) -> Document {
        Document {
            doc_id: 0,
            source: source.to_string(),
            metadata: serde_json::json!({}),
            content_hash: content_hash(text),
        }
    }

    fn make_chunks(text: &str) -> Vec<Chunk> {
        Chunker::default().chunk(0, text)
    }

    #[test]
    fn 幂等upsert不产生重复分片() {
        let analyzer = MixedAnalyzer::new();
        let mut index = Index::new();
        let text = "BM25 是一种检索算法";

        let (d1, _) = index
            .add(make_doc("a", text), make_chunks(text), &analyzer)
            .unwrap();
        let n1 = index.num_chunks();

        // 同内容再次 upsert：应跳过，chunk 数不翻倍
        let (d2, c2) = index
            .add(make_doc("b", text), make_chunks(text), &analyzer)
            .unwrap();
        assert_eq!(d1, d2, "重复内容应返回同一 doc_id");
        assert!(c2.is_empty(), "重复内容不应新增分片");
        assert_eq!(index.num_chunks(), n1);
        assert_eq!(index.num_docs(), 1);

        // 不同内容正常入库
        let text2 = "向量检索完全不同";
        index
            .add(make_doc("c", text2), make_chunks(text2), &analyzer)
            .unwrap();
        assert_eq!(index.num_chunks(), n1 + make_chunks(text2).len() as u32);
    }

    #[test]
    fn 删除后可重新upsert() {
        let analyzer = MixedAnalyzer::new();
        let mut index = Index::new();
        let text = "删除后再写入同一内容";

        let (d1, _) = index
            .add(make_doc("a", text), make_chunks(text), &analyzer)
            .unwrap();
        index.remove(d1, &analyzer).unwrap();
        assert_eq!(index.num_chunks(), 0);

        // 删除后同 hash 应能重新入库（hash 映射已回滚）
        let (_, c2) = index
            .add(make_doc("a", text), make_chunks(text), &analyzer)
            .unwrap();
        assert!(!c2.is_empty(), "删除后重新 upsert 应正常入库");
        assert_eq!(index.num_chunks(), c2.len() as u32);
    }

    #[test]
    fn 删除后统计量与全量重建一致() {
        let analyzer = MixedAnalyzer::new();
        let texts = ["文档一内容检索", "文档二向量检索", "文档三倒排索引"];

        // 增量：加三个、删第一个
        let mut incremental = Index::new();
        let mut ids = Vec::new();
        for t in texts {
            let (d, _) = incremental
                .add(make_doc("s", t), make_chunks(t), &analyzer)
                .unwrap();
            ids.push(d);
        }
        incremental.remove(ids[0], &analyzer).unwrap();

        // 全量重建（只含后两个）
        let mut rebuilt = Index::new();
        for t in &texts[1..] {
            rebuilt
                .add(make_doc("s", t), make_chunks(t), &analyzer)
                .unwrap();
        }

        assert_eq!(incremental.num_chunks(), rebuilt.num_chunks());
        assert_eq!(incremental.total_len(), rebuilt.total_len());
        for term in ["检索", "向量", "倒排", "索引"] {
            assert_eq!(
                incremental.doc_freq(term),
                rebuilt.doc_freq(term),
                "term={term} 的 df 不一致"
            );
        }
    }
}
