//! V2 Step 8 视图模型（`S8-02`）：**不可变段 + 追加段 + 原子发布**。
//!
//! # 三个类型
//!
//! - [`Segment`]：一段内容（倒排 / 正排 / 向量 / 原始向量）。段内 ID 从 0 起，
//!   对外用 `base_*` 折算成全局 ID（`D-S8-04`）。
//! - [`View`]：读端看到的一致快照。**不可变**：任何变化都产出新的 `Arc<View>`。
//! - [`Shared`]：写端与读端共享的唯一对象（`RwLock<Arc<View>>` + 发号 + 装配）。
//!
//! ⚠️ **不引入 `arc-swap`**：守门约束「无新增第三方依赖」（设计 §3.4）。
//! `RwLock<Arc<View>>` 的**写锁持有时长 = 一次指针替换**（纳秒级），足以满足 I8-1。
//!
//! # 不变式（`I8-1` ~ `I8-3`）
//!
//! | ID | 不变式 |
//! | --- | --- |
//! | **I8-1** | **写锁只用于指针替换**：[`Shared::publish`] 的写锁内**不得**做任何计算 / I/O / 分配（除 `Arc::new`）。任何「先取写锁再干活」的写法都必须在评审里被拒。 |
//! | **I8-2** | **段一旦进入过任何 `View`，永不再被修改**。⚠️ **PR3（`S8-02`）阶段尚未成立**：此时 `deltas` 恒空、内容唯一，写端经 [`Shared::with_main_mut`] **就地修改** `main` ⇒ 该约束自 **`S8-03`（delta 写入）** 起才真正成立。 |
//! | **I8-3** | **读端只在取快照那一刻取一次读锁**（[`Shared::snapshot`]），之后全程无锁；一次检索**只用一个 `View`**。 |
//!
//! 🔑 **I8-3 的最后半句是正确性的关键**：若一次检索中途重新取 view，可能出现
//! 「BM25 用了 v1 的段、向量用了 v2 的段」⇒ 跨 lane 的 chunk 语义不一致。
//!
//! # ⚠️ PR3 的过渡约束（必须在评审里说清）
//!
//! `deltas` 恒空 ⇒ 内容只有一份（`main`）⇒ 写入必须**独占**该段：
//! [`Shared::with_main_mut`] 用 `Arc::get_mut`，要求**没有读者持有当前 `Arc<View>`**。
//! 因此 PR3 的写入在**恰好有并发检索进行中**时会返回 `Err`（消息里指明原因）。
//!
//! **这不是终态**：`S8-03` 引入 `SegmentBuilder` + delta 追加后，写入落在写端私有的
//! builder 上、不再需要 `get_mut`，该约束随之消失（`FR-17` 的「读不阻塞写」
//! 由 `S8-03` ~ `S8-06` 交付）。

use std::sync::{Arc, Mutex, RwLock};

use crate::error::{Error, Result};
use crate::index::Index;
use crate::types::{ChunkId, DocId};
use crate::vector::VectorIndex;

use super::config::{Config, GraphOpts, VectorBackend};

/// 一个**段**：段内 `Index` 的本地 ID 从 0 起，对外用 `base_*` 折算成全局 ID。
///
/// ⚠️ **`PR3` 阶段本类型由写端独占可变**（见模块文档的 I8-2 说明）；
/// `S8-03` 起段在进入 `View` 之后**不再被修改**（要改就造新段）。
pub(crate) struct Segment {
    /// 倒排 + 正排 + 统计量（段内本地 ID）
    pub(crate) index: Index,
    /// 该段的向量侧（`None` = 纯 BM25 段）
    pub(crate) vector_index: Option<Box<dyn VectorIndex>>,
    /// 原始向量（快照策略 D1：存原始向量不存图；`None` = 未启用 `keep_raw`）
    pub(crate) raw_vectors: Option<Vec<(ChunkId, Vec<f32>)>>,
    /// 本段在全局 `doc_id` 空间中的基址（= 前序所有段的 `docs.len()` 之和）
    ///
    /// ⚠️ `PR3` 单段下恒 `0`（全局 ID == 本地 ID）；`S8-03` 起由 [`IdAllocator`] 计算。
    pub(crate) base_doc: DocId,
    /// 本段在全局 `chunk_id` 空间中的基址（同上）
    pub(crate) base_chunk: ChunkId,
    /// 段的创建序号（诊断 / FIFO 合并排序）
    pub(crate) generation: u64,
}

