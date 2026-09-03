//! 倒排索引：term 字典 + postings 列表。
//!
//! 数据结构选型（p1-design.md D2）：postings 用 **`Vec<Vec<Posting>>`** 按 `TermId`
//! 索引，而非 `HashMap<TermId, Vec<Posting>>`。理由：遍历顺序天然确定（NFR-06），
//! 且更省内存、缓存友好。`df` **不单独存**，直接取 `postings[t].len()`——
//! 删除时同步移除 posting，`df` 自动保持一致，杜绝"df 与 postings 漂移"。

use std::collections::HashMap;

use smol_str::SmolStr;

use crate::types::{ChunkId, TermId};

use super::posting::Posting;

/// 倒排索引：词项 → 倒排链（chunk_id 升序，服务于确定性 NFR-06）。
#[derive(Debug, Default)]
pub struct InvertedIndex {
    /// term → TermId
    terms: HashMap<SmolStr, TermId>,
    /// TermId → 按 chunk_id 升序排列的 postings（有序性服务于确定性 NFR-06）
    postings: Vec<Vec<Posting>>,
}

impl InvertedIndex {
    /// 创建空倒排索引。
    pub fn new() -> Self {
        Self::default()
    }

    /// 取词项 ID；不存在则新建。
    fn intern(&mut self, term: &str) -> TermId {
        if let Some(&id) = self.terms.get(term) {
            return id;
        }
        let id = self.postings.len() as TermId;
        self.terms.insert(SmolStr::new(term), id);
        self.postings.push(Vec::new());
        id
    }

    /// 新增一条 posting（含 tf；positions 走 feature）。
    pub fn add(&mut self, term: &str, chunk_id: ChunkId, tf: u32, positions: &[u32]) {
        let id = self.intern(term);
        #[cfg(feature = "positions")]
        {
            self.postings[id as usize].push(Posting::with_positions(
                chunk_id,
                tf,
                positions.to_vec(),
            ));
        }
        #[cfg(not(feature = "positions"))]
        {
            let _ = positions;
            self.postings[id as usize].push(Posting::new(chunk_id, tf));
        }
    }

    /// 移除某词项下、某分片的 posting。返回是否移除成功。
    pub fn remove(&mut self, term: &str, chunk_id: ChunkId) -> bool {
        let Some(&id) = self.terms.get(term) else {
            return false;
        };
        let list = &mut self.postings[id as usize];
        if let Some(idx) = list.iter().position(|p| p.chunk_id == chunk_id) {
            list.remove(idx);
            true
        } else {
            false
        }
    }

    /// 查询某词项的 posting 列表（不含墓碑，由上层 Index 过滤）。
    pub fn postings(&self, term: &str) -> Option<&[Posting]> {
        self.terms
            .get(term)
            .map(|&id| self.postings[id as usize].as_slice())
    }

    /// 词项的文档（分片）频率 = 该词项 posting 数。
    pub fn doc_freq(&self, term: &str) -> u32 {
        self.terms
            .get(term)
            .map(|&id| self.postings[id as usize].len() as u32)
            .unwrap_or(0)
    }

    /// 词项总数（供调试 / 统计）。
    pub fn num_terms(&self) -> usize {
        self.terms.len()
    }

    /// 获取词项 ID（供 BM25 预计算 idf 使用；不存在返回 None）。
    pub fn term_id(&self, term: &str) -> Option<TermId> {
        self.terms.get(term).copied()
    }

    /// 按 TermId 取 postings（内部使用，避免重复查字典）。
    pub fn postings_by_id(&self, id: TermId) -> &[Posting] {
        &self.postings[id as usize]
    }

    /// 导出快照用的 (term_dict, postings)。
    ///
    /// term_dict **按 TermId 升序**导出：HashMap 本身无序，若按哈希序导出，
    /// round-trip 后 TermId 仍然由 postings 下标决定、不会漂移——但按 id 排序
    /// 能保证快照字节流的确定性（NFR-06）。
    pub fn export(&self) -> (Vec<(String, TermId)>, Vec<Vec<Posting>>) {
        let mut dict: Vec<(String, TermId)> = self
            .terms
            .iter()
            .map(|(k, &v)| (k.to_string(), v))
            .collect();
        dict.sort_unstable_by_key(|(_, id)| *id);
        (dict, self.postings.clone())
    }

    /// 从快照恢复。
    pub fn import(term_dict: Vec<(String, TermId)>, postings: Vec<Vec<Posting>>) -> Self {
        let terms: HashMap<SmolStr, TermId> = term_dict
            .into_iter()
            .map(|(k, v)| (SmolStr::new(k), v))
            .collect();
        debug_assert_eq!(
            terms.len(),
            postings.len(),
            "term_dict 与 postings 数量应一致"
        );
        Self { terms, postings }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn df_与postings一致() {
        let mut idx = InvertedIndex::new();
        idx.add("rust", 0, 2, &[]);
        idx.add("rust", 1, 1, &[]);
        idx.add("bm25", 0, 1, &[]);
        assert_eq!(idx.doc_freq("rust"), 2);
        assert_eq!(idx.doc_freq("bm25"), 1);
        assert_eq!(idx.doc_freq("absent"), 0);
    }

    #[test]
    fn 删除后df同步() {
        let mut idx = InvertedIndex::new();
        idx.add("rust", 0, 2, &[]);
        idx.add("rust", 1, 1, &[]);
        assert!(idx.remove("rust", 0));
        assert_eq!(idx.doc_freq("rust"), 1);
        assert!(!idx.remove("rust", 0), "重复删除应返回 false");
    }
}
