//! 正排存储：分片与文档的随机访问 + 墓碑。
//!
//! 用 `Vec<Option<T>>`，`None` 即墓碑：删除时不移动元素（保持 ID 稳定），
//! 检索时按 ID O(1) 取回，再判断 `None` 过滤。

use crate::document::{Chunk, DocRecord};
use crate::types::{ChunkId, DocId};

/// 正排存储：chunk_id / doc_id → 内容与元数据（`None` 表示墓碑位）。
#[derive(Debug, Default)]
pub struct ForwardStore {
    chunks: Vec<Option<Chunk>>,
    docs: Vec<Option<DocRecord>>,
}

impl ForwardStore {
    /// 创建空正排存储。
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入文档并返回分配的 DocId。
    pub fn insert_doc(&mut self, mut doc: DocRecord) -> DocId {
        let id = self.docs.len() as DocId;
        doc.doc_id = id;
        self.docs.push(Some(doc));
        id
    }

    /// 插入分片并返回分配的 ChunkId。
    pub fn insert_chunk(&mut self, mut chunk: Chunk) -> ChunkId {
        let id = self.chunks.len() as ChunkId;
        chunk.chunk_id = id;
        self.chunks.push(Some(chunk));
        id
    }

    /// 取分片（墓碑位返回 `None`）。
    pub fn chunk(&self, id: ChunkId) -> Option<&Chunk> {
        self.chunks.get(id as usize).and_then(|o| o.as_ref())
    }

    /// 取文档（墓碑位返回 `None`）。
    pub fn doc(&self, id: DocId) -> Option<&DocRecord> {
        self.docs.get(id as usize).and_then(|o| o.as_ref())
    }

    /// 墓碑化一个分片。
    pub fn tombstone_chunk(&mut self, id: ChunkId) {
        if let Some(slot) = self.chunks.get_mut(id as usize) {
            *slot = None;
        }
    }

    /// 墓碑化一个文档。
    pub fn tombstone_doc(&mut self, id: DocId) {
        if let Some(slot) = self.docs.get_mut(id as usize) {
            *slot = None;
        }
    }

    /// 某文档的所有分片 ID（含已墓碑化的，遍历时需再判活）。
    pub fn chunk_ids_of_doc(&self, doc_id: DocId) -> Vec<ChunkId> {
        self.chunks
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                c.as_ref()
                    .filter(|c| c.doc_id == doc_id)
                    .map(|_| i as ChunkId)
            })
            .collect()
    }

    /// 活的分片总数（用于统计，实际以 Index.num_chunks 为准）。
    pub fn live_chunks(&self) -> usize {
        self.chunks.iter().filter(|c| c.is_some()).count()
    }

    /// 存活文档数（不含墓碑位）。
    pub fn live_docs(&self) -> usize {
        self.docs.iter().filter(|d| d.is_some()).count()
    }

    /// 迭代所有活分片（确定性顺序，用于"全量重建"对照）。
    pub fn iter_live_chunks(&self) -> impl Iterator<Item = &Chunk> {
        self.chunks.iter().filter_map(|c| c.as_ref())
    }

    /// 导出快照用的 (docs, chunks)。`Option::None` 即墓碑，原样保留。
    pub fn export(&self) -> (&[Option<DocRecord>], &[Option<Chunk>]) {
        (&self.docs, &self.chunks)
    }

    /// 从快照恢复。
    pub fn import(docs: Vec<Option<DocRecord>>, chunks: Vec<Option<Chunk>>) -> Self {
        Self { chunks, docs }
    }
}