impl Segment {
    /// 空段（无内容、无向量、基址 0）。
    pub(crate) fn empty(vector_index: Option<Box<dyn VectorIndex>>) -> Self {
        Self {
            index: Index::new(),
            vector_index,
            raw_vectors: Some(Vec::new()),
            base_doc: 0,
            base_chunk: 0,
            generation: 0,
        }
    }
}

/// 跨段墓碑（针对**既往段**的全局 doc 墓碑）。
///
/// ⚠️ **`PR3` 阶段恒为空**：内容只有一段、且删除仍走 `Index::remove` 的**物理**路径
/// （与今天逐位一致）。`S8-03` 起「删除落在既往段上」的 doc 记进这里，
/// 由 `S8-06` 的合并**物理化**（设计 §4.9.2）。
#[derive(Debug, Default)]
pub(crate) struct Tombstones {
    /// 已墓碑化的全局 doc_id（升序去重）
    doc_ids: Vec<DocId>,
}

impl Tombstones {
    /// 是否为空（`PR3` 阶段恒 `true`）。
    ///
    /// ⚠️ **不能加 `#[cfg(debug_assertions)]`**（`2026-09-18` CI `build (release)` 实测失败）：
    /// `debug_assert!` 展开成 `if cfg!(debug_assertions) { … }` —— `cfg!` 是**运行时**宏，
    /// 不是 `#[cfg]` ⇒ **它的实参在 release 下照样要编译** ⇒ 用 `#[cfg]` 把本方法去掉会让
    /// release 构建**编译失败**。
    pub(crate) fn is_empty(&self) -> bool {
        self.doc_ids.is_empty()
    }
}

/// 读端看到的一致快照。**不可变**：任何变化都产出新的 `Arc<View>`。
pub(crate) struct View {
    /// 主段（唯一承载全部内容的段）
    pub(crate) main: Arc<Segment>,
    /// 追加段（FIFO；合并按序消费）
    ///
    /// ⚠️ **`PR3` 恒空**（`S8-02` 的验收条件之一）⇒ 读路径只需看 `main`。
    /// `S8-03` 起由 `commit()` 追加；`S8-06` 的合并按序消费。
    /// 本阶段由 [`super::index::SearchIndex::tombstone_stats`] 的跨段求和与
    /// `S8-T9` 的「恒空」断言消费。
    pub(crate) deltas: Arc<[Arc<Segment>]>,
    /// 针对**既往段**的全局 doc 墓碑（`PR3` 恒空，见 [`Tombstones`]）
    pub(crate) tombstones: Arc<Tombstones>,
    /// 视图序号（每次 `publish` +1；用于诊断与「同 view 内确定性」的判据）
    pub(crate) generation: u64,
}

impl View {
    /// 由单个主段构造初始视图（`deltas` 空、墓碑空、`generation = 0`）。
    pub(crate) fn single(main: Arc<Segment>) -> Self {
        Self {
            main,
            deltas: Arc::from(Vec::<Arc<Segment>>::new()),
            tombstones: Arc::new(Tombstones::default()),
            generation: 0,
        }
    }
}

/// 全局 ID 发号器（`D-S8-04`）。
///
/// ⚠️ **`PR3`（`S8-02`）里只承载「段的基址 + 视图序号」**：单段下 `base_*` 恒 `0`，
/// 段的本地 ID 仍由 `Index::add` 分配 ⇒ **全局 ID == 本地 ID**（与今天逐位一致）。
/// `S8-03` 起由它统一发号。
///
/// ⚠️ **不用 `AtomicU64`**：`DocId` / `ChunkId` 是 `u32`，且写端本就串行
/// ⇒ `Mutex` 语义更清楚，也便于把「一次分配一对」做成不可分割的操作（设计 §4.4.3）。
#[derive(Debug, Default)]
pub(crate) struct IdAllocator {
    /// 已发布的 doc 数（下一段的 `base_doc` 来源）
    pub(crate) next_doc: DocId,
    /// 已发布的 chunk 数（下一段的 `base_chunk` 来源）
    pub(crate) next_chunk: ChunkId,
    /// 已发布的视图序号
    pub(crate) generation: u64,
}

