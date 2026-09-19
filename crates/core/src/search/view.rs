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
//! | **I8-2** | **段一旦进入过任何 `View`，永不再被修改**。✅ **`S8-03` 起成立**：写端只写私有的 `SegmentBuilder`；`commit()` 把段追加进 `View.deltas`；`fold_deltas` **克隆**主段内容后发布**新** `main`；`compact` 亦发布新段 —— 三条路径都不就地改已发布的段。 |
//! | **I8-3** | **读端只在取快照那一刻取一次读锁**（[`Shared::snapshot`]），之后全程无锁；一次检索**只用一个 `View`**。 |
//!
//! 🔑 **I8-3 的最后半句是正确性的关键**：若一次检索中途重新取 view，可能出现
//! 「BM25 用了 v1 的段、向量用了 v2 的段」⇒ 跨 lane 的 chunk 语义不一致。
//!
//! # ⚠️ PR3 的过渡约束（**已在 `S8-03` 解除**，此处留作历史）
//!
//! `deltas` 恒空 ⇒ 内容只有一份（`main`）⇒ 那时写入必须**独占**该段（用 `Arc::get_mut`，
//! 要求**没有读者持有当前 `Arc<View>`**）⇒ 有并发检索进行中时会返回 `Err`。
//! 🔴 **该入口（`with_main_mut`）与配套的 `write_needs_exclusive()` 已随 `S8-03` 一起删除**
//! （`S8-03` 评审 P3-4.4 / P4-3：生产路径已无调用点，留着只会腐烂）。
//!
//! **这不是终态**：`S8-03` 引入 `SegmentBuilder` + delta 追加后，写入落在写端私有的
//! builder 上、不再需要 `get_mut`，该约束随之消失（`FR-17` 的「读不阻塞写」
//! 由 `S8-03` ~ `S8-06` 交付）。

use std::sync::{Arc, Mutex, RwLock};

use crate::error::{Error, Result};
use crate::index::Index;
use crate::predicate::{CandidateFilter, FilterKind};
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

    // ⚠️ 原 `Segment::global_chunk()` 已删除（**零调用点**）：跨段向量折算（`S8-05`）
    //    需要时再加 —— 不留 dead code（`clippy -D warnings` 会红）。
    //    `global_doc()` 有调用点（跨段查重），保留。

    /// 段内本地 `doc_id` → **全局** `doc_id`。
    pub(crate) fn global_doc(&self, local: DocId) -> DocId {
        self.base_doc + local
    }
}

