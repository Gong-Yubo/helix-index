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
//! | **I8-1** | **写锁只用于指针替换**：发布路径 [`Shared::commit_view`] 的写锁内**不得**做任何计算 / I/O / 分配（除 `Arc::new`）。任何「先取写锁再干活」的写法都必须在评审里被拒。**发布逻辑内联在该方法内、不另开 `publish`** ⇒ 「锁外三段」在可见性层面写不出来（`S8-02` 评审 P2 的结构性对策）。 |
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

    /// 墓碑数量（`S8-02` 期恒 `0`）。
    ///
    /// 由 `SearchIndex::tombstone_stats()` 的**跨段求和**消费：墓碑指向的 doc 在段内
    /// 仍然存活，但对外必须计成「已死」⇒ 要从存活文档数里扣掉（`S8-02` 期扣 0）。
    pub(crate) fn len(&self) -> usize {
        self.doc_ids.len()
    }
}

/// `main` + `deltas` 的六项计数之和（`tombstone_stats` 的中间量）。
///
/// 🔑 抽成类型 + [`View::sums`] 的**纯函数**形态是为了**可测**（`S8-02` 评审 P3-2）：
/// `S8-02` 期 `deltas` 恒空 ⇒ 若求和逻辑内联在 `SearchIndex::tombstone_stats()` 里
/// （而视图只能经 `Shared::snapshot()` 拿到），「`deltas` 非空时是否正确累加」
/// **永远测不到**。纯函数可以直接喂一个**手工构造的、带 delta 的 `View`**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SegmentSums {
    /// 正排分片槽位总数（含墓碑）
    pub(crate) chunks_total: usize,
    /// 存活分片数
    pub(crate) chunks_alive: usize,
    /// 正排文档槽位总数（含墓碑）
    pub(crate) docs_total: usize,
    /// 存活文档数（**不含**跨段墓碑扣减 —— 那一步在 `tombstone_stats` 里）
    pub(crate) docs_alive: usize,
    /// 向量索引中的点数（含墓碑）
    pub(crate) graph_points: usize,
    /// `raw_vectors` 原始向量条数
    pub(crate) raw_vectors: usize,
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
    /// `main` + 全部 `deltas` 的六项计数**逐段求和**（设计 §4.9.5）。
    ///
    /// ⚠️ **这里真的逐段累加**（不是「只读 `main` + 一条阶段断言」）：`deltas` 恒空时
    /// 结果与只读 `main` **逐位一致**；非空时（`S8-03` 起）也直接正确。
    /// 『先前的版本只有断言、没有求和』是 `S8-02` 评审 P3-2 指出的问题。
    pub(crate) fn sums(&self) -> SegmentSums {
        let mut out = SegmentSums::default();
        for seg in std::iter::once(&self.main).chain(self.deltas.iter()) {
            out.chunks_total += seg.index.total_chunks();
            out.chunks_alive += seg.index.alive_count();
            out.docs_total += seg.index.total_docs();
            out.docs_alive += seg.index.num_docs();
            out.graph_points += seg.vector_index.as_ref().map(|v| v.len()).unwrap_or(0);
            out.raw_vectors += seg.raw_vectors.as_ref().map(|v| v.len()).unwrap_or(0);
        }
        out
    }

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

    /// **原子提交**：持 `ids` 锁完成「取快照 → 算下一段基址 → 推进序号 → 发布」**四步**，
    /// 返回新视图序号。
    ///
    /// # 为什么必须在一把锁内（`S8-02` 评审 P2；`2026-09-18` 两套实测）
    ///
    /// `into_index()` 起总是成功 ⇒ 同一 `Arc<Shared>` 上可有**多个写端句柄**
    /// （`SearchIndex: Send`）⇒ 两写端并发 `commit()` 合法。四步分离时会这样交错：
    ///
    /// ```text
    /// A: snapshot(gen N) → advance(N+1)
    /// B: snapshot(gen N) → advance(N+2) → publish(N+2)
    /// A:                                   publish(N+1)   ⇒ generation 2 → 1（回退）
    /// ```
    ///
    /// **实测（修复前）**：① 手工编排上述交错 ⇒ 终值 `generation = 1`（应为 2），
    /// 且原「单调性告警」判据 `generation <= cur.generation` = `1 <= 0` = **false ⇒ 不触发**
    /// （它比较的是**发布前的旧 `cur`**，形同虚设）；② **真实并发**：4 写端 × 150 次
    /// `commit()` ⇒ 观察线程看到 **70 次回退**（样本 `(65,64)` / `(82,81)` / `(134,127)`）。
    /// 回归判据见 `crate::search::index` 的 `S8_02_并发commit不丢失视图序号`。
    ///
    /// 🔑 这不只是「序号难看」：**同一形状原样带入 `S8-03`（delta 分段）会让两个 delta 段
    /// 拿到相同 `base_*`** ⇒ 段 ID 空间重叠 ⇒ 破坏 `I8-7`。⇒ 本方法是 `S8-03` 的**形状前置**。
    ///
    /// # 锁序与唯一发布路径
    ///
    /// 锁序 `ids` → `view`；反向路径不存在（[`Shared::snapshot`] 与
    /// [`Shared::with_main_mut`] 都只取 `view`）⇒ 无死锁。
    ///
    /// 本方法是**唯一**能替换 `view` 指针的地方（发布逻辑内联在此，不另开 `publish`）
    /// ⇒ 「锁外三段」在**可见性层面**写不出来。这是对评审 P2 的**结构性**对策：
    /// 不是加一个检查，而是**消除违反它的入口**。
    pub(crate) fn commit_view(&self) -> u64 {
        let mut ids = self.ids.lock().expect("发号锁中毒");
        let cur = Arc::clone(&self.view.read().expect("视图锁中毒"));

        // 下一段的基址 = 本段基址 + 本段长度（设计 §4.4.1：`base = 前序所有段长度之和`）。
        // ⚠️ `Index` 的计数是 `usize`、全局 ID 是 `u32`：本阶段（单段、`base = 0`）
        // 不会溢出；`S8-03` 引入真正的多段发号时须在此显式处理上限（设计 §4.4.3）。
        let next_base_doc = cur.main.base_doc + cur.main.index.total_docs() as DocId;
        let next_base_chunk = cur.main.base_chunk + cur.main.index.total_chunks() as ChunkId;
        ids.next_doc = next_base_doc;
        ids.next_chunk = next_base_chunk;
        ids.generation += 1;
        let generation = ids.generation;

        // 不变式守卫：发布序号必须严格大于当前视图序号。
        //
        // ⚠️ 在本临界区内它**结构性成立**（`ids.generation` 只增、`view` 只由本方法更新）
        // ⇒ 这是**防旁路守卫**（防未来有人从别处替换指针），**不是**可测行为 ——
        // 别把它当成「覆盖了回退场景」；那条判据在 `index.rs` 的并发用例里。
        debug_assert!(
            generation > cur.generation,
            "发布序号必须严格递增：ids = {generation}，view = {}",
            cur.generation
        );

        // ⚠️ I8-1：写锁内只有一次指针替换（内容全部来自 `cur`，不做计算 / I/O）。
        *self.view.write().expect("视图锁中毒") = Arc::new(View {
            main: Arc::clone(&cur.main),
            deltas: Arc::clone(&cur.deltas),
            tombstones: Arc::clone(&cur.tombstones),
            generation,
        });
        generation
    }
    /// 下一段的 `base_*`（`S8-03` 起由段构造消费；本阶段由 `commit()` 的 `tracing` 字段消费）。
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
    // ⚠️ **`Error::Busy` 而不是 `Error::InvalidInput`**（`S8-02` 评审 P4-5a）：
    // 这是**瞬态并发状态**（重试即可），不是参数错误（重试无用）。
    Error::Busy(
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

    /// **`S8-02`**：`commit_view` 之后 `generation` **单调递增**，且 `deltas` 仍空
    /// （骨架期提交**不产生新段**：内容只在 `main` 里，指针替换是唯一的可见性跃迁）。
    #[test]
    fn S8_02_提交只递增序号且不产生新段() {
        let sh = shared();
        let before = sh.snapshot();
        assert_eq!(sh.next_base(), (0, 0), "未提交时应为 (0, 0)");

        let g1 = sh.commit_view();
        assert_eq!(g1, 1, "序号应从 1 开始");
        assert_eq!(
            sh.next_base(),
            (0, 0),
            "空段（`Segment::empty`）⇒ 下一段基址仍为 (0, 0)"
        );
        let after = sh.snapshot();
        assert_eq!(after.generation, 1);
        assert!(after.deltas.is_empty(), "提交不得产生新段（骨架期）");
        assert!(
            Arc::ptr_eq(&after.main, &before.main),
            "骨架期提交必须复用同一个 main 段（不复制内容）"
        );

        let g2 = sh.commit_view();
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
        // ⚠️ 必须是**独立的瞬态变体**（`S8-02` 评审 P4-5a）：它表示「稍后重试即可」，
        // 不是「参数错了」。混用 `InvalidInput` 会让调用方无法区分这两者。
        assert!(
            matches!(err, Error::Busy(_)),
            "并发冲突必须报 `Error::Busy`（可重试），实际：{err:?}"
        );
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

    /// **`S8-02` 评审 P3-2**：跨段求和必须**真的遍历 `deltas`**。
    ///
    /// 🔑 判据刻意用 `raw_vectors` 而**不是** chunk / doc 计数：后者在
    /// `Segment::empty` 下恒 0，两段相加仍是 0 ⇒ 与「只读 `main`」**无差别**
    /// （那就是原缺陷能藏住的原因）。`raw_vectors` 可手工构造非零值 ⇒ 有鉴别力。
    #[test]
    fn S8_02_跨段求和真的遍历deltas() {
        let seg = |n: usize| {
            Arc::new(Segment {
                index: Index::new(),
                vector_index: None,
                raw_vectors: Some(vec![(0 as ChunkId, vec![1.0f32, 0.0]); n]),
                base_doc: 0,
                base_chunk: 0,
                generation: 0,
            })
        };
        let view = View {
            main: seg(1),
            deltas: Arc::from(vec![seg(1), seg(1)]),
            tombstones: Arc::new(Tombstones::default()),
            generation: 0,
        };
        assert_eq!(view.deltas.len(), 2, "前提：两个 delta 段");

        let s = view.sums();
        assert_eq!(
            s.raw_vectors, 3,
            "必须 1（main）+ 1 + 1（两个 delta）—— 只读 `main` 会得 1"
        );
        // 空 `Index` 的计数恒 0（这也是「用 raw_vectors 当判据」的理由）
        assert_eq!(s.chunks_total, 0);
        assert_eq!(s.docs_alive, 0);
    }

    /// **`S8-02`**：`View` 的「一次检索只用一个快照」语义 —— `snapshot()` 拿到的 `Arc<View>`
    /// 在后续提交之后**内容不变**（`I8-3` 的基石）。
    #[test]
    fn S8_02_已取出的快照不受后续提交影响() {
        let sh = shared();
        let held = sh.snapshot();
        let g = sh.commit_view();
        assert_eq!(g, 1);
        assert_eq!(held.generation, 0, "已取出的快照内容不得被后续提交改动");
        assert_eq!(sh.snapshot().generation, 1, "新快照必须看到新序号");
    }
}