/// 写端与读端共享的唯一对象。
pub(crate) struct Shared {
    /// 当前视图。⚠️ 写锁**只**用于替换指针（I8-1）。
    view: RwLock<Arc<View>>,
    /// 发号器（写端串行取用；**读端永不触碰** ⇒ 不进读路径，不违反 I8-1）
    ids: Mutex<IdAllocator>,
    /// 装配（`Arc` 共享，读端直接引用）
    pub(crate) cfg: Arc<Config>,
    /// 图持久化开关
    pub(crate) graph: GraphOpts,
    /// 向量后端
    pub(crate) backend: VectorBackend,
}

impl Shared {
    /// 构造：以给定段作为唯一主段。
    pub(crate) fn new(
        cfg: Arc<Config>,
        graph: GraphOpts,
        backend: VectorBackend,
        main: Arc<Segment>,
    ) -> Self {
        Self {
            view: RwLock::new(Arc::new(View::single(main))),
            ids: Mutex::new(IdAllocator::default()),
            cfg,
            graph,
            backend,
        }
    }

    /// 取当前视图快照（**唯一**允许的读入口：一次检索只调一次，`I8-3`）。
    pub(crate) fn snapshot(&self) -> Arc<View> {
        Arc::clone(&self.view.read().expect("视图锁中毒"))
    }

    /// **发布**新视图（唯一写点，`I8-1`：写锁内只有一次指针替换）。
    pub(crate) fn publish(&self, next: View) {
        *self.view.write().expect("视图锁中毒") = Arc::new(next);
    }

    /// 一次推进「已发布 doc / chunk 总数 + 视图序号」，返回新序号。
    ///
    /// ⚠️ 发号与「算段基址」必须在**同一临界区**内（设计 §4.4.3 / 评审 P3-4）：
    /// 保留 `into_index()` 后同一 `Arc<Shared>` 上可同时存在两个写端句柄，
    /// 若两者分别算 `base_*` 会得到相同值 ⇒ 段 ID 空间重叠 ⇒ 破坏 I8-7。
    /// `PR3` 里该锁只被 `commit()` 取（写端串行）。
    ///
    /// `published_docs` / `published_chunks` 是「本次发布之后、全局空间的已用长度」，
    /// 即**下一段的 `base_*`**（设计 §4.4.1：`base = 前序所有段长度之和`）。
    pub(crate) fn advance(&self, published_docs: DocId, published_chunks: ChunkId) -> u64 {
        let mut ids = self.ids.lock().expect("发号锁中毒");
        ids.next_doc = published_docs;
        ids.next_chunk = published_chunks;
        ids.generation += 1;
        ids.generation
    }
    /// 下一段的 `base_*`（`S8-03` 起由段构造消费；本阶段用于自洽断言）。
    pub(crate) fn next_base(&self) -> (DocId, ChunkId) {
        let ids = self.ids.lock().expect("发号锁中毒");
        (ids.next_doc, ids.next_chunk)
    }

    /// 取 `main` 段的**可变**引用（`PR3` 过渡入口，见模块文档）。
    ///
    /// 要求没有读者持有当前 `Arc<View>`；否则返回 `Err`
    /// （`S8-03` 的 delta 写入解除该约束）。
    pub(crate) fn with_main_mut<R>(&self, f: impl FnOnce(&mut Segment) -> R) -> Result<R> {
        let mut guard = self.view.write().expect("视图锁中毒");
        let view = Arc::get_mut(&mut *guard).ok_or_else(write_needs_exclusive)?;
        let seg = Arc::get_mut(&mut view.main).ok_or_else(write_needs_exclusive)?;
        // ⚠️ 这里只做「就地修改」、**不**替换指针 ⇒ 包在写锁内是必要的
        // （否则读者可能看到半修改状态）。`PR3` 的写路径短（一次 add / flush 的量级），
        // 且 `S8-03` 起本入口整体被 builder 取代。
        Ok(f(seg))
    }
}