/// 跨段墓碑（针对**既往段**的全局 doc 墓碑）。
///
/// ⚠️ **`PR3` 阶段恒为空**：内容只有一段、且删除仍走 `Index::remove` 的**物理**路径
/// （与今天逐位一致）。`S8-03` 起「删除落在既往段上」的 doc 记进这里，
/// 由 `S8-06` 的合并**物理化**（设计 §4.9.2）。
#[derive(Debug, Clone, Default)]
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

    /// 记一条墓碑（**幂等**）；`doc_id` 是**全局** ID。
    ///
    /// 保持**升序去重**：这样 `tombstone_stats` 的扣减与合并时的物理化遍历顺序都确定
    /// （NFR-06），且 `blocks_doc` 可以用二分。
    pub(crate) fn add(&mut self, doc_id: DocId) {
        match self.doc_ids.binary_search(&doc_id) {
            Ok(_) => {}
            Err(pos) => self.doc_ids.insert(pos, doc_id),
        }
    }

    /// 该 doc 是否已被墓碑挡住（`O(log n)`）。
    pub(crate) fn blocks_doc(&self, doc_id: DocId) -> bool {
        self.doc_ids.binary_search(&doc_id).is_ok()
    }

    /// 按升序遍历全部墓碑（合并时**物理化**用，设计 §4.9.2）。
    pub(crate) fn iter(&self) -> impl Iterator<Item = DocId> + '_ {
        self.doc_ids.iter().copied()
    }

    /// 移除一条墓碑（物理化之后调用 ⇒ 热路径可回到零谓词，见 §4.7.3）。
    pub(crate) fn remove(&mut self, doc_id: DocId) {
        if let Ok(pos) = self.doc_ids.binary_search(&doc_id) {
            self.doc_ids.remove(pos);
        }
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

    /// **最新优先**：`deltas` 末尾 → … → `main`（跨段 `content_hash` 查重的顺序，
    /// 设计 §4.8.3：越新的段越可能是「同一逻辑文档」的最近一次写入）。
    pub(crate) fn segments_newest_first(&self) -> impl Iterator<Item = &Arc<Segment>> {
        self.deltas.iter().rev().chain(std::iter::once(&self.main))
    }

    /// **FIFO 顺序**的全部段：`main` 在最前，其后依次 `deltas[0], deltas[1], …`。
    ///
    /// ⚠️ 这是跨段遍历的**唯一**顺序（设计 §4.4.2 / §4.5.2）：合并只能从头消费一个前缀
    /// （否则基址不变式破裂），BM25 的 TAAT 也必须按它累加（否则 `I8-6` 破）。
    pub(crate) fn segments_in_order(&self) -> impl Iterator<Item = &Arc<Segment>> {
        std::iter::once(&self.main).chain(self.deltas.iter())
    }

    /// BM25 的**全局统计量**（`S8-04` / 设计 §4.5.1）：返回 `(N, total_len)`。
    ///
    /// - `N` = Σ 各段**活**分片数（`Index::num_chunks` = `Stats::num_chunks`）；
    /// - `total_len` = Σ 各段 `Stats::total_len`。
    ///
    /// 🔴 **必须是精确整数和**（`I8-5`）：`avgdl` 要用**全局** `total_len / N` 算**一次**。
    /// 若各段各算 `avgdl` 再加权平均，会引入两次浮点舍入 ⇒ 分数**只在分段布局下**漂移
    /// （结果「看起来对」，但此后所有质量对比失去可比性 —— 这正是 `S8-T4` 要钉的东西）。
    pub(crate) fn bm25_totals(&self) -> (u32, u64) {
        let mut n = 0u32;
        let mut total = 0u64;
        for seg in self.segments_in_order() {
            n += seg.index.num_chunks();
            total += seg.index.total_len();
        }
        (n, total)
    }

    // ⚠️ 原 `View::locate()` 已删除（`S8-03` 评审 P4-2：`locate` 有三份等价实现）：
    //    真正被调用的两份是 `ViewFilter::locate`（跨段谓词用，见本文件下方）与
    //    `SegmentSet::locate`（`retriever::bm25`，检索回捞用）；这一份**零调用点**
    //    （`dead_code` 会让 `clippy -D warnings` 红），删掉以免三处口径分叉。
    //    ⚠️ 折算语义（`checked_sub` + 槽位数上界、死 chunk 也有合法折算）在两份实现里保持。

    // ⚠️ 删除了原 `needs_bm25_tombstone_filter()`（`S8-03` 评审 P4-3，**零调用点**）：
    //    「有跨段墓碑 ⇒ 才需要对 BM25 传谓词」这条 R52 热路径判据的**唯一权威落点**现在是
    //    `query/searcher.rs` 的 `bm25_needs_filter`（内联在热路径上，省掉每 posting 一次
    //    dyn 调用）。留同义助手 ⇒ 两处口径可能分叉。
    //
    // 📌 判据为什么是「有墓碑」而不是「有多段」：`remove` 落在**段内**时总是**物理摘除**
    //    该段的 postings（§4.8.3 分支 ①）⇒ 活 postings 里不可能有本段已删的 chunk。
    //    真正需要判活的只有**跨段墓碑**（目标 doc 在它所属的段里仍存活、postings 还在）。
    // ⚠️ 设计 §4.7.3 的「有 delta 但无墓碑 ⇒ 逐段 `AliveOnly`」**实现没有采纳**：
    //    无墓碑时直接零谓词（**实现优于设计文本**），设计文本待同步（评审 P3-4.5）。
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
    /// **ID 空间世代**（`S8-03` 评审 **P2-4** 的修复）：只在
    /// [`Shared::publish_reset`]（`compact` 重编号）时 `+1`。
    ///
    /// # 为什么「基址等值」不够
    ///
    /// 段的基址对账（[`Shared::commit_view`] 的 `used >= base` 与 `commit()` 的
    /// `(base_doc, base_chunk) == expected`）只对**段的槽位基址**是严的，
    /// 对**引用旧 ID 空间的墓碑 / 去重条目**不严：
    ///
    /// ```text
    /// 写端 B 的 builder：base_doc = 1，墓碑 {doc 0}（指向旧 ID 空间）
    /// 写端 A：compact() ⇒ 重编号 ⇒ ids 归零；随后 A 再写 1 篇 ⇒ ids.next_doc 回到 1
    /// B.commit()：base 1 == 1 ⇒ **等值对账通过**（ABA 窗口）⇒ 过期墓碑被并进视图
    ///              ⇒ 下一次物理化 `Index::remove(0)` 删掉的是**重编号后的另一个文档**
    /// ```
    ///
    /// ⇒ 世代号让这条窗口**结构上不存在**：`publish_reset` 一发生，
    /// 所有既有 builder / 合并快照的发布就全部被拒（不是「碰巧不相等」）。
    pub(crate) epoch: u64,
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
        // 🔴 发号器**必须**从「主段已用的长度」起步，而不是 `default()`（全 0）：
        //    `load` 出来的主段已经有 N 个槽位，而**新段**的基址 = 前序所有段长度之和
        //    （§4.4.1）⇒ 若从 0 起步，新段的本地 ID `i` 会被折算成全局 `i`，与主段的
        //    全局 ID **直接重叠** ⇒ 破坏 `I8-7`（不同 chunk 的分数被合并 = 结果错）。
        //    实测：`S6_T8_追加分配的id严格大于历史最大id` 正是钉这个不变量。
        let next_doc = main.index.total_docs() as DocId;
        let next_chunk = main.index.total_chunks() as ChunkId;
        Self {
            view: RwLock::new(Arc::new(View::single(main))),
            ids: Mutex::new(IdAllocator {
                next_doc,
                next_chunk,
                generation: 0,
                // `load` / 新建时是「第 0 代 ID 空间」；只有 `compact` 重编号才会换代
                epoch: 0,
            }),
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
    /// 锁序 `ids` → `view`；反向路径不存在（[`Shared::snapshot`] 与发布路径都只取
    /// `view`、不回头取 `ids`）⇒ 无死锁。
    ///
    /// 本方法是**唯一**能替换 `view` 指针的地方（发布逻辑内联在此，不另开 `publish`）
    /// ⇒ 「锁外三段」在**可见性层面**写不出来。这是对评审 P2 的**结构性**对策：
    /// 不是加一个检查，而是**消除违反它的入口**。
    /// **合并发布时的「窗口吸收」**（`S8-03` 评审 **P1-1** 的修复核心，纯函数以便单测）。
    ///
    /// # 背景（原缺陷）
    ///
    /// `fold_deltas` / `compact` 属于「读-改-发布」路径：它们在**锁外**读快照、克隆合并，
    /// 再进 `commit_view` 替换指针。若另一个写端在「读快照」与「拿锁」之间提交了新 delta，
    /// 而发布闭包**无视锁内的 `cur`**（原写法是 `move |_cur, …|`），那个 delta 会：
    /// ① 从视图里**消失**（读不到已提交内容）；② `ids.next_*` **回缩** ⇒ 下一个 builder 的
    /// 基址落回已发号区间 ⇒ **ID 复用**（`AddOutcome` 给出的 ID 被后续内容别名）。
    /// debug 下只靠 `used >= base` 断言偶然拦一下，release 下**全静默**。
    ///
    /// # 修法：吸收，而不是报错
    ///
    /// 合并**不改 ID**（`D-S8-04`：`merge_from` 按本地顺序 append、不重编号）⇒ 主段吸收快照里
    /// 那些 delta 之后，**全局 ID 空间的长度不变**；窗口内新提交的 delta 的 `base_*` 是它
    /// 提交时算的「前序所有段长度之和」，恰好等于吸收后的长度 ⇒ **基址仍然有效**，
    /// 直接挂在 `main` 之后即可（FIFO 不变式 `main, deltas[0], …` 保持）。
    ///
    /// 墓碑同理：本次只物理化了**快照里**的墓碑 ⇒ 从 `cur.tombstones` 里**逐个摘掉它们**，
    /// 窗口内新增的墓碑**原样保留**（它们的 `fold` 留给下一次）。
    ///
    /// # 前提
    ///
    /// `cur_deltas.len() >= snapshot_deltas`（delta 只增）。**等号**在单写端恒成立；
    /// 严格大于即「窗口内有提交」。⚠️ 若**小于**（并发 `compact` 重编号过视图），
    /// 本函数的假设不成立 —— 那条路径**已由 [`Shared::commit_view`] 的 `expected_epoch`
    /// 对账在进入本函数之前拦下**（`S8-03` 评审 P2-4，本 PR 落地）；此处的 `debug_assert`
    /// 是**第二道**防旁路守卫（防将来有人绕过 `commit_view` 直接发布）。
    pub(crate) fn absorb_window(
        cur_deltas: &[Arc<Segment>],
        snapshot_deltas: usize,
        cur_tombstones: &Tombstones,
        physicalized: &Tombstones,
    ) -> (Arc<[Arc<Segment>]>, Arc<Tombstones>) {
        debug_assert!(
            cur_deltas.len() >= snapshot_deltas,
            "发布窗口内 delta 数不得减少：cur = {}，snapshot = {}（并发 compact 重编号过视图？\
             —— 该组合应由 commit_view 的 epoch 对账拦下，若走到这里说明有旁路发布点）",
            cur_deltas.len(),
            snapshot_deltas
        );
        let carried: Vec<Arc<Segment>> = cur_deltas.iter().skip(snapshot_deltas).cloned().collect();
        let mut tombstones = cur_tombstones.clone();
        for doc in physicalized.iter() {
            tombstones.remove(doc);
        }
        (Arc::from(carried), Arc::new(tombstones))
    }

    /// # `expected_epoch`（`S8-03` 评审 **P2-4** 的修复）
    ///
    /// 调用方把「它据以构造新视图的那一代 ID 空间」传进来（builder 创建时记录的
    /// `epoch`；`fold_deltas` 则是取快照时读到的 `epoch`）。若与锁内世代不符 ⇒
    /// **拒绝发布**并返回 [`Error::Busy`] —— 因为 `compact` 的重编号已经把
    /// 「基址 / 墓碑 / 去重条目」全部作废，此时**基址等值检查是失效的**
    /// （见 [`IdAllocator::epoch`] 的 ABA 推演）。
    ///
    /// ⚠️ 这是**纯加法**：单写端下 `epoch` 只在同一个 `&mut self` 的 `compact()` 内部变，
    /// 而 `compact()` 之后**总是立刻换新 builder** ⇒ 单写端永不触发。
    pub(crate) fn commit_view(
        &self,
        expected_epoch: u64,
        build_next: impl FnOnce(&Arc<View>, u64, DocId, ChunkId) -> (View, DocId, ChunkId),
    ) -> Result<u64> {
        let mut ids = self.ids.lock().expect("发号锁中毒");
        if ids.epoch != expected_epoch {
            return Err(Error::Busy(format!(
                "ID 空间已换代（epoch {expected_epoch} → {}，`compact()` 的重编号会作废整个旧 ID \
                 空间）⇒ 本次发布依据的基址 / 跨段墓碑 / 去重条目都可能指向旧 ID 空间，已拒绝发布。\
                 ⚠️ 本写端**未提交内容已被丢弃**（builder 已换成与当前世代对齐的新 builder），\
                 重试**不会**恢复它；请重新 `add` 后再 `commit()`",
                ids.epoch
            )));
        }
        let cur = Arc::clone(&self.view.read().expect("视图锁中毒"));
        // 新段的基址 = 「前序所有段的槽位数之和」= 上一次发布后的已用长度（§4.4.1 / §4.4.2）
        let (base_doc, base_chunk) = (ids.next_doc, ids.next_chunk);
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

        // `build_next` 在锁内构造新视图（`S8-03`）：它拿到的基址与序号**就是本次发布的**
        // ⇒ 「算基址 → 追加段 → 发布」不可能被另一个写端插队（`I8-7` 的结构性保证）。
        let (next, used_doc, used_chunk) = build_next(&cur, generation, base_doc, base_chunk);
        debug_assert!(
            used_doc >= base_doc && used_chunk >= base_chunk,
            "已用长度不得回退：base = ({base_doc}, {base_chunk})，\
             used = ({used_doc}, {used_chunk})（回退会让下一段的基址落在本段之前）"
        );
        ids.next_doc = used_doc;
        ids.next_chunk = used_chunk;

        // ⚠️ I8-1：`view` 的写锁内只有一次指针替换（`next` 已在锁内构造完毕）。
        *self.view.write().expect("视图锁中毒") = Arc::new(next);
        Ok(generation)
    }
    /// **重置式发布**（`compact` 专用）：`compact` 会**重编号**（稠密化）⇒ 新主段
    /// **比原来短**，且**整个旧 ID 空间作废**。
    ///
    /// ⚠️ 与 [`Self::commit_view`] 的唯一差别：这里**允许「已用长度变小」**。
    /// 这是 `I8-7`（段 ID 空间不重叠）的**唯一合法例外** —— 调用方必须保证发布后的视图里
    /// **没有任何段引用旧 ID 空间**（`compact` 正是如此：它重建了主段、清空 `deltas`
    /// 与跨段墓碑）。
    ///
    /// 🔴 **本方法同时把 ID 空间世代 `+1`**（`S8-03` 评审 P2-4）：这是「作废旧 ID 空间」
    /// 这件事**唯一**的记账点。之后任何持旧 `epoch` 的写端（另一个写端的 builder）或
    /// 旧快照（`fold_deltas`）走到 [`Self::commit_view`] 都会被拒 —— 包括那种
    /// 「重编号后基址恰好又相等」的 ABA 窗口。
    pub(crate) fn publish_reset(&self, next: View, used_doc: DocId, used_chunk: ChunkId) -> u64 {
        let mut ids = self.ids.lock().expect("发号锁中毒");
        ids.generation += 1;
        let generation = ids.generation;
        ids.next_doc = used_doc;
        ids.next_chunk = used_chunk;
        ids.epoch += 1;
        *self.view.write().expect("视图锁中毒") = Arc::new(View { generation, ..next });
        generation
    }

    /// 下一段的「出生点」：`(base_doc, base_chunk, epoch)`（`S8-03` 起由段构造消费）。
    ///
    /// ⚠️ **三项必须在同一把锁内一次取回**：分两次调用（先 `next_base` 再 `epoch`）时，
    /// 另一个写端可以在两次调用之间 `compact()` ⇒ 得到「旧世代的基址 + 新世代的 epoch」
    /// 这种**自相矛盾**的 builder（它的对账会通过，正是 P2-4 要堵的窗口）。
    pub(crate) fn next_origin(&self) -> (DocId, ChunkId, u64) {
        let ids = self.ids.lock().expect("发号锁中毒");
        (ids.next_doc, ids.next_chunk, ids.epoch)
    }

    /// 当前 ID 空间世代（`S8-03` 评审 P2-4）。取快照的写路径（`fold_deltas`）必须在
    /// **取视图快照的同时**记下它，并在 [`Self::commit_view`] 里回报 ⇒ 期间发生过
    /// `compact()` 重编号就必须拒绝发布，而不是拿旧快照的段/墓碑去覆盖新视图。
    pub(crate) fn epoch(&self) -> u64 {
        self.ids.lock().expect("发号锁中毒").epoch
    }

    // ⚠️ 原 `with_main_mut()`（`S8-02` 的过渡入口）已删除（`S8-03` 评审 P3-4.4）：
    //    `S8-03` 起写端只写**自己私有的** builder，不再就地改已发布的段 ⇒ 该入口
    //    在生产路径上零调用点（`dead_code` 会让 `clippy -D warnings` 直接红）。
    //    「有读者时写入失败」这条过渡期约束随之消失，对应用例也已删除。
}

/// **跨段**候选谓词（`S8-04` / 设计 §4.7.1）。
///
/// 与单段的 `ChunkFilter` / `AliveOnly` 的两点差别：
/// ① 判定前要先 [`View::locate`] 到段（全局 → 段内 ID），再在该段内判存活 + doc 位图；
/// ② 要额外挡掉**跨段墓碑**指向的 doc（它们在所属段里仍然存活 —— 那正是墓碑存在的理由）。
///
/// # ⚠️ `allowed_count()` 必须**精确**（`predicate.rs` 的硬契约）
///
/// ```text
/// allowed = Σ_seg （该段通过过滤的活分片数）
///         − Σ_{墓碑 doc} （该 doc 在所属段里通过过滤的活分片数）
/// ```
///
/// 后半项**必须精确计算**、**不得估算**：向量路用它做 `prefers_exact` 分派，一旦估算会
/// **静默失效**（结果仍然正确，只是低选择度退化成精确扫描，且只表现为标定表里
/// 「精确占比 0%」，极难归因）。
pub(crate) struct SegmentFilter<'a> {
    /// 该段的段内索引（存活位图的来源）
    index: &'a Index,
    /// 该段内**通过用户过滤**的 doc 位图（**段内** `doc_id`）。
    /// `None` = 无用户过滤（全部通过）—— 这样 `AliveOnly` 形态无需构造位图（`O(1)`）。
    doc_bits: Option<crate::bitmap::DocBits>,
    /// 段基址（全局 ↔ 段内折算）
    base_doc: DocId,
    base_chunk: ChunkId,
}

