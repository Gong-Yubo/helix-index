//! 暴力线性扫描索引：精确、增量、确定性。P2 的 CLI 默认用它（30 篇规模足够），
//! 同时作为 HNSW 的对照 oracle（T2-05 重合率单测）。

use std::cmp::Ordering;

use crate::error::Result;
use crate::types::ChunkId;

use super::{NormalizedVector, VectorIndex};

/// 暴力索引
#[derive(Debug, Default)]
pub struct BruteForceIndex {
    entries: Vec<(ChunkId, NormalizedVector)>,
}

impl BruteForceIndex {
    /// 创建空索引。
    pub fn new() -> Self {
        Self::default()
    }

    /// 由既有向量批量构造（诊断/基线对照用，见 p5-design 8.6）。
    pub fn from_entries(entries: Vec<(ChunkId, NormalizedVector)>) -> Self {
        Self { entries }
    }
}

impl VectorIndex for BruteForceIndex {
    fn add(&mut self, id: ChunkId, vec: NormalizedVector) -> Result<()> {
        self.entries.push((id, vec));
        Ok(())
    }

    fn search(&self, query: &NormalizedVector, k: usize) -> Result<Vec<(ChunkId, f32)>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let mut scored: Vec<(ChunkId, f32)> = self
            .entries
            .iter()
            .map(|(id, v)| (*id, query.distance_sq(v)))
            .collect();
        // 距离升序；同距离按 chunk_id 升序（确定性 NFR-06）
        scored.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(k);
        Ok(scored)
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 暴力检索按距离升序() {
        let mut idx = BruteForceIndex::new();
        // 三个单位向量：0=(1,0) 1=(0,1) 2=(0.6,0.8)
        idx.add(0, NormalizedVector::new(vec![1.0, 0.0])).unwrap();
        idx.add(1, NormalizedVector::new(vec![0.0, 1.0])).unwrap();
        idx.add(2, NormalizedVector::new(vec![0.6, 0.8])).unwrap();

        // 查询 (1,0)：最近的是 0，其次 2（cos 0.6），最远 1（cos 0）
        let q = NormalizedVector::new(vec![1.0, 0.0]);
        let got = idx.search(&q, 10).unwrap();
        let ids: Vec<ChunkId> = got.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![0, 2, 1]);
    }
}