/// 「写入需要独占」的统一错误（消息里指明阶段与出路，避免被误读成 bug）。
fn write_needs_exclusive() -> Error {
    Error::InvalidInput(
        "写入需要独占当前视图（此刻有并发检索持有快照）—— S8-02 过渡期约束；\
         并发读写由 S8-03 的 delta 分段交付"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;
    use crate::search::config::SearchIndexBuilder;

    fn shared() -> Shared {
        let cfg = SearchIndexBuilder::default().embedder(None).build_config();
        Shared::new(
            Arc::new(cfg),
            GraphOpts::default(),
            VectorBackend::Brute,
            Arc::new(Segment::empty(None)),
        )
    }

    /// **`S8-02` 骨架不变式**：初始视图 `deltas` **恒空**、墓碑空、`generation = 0`。
    ///
    /// 这条是 §1.3 第 2 条（「`deltas` 恒空 ⇒ 与重构前逐位一致」）的**可执行判据**：
    /// 只要它绿，读路径「只看 `main`」就与「遍历全部段」等价。
    #[test]
    fn S8_02_初始视图的deltas恒空且序号为零() {
        let sh = shared();
        let v = sh.snapshot();
        assert!(v.deltas.is_empty(), "初始视图的 deltas 必须为空");
        assert!(v.tombstones.is_empty(), "初始视图的跨段墓碑必须为空");
        assert_eq!(v.generation, 0, "初始视图序号应为 0");
        assert_eq!(v.main.base_doc, 0, "单段下 base_doc 恒为 0");
        assert_eq!(v.main.base_chunk, 0, "单段下 base_chunk 恒为 0");
    }

    /// **`S8-02`**：`publish` 之后 `generation` **单调递增**，且 `deltas` 仍空
    /// （骨架期发布**不产生新段**：内容只在 `main` 里，指针替换是唯一的可见性跃迁）。
    #[test]
    fn S8_02_发布只递增序号且不产生新段() {
        let sh = shared();
        let before = sh.snapshot();
        assert_eq!(sh.next_base(), (0, 0), "未发布时应为 (0, 0)");

        let g1 = sh.advance(3, 7);
        assert_eq!(g1, 1, "序号应从 1 开始");
        assert_eq!(sh.next_base(), (3, 7), "advance 必须同临界区更新基址");
        sh.publish(View {
            main: Arc::clone(&before.main),
            deltas: Arc::clone(&before.deltas),
            tombstones: Arc::clone(&before.tombstones),
            generation: g1,
        });
        let after = sh.snapshot();
        assert_eq!(after.generation, 1);
        assert!(after.deltas.is_empty(), "发布不得产生新段（骨架期）");
        assert!(
            Arc::ptr_eq(&after.main, &before.main),
            "骨架期发布必须复用同一个 main 段（不复制内容）"
        );

        let g2 = sh.advance(3, 7);
        assert_eq!(g2, 2, "序号必须单调递增");
    }

    /// **`S8-02` 过渡约束**：有读者持有当前 `Arc<View>` 时，`with_main_mut` **必须失败**
    /// 而不是静默改到读者正在看的段上。
    ///
    /// 🔑 这条锁住的是**正确性**：若 `get_mut` 失败被忽略（例如退化成「直接改」），
    /// 读者会在检索中途看到半修改的状态。`S8-03` 起该约束被 delta 写入解除，
    /// 届时本用例**应当被删除**（连同 `with_main_mut` 一起）——不要改成「永远绿」。
    #[test]
    fn S8_02_有读者时写入必须失败() {
        let sh = shared();
        // 无读者：可写
        sh.with_main_mut(|seg| seg.index.total_docs()).unwrap();

        // 造一个读者持有当前视图
        let held = sh.snapshot();
        let err = sh.with_main_mut(|seg| seg.index.total_docs()).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("需要独占"),
            "错误消息必须指明「写入需要独占」，实际：{msg}"
        );

        // 读者释放后恢复可写（约束是瞬时的，不是永久的）
        drop(held);
        sh.with_main_mut(|seg| seg.index.total_docs())
            .expect("读者释放后必须恢复可写");
    }

    /// **`S8-02`**：`View` 的「一次检索只用一个快照」语义 —— `snapshot()` 拿到的 `Arc<View>`
    /// 在后续 `publish` 之后**内容不变**（`I8-3` 的基石）。
    #[test]
    fn S8_02_已取出的快照不受后续发布影响() {
        let sh = shared();
        let held = sh.snapshot();
        let g = sh.advance(0, 0);
        let cur = sh.snapshot();
        sh.publish(View {
            main: Arc::clone(&cur.main),
            deltas: Arc::clone(&cur.deltas),
            tombstones: Arc::clone(&cur.tombstones),
            generation: g,
        });
        assert_eq!(held.generation, 0, "已取出的快照内容不得被后续发布改动");
        assert_eq!(sh.snapshot().generation, 1, "新快照必须看到新序号");
    }
}