impl<'a> SegmentFilter<'a> {
    /// 无用户过滤（只判存活）。
    fn alive_only(seg: &'a Segment) -> Self {
        Self {
            index: &seg.index,
            doc_bits: None,
            base_doc: seg.base_doc,
            base_chunk: seg.base_chunk,
        }
    }

    /// 有用户过滤（`doc_bits` 按**段内** `doc_id` 索引）。
    fn filtered(seg: &'a Segment, doc_bits: crate::bitmap::DocBits) -> Self {
        Self {
            index: &seg.index,
            doc_bits: Some(doc_bits),
            base_doc: seg.base_doc,
            base_chunk: seg.base_chunk,
        }
    }

    /// 段内本地 `chunk_id` 是否通过（存活 + 用户过滤）。
    fn chunk_allowed(&self, local_chunk: ChunkId) -> bool {
        if !self.index.alive_chunks().contains(local_chunk) {
            return false;
        }
        match (self.doc_bits.as_ref(), self.index.doc_of(local_chunk)) {
            (None, _) => true,
            (Some(bits), Some(local_doc)) => bits.contains(local_doc),
            (Some(_), None) => false,
        }
    }

    /// 段内本地 `doc_id` 是否通过**用户过滤**（墓碑扣减用）。
    fn doc_passes_filter(&self, local_doc: DocId) -> bool {
        match self.doc_bits.as_ref() {
            None => true,
            Some(bits) => bits.contains(local_doc),
        }
    }

