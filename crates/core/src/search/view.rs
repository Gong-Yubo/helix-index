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

    /// 段内本地 `chunk_id` → **全局** `chunk_id`（`base + local`，设计 §4.4.1）。
    pub(crate) fn global_chunk(&self, local: ChunkId) -> ChunkId {
        self.base_chunk + local
    }

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

    /// 把**全局** `chunk_id` 折算成 `(段下标, 段内本地 id)`；不属于任何段则 `None`。
    ///
    /// 段数极少（主段 + 少量 delta）⇒ **线性扫描 `base` 区间**（每段一次 `u32` 比较）即可。
    /// ⚠️ 刻意**不**为此建 `HashMap`：段数 ≤ 8 时更慢，且多一份要维护的一致性状态（§4.7.2）。
    ///
    /// 用的是 `total_chunks()`（**槽位**数，含墓碑）而非 `num_chunks()`（活数）——
    /// 全局 ID 空间覆盖的是槽位，死 chunk 也必须有合法折算（它会被存活判定挡掉）。
    pub(crate) fn locate(&self, global_chunk: ChunkId) -> Option<(usize, ChunkId)> {
        for (i, seg) in self.segments_in_order().enumerate() {
            if let Some(local) = global_chunk.checked_sub(seg.base_chunk) {
                if (local as usize) < seg.index.total_chunks() {
                    return Some((i, local));
                }
            }
        }
        None
    }

    /// **BM25 是否需要跨段谓词**（R52 热路径判据，`S8-04` / 设计 §4.7.3）。
    ///
    /// `false` ⇒ 调用方可以传 `filter = None`（零谓词热路径）。
    ///
    /// 🔑 判据是「**有跨段墓碑**」而非「有多段」：
    /// `remove` 落在**段内**时总是**物理摘除**该段的 postings（§4.8.3 分支 ①）⇒
    /// 活 postings 里**不可能**有本段已删的 chunk（与今天同理）。真正需要判活的只有
    /// **跨段墓碑**：目标 doc 在它所属的段里**仍然存活**、postings 也还在，只有墓碑
    /// 知道它已删。⇒ 无墓碑时保持 `None`，R52 的回归**只发生在确实有跨段墓碑时**。
    pub(crate) fn needs_bm25_tombstone_filter(&self) -> bool {
        !self.tombstones.is_empty()
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
    pub(crate) fn commit_view(
        &self,
        build_next: impl FnOnce(&Arc<View>, u64, DocId, ChunkId) -> (View, DocId, ChunkId),
    ) -> u64 {
        let mut ids = self.ids.lock().expect("发号锁中毒");
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
        generation
    }
    /// **重置式发布**（`compact` 专用）：`compact` 会**重编号**（稠密化）⇒ 新主段
    /// **比原来短**，且**整个旧 ID 空间作废**。
    ///
    /// ⚠️ 与 [`Self::commit_view`] 的唯一差别：这里**允许「已用长度变小」**。
    /// 这是 `I8-7`（段 ID 空间不重叠）的**唯一合法例外** —— 调用方必须保证发布后的视图里
    /// **没有任何段引用旧 ID 空间**（`compact` 正是如此：它重建了主段、清空 `deltas`
    /// 与跨段墓碑）。
    pub(crate) fn publish_reset(&self, next: View, used_doc: DocId, used_chunk: ChunkId) -> u64 {
        let mut ids = self.ids.lock().expect("发号锁中毒");
        ids.generation += 1;
        let generation = ids.generation;
        ids.next_doc = used_doc;
        ids.next_chunk = used_chunk;
        *self.view.write().expect("视图锁中毒") = Arc::new(View { generation, ..next });
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
        assert_eq!(sh.next_base(), (0, 0), "未提交时应为 (0, 0)");

        let g1 = sh.commit_view(empty_commit);
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

        let g2 = sh.commit_view(empty_commit);
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
        let g = sh.commit_view(empty_commit);
        assert_eq!(g, 1);
        assert_eq!(held.generation, 0, "已取出的快照内容不得被后续提交改动");
        assert_eq!(sh.snapshot().generation, 1, "新快照必须看到新序号");
    }
}
