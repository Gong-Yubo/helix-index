//! 正排存储：分片与文档的随机访问 + 墓碑。
//!
//! 用 `Vec<Option<T>>`，`None` 即墓碑：删除时不移动元素（保持 ID 稳定），
//! 检索时按 ID O(1) 取回，再判断 `None` 过滤。
//!
//! # 存活状态的单一真源（V2 Step 1 / D-S1-01）
//!
//! `alive` 位图与 `chunks` 的 `Some`/`None` 表示同一件事，**必须同入口维护**：
//!
//! - 置位只在 [`ForwardStore::insert_chunk`]，清位只在 [`ForwardStore::tombstone_chunk`]；
//! - 两处都在**同一个函数内**、紧邻 `chunks` 的写入，杜绝漂移；
//! - `[Default]` 与快照 [`ForwardStore::import`] 走 [`ForwardStore::rebuild`] 重建。
//!
//! 之所以让正排独担此责而不是给 `VectorIndex` 加墓碑：向量侧状态无法从快照恢复
//! （`load` 时 `raw_vectors` 全量重灌），双写必然漂移。

use crate::bitmap::{Bitmap, ChunkBits};
use crate::document::{Chunk, DocRecord};
use crate::types::{ChunkId, DocId};

/// 正排存储：chunk_id / doc_id → 内容与元数据（`None` 表示墓碑位）。
#[derive(Debug, Default)]
pub struct ForwardStore {
    chunks: Vec<Option<Chunk>>,
    docs: Vec<Option<DocRecord>>,
    /// 存活分片位图（与 `chunks` 的 `Some`/`None` 同源；唯一维护入口在本 impl）
    alive: ChunkBits,
    /// 每个文档的**存活**分片数（供 `allowed_chunk_count` 在 O(匹配文档数) 内求值）
    doc_chunk_count: Vec<u32>,
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
    ///
    /// **这是 `alive` 位图的唯一置位入口**，也是 `doc_chunk_count` 的唯一自增入口。
    pub fn insert_chunk(&mut self, mut chunk: Chunk) -> ChunkId {
        let id = self.chunks.len() as ChunkId;
        chunk.chunk_id = id;
        let doc_id = chunk.doc_id;
        self.chunks.push(Some(chunk));
        self.alive.set(id);
        let need = doc_id as usize + 1;
        if need > self.doc_chunk_count.len() {
            self.doc_chunk_count.resize(need, 0);
        }
        self.doc_chunk_count[doc_id as usize] += 1;
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
    ///
    /// **这是 `alive` 位图的唯一清位入口**。幂等：重复调用不重复扣减。
    pub fn tombstone_chunk(&mut self, id: ChunkId) {
        let Some(slot) = self.chunks.get_mut(id as usize) else {
            return;
        };
        if let Some(chunk) = slot.as_ref() {
            let doc_id = chunk.doc_id as usize;
            if let Some(c) = self.doc_chunk_count.get_mut(doc_id) {
                *c = c.saturating_sub(1);
            }
        }
        *slot = None;
        self.alive.clear(id);
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

    /// 存活分片位图（检索期判定用；与 `chunks` 的 `Some`/`None` 同源）。
    pub fn alive_chunks(&self) -> &ChunkBits {
        &self.alive
    }

    /// 存活分片总数（O(1)）。
    pub fn alive_count(&self) -> usize {
        self.alive.count_ones()
    }

    /// 分片所属文档的 ID（O(1) 数组索引；墓碑位返回 `None`）。
    pub fn doc_of(&self, chunk_id: ChunkId) -> Option<DocId> {
        self.chunks
            .get(chunk_id as usize)
            .and_then(|o| o.as_ref())
            .map(|c| c.doc_id)
    }

    /// 某文档的存活分片数（O(1)；越界返回 0）。
    pub fn chunk_count_of_doc(&self, doc_id: DocId) -> u32 {
        self.doc_chunk_count
            .get(doc_id as usize)
            .copied()
            .unwrap_or(0)
    }

    /// 活的分片总数（用于统计，实际以 Index.num_chunks 为准）。
    pub fn live_chunks(&self) -> usize {
        self.alive.count_ones()
    }

    /// 存活文档数（不含墓碑位）。
    pub fn live_docs(&self) -> usize {
        self.docs.iter().filter(|d| d.is_some()).count()
    }

    /// 迭代所有活分片（确定性顺序，用于"全量重建"对照）。
    pub fn iter_live_chunks(&self) -> impl Iterator<Item = &Chunk> {
        self.chunks.iter().filter_map(|c| c.as_ref())
    }

    /// 迭代所有存活文档（含 `(doc_id, record)`，确定性顺序）。
    ///
    /// 供过滤的全扫兜底（`doc_bits_scan`）作为 oracle 使用：字段索引按 **doc** 级维护，
    /// 兜底实现也必须以 doc 为遍历单位，否则两者语义会错开。
    pub fn iter_live_docs(&self) -> impl Iterator<Item = (DocId, &DocRecord)> {
        self.docs
            .iter()
            .enumerate()
            .filter_map(|(i, d)| d.as_ref().map(|d| (i as DocId, d)))
    }

    /// 导出快照用的 (docs, chunks)。`Option::None` 即墓碑，原样保留。
    ///
    /// 注意：`alive` 与 `doc_chunk_count` **不入快照**——它们可由 `chunks` 重建，
    /// 因此 `FORMAT_VERSION` 无需升版（D-S1-07）。
    pub fn export(&self) -> (&[Option<DocRecord>], &[Option<Chunk>]) {
        (&self.docs, &self.chunks)
    }

    /// 从快照恢复（`alive` 与 `doc_chunk_count` 由 [`Self::rebuild`] 重建）。
    pub fn import(docs: Vec<Option<DocRecord>>, chunks: Vec<Option<Chunk>>) -> Self {
        let mut s = Self {
            chunks,
            docs,
            alive: Bitmap::new(),
            doc_chunk_count: Vec::new(),
        };
        s.rebuild();
        s
    }

    /// 从 `chunks` 重建 `alive` 与 `doc_chunk_count`（O(N)，快照导入时一次）。
    ///
    /// 这是存活状态的**唯一重建入口**：`import` 与 `Default` 之外的路径不允许
    /// 直接改这两个字段，否则会与 `chunks` 漂移。
    pub fn rebuild(&mut self) {
        let mut alive = ChunkBits::new();
        let mut counts: Vec<u32> = vec![0; self.docs.len()];
        for (i, slot) in self.chunks.iter().enumerate() {
            if let Some(chunk) = slot {
                alive.set(i as ChunkId);
                let d = chunk.doc_id as usize;
                if d >= counts.len() {
                    counts.resize(d + 1, 0);
                }
                counts[d] += 1;
            }
        }
        self.alive = alive;
        self.doc_chunk_count = counts;
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;

    fn chunk(doc_id: DocId, text: &str) -> Chunk {
        Chunk {
            chunk_id: 0,
            doc_id,
            ordinal: 0,
            text: text.to_string(),
            char_start: 0,
            char_end: 0,
        }
    }

    fn doc(source: &str) -> DocRecord {
        DocRecord {
            doc_id: 0,
            source: source.to_string(),
            metadata: serde_json::json!({}),
            content_hash: 0,
        }
    }

    /// 断言 `alive` 与 `chunks` 的 Some/None **逐位一致**（T5 的核心不变式）。
    fn assert_consistent(s: &ForwardStore) {
        for (i, slot) in s.chunks.iter().enumerate() {
            assert_eq!(
                s.alive.contains(i as ChunkId),
                slot.is_some(),
                "chunk {i} 的存活位与正排不一致"
            );
        }
        assert_eq!(s.alive.count_ones(), s.alive_count());
        assert_eq!(
            s.alive_count(),
            s.chunks.iter().filter(|c| c.is_some()).count()
        );
    }

    #[test]
    fn 插入即置位() {
        let mut s = ForwardStore::new();
        let d = s.insert_doc(doc("d0"));
        let a = s.insert_chunk(chunk(d, "甲"));
        let b = s.insert_chunk(chunk(d, "乙"));
        assert!(s.alive_chunks().contains(a));
        assert!(s.alive_chunks().contains(b));
        assert_eq!(s.alive_count(), 2);
        assert_eq!(s.chunk_count_of_doc(d), 2);
        assert_eq!(s.doc_of(a), Some(d));
        assert_consistent(&s);
    }

    #[test]
    fn 墓碑化即清位且幂等() {
        let mut s = ForwardStore::new();
        let d = s.insert_doc(doc("d0"));
        let a = s.insert_chunk(chunk(d, "甲"));
        s.tombstone_chunk(a);
        assert!(!s.alive_chunks().contains(a));
        assert_eq!(s.alive_count(), 0);
        assert_eq!(s.chunk_count_of_doc(d), 0);
        s.tombstone_chunk(a); // 幂等
        s.tombstone_chunk(999); // 越界不 panic
        assert_eq!(s.alive_count(), 0);
        assert_eq!(s.chunk_count_of_doc(d), 0, "重复墓碑不应把计数扣成负数");
        assert_consistent(&s);
    }

    #[test]
    fn doc_of对墓碑位返回None() {
        let mut s = ForwardStore::new();
        let d = s.insert_doc(doc("d0"));
        let a = s.insert_chunk(chunk(d, "甲"));
        assert_eq!(s.doc_of(a), Some(d));
        s.tombstone_chunk(a);
        assert_eq!(s.doc_of(a), None);
        assert_eq!(s.doc_of(999), None, "越界返回 None");
    }

    #[test]
    fn 每文档分片数按文档分别统计() {
        let mut s = ForwardStore::new();
        let d0 = s.insert_doc(doc("d0"));
        let d1 = s.insert_doc(doc("d1"));
        s.insert_chunk(chunk(d0, "a"));
        s.insert_chunk(chunk(d0, "b"));
        s.insert_chunk(chunk(d1, "c"));
        assert_eq!(s.chunk_count_of_doc(d0), 2);
        assert_eq!(s.chunk_count_of_doc(d1), 1);
        s.tombstone_chunk(0);
        assert_eq!(s.chunk_count_of_doc(d0), 1);
        assert_eq!(s.chunk_count_of_doc(d1), 1);
        assert_consistent(&s);
    }

    #[test]
    fn 随机增删后存活位图与正排逐位一致() {
        // 确定性伪随机（xorshift），避免引入 rand 依赖
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        let mut s = ForwardStore::new();
        let mut doc_ids: Vec<DocId> = Vec::new();
        let mut chunk_ids: Vec<ChunkId> = Vec::new();

        for _ in 0..2000 {
            match next() % 3 {
                0 => {
                    let d = s.insert_doc(doc("x"));
                    doc_ids.push(d);
                    let n = (next() % 3) as usize; // 0~2 个分片
                    for _ in 0..n {
                        chunk_ids.push(s.insert_chunk(chunk(d, "文本")));
                    }
                }
                1 if !chunk_ids.is_empty() => {
                    let idx = (next() as usize) % chunk_ids.len();
                    s.tombstone_chunk(chunk_ids[idx]);
                }
                _ if !doc_ids.is_empty() => {
                    let idx = (next() as usize) % doc_ids.len();
                    let d = doc_ids[idx];
                    for cid in s.chunk_ids_of_doc(d) {
                        s.tombstone_chunk(cid);
                    }
                    s.tombstone_doc(d);
                }
                _ => {}
            }
        }
        assert_consistent(&s);
    }

    #[test]
    fn 快照往返后存活位图重建一致() {
        let mut s = ForwardStore::new();
        let d0 = s.insert_doc(doc("d0"));
        let d1 = s.insert_doc(doc("d1"));
        let mut ids = Vec::new();
        for d in [d0, d1] {
            for _ in 0..3 {
                ids.push(s.insert_chunk(chunk(d, "文本")));
            }
        }
        s.tombstone_chunk(ids[1]);
        s.tombstone_chunk(ids[3]);
        assert_consistent(&s);

        let (docs, chunks) = s.export();
        let restored = ForwardStore::import(docs.to_vec(), chunks.to_vec());

        assert_eq!(restored.alive_count(), s.alive_count());
        assert_eq!(restored.chunk_count_of_doc(d0), s.chunk_count_of_doc(d0));
        assert_eq!(restored.chunk_count_of_doc(d1), s.chunk_count_of_doc(d1));
        assert_eq!(restored.alive_chunks(), s.alive_chunks());
        for id in &ids {
            assert_eq!(restored.doc_of(*id), s.doc_of(*id));
        }
        assert_consistent(&restored);
    }

    #[test]
    fn 空存储的重build不panic() {
        let s = ForwardStore::import(Vec::new(), Vec::new());
        assert_eq!(s.alive_count(), 0);
        assert!(s.alive_chunks().is_empty());
        assert_eq!(s.chunk_count_of_doc(0), 0);
    }
}
