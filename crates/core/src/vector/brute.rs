//! 暴力线性扫描索引：精确、增量、确定性。P2 的 CLI 默认用它（30 篇规模足够），
//! 同时作为 HNSW 的对照 oracle（T2-05 重合率单测）。

use std::cmp::Ordering;

use crate::error::Result;
use crate::predicate::CandidateFilter;
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

    /// 线性扫描时判定谓词：**精确、零召回损失**，作为 HNSW 的对照 oracle。
    fn search_filtered(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let mut scored: Vec<(ChunkId, f32)> = self
            .entries
            .iter()
            .filter(|(id, _)| filter.is_none_or(|f| f.contains(*id)))
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
    #![allow(non_snake_case)] // 中文测试名
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

    /// **T17**：`filter = None` 的契约——**不过滤，含已软删除条目**。
    ///
    /// 这条测试存在的意义是防止有人误以为"Q-C1 已在向量层修好"。
    /// 向量层**做不到**（hnsw_rs 无 remove API），存活过滤只能靠编排层注入谓词。
    #[test]
    fn None谓词会召回已软删除的chunk() {
        use crate::bitmap::Bitmap;
        use crate::predicate::AliveOnly;

        let mut idx = BruteForceIndex::new();
        for id in 0..4u32 {
            idx.add(id, NormalizedVector::new(vec![id as f32, 1.0]))
                .unwrap();
        }
        // 模拟：chunk 1、3 已被软删除
        let mut alive = Bitmap::new();
        alive.set(0);
        alive.set(2);
        let predicate = AliveOnly::new(&alive);

        let q = NormalizedVector::new(vec![1.0, 0.0]);

        // None → 幽灵候选照常出现（这是契约，不是 bug）
        let all: Vec<ChunkId> = idx
            .search(&q, 10)
            .unwrap()
            .iter()
            .map(|(i, _)| *i)
            .collect();
        assert_eq!(all.len(), 4, "None 应不过滤，返回全部 4 条");
        assert!(
            all.contains(&1) && all.contains(&3),
            "已软删除的 chunk 仍会被召回"
        );

        // Some(AliveOnly) → 幽灵候选被挡掉（按距离升序：2 比 0 更近）
        let filtered: Vec<ChunkId> = idx
            .search_filtered(&q, 10, Some(&predicate))
            .unwrap()
            .iter()
            .map(|(i, _)| *i)
            .collect();
        assert_eq!(filtered, vec![2, 0], "AliveOnly 必须挡掉已软删除的 chunk");
        assert!(!filtered.contains(&1) && !filtered.contains(&3));
    }

    #[test]
    fn 过滤后不足k条时只返回通过的部分() {
        use crate::bitmap::Bitmap;
        use crate::predicate::AliveOnly;

        let mut idx = BruteForceIndex::new();
        for id in 0..5u32 {
            idx.add(id, NormalizedVector::new(vec![1.0, 0.0])).unwrap();
        }
        let mut alive = Bitmap::new();
        alive.set(4);
        let predicate = AliveOnly::new(&alive);

        let q = NormalizedVector::new(vec![1.0, 0.0]);
        let got = idx.search_filtered(&q, 3, Some(&predicate)).unwrap();
        assert_eq!(
            got.len(),
            1,
            "只允许 1 条通过时，不能用未通过的候选把 k 凑满"
        );
        assert_eq!(got[0].0, 4);
    }
}