    /// 本段通过过滤的**活分片数**（精确）。
    fn allowed_chunks(&self) -> usize {
        match self.doc_bits.as_ref() {
            // 无用户过滤 ⇒ 直接是存活数（位图缓存，`O(1)`）
            None => self.index.alive_count(),
            Some(bits) => crate::query::filter::allowed_chunk_count(bits, self.index),
        }
    }

    /// 全局 `doc_id` 是否落在本段。
    fn owns_doc(&self, global_doc: DocId) -> bool {
        global_doc >= self.base_doc
            && ((global_doc - self.base_doc) as usize) < self.index.total_docs()
    }
}

/// 跨段谓词本体（`CandidateFilter` 的实现）。
pub(crate) struct ViewFilter<'a> {
    /// 与 `View::segments_in_order()` **一一对应**（顺序也一致）
    per_seg: Vec<SegmentFilter<'a>>,
    /// 针对**既往段**的全局 doc 墓碑（命中即挡）
    tombstones: &'a Tombstones,
    allowed: usize,
    kind: FilterKind,
}

impl<'a> ViewFilter<'a> {
    /// 组装：`per_seg` 必须与 FIFO 段列表一一对应；`allowed` 在此算清。
    fn new(view: &'a View, per_seg: Vec<SegmentFilter<'a>>, kind: FilterKind) -> Self {
        debug_assert_eq!(
            per_seg.len(),
            view.segments_in_order().count(),
            "per_seg 必须与 FIFO 段列表一一对应（含顺序）"
        );
        let mut allowed: usize = per_seg.iter().map(|s| s.allowed_chunks()).sum();
        // 🔴 精确扣减：墓碑 doc 在所属段里仍存活（postings 也在），必须从 `allowed` 里去掉
        // 它**通过过滤**的那些活分片 —— 不得估算（硬契约，见类型文档）。
        for doc in view.tombstones.iter() {
            if let Some(s) = per_seg.iter().find(|s| s.owns_doc(doc)) {
                let local_doc = doc - s.base_doc;
                if s.doc_passes_filter(local_doc) {
                    allowed =
                        allowed.saturating_sub(s.index.chunk_count_of_doc(local_doc) as usize);
                }
            }
        }
        Self {
            per_seg,
            tombstones: view.tombstones.as_ref(),
            allowed,
            kind,
        }
    }

