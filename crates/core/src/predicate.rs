//! 候选过滤谓词：过滤下推的抽象层（**无依赖叶模块**）。
//!
//! # 为什么单独成一个模块
//!
//! 过滤下推要求「编排层求值一次，各 lane 在遍历期判定」。判定谓词要同时被
//! `retriever`（文本路）与 `vector`（向量路）使用，而这两层**都不认识 `Index`**
//! （架构文档 lib.rs 的模块边界守则：vector 不认识文本/索引）。
//!
//! 因此本模块只放 **trait + 不依赖 `Index` 的实现**，是依赖图的叶子；
//! 需要 `Index` 的实现（`ChunkFilter`）放在 `query/filter.rs`。
//! 两路只需 `use crate::predicate::CandidateFilter`，不会把 `vector` 拖向 `index`。
//!
//! # ⚠️ `None` 的语义（评审 P1-6，必须写死）
//!
//! `Option<&dyn CandidateFilter>` 为 **`None` 表示"不过滤"，包含已软删除的条目**。
//!
//! hnsw_rs 无法从图中物理摘除向量（无 remove API），已删 chunk 的向量仍留在图里。
//! 因此**唯一正确的调用方式是经过编排层**（由它注入存活谓词），或在测试/bench 中
//! 自行构造存活谓词。直接调用 `VectorIndex::search()` / `Retriever::search()`
//! （它们默认转发 `None`）会**依然召回幽灵候选**——这是逃生舱路径的已知契约，
//! 不是 bug（见 T17）。
//!
//! # 为什么必须 `Send + Sync`
//!
//! Hybrid 模式下两路召回在 `rayon::join` 中执行，共享同一个谓词引用。

use crate::bitmap::ChunkBits;
use crate::types::ChunkId;

/// 谓词种类：向量路据此选择检索路径。
///
/// 两者的成本结构完全不同（见设计文档 §5.7）：
/// `Alive` 选择度≈1，可走带 fast-return 的普通 `search()`；
/// `Filtered` 会关闭 fast-return，必须限制过采样宽度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    /// 无用户过滤，仅存活过滤（选择度≈1，热路径）
    Alive,
    /// 真实的用户过滤（已 AND 存活位图）
    Filtered,
}

/// 候选过滤谓词：编排层求值一次，传给各 lane 在遍历期判定。
pub trait CandidateFilter: Send + Sync {
    /// `chunk_id` 是否允许进入结果。
    ///
    /// **必须同时覆盖存活判定**：仅判定用户过滤条件会让已软删除的 chunk 漏进来。
    fn contains(&self, chunk_id: ChunkId) -> bool;

    /// 允许通过的总数（O(1)）。
    ///
    /// - `Alive`    = 存活 chunk 数
    /// - `Filtered` = 通过过滤的 chunk 数
    ///
    /// 向量路用它决定过采样宽度（`knbn`）与是否可能 shortfall。
    fn allowed_count(&self) -> usize;

    /// 谓词种类（决定向量路走哪条路径）。
    fn kind(&self) -> FilterKind;
}

/// 无用户过滤场景：只做存活过滤。
///
/// 这是 Q-C1（删除后向量残留）的修复路径，也是**最热的路径**——
/// 它只是对 `Index` 已有存活位图的借用，**零构建成本**。
#[derive(Debug, Clone, Copy)]
pub struct AliveOnly<'a> {
    alive: &'a ChunkBits,
    count: usize,
}

impl<'a> AliveOnly<'a> {
    /// 借用索引的存活位图构造谓词（O(1)，`count` 直接来自位图缓存）。
    pub fn new(alive: &'a ChunkBits) -> Self {
        let count = alive.count_ones();
        Self { alive, count }
    }
}

impl CandidateFilter for AliveOnly<'_> {
    fn contains(&self, chunk_id: ChunkId) -> bool {
        self.alive.contains(chunk_id)
    }

    fn allowed_count(&self) -> usize {
        self.count
    }

    fn kind(&self) -> FilterKind {
        FilterKind::Alive
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;
    use crate::bitmap::Bitmap;

    #[test]
    fn AliveOnly只放行存活位() {
        let mut alive = Bitmap::new();
        alive.set(0);
        alive.set(2);
        alive.set(3);

        let p = AliveOnly::new(&alive);
        assert_eq!(p.kind(), FilterKind::Alive);
        assert_eq!(p.allowed_count(), 3);
        assert!(p.contains(0));
        assert!(!p.contains(1), "未置位 = 已软删除，必须挡掉");
        assert!(p.contains(2));
        assert!(p.contains(3));
        assert!(!p.contains(999), "越界 ID 不 panic，返回 false");
    }

    #[test]
    fn 存活位变化后新谓词立即反映() {
        // AliveOnly 借用位图，不快照——同一份位图删一位后重建谓词应立刻生效
        let mut alive = Bitmap::new();
        for i in 0..5 {
            alive.set(i);
        }
        assert_eq!(AliveOnly::new(&alive).allowed_count(), 5);
        alive.clear(2);
        let p = AliveOnly::new(&alive);
        assert_eq!(p.allowed_count(), 4);
        assert!(!p.contains(2));
    }

    #[test]
    fn 谓词可跨线程共享() {
        // Hybrid 两路在 rayon::join 中共享同一谓词引用，编译期即要求 Send + Sync
        fn assert_send_sync<T: Send + Sync>(_: &T) {}
        let alive = Bitmap::new();
        let p = AliveOnly::new(&alive);
        assert_send_sync(&p);
        let boxed: Box<dyn CandidateFilter> = Box::new(AliveOnly::new(&alive));
        assert_send_sync(&boxed);
    }
}