    /// 无用户过滤形态：只判存活 + 墓碑（各段位图**借用**，零重建成本）。
    pub(crate) fn alive_only(view: &'a View) -> Self {
        let per_seg = view
            .segments_in_order()
            .map(|seg| SegmentFilter::alive_only(seg))
            .collect();
        Self::new(view, per_seg, FilterKind::Alive)
    }

    /// 有用户过滤形态：`per_seg_bits` 是**逐段**求好的「段内 `doc_id`」位图（FIFO 顺序）。
    ///
    /// 逐段求值（而不是拼一张全局位图）的原因：字段索引是**段内**结构（`FieldIndex`
    /// 按段内 `doc_id` 登记），跨段拼装只会多一份要维护的状态。
    pub(crate) fn filtered(view: &'a View, per_seg_bits: Vec<crate::bitmap::DocBits>) -> Self {
        debug_assert_eq!(
            per_seg_bits.len(),
            view.segments_in_order().count(),
            "`per_seg_bits` 必须与 FIFO 段列表一一对应"
        );
        let per_seg = view
            .segments_in_order()
            .zip(per_seg_bits)
            .map(|(seg, bits)| SegmentFilter::filtered(seg, bits))
            .collect();
        Self::new(view, per_seg, FilterKind::Filtered)
    }

    /// 全局 → `(段下标, 段内本地 id)`（谓词的定位入口）。
    fn locate(&self, global_chunk: ChunkId) -> Option<(usize, ChunkId)> {
        for (i, s) in self.per_seg.iter().enumerate() {
            if let Some(local) = global_chunk.checked_sub(s.base_chunk) {
                if (local as usize) < s.index.total_chunks() {
                    return Some((i, local));
                }
            }
        }
        None
    }
}

impl CandidateFilter for ViewFilter<'_> {
    fn contains(&self, global_chunk_id: ChunkId) -> bool {
        let Some((i, local)) = self.locate(global_chunk_id) else {
            return false;
        };
        let s = &self.per_seg[i];
        // ① 跨段墓碑：doc 在该段内**仍然存活**，只有墓碑知道它已删（§4.8.3 分支 ②）
        if let Some(local_doc) = s.index.doc_of(local) {
            if self.tombstones.blocks_doc(s.base_doc + local_doc) {
                return false;
            }
        }
        // ② 存活 + 用户过滤
        s.chunk_allowed(local)
    }

    fn allowed_count(&self) -> usize {
        self.allowed
    }

    fn kind(&self) -> FilterKind {
        self.kind
    }
}

// ⚠️ 原 `write_needs_exclusive()` 已随 `with_main_mut()` 一起删除（`S8-03` 评审 P3-4.4）：
//    它是 `S8-02` 过渡期的唯一 `Error::Busy` 产生点；`S8-03` 起该变体的产生点是
//    `SearchIndex::commit()` 的**基址对账**（另一个写端插队），语义不同（**不可重试**），
//    已同步更正 `error.rs` 的文档。

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;
    use crate::search::config::SearchIndexBuilder;

    /// 测试用「空提交」闭包：**不追加任何段**、`generation` 照常 +1
    /// （= `S8-02` 骨架期 `commit_view` 的行为，也是 `S8-03` 起「空 builder 不追加段」的等价物）。
    ///
    /// 它是**函数项**（不是闭包），因此实现 `FnOnce` 且可被反复传入。
    fn empty_commit(
        cur: &Arc<View>,
        generation: u64,
        base_doc: DocId,
        base_chunk: ChunkId,
    ) -> (View, DocId, ChunkId) {
        (
            View {
                main: Arc::clone(&cur.main),
                deltas: Arc::clone(&cur.deltas),
                tombstones: Arc::clone(&cur.tombstones),
                generation,
            },
            base_doc,
            base_chunk,
        )
    }

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
        assert_eq!(sh.next_origin(), (0, 0, 0), "未提交时应为 (0, 0) 且第 0 代");
        assert_eq!(sh.epoch(), 0, "未 compact ⇒ 恒第 0 代 ID 空间");

        let g1 = sh.commit_view(0, empty_commit).expect("第 0 代发布应通过");
        assert_eq!(g1, 1, "序号应从 1 开始");
        let (bd, bc, ep) = sh.next_origin();
        assert_eq!(
            (bd, bc),
            (0, 0),
            "空段（`Segment::empty`）⇒ 下一段基址仍为 (0, 0)"
        );
        assert_eq!(ep, 0, "普通发布不得换代");
        let after = sh.snapshot();
        assert_eq!(after.generation, 1);
        assert!(after.deltas.is_empty(), "提交不得产生新段（骨架期）");
        assert!(
            Arc::ptr_eq(&after.main, &before.main),
            "骨架期提交必须复用同一个 main 段（不复制内容）"
        );

        let g2 = sh.commit_view(0, empty_commit).expect("第 0 代发布应通过");
        assert_eq!(g2, 2, "序号必须单调递增");
    }

    /// **`S8-03` 评审 P2-4 的回归锁（P2-4 的核心判据）**：`publish_reset`（`compact` 重编号）
    /// 之后，持**旧世代**的写端再发布必须被**拒绝**，不能只靠「基址等值」。
    ///
    /// 🔑 判据刻意构造成 **ABA 形态**：让 `publish_reset` 之后的 `next_*` 与发布前**完全相等**
    /// ⇒ 基址等值检查在这种情况下**恒通过**，唯一能拦住它的是 `epoch`。
    /// 这条用例在「只有基址对账」的实现下必然红（`commit_view` 不会返回 `Err`）。
    #[test]
    fn S8_03_换代后旧世代发布被拒_基址等值也拦不住ABA() {
        let sh = shared();
        // 前提：publish_reset 前后「**基址**」完全相等（= ABA 窗口的最小构造）；
        // ⚠️ 比的是**基址两项**，不是整个 `next_origin()`（后者含 `epoch`，换代后必然不等）。
        let (bd0, bc0, ep0) = sh.next_origin();
        sh.publish_reset(View::single(Arc::new(Segment::empty(None))), 0, 0);
        let (bd1, bc1, ep1) = sh.next_origin();
        assert_eq!(
            (bd1, bc1),
            (bd0, bc0),
            "前提：本用例要构造的是「基址**等值**」的 ABA 窗口 ⇒ 前后基址必须相同"
        );
        assert_eq!((ep0, ep1), (0, 1), "publish_reset 必须换代");

        // 旧世代的发布被拒（★ 这条是新增的鉴别力：只有基址检查的实现会放行）
        let err = sh.commit_view(0, empty_commit).unwrap_err();
        assert!(
            matches!(err, Error::Busy(_)),
            "换代后旧世代的发布必须报 Busy，实际 = {err:?}"
        );
        assert!(
            err.to_string().contains("epoch 0 → 1"),
            "错误消息必须点名换代（可诊断，NFR-07），实际 = {err}"
        );

        // 新世代的发布照常
        assert_eq!(
            sh.commit_view(1, empty_commit).expect("新世代发布应通过"),
            2,
            "被拒的那次不得消耗序号"
        );
    }

    // ⚠️ 原用例 `S8_02_有读者时写入必须失败` 已按承诺删除（`S8-03` 评审 P3-4.4）：
    //    它锁的过渡期约束（`with_main_mut` 在有读者时必须失败）随 `S8-03` 落地而消失
    //    —— 现在**写入根本不再碰已发布的段**（写端私有 builder），该断言无处可依。
    //    ⚠️ **不是**改成「永远绿」的模糊断言，而是连同入口一起移除。
    //
    //    替代覆盖见 `crates/core/tests/step8_rw_concurrency.rs`（读写并存的活性判据）
    //    与 `S8_03_未commit不可见_commit后立即可见`（可见性边界）。

    /// **`S8-03` 评审 P1-1 的回归锁**：`absorb_window` 必须**无损吸收**窗口内新提交的 delta，
    /// 且只摘掉**本次已物理化**的墓碑。
    ///
    /// 🔑 为什么值得单测：真实的交错（另一写端恰在「取快照」与「拿锁」之间提交）在集成测试里
    /// **无法确定性构造**（需要线程时序）；而缺陷本身是**纯函数级**的 —— 「锁内的 `cur`
    /// 被无视」⇒ 把它抽成纯函数之后，判据就有了确定性、有牙齿：
    /// 删掉吸收（回到 `deltas = []`）本用例立刻红。
    #[test]
    fn S8_03_窗口吸收保留新提交的delta且只摘已物理化的墓碑() {
        let d1 = Arc::new(Segment::empty(None));
        let d2 = Arc::new(Segment::empty(None));
        let d3 = Arc::new(Segment::empty(None));
        let cur_deltas: Vec<Arc<Segment>> = vec![d1, d2, d3];

        let cur_tomb = Tombstones {
            doc_ids: vec![7, 9],
        };
        // 本次 fold 物理化的是快照里那批（只含 7）
        let physicalized = Tombstones { doc_ids: vec![7] };

        // 快照只有 1 个 delta ⇒ 窗口内新增了 d2 / d3 ⇒ 必须原样带上
        let (carried, tomb) = Shared::absorb_window(&cur_deltas, 1, &cur_tomb, &physicalized);
        assert_eq!(carried.len(), 2, "窗口内新提交的 delta 不得被丢弃");
        assert!(
            Arc::ptr_eq(&carried[0], &cur_deltas[1]),
            "必须按 FIFO 原样带上"
        );
        assert!(!tomb.blocks_doc(7), "本次已物理化的墓碑必须从视图里摘掉");
        assert!(tomb.blocks_doc(9), "窗口内新增的墓碑必须保留");

        // 无窗口新增时行为与旧写法等价（deltas 清空）
        let (carried, tomb) = Shared::absorb_window(&cur_deltas, 3, &cur_tomb, &physicalized);
        assert!(carried.is_empty());
        assert!(!tomb.blocks_doc(7));
        assert!(tomb.blocks_doc(9));
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
        let g = sh.commit_view(0, empty_commit).expect("第 0 代发布应通过");
        assert_eq!(g, 1);
        assert_eq!(held.generation, 0, "已取出的快照内容不得被后续提交改动");
        assert_eq!(sh.snapshot().generation, 1, "新快照必须看到新序号");
    }
}
