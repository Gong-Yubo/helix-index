//! 写端门面：`SearchIndex`（拥有型；持 `Arc<Shared>` 与**私有**写缓冲）。
//!
//! # 可见性语义（p6-design 6.3；`S8-02` 起见 `super::view`）
//!
//! **`S8-03` 起：可见性 = `commit()` 后**（NFR-11 / 设计 §1.3 第 7 条）。`add` / `flush`
//! 写的是**写端私有的** `SegmentBuilder` ⇒ 读端看不到；`commit()` = `flush` → **封段** →
//! 持 `ids` 锁**原子追加进 `View.deltas`** → 发布新 `View`（`generation` +1）。
//! ⚠️ `S8-02`（视图骨架）期**恰好相反**（`deltas` 恒空、`add` 直写 `main` ⇒ 立即可见，
//! 与重构前逐位一致）；那条语义**按计划在本 PR 收紧**，对应用例已改名 + 反转
//! （`tests/step8_segments.rs` 的 `S8_03_未commit不可见_commit后立即可见`）。
//!
//! `searcher(&self)` **不隐含 flush**（对齐 NFR-11：可见性 = `commit()` 后）；
//! 旧的 `into_searcher()` 保留为 `#[deprecated]` 薄封装 = `commit()` + `searcher()`，
//! **行为与重构前逐位一致**（41 处既有调用点零改动）。

use std::sync::Arc;

use crate::document::{content_hash, DocRecord, Document};
use crate::error::{Error, Result};
use crate::index::Index;
use crate::types::{ChunkId, DocId};
use crate::vector::{
    BruteForceIndex, HnswRsIndex, NormalizedVector, VectorGraphPersist, VectorIndex,
};

use super::config::{Config, GraphPersistMode, SearchIndexBuilder, VectorBackend};
use super::view::{Segment, Shared, Tombstones, View};

/// 图 sidecar 的状态（V2 Step 2 / NFR-07：降级不能静默）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphStatus {
    /// 图从 sidecar 加载成功（冷启动快路径）
    Loaded,
    /// 不适用（Brute 后端 / 纯 BM25 / 无向量）
    NotApplicable,
    /// 降级重建（含原因：manifest 缺失 / CRC 不符 / 参数漂移 / 平台不符…）
    ///
    /// 语义：**用了图以外的东西重建**，检索结果仍然正确，只是冷启动慢。
    Rebuilt(String),
    /// 写侧失败：图**没**落盘（非 Strict 模式下 `save()` 仍返回 Ok，快照完好）。
    ///
    /// 与 [`GraphStatus::Rebuilt`] 的区别：这里什么都没重建，也不是「读不到图」，
    /// 而是「写图失败」——混进 Rebuilt 会让使用者误以为重建已完成（评审 #12 nit）。
    PersistFailed(String),
}

/// 写图 sidecar 的完整链路：dump（R19 panic 边界内）→ CRC → manifest 原子发布
/// （v2-step2-design §4.5 步骤 5~7）。
///
/// 独立成函数是为了让「失败」有一个**统一的边界**：任一步出错都回到调用方，
/// 由 `GraphPersistMode` 决定升级为 Err 还是降级为警告。此前只有 `dump_graph`
/// 被 Lenient 捕获，紧随其后的 CRC / manifest 发布仍会让 `save()` 返回 Err
/// （评审 #12 发现 2）。
///
/// V2 Step 3（D-S3-04）：dump 经 [`crate::vector::dump_graph_caught`] 的 panic
/// 边界——`hnsw_rs` 写路径的 `panic_any`（R19 TOCTOU 残余）降级为
/// `Err(VectorGraph)`，汇入本函数既有的失败语义链。
fn write_graph_sidecar(
    path: &std::path::Path,
    g: &dyn VectorGraphPersist,
    body_crc: u32,
    dim: u32,
) -> Result<()> {
    // 路径必须有文件名，否则 sidecar 会落到凭空捏造的位置（评审 #12 nit）
    crate::storage::require_file_name(path)?;

    let stats = crate::vector::dump_graph_caught(|| g.dump_graph(path))?;

    // CRC + 长度（流式）
    let paths = crate::storage::graph_paths(path);
    let (graph_crc, graph_len) = crate::storage::file_crc32_len(&paths.graph)?;
    let (data_crc, data_len) = crate::storage::file_crc32_len(&paths.data)?;

    let m = crate::storage::GraphManifest {
        manifest_version: crate::storage::MANIFEST_VERSION,
        producer: format!("helix-core-{}", env!("CARGO_PKG_VERSION")),
        graph_format: stats.graph_format,
        dist_id: crate::storage::DIST_ID.to_string(),
        platform: crate::storage::PLATFORM_FINGERPRINT,
        max_nb_connection: stats.max_nb_connection,
        ef_construction: stats.ef_construction,
        snapshot_crc: body_crc,
        snapshot_len: std::fs::metadata(path).map(|md| md.len()).unwrap_or(0),
        dim,
        nb_point: stats.nb_point,
        graph_crc,
        graph_len,
        data_crc,
        data_len,
    };
    crate::storage::write_manifest_atomic(&paths.manifest, &m)?;
    Ok(())
}

/// 尝试从图 sidecar 加载 HNSW 图（v2-step2-design §4.6 的读取序列）。
///
/// 返回 `Err(reason)` 表示**应降级重建**（图是缓存，不丢功能）。
/// 调用方负责按 `GraphPersistMode` 决定「警告」还是「报错」。
///
/// # 安全前提（C3）
///
/// `hnsw_rs` 的 reload 路径有 12 处 `assert_eq!` / `unwrap()` / `exit(1)`，
/// 「文件能打开但内容坏」会 panic 或强杀进程。因此**必须在把文件交给
/// `load_hnsw` 之前**完成全部校验：manifest 逐项 → 两文件 CRC → Description 预校验。
fn try_load_graph(
    snapshot: &std::path::Path,
    body_crc: &u32,
    fingerprint: &crate::storage::ConfigFingerprint,
    raw_vectors: &[(ChunkId, Vec<f32>)],
    graph: super::config::GraphOpts,
    ef_search: usize,
    parallel_build: bool,
) -> std::result::Result<HnswRsIndex, String> {
    // 逃生舱：--no-graph-persist 时不碰图
    if !graph.persist {
        return Err("图持久化已关闭（--no-graph-persist）".to_string());
    }
    // 校验 + 加载的完整序列在 `persist::load_graph_checked`：门面层与 bench（底层
    // 路径）共用同一份校验——**复制校验逻辑必然漏项**，漏项 = 把坏文件交给满是
    // `unwrap()` 的 `load_hnsw`（C3）
    crate::vector::load_graph_checked(
        snapshot,
        *body_crc,
        fingerprint.dim,
        raw_vectors.len() as u64,
        ef_search,
        parallel_build,
    )
}

fn warn_graph_degraded(reason: &str, mode: GraphPersistMode) -> Result<()> {
    match mode {
        GraphPersistMode::Lenient => {
            eprintln!(
                "[警告] 向量图 sidecar 不可用（原因：{reason}），已降级为加载后重建（冷启动会变慢）"
            );
            Ok(())
        }
        GraphPersistMode::Strict => Err(Error::GraphStale {
            reason: reason.to_string(),
        }),
    }
}

/// 从原始向量**重建向量索引**（V2 Step 4 / S4-06，D-S4-07）。
///
/// 与 `load_with` 的降级分支、`flush` 走同一条 `add_batch` 路径，行为一致。
/// 重建**不需要 embedder**（向量来自 `raw_vectors` 真源），纯 BM25 装配也可用
/// （此时不会走到本函数）。Brute 用 `from_entries`（O(N) 拷贝）；Hnsw 用
/// `with_capacity + add_batch`（沿用 ef_search / parallel_build 装配）。
fn rebuild_vector_index(
    backend: VectorBackend,
    raw: &[(ChunkId, Vec<f32>)],
    ef_search: Option<usize>,
    parallel_build: bool,
) -> Result<Box<dyn VectorIndex>> {
    let entries: Vec<(ChunkId, NormalizedVector)> = raw
        .iter()
        .map(|(id, v)| (*id, NormalizedVector::new(v.clone())))
        .collect();
    Ok(match backend {
        VectorBackend::Brute => {
            Box::new(BruteForceIndex::from_entries(entries)) as Box<dyn VectorIndex>
        }
        VectorBackend::Hnsw => {
            let ef = ef_search.unwrap_or(HnswRsIndex::default_ef_search());
            let mut vi = HnswRsIndex::with_capacity(raw.len().max(1024))
                .with_ef_search(ef)
                .with_parallel_build(parallel_build);
            vi.add_batch(&entries)?;
            Box::new(vi) as Box<dyn VectorIndex>
        }
    })
}

/// 统计某落盘快照的三个 sidecar 文件的字节体积（`fs::metadata`）。
fn snapshot_bytes(path: &std::path::Path) -> std::io::Result<SizeBytes> {
    let g = crate::storage::graph_paths(path);
    Ok(SizeBytes {
        snapshot: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        graph: std::fs::metadata(&g.graph).map(|m| m.len()).unwrap_or(0),
        data: std::fs::metadata(&g.data).map(|m| m.len()).unwrap_or(0),
    })
}

// ⚠️ 原 `pub(crate) struct Inner`（`index` / `vector_index` / `raw_vectors`）已上移为
// `super::view::Segment`（私有项，故不用 intra-doc 链接）：`S8-02` 起「内容容器」= 段；
// `S8-03` 起写入落在写端私有的 `SegmentBuilder` 上、由 `commit()` 追加为 `View.deltas` 的一段。

/// 待 embed 的缓冲项（写缓冲，p6-design 6.2）。
pub(crate) struct PendingChunk {
    /// 分片 ID（`Index::add` 已分配）
    pub chunk_id: ChunkId,
    /// 分片正文（embed 输入）
    pub text: String,
}

/// `add(doc)` 的结果（p6-design 4.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddOutcome {
    /// 分配的文档 ID（幂等命中时为既有 ID）
    pub doc_id: DocId,
    /// 新增的分片 ID（幂等命中时为 `[]`）
    pub chunk_ids: Vec<ChunkId>,
    /// 是否命中幂等去重（FR-15）
    pub deduped: bool,
}

// ---------------------------------------------------------------------------
// compaction（V2 Step 4）的可观测结构与报告
// ---------------------------------------------------------------------------

/// 三个落盘文件的字节体积（`.idx` / `.hnsw.graph` / `.hnsw.data`）。
///
/// compaction 验收 1 的判据就是三体积；`--json` 消费者（A/B 脚本）直接透传，不自己猜文件名。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SizeBytes {
    /// 快照正文字节数（`.idx`）
    pub snapshot: u64,
    /// HNSW 图文件字节数（`.hnsw.graph`）
    pub graph: u64,
    /// HNSW 数据文件字节数（`.hnsw.data`）
    pub data: u64,
}

/// 墓碑统计（compaction 前的决策依据，NFR-07 可观测）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TombstoneStats {
    /// 正排分片槽位总数（含墓碑）
    pub chunks_total: usize,
    /// 存活分片数
    pub chunks_alive: usize,
    /// 正排文档槽位总数（含墓碑）
    pub docs_total: usize,
    /// 存活文档数
    pub docs_alive: usize,
    /// 向量索引中的点数（含墓碑；hnsw_rs 无 remove，墓碑留图）
    pub graph_points: usize,
    /// `raw_vectors` 原始向量条数
    pub raw_vectors: usize,
    /// 墓碑占比（chunk 口径）`1 - alive/total`
    pub tombstone_ratio: f64,
}

/// 一次 compaction 的结果报告。
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionReport {
    /// compact 前的墓碑统计
    pub before: TombstoneStats,
    /// compact 后的墓碑统计
    pub after: TombstoneStats,
    /// 三体积（字节）。**内存-only `compact()` 下 `bytes_after` 恒为 `None`**——
    /// 磁盘此时还没变，填任何值都是撒谎；`compact_and_save` 才填。
    pub bytes_before: Option<SizeBytes>,
    /// compact 落盘后的三体积（仅 `compact_and_save` 填写）
    pub bytes_after: Option<SizeBytes>,
    /// 回收的墓碑分片数
    pub reclaimed_chunks: usize,
    /// 回收的墓碑文档数
    pub reclaimed_docs: usize,
    /// 摘除的死词数
    pub reclaimed_terms: usize,
    /// 回收的图中墓碑点数
    pub reclaimed_graph_points: usize,
    /// 向量索引重建耗时（毫秒）
    pub vector_rebuild_ms: u128,
    /// 本次 compaction 总耗时（毫秒）
    pub total_ms: u128,
    /// `false` = 无墓碑，本次是 no-op（`doc_id` / `chunk_id` 一个都没变，验收 T11）
    pub remapped: bool,
    /// 落盘后的最终图状态；内存-only 时为磁盘旧值（§4.6）
    pub graph_status: GraphStatus,
}

/// 写端门面（拥有型）。持 `Shared`（与读端共享的视图 + 装配）与**私有**写缓冲。
///
/// 注意：`SearchIndex` 仍是 **`!Sync`**（含可变 `pending`，`add(&mut self)`）——但
/// `S8-02` 起这**不再**是「读写互斥」的来源：读端持独立的 `Searcher`（`Arc<Shared>`），
/// 与写端**并存**。并发来自：「写端线程独占 `SearchIndex`」+「读端持 owned `Searcher`」。
///
/// # 内容的位置（`S8-02`）
///
/// 内容**不在**本结构里，而在 `shared.view.main`（`Segment`）。本结构只持
/// 「写缓冲 `pending`」与「诊断计数」。
///
/// ⚠️ **`S8-03` 起不再有「写入需独占」的过渡约束**：`add` / `flush` / `remove` 只写
/// **写端私有的 builder**，`commit()` 持 `Shared::ids` 锁把段原子追加进 `View.deltas`
/// ⇒ **并发检索进行中也能写**。（`S8-02` 期 `deltas` 恒空、内容唯一，写端就地改 `main`，
/// 那时才有该约束；`S8-03` 评审 P3-4 指出该段已过时。）
///
/// # 所有权切换（p6-design 6.4 方案 A → `S8-02` 起改用共享视图）
///
/// `shared` 是 `Arc<Shared>`：`searcher(&self)` 只克隆该 `Arc`（不消耗写端）；
/// `Searcher::into_index()` 同样只克隆该 `Arc`（**总是成功**，不再要求 refcount==1
/// ——「Searcher clone 残留」与「写端存活」在新模型下不可区分，后者是合法状态）。
/// 写端的**段构建器**（`S8-03`）：一块**尚未发布**的内容。
///
/// 生命周期：`add` / `flush` 往里写 → [`SearchIndex::commit`] 把它**冻结成 `Segment`**
/// 并追加进 `View.deltas` → 换一个空的。⇒ **未 `commit` 的内容对读端不可见**
/// （这正是 NFR-11 的可见性边界，也是 §1.3 第 7 条的落地形态）。
///
/// # 为什么 `base_*` 记在这里（而不是每次 `add` 重算）
///
/// 本 builder 的本地 ID `i` 对应的全局 ID 恒为 `base + i`（`base` = 它被创建时的
/// 「已发布长度」，§4.4.1）。把它**钉在创建时刻**有两个好处：
/// ① `add` 不需要每篇文档读一次锁；
/// ② `commit` 时可以拿锁内的真实基址与它**对账** —— 不等即说明**另一个写端插过队**
///    （`into_index()` 让同一 `Arc<Shared>` 上可以有两个写端），此时段内 ID 与全局 ID
///    已不对应 ⇒ 必须**报错**而不是把错位的 ID 发布出去。
pub(crate) struct SegmentBuilder {
    /// 段内容（**段内**本地 ID 从 0 起）
    index: Index,
    /// 写缓冲（批量 embed；`add` 入缓冲前已分配段内 `chunk_id`）
    pending: Vec<PendingChunk>,
    /// 段内原始向量（快照用，本地 `chunk_id`）
    raw_vectors: Vec<(ChunkId, Vec<f32>)>,
    /// 该段的向量索引（纯 BM25 段为 `None`）
    vector: Option<Box<dyn VectorIndex>>,
    /// 针对**既往段**的全局 `doc_id` 墓碑（`remove` 命中既往段时记这里，§4.8.3 分支 ②）
    tombstones: Tombstones,
    /// 本段的全局基址（创建时刻的「已发布长度」，见类型文档）
    base_doc: DocId,
    base_chunk: ChunkId,
    /// 创建时刻的 **ID 空间世代**（`S8-03` 评审 **P2-4**）。
    ///
    /// 与 `base_*` 一起**在同一把锁内**取回（[`Shared::next_origin`]），并在 `commit()` 时
    /// 回报给 [`Shared::commit_view`] ⇒ `compact()` 的重编号（[`Shared::publish_reset`]）
    /// 会把本 builder 的发布**整条拒掉**。
    /// ⚠️ 光有基址对账**不够**：重编号后基址可能恰好又相等（ABA），而本 builder 记的
    /// 墓碑 / 去重条目仍指向旧 ID 空间 ⇒ 会静默错挂到无辜文档上。
    epoch: u64,
}

impl SegmentBuilder {
    pub(crate) fn new(
        vector: Option<Box<dyn VectorIndex>>,
        base_doc: DocId,
        base_chunk: ChunkId,
        epoch: u64,
    ) -> Self {
        Self {
            index: Index::new(),
            pending: Vec::new(),
            raw_vectors: Vec::new(),
            vector,
            tombstones: Tombstones::default(),
            base_doc,
            base_chunk,
            epoch,
        }
    }

    /// 段内本地 `doc_id` → 全局（`base + local`）。
    fn global_doc(&self, local: DocId) -> DocId {
        self.base_doc + local
    }

    /// 该 builder 是否**空**（无内容、无墓碑）⇒ `commit()` 不追加空段（设计 §4.8.3 ③）。
    ///
    /// ⚠️ **「墓碑-only」builder 被判为非空是有意的**：跨段墓碑**只能**搭一次发布的便车进
    /// `View.tombstones`（`commit()` 的追加路径），所以哪怕零内容也必须入列。两个次生效应
    /// （`S8-03` 第 2 轮评审 **P4-1**，**本 PR 不修**、登记给 `S8-06` 合并器）：
    /// ① `Metrics.segments` 被零内容段**膨胀**（删除密集型负载下持续偏大）；
    /// ② `fold` 期 `merge_from(空 Index)` 仍走一遍 `append_from` 的 `ForwardStore::rebuild`
    ///    + `field_index.rebuild`（各 O(N)）⇒ 每个墓碑-only 段白付一次**全量 pass**。
    ///
    /// 预期取形：零内容段不占 `deltas` 名额、墓碑直接并入 `View` 层。
    fn is_empty(&self) -> bool {
        self.index.total_chunks() == 0 && self.tombstones.is_empty()
    }
}

/// 写端门面（拥有型）。持 `Shared`（与读端共享的视图 + 装配）与**写端私有**的
/// `SegmentBuilder`（`S8-03` 起：未 `commit` 的内容都在那里，读端看不到）。
///
/// 注意：`SearchIndex` 仍是 **`!Sync`**（`add(&mut self)`）——但 `S8-02` 起这**不再**
/// 是「读写互斥」的来源：读端持独立的 `Searcher`（`Arc<Shared>`），与写端**并存**。
///
/// # `S8-03` 起的写路径
///
/// | 方法 | 落点 |
/// | --- | --- |
/// | `add` / `flush` | **自己的 builder**（不碰已发布的段）⇒ **并发检索进行中也能写** |
/// | `commit()` | 把 builder 冻成 `Segment`，**持 `ids` 锁原子追加**进 `View.deltas` |
/// | `remove` | 三分支：当前 builder 就地物理删 / 既往段记**跨段墓碑** / 不存在 no-op |
///
/// # 所有权切换
///
/// [`Self::searcher`] 只克隆 `Arc<Shared>`（不消耗写端）；`Searcher::into_index()` 同样
/// 只克隆（**总是成功** ——「`Searcher` clone 残留」与「写端存活」不可区分）。⚠️ 后者是
/// **双写端**的入口 ⇒ `commit()` 里那条「基址对账」就是为它准备的。
pub struct SearchIndex {
    /// 与读端共享的视图 + 装配（唯一内容来源）
    pub(crate) shared: Arc<Shared>,
    /// 写端的段构建器（`S8-03` 起取代裸 `pending`：未 `commit` 的内容都在这里）
    pub(crate) builder: SegmentBuilder,
    /// 累计 embed 推理耗时（`flush` 中累加，供 NFR-03 构建耗时口径观测）。
    /// 只计 `embed_documents` 推理本身，不含归一化 / 灌向量索引。
    pub(crate) embed_elapsed: std::time::Duration,
    /// 累计送入 `embed_documents` 的**条数**（与 `embed_elapsed` 同源累加）。
    ///
    /// 存在的理由：增量构建时 `num_chunks()` 是**索引总量**，而本次真正
    /// 吃 ONNX 推理的只有新增的那一批 —— 用 `num_chunks()` 报「embed N 条」
    /// 会把 NFR-03 的口径讲错（增量场景下高估）。
    pub(crate) embed_count: usize,
    /// 最近一次图 sidecar 的状态（NFR-07：降级必须可观测）
    pub(crate) graph_status: GraphStatus,
    /// 最近一次 `save` 中图 sidecar 落盘（dump + CRC + manifest 发布）的耗时。
    /// `None` = 本次 `save` 未走图持久化（验收 7：dump 要有实测记录）。
    pub(crate) graph_dump_elapsed: Option<std::time::Duration>,
}

impl SearchIndex {
    /// 配置入口：全部组件可选，不调即用默认值（零配置可用）。
    pub fn builder() -> SearchIndexBuilder {
        SearchIndexBuilder::default()
    }

    /// 从已构建的 `Config` 与后端选择组装空索引。
    pub(crate) fn from_config(
        cfg: Config,
        backend: VectorBackend,
        graph: super::config::GraphOpts,
    ) -> Self {
        let cfg = Arc::new(cfg);
        // 主段与 builder **各自**持一个空的向量索引（两者独立演进：主段已发布、builder 在写）
        let main = Arc::new(Segment::empty(Self::make_vector_index(&cfg, backend)));
        Self {
            shared: Arc::new(Shared::new(Arc::clone(&cfg), graph, backend, main)),
            builder: SegmentBuilder::new(Self::make_vector_index(&cfg, backend), 0, 0, 0),
            embed_elapsed: std::time::Duration::ZERO,
            embed_count: 0,
            graph_status: GraphStatus::NotApplicable,
            graph_dump_elapsed: None,
        }
    }

    /// 按配置与后端造一个**空的**向量索引（`None` = 纯 BM25）。
    ///
    /// ⚠️ `from_config`（主段 + 首任 builder）与 [`Self::new_builder`]（`into_index()`
    /// 换回写端时）**必须共用本函数** —— 各写一遍就会给「换回写端后段类型与
    /// `shared.backend` 不一致」留后门。
    fn make_vector_index(cfg: &Config, backend: VectorBackend) -> Option<Box<dyn VectorIndex>> {
        let ef_search = cfg.ef_search;
        match cfg.embedder.as_ref() {
            Some(_) => Some(match backend {
                VectorBackend::Brute => Box::new(BruteForceIndex::new()) as Box<dyn VectorIndex>,
                VectorBackend::Hnsw => {
                    let idx = HnswRsIndex::with_capacity(1024);
                    let idx = match ef_search {
                        Some(ef) => idx.with_ef_search(ef),
                        None => idx,
                    };
                    Box::new(idx.with_parallel_build(cfg.parallel_build)) as Box<dyn VectorIndex>
                }
            }),
            None => None,
        }
    }

    /// 造一个与 `shared` 装配一致的空 builder（基址 **+ 世代**取当前「出生点」）。
    ///
    /// ⚠️ 基址与 `epoch` **必须一次取回**（[`Shared::next_origin`]）：分两次取时另一个写端
    /// 可以在两次调用之间 `compact()` ⇒ 得到「旧世代基址 + 新世代 epoch」的 builder，
    /// 它的对账会通过（`S8-03` 评审 P2-4 的窗口）。
    pub(crate) fn new_builder(shared: &Shared) -> SegmentBuilder {
        let (base_doc, base_chunk, epoch) = shared.next_origin();
        SegmentBuilder::new(
            Self::make_vector_index(&shared.cfg, shared.backend),
            base_doc,
            base_chunk,
            epoch,
        )
    }

    /// 当前视图的快照（`I8-3`：一次操作只取一次读锁）。
    ///
    /// ⚠️ `S8-03` 起内容分布在 `main + deltas` ⇒ **任何**「全量」语义（统计、查重、
    /// 遍历）都必须走 `view`，不能只看 `main`。
    fn view(&self) -> Arc<View> {
        self.shared.snapshot()
    }

    /// **跨段** `content_hash` 查重（FR-15 幂等 upsert 的必要条件，设计 §4.8.3）。
    ///
    /// 顺序：当前 builder（未发布、最新）→ `deltas` 逆序 → `main`。
    ///
    /// 🔑 **命中后还要判「该 doc 是否已被墓碑挡住」**：被挡住 ⇒ **不算命中**（可重新 upsert）。
    /// 今天 `remove` 会 `content_hashes.remove(&hash)`（`Index::remove`）⇒「删除后可重新
    /// upsert」自然成立；跨段后**主段的 hash 表删不掉**（`remove` 只作用于它自己那段），
    /// 同一语义只能靠墓碑在这里挡一下 —— 由 `S8-T6` 钉住。
    ///
    /// 🔴 **两处墓碑都要查**（`S8-03` 第 2 轮评审 **P1-1**）：`remove` 的**分支②**（目标在
    /// 既往段）把墓碑记在 **`self.builder.tombstones`** 上，要 `commit()` 才进
    /// `View.tombstones`。只查后者的话——「`remove(X)` 后**不 `commit()`** 直接重加同 hash
    /// 内容」——查重会命中**刚被删除的** X ⇒ 返回 `deduped = true` + `doc_id = X`
    /// ⇒ **替换 / 重加静默丢失**（`commit()` 后 X 被墓碑挡住，而新内容**根本没被创建**，
    /// 调用方还拿到一个已死 doc 的 ID）。
    ///
    /// 🔑 为什么这**是缺口而不是设计**：同一序列在**分支①**（目标未发布）下是
    /// 「就地物理删 + 条件式摘 hash 条目」⇒ 重加正常工作。而 FR-15 的「替换文档」流程
    /// （`remove(id)` 后用同 `dedup_key`、不同正文重新 upsert）**总是**落在分支②
    /// （目标必然已发布才谈得上"替换"）⇒ 它是用户会真的走到的那条路。
    fn doc_id_by_hash_global(&self, hash: u64) -> Option<DocId> {
        // ① 未发布的 builder：它是最新的，且它的 doc 不可能被墓碑挡（墓碑只针对既往段）
        if let Some(local) = self.builder.index.doc_id_by_hash(hash) {
            return Some(self.builder.global_doc(local));
        }
        // ② 已发布的段：**最新优先**（deltas 末尾 → … → main）
        let view = self.view();
        for seg in view.segments_newest_first() {
            if let Some(local) = seg.index.doc_id_by_hash(hash) {
                let global = seg.global_doc(local);
                // ⚠️ **两处都查**：已发布的墓碑 + 本 builder 上**尚未发布**的墓碑（P1-1）。
                //    「命中即 return」仍成立：单写端不变式下，若最新命中被墓碑挡住，更旧的
                //    同 hash 命中必然也已被挡住（同一条替换链上的前身）；
                //    而「已替换 + 已删除」的旧 doc 本就不该被新的 upsert 复用。
                if view.tombstones.blocks_doc(global) || self.builder.tombstones.blocks_doc(global)
                {
                    return None; // 已被墓碑挡住 ⇒ 视为未命中（可重新 upsert）
                }
                return Some(global);
            }
        }
        None
    }

    /// 摄入一篇文档：查重 → 分块 → 倒排 → 写缓冲（p6-design 6.1）。
    ///
    /// - **幂等 upsert（FR-15）**：`content_hash = xxh64(dedup_key.unwrap_or(text))`
    ///   命中已存在文档时直接返回 `deduped = true`，不重复分块/插入。
    /// - 向量化**延后到 `flush`**（批量 embed，NFR-03）；此刻只进倒排与写缓冲。
    /// - 缓冲满（`batch_size`）时自动同步 flush。
    ///
    /// # 失败语义（`S8-03` 起）
    ///
    /// 全部写入都落在**写端私有的 builder** 上（不碰已发布的段）⇒ `add` **没有**
    /// 「部分生效」形态：`Ok` = 已进 builder；`Err` = 本次未写入。
    /// ⚠️ 但 builder 里的内容要 `commit()` 才对外可见（§1.3 第 7 条 / NFR-11）。
    ///
    /// （`S8-02` 期那套「两处写点、① 成功后 ② 可能失败 ⇒ 部分生效」的描述已**作废**；
    /// `S8-03` 评审 P3-4.2 指出它当时已与实现矛盾。）
    pub fn add(&mut self, doc: impl Into<Document>) -> Result<AddOutcome> {
        let doc = doc.into();
        let text = doc.text.clone();
        let hash = content_hash(doc.dedup_key.as_deref().unwrap_or(&text));

        // ① **跨段**查重（builder → deltas 逆序 → main；被墓碑挡住的**不算命中**）
        if let Some(existing) = self.doc_id_by_hash_global(hash) {
            return Ok(AddOutcome {
                doc_id: existing,
                chunk_ids: Vec::new(),
                deduped: true,
            });
        }

        let record = DocRecord {
            doc_id: 0,
            source: doc.source.clone(),
            metadata: doc.metadata.clone(),
            content_hash: hash,
        };

        // ② 分块 → 写进**写端私有的 builder**（段内本地 ID）
        //
        // 🔑 `S8-03` 的核心：这里**不再就地改 `main`** —— 写的是自己的 builder，
        // 不碰任何已发布的段 ⇒ **并发检索进行中照样能写**（`FR-17` 的「读不阻塞写」
        // 从「靠约束绕开」变成「结构上不冲突」）。
        let cfg = Arc::clone(&self.shared.cfg);
        let chunks = cfg.chunker.chunk(0, &text);
        let chunk_texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let (local_doc, local_chunks) =
            self.builder
                .index
                .add(record, chunks, cfg.analyzer.as_ref())?;

        // ③ 段内 → 全局（`AddOutcome` 的公开契约是**全局** ID，设计 §4.4.1）
        let doc_id = self.builder.global_doc(local_doc);
        let chunk_ids: Vec<ChunkId> = local_chunks
            .iter()
            .map(|c| self.builder.base_chunk + c)
            .collect();

        // ④ 进写缓冲（有向量侧时才需要 embed；缓冲里存的是**段内** chunk_id —— 灌索引用）
        if cfg.embedder.is_some() {
            for (local, text) in local_chunks.iter().zip(chunk_texts) {
                self.builder.pending.push(PendingChunk {
                    chunk_id: *local,
                    text,
                });
            }
            if self.builder.pending.len() >= cfg.batch_size {
                self.flush()?;
            }
        }

        Ok(AddOutcome {
            doc_id,
            chunk_ids,
            deduped: false,
        })
    }

    /// 批量摄入（吞吐最优路径，`helix build` 走这条）。
    ///
    /// 与逐条 `add` 的区别：一次性把全部文档 chunk 完、分批 embed，
    /// 避免逐条的缓冲检查与 Arc::make_mut 抖动。
    pub fn add_documents<I>(&mut self, docs: I) -> Result<Vec<AddOutcome>>
    where
        I: IntoIterator<Item = Document>,
    {
        docs.into_iter().map(|d| self.add(d)).collect()
    }

    /// 强制冲刷写缓冲：批量 embed + L2 归一化 + 灌向量索引。
    ///
    /// - **L2 归一化在此内部调用 `NormalizedVector::new` 保证**（消除静默陷阱 ⑥）
    /// - 纯 BM25（无 embedder）时为 no-op
    pub fn flush(&mut self) -> Result<()> {
        if self.builder.pending.is_empty() {
            return Ok(());
        }
        let cfg = Arc::clone(&self.shared.cfg);
        let Some(embedder) = cfg.embedder.as_ref() else {
            // 无向量侧：缓冲不应存在（add 里已短路）；防御性清空
            self.builder.pending.clear();
            return Ok(());
        };

        let texts: Vec<String> = self
            .builder
            .pending
            .iter()
            .map(|p| p.text.clone())
            .collect();
        let t = std::time::Instant::now();
        let vecs = embedder.embed_documents(&texts)?;
        self.embed_elapsed += t.elapsed();
        self.embed_count += texts.len();
        debug_assert_eq!(vecs.len(), texts.len(), "embed 输出条数应与输入一致");

        // 归一化 + 灌向量索引 + 记录原始向量（快照用）。
        // V2 Step 2：改用 `add_batch`——达阈值（`PARALLEL_INSERT_THRESHOLD = 1000`）
        // 且开了并行建图开关时由实现走 `parallel_insert_slice`，否则串行。
        // ⚠️ T7-21 / D-J11（2026-09-14）后该开关**默认开**。
        // ⚠️ **本路径的批量不是「≤ batch_size」**（评审 #49 P2-1 勘误）：`flush` 交付给
        // `add_batch` 的是**整个 `pending`**，故上界 = 缓冲存量（`add()` 入口处 `< batch_size`，
        // 即 ≤ 63）+ **当前文档的 chunk 数 k**。默认 `batch_size = 64` 且文档不大时（k ≪ 937）
        // 批量远低于阈值 ⇒ 仍串行；但 **k ≥ 937（缓冲已满）或 k ≥ 1000（保证）时同样走并行**
        // （默认 `Chunker(512/64)` 步长约 448 字符 ⇒ **单篇约 42~45 万字符**的长文档就可能触发；
        // `Chunker` **无单文档 chunk 数上限**，段落边界切分还会让所需长度更短）。
        // ⇒ **大单文档的增量建库，图拓扑同样不可复现**（与 `rebuild_vector_index` 的单次全量
        // 建图同族：`compact()` 重建 / `load_with` 的降级重建）。
        // 小批量下批量路径与逐条 add 行为等价，故无需分支。
        //
        // V2 Step 4 / S4-01（D-S4-05 / §2.3 / T5）：**整批 embed 之后、入库之前**按
        // liveness 过滤。`add(d)` 在入 `pending` 前就分配了真实 chunk_id，而 `remove(d)`
        // 只墓碑化正排/倒排、不摘 pending；于是 `add → remove → commit`（默认
        // `batch_size = 64` 下的常见时序，CLI 逐条 add 后删除即此路径）会让已删 chunk
        // 仍留在 pending 里，若不滤掉，其向量会被灌进 `raw_vectors` 与图并跨快照永续。
        //
        // 刻意**先整批 embed、再过滤**（不改变 `embed_documents` 入参组成）：fastembed
        // 推理是否受 batch 组成影响未经实测（Step 2 教训是「别赌」），「白算一条 embed」
        // 的代价只是时间；若后续实测证明组成无关，可再改「先过滤再 embed」（见设计 §9 Q5）。
        // ⚠️ 同样写**自己的 builder**（不再就地改 `main`）⇒ 与并发读者无冲突。
        // `pending` 与 `raw_vectors` 都要可变 ⇒ 先把缓冲 `take` 出来（避免同时借两个字段）。
        let b = &mut self.builder;
        let pending = std::mem::take(&mut b.pending);
        let vi = b.vector.as_mut().ok_or(Error::NoEmbedder)?;
        let mut items: Vec<(ChunkId, NormalizedVector)> = Vec::new();
        for (p, v) in pending.iter().zip(vecs) {
            if !b.index.is_live_chunk(p.chunk_id) {
                continue; // 墓碑 chunk：向量丢弃，不落 raw_vectors、不进图
            }
            let nv = NormalizedVector::new(v);
            b.raw_vectors.push((p.chunk_id, nv.as_slice().to_vec()));
            items.push((p.chunk_id, nv));
        }
        vi.add_batch(&items)?;
        Ok(())
    }

    /// 使已摄入的文档对检索可见：`flush()` + **发布新视图**（`generation` +1）。
    ///
    /// # `S8-02` 期的语义（与重构前逐位一致）
    ///
    /// 内容只有一段（`deltas` 恒空）⇒ `commit()` **不移动**任何内容，只把
    /// 「当前视图」重新发布一次（`generation` +1）。因此「`add` 后未 `commit`
    /// 也能查到倒排」这一既有语义**保持不变**（既有测试 `add后未commit也能查到倒排`）。
    ///
    /// `S8-03` 起 `commit()` 改为「封段（把 builder 冻结成 `Segment`）→ 追加进 `deltas`
    /// → 发布」，届时它才成为**唯一的可见性边界**（设计 §4.8.3 / §1.3 第 7 条）。
    pub fn commit(&mut self) -> Result<()> {
        self.flush()?;

        // ⚠️ **对账（两项）**：① **世代**（`epoch`）—— 本 builder 创建后是否有另一个写端
        //    `compact()` 过（重编号 ⇒ 整个旧 ID 空间作废）；② **基址** —— 是否有另一个写端
        //    在本写端 `add` 之后提交过（`into_index()` 让同一 `Arc<Shared>` 上可以有两个写端）。
        //    任一项不符 ⇒ 段内 ID / 墓碑 / 去重条目已与全局不对应 ⇒ **必须报错**，
        //    不能把错位的 ID 发布出去（单写端下两项恒不触发）。
        // 🔴 **① 是 P2-4 的修复**：只有 ② 时存在 **ABA 窗口** —— `compact()` 允许「已用长度
        //    变小」，若此后恰好又提交了等量内容，`ids.next_*` 会回到与本 builder 记录的基址
        //    **完全相等**的值 ⇒ 基址对账**通过** ⇒ 本 builder 里那条指向旧 ID 空间的跨段墓碑
        //    被并进视图，下一次物理化就会删掉**重编号后的另一个无辜文档**（静默错删）。
        //    世代号把这条窗口变成「结构上不存在」。
        // 🔴 **但本路径的语义不满足 `Error::Busy` 的「重试即可」承诺**（`S8-03` 评审 P2-1）：
        //    不匹配分支**不追加段**，而末尾**无条件**换新 builder ⇒ builder 里未提交的内容
        //    （含已返回给调用方的 ID 与已记的墓碑）**被丢弃**，重试**不会**恢复它。
        //    ⇒ 本次已在错误消息里如实声明；「无损重基」（保留 builder、把 `base_*` 对齐到
        //    锁内真实值后重新发布）留给下一提交，与 `error.rs` 的文档同步。
        // 🔴 **恢复指引必须同时点名 `remove`**（`S8-03` 第 2 轮评审 **P3-1**）：被丢弃的
        //    builder 内容**含分支②记下的跨段墓碑** ⇒ 对「`remove(X)` → `commit()` 被拒」的
        //    流程，只让用户「重新 `add`」的话他**什么都不会恢复** —— X 的删除静默失效
        //    （X 继续可检索）。NFR-07：**恢复路径也要如实**。
        let mismatched = std::cell::Cell::new(false);
        // 取出 builder（下面在锁内被消耗）；先用占位顶上，末尾统一换成新的
        let builder = std::mem::replace(&mut self.builder, SegmentBuilder::new(None, 0, 0, 0));
        let expected = (builder.base_doc, builder.base_chunk);
        let expected_epoch = builder.epoch;

        let published =
            self.shared
                .commit_view(expected_epoch, |cur, generation, base_doc, base_chunk| {
                    // 基址不符 **或** builder 为空 ⇒ 不追加任何段（视图内容不变），但仍推进序号
                    // （「每次 commit 都发布」是既有契约，见单测 `S8_02_commit递增视图序号`；
                    //  空段**不入列**则避免 `deltas` 被空段撑大，设计 §4.8.3 ③）。
                    // ⚠️ 「**墓碑-only**」不算空（见 `SegmentBuilder::is_empty`）⇒ 会追加一个
                    //    **零内容段**（发布墓碑所必需）；其两个次生成本见该方法文档（P4-1 / `S8-06`）。
                    if (base_doc, base_chunk) != expected || builder.is_empty() {
                        if (base_doc, base_chunk) != expected {
                            mismatched.set(true);
                        }
                        return Ok((
                            View {
                                main: Arc::clone(&cur.main),
                                deltas: Arc::clone(&cur.deltas),
                                tombstones: Arc::clone(&cur.tombstones),
                                generation,
                            },
                            base_doc,
                            base_chunk,
                        ));
                    }
                    let SegmentBuilder {
                        index,
                        raw_vectors,
                        vector,
                        tombstones,
                        ..
                    } = builder;
                    let used_doc = base_doc + index.total_docs() as DocId;
                    let used_chunk = base_chunk + index.total_chunks() as ChunkId;
                    let seg = Arc::new(Segment {
                        index,
                        vector_index: vector,
                        raw_vectors: Some(raw_vectors),
                        base_doc,
                        base_chunk,
                        generation,
                    });
                    // 追加进 `deltas` **尾部**（FIFO 不变式：`main, deltas[0], …`，§4.4.2）
                    let mut deltas: Vec<Arc<Segment>> = cur.deltas.iter().cloned().collect();
                    deltas.push(seg);
                    // builder 的墓碑并入视图（全局 doc_id；`add` 幂等去重）
                    let mut acc = (*cur.tombstones).clone();
                    for d in tombstones.iter() {
                        acc.add(d);
                    }
                    Ok((
                        View {
                            main: Arc::clone(&cur.main),
                            deltas: Arc::from(deltas),
                            tombstones: Arc::new(acc),
                            generation,
                        },
                        used_doc,
                        used_chunk,
                    ))
                });

        // 换一个与**新**「已发布长度」对齐的空 builder
        self.builder = Self::new_builder(&self.shared);

        // ① 世代对账失败（`P2-4`）：`commit_view` 在**进入闭包前**就返回了 `Err` ⇒
        //    本次既没追加段、也没推进任何计数器（`generation` 都不消耗）。
        //    ⚠️ 注意这里是**发布之后**才 `?`：builder 已换成与**当前**世代对齐的新的一枚，
        //    契约与下面的基址分支一致（未提交内容被丢弃、重试不恢复）。
        let generation = published?;

        if mismatched.get() {
            return Err(Error::Busy(
                "另一个写端在本写端 add 之后提交过 ⇒ 本次段基址已过期（段内 ID 与全局 ID \
                 不再对应）；单写端下不会发生 —— 同一 Arc<Shared> 上请勿并发写。\
                 ⚠️ 本次**未提交内容已被丢弃**（builder 已换成新的），重试**不会**恢复它；\
                 请重新执行未成功的 `add` **与 `remove`** 后再 `commit()`"
                    .to_string(),
            ));
        }

        // 可观测性（NFR-07）：发布是唯一的状态跃迁，必须留痕。
        let view = self.shared.snapshot();
        tracing::debug!(
            generation,
            segments = view.segments_in_order().count(),
            deltas = view.deltas.len(),
            tombstones = view.tombstones.len(),
            "视图已发布（S8-03：封段进 deltas）"
        );
        Ok(())
    }

    /// 当前已提交的分片数（不含 `builder` 里未提交的内容）。
    ///
    /// ⚠️ `S8-03` 起是**跨段求和**（`main + deltas`），不是只看 `main`。
    pub fn num_chunks(&self) -> u32 {
        self.view().bm25_totals().0
    }

    /// 当前已提交的文档数。
    ///
    /// ⚠️ **不扣跨段墓碑**：墓碑指向的 doc 在它所属的段里仍然存活 ⇒ 会被计入。
    /// 需要「对外真实的存活文档数」请用 [`Self::tombstone_stats`] 的 `docs_alive`
    /// （`S8-03` 评审 P3-3 指出两者在存在跨段删除时会 silently 分叉）。
    pub fn num_docs(&self) -> usize {
        self.view()
            .segments_in_order()
            .map(|seg| seg.index.num_docs())
            .sum()
    }

    /// 全语料词项总数（所有分片的分词数之和）。
    pub fn total_len(&self) -> u64 {
        self.view().bm25_totals().1
    }

    /// 平均分片长度（BM25 长度归一化用）。
    ///
    /// 🔴 用**全局** `total_len / N` 算一次（`I8-5`）：各段各算再平均会引入两次浮点舍入
    /// ⇒ 分数只在分段布局下漂移。
    pub fn avgdl(&self) -> f32 {
        let (n, total) = self.view().bm25_totals();
        if n == 0 {
            0.0
        } else {
            total as f32 / n as f32
        }
    }

    /// 累计 embed 推理耗时（NFR-03 构建耗时口径观测）。
    ///
    /// 只计 `embed_documents` 推理本身，不含归一化 / 灌向量索引。
    /// `add_documents` 会按 `batch_size` 分批自动 flush，因此 build 命令
    /// 不能在 `commit()` 处计时（那时大部分 embed 已在 add 阶段分批完成）——
    /// 应取这个累计值。
    pub fn embed_elapsed(&self) -> std::time::Duration {
        self.embed_elapsed
    }

    /// 累计送入 `embed_documents` 的条数（与 [`Self::embed_elapsed`] 同源累加）。
    ///
    /// ⚠️ **不要**用 `num_chunks()` 代替它来报「本次 embed 了多少条」：
    /// 增量构建（`build --index`）时 `num_chunks()` 是索引**总量**，
    /// 而本次真正吃推理的只有新增那批 ⇒ 会高估 NFR-03 的口径。
    pub fn embed_count(&self) -> usize {
        self.embed_count
    }

    /// 产出一个只读 `Searcher`（`'static + Clone + Send + Sync`），**不消耗写端**。
    ///
    /// # 与 `into_searcher()` 的两点差别（`D-S8-06`）
    ///
    /// | 项 | `into_searcher(mut self)`（旧） | 本方法（新） |
    /// | --- | --- | --- |
    /// | 接收者 | `self`（消耗） | `&self`（**不消耗**，写端可继续 `add` / `commit`） |
    /// | 隐含 `flush()` | **是** | **否**（⚠️ 调用方须自己 `commit()`） |
    ///
    /// ⚠️ **「不隐含 flush」是有意的语义收紧**：它让「可见性 = `commit()` 后」在 API
    /// 层面显式化（对齐 NFR-11），而不是靠一个隐式副作用。
    ///
    /// ⚠️ **`S8-02` 期有过一条过渡约束**（写端在有并发检索时写入会返回 `Err`）——
    /// `S8-03` 起**已解除**（写入落在写端私有的 builder 上）。
    ///
    /// ⚠️ **可见性口径要连读模块文档**（评审 P4-6b）：「不隐含 `flush`」说的是
    /// **向量 / 写缓冲**的落地；`S8-02` 期**倒排是边 `add` 边可见的**（内容就在 `main` 里，
    /// 这就是「行为与重构前逐位一致」的含义）。「可见性 = `commit()` 后」是 `S8-03`
    /// （delta 分段）起的**终态**语义 —— 只读本方法文档容易误判成前者已经成立。
    pub fn searcher(&self) -> crate::search::Searcher {
        crate::search::Searcher {
            shared: Arc::clone(&self.shared),
            graph_status: self.graph_status.clone(),
        }
    }

    /// 交出所有权，产出只读 `Searcher`（**隐含 flush**）。
    ///
    /// ⚠️ **已废弃**：改用 [`Self::searcher`]（不消耗写端）并**显式** `commit()`。
    ///
    /// 本方法保留为薄封装 = `{ self.commit()?; self.searcher() }` ⇒ 既有调用点零改动、
    /// **行为与重构前逐位一致**（`commit()` == `flush()` + 发布，见 [`Self::commit`]）。
    #[deprecated(note = "改用 searcher()；并显式 commit()")]
    pub fn into_searcher(mut self) -> Result<crate::search::Searcher> {
        self.commit()?;
        Ok(self.searcher())
    }

    /// 删除一个文档（及其全部分片），含统计量回滚（复用 `Index::remove`）。
    ///
    /// # 删除的三层语义（V2 Step 1 / D-S1-01）
    ///
    /// | 层             | 是否物理摘除                       | 失效方式                    |
    /// | ------------- | ---------------------------- | ----------------------- |
    /// | 倒排 postings   | ✅ `Index::remove` 时物理摘除          | ——                      |
    /// | 正排 chunk      | ✅ 置 `None`（墓碑位）                | `alive` 位图（O(1)）        |
    /// | 向量（HNSW 图）    | ❌ hnsw_rs 无 remove API           | 检索期用存活位图谓词挡掉（Q-C1 的修复点） |
    /// | `raw_vectors` | ✅ 本方法顺带摘除                      | 防止幽灵候选**跨快照永续**        |
    ///
    /// > ⚠️ 旧注释曾写「`raw_vectors` 中的旧向量会在下次 flush/重建时被丢弃」——
    /// > **这是错的**：`raw_vectors` 没有任何移除路径，`save` 原样导出、`load` 全量重灌，
    /// > 幽灵候选因此会跨快照永续。现在 `remove` 主动摘除，另由存活位图在检索期兜底。
    pub fn remove(&mut self, doc_id: DocId) -> Result<()> {
        let cfg = Arc::clone(&self.shared.cfg);

        // ── 分支 ①：目标在**当前 builder**（未发布）⇒ 就地物理删（与今天同语义）
        //
        // 这是最常见的一支（`add` 后立刻 `remove` 同一篇）：不产生墓碑、不留半修改状态。
        if doc_id >= self.builder.base_doc
            && ((doc_id - self.builder.base_doc) as usize) < self.builder.index.total_docs()
        {
            let local = doc_id - self.builder.base_doc;
            self.builder.index.remove(local, cfg.analyzer.as_ref())?;
            // 顺带摘除原始向量（O(len)，与 `Index::remove` 的 O(N) 同量级）：既缩小快照体积，
            // 也断掉「删除 → save → load → 幽灵候选复活」的路径。内存中的 HNSW 图仍需靠
            // 存活位图过滤（物理回收归 compaction）。
            let b = &mut self.builder;
            b.raw_vectors.retain(|(id, _)| b.index.is_live_chunk(*id));
            return Ok(());
        }

        // ── 分支 ②：目标在**既往段**（已发布的 `main` / `deltas`）⇒ 记墓碑，**不动既往段**
        //
        // 为什么不能就地删：段一旦进过任何 `View` 就**不可变**（`I8-2`）—— 那是读端拿到
        // 一致快照的前提。⇒ 只能记一条**跨段墓碑**，由合并时**物理化**（§4.9.2）。
        // 代价：热路径上有墓碑时要传存活谓词（`R52`，见 §4.7.3）—— 但**可逆**（合并后消失）。
        let view = self.view();
        let in_past = view.segments_in_order().any(|seg| {
            doc_id >= seg.base_doc && ((doc_id - seg.base_doc) as usize) < seg.index.total_docs()
        });
        if in_past {
            self.builder.tombstones.add(doc_id);
            return Ok(());
        }

        // ── 分支 ③：不存在 ⇒ no-op（与既有 `Index::remove` 对未知 ID 的越界安全行为一致）
        Ok(())
    }

    /// 落盘快照（**隐含 commit**，p6-design 6.2：不允许带未刷缓冲落盘）。
    ///
    /// 快照写入当前装配的配置指纹（p6-design 8.2），供 `load` 校验。
    ///
    /// # 图 sidecar（V2 Step 2 / §4.5）
    ///
    /// 顺序：写快照正文（取回 body_crc）→ dump 图 → 算两文件 CRC → 读回
    /// `Description` → 原子发布 manifest。**manifest 是唯一发布点**：
    /// 图 dump 失败时（P0-3）默认只警告、不发布 manifest，`save` 仍返回 Ok——
    /// 图是缓存，下次冷启动降级重建即可，快照本身完好。
    pub fn save(&mut self, path: &std::path::Path) -> Result<()> {
        // `D-S8-01`：**第一步就合并** ⇒ 落盘形态仍是**单段 `Index`**
        // （`SnapshotSections` / `FORMAT_VERSION` / `GraphManifest` 一个字节都不用改）
        self.commit()?;
        self.fold_deltas()?;
        let seg = Arc::clone(&self.view().main);
        let vectors: Vec<(ChunkId, Vec<f32>)> = seg.raw_vectors.as_deref().unwrap_or(&[]).to_vec();
        let body_crc = crate::storage::save_with_crc(
            path,
            &seg.index,
            &vectors,
            &self.shared.cfg.fingerprint(),
        )?;

        // 图 sidecar 单独计时（验收 7）：`save` 的总耗时里，快照写入与图 dump
        // 是两个数量级完全不同的成本，混在一起看不出图持久化的真实代价。
        let t_dump = std::time::Instant::now();
        self.graph_dump_elapsed = None;
        match self.persist_graph(path, &vectors, body_crc) {
            Ok(st) => self.graph_status = st,
            Err(e) => {
                // Strict：图失败升级为 Err。快照这会儿**已经落盘**，但状态仍要
                // 记录下来——调用方手里还有这个 idx，诊断时不能无从查证。
                self.graph_status = GraphStatus::PersistFailed(format!("{e}"));
                return Err(e);
            }
        }
        if !matches!(self.graph_status, GraphStatus::NotApplicable) {
            self.graph_dump_elapsed = Some(t_dump.elapsed());
        }
        // D-S4-02（触发方式）：`save` 只告警、不自动 compaction——把 10~100s 的重建
        // 塞进写路径会让耗时不可预测。墓碑占比 ≥ 阈值且总量足够时才提示。
        let stats = self.tombstone_stats();
        if stats.chunks_total >= 1024 && stats.tombstone_ratio >= 0.2 {
            eprintln!(
                "[提示] 墓碑占比 {:.1}%，建议运行 helix compact 回收（chunks {}/{}）",
                stats.tombstone_ratio * 100.0,
                stats.chunks_total - stats.chunks_alive,
                stats.chunks_total
            );
        }
        Ok(())
    }

    /// 最近一次 `save` 中图 sidecar 落盘的耗时（验收 7 观测点）。
    ///
    /// `None` = 未走图持久化（纯 BM25 / Brute 后端 / `--no-graph-persist`）。
    /// 注意这是**纯 dump 成本**，不含 HNSW 建图（建图发生在 `flush`/`add` 阶段）。
    pub fn graph_dump_elapsed(&self) -> Option<std::time::Duration> {
        self.graph_dump_elapsed
    }

    /// 写图 sidecar 并发布 manifest（§4.5 步骤 3~7）。
    ///
    /// 返回最终的 `GraphStatus`；`--no-graph-persist` 或纯 BM25 / Brute 时
    /// 清理可能残留的旧 sidecar 后返回 `NotApplicable`。
    fn persist_graph(
        &mut self,
        path: &std::path::Path,
        vectors: &[(ChunkId, Vec<f32>)],
        body_crc: u32,
    ) -> Result<GraphStatus> {
        // 逃生舱 / 无向量 / Brute 后端：不写图，并清掉可能存在的僵尸 sidecar
        //（否则「关掉向量重建库」会留下永远匹配不上的旧图文件，§5.4）
        let seg = Arc::clone(&self.view().main);
        let Some(vi) = seg.vector_index.as_ref() else {
            crate::storage::remove_sidecars(path)?;
            return Ok(GraphStatus::NotApplicable);
        };
        if !self.shared.graph.persist || vectors.is_empty() {
            crate::storage::remove_sidecars(path)?;
            return Ok(GraphStatus::NotApplicable);
        }
        let Some(g) = vi.as_graph_persist() else {
            // Brute 无图（类型事实，P0-4）
            crate::storage::remove_sidecars(path)?;
            return Ok(GraphStatus::NotApplicable);
        };

        // P0-3（评审 #12 发现 2 修订）：**整个写图链路**都按「缓存」语义处理。
        // 只把 `dump_graph` 包进 Lenient 是不够的——紧随其后的 CRC 扫描、
        // manifest 原子发布同样会返回 Err，而此刻快照已完整落盘，
        // 让 `save()` 失败等于「缓存写坏了把主数据一起否决」。
        match write_graph_sidecar(path, g, body_crc, self.shared.cfg.fingerprint().dim) {
            Ok(()) => Ok(GraphStatus::Loaded),
            Err(e) if self.shared.graph.mode == GraphPersistMode::Strict => {
                // D-S3-07（拍板）：Strict 返回 Err 前也补 best-effort 清理——
                // 「失败上抛」不等于「失败且留垃圾」：此刻 dump 可能已删旧图，
                // 残留半截 `*.hnsw.graph`/`*.hnsw.data` + 已失效的旧 manifest
                // （其 CRC 锚已不匹配新快照，load 必降级重建——功能安全但目录
                // 留尸体）。被删的都是垃圾；清理自身失败不升级、不改 Err 语义。
                if let Err(ce) = crate::storage::remove_sidecars(path) {
                    eprintln!("[警告] 图 sidecar 清理失败（残留文件不影响正确性）: {ce}");
                }
                Err(e)
            }
            Err(e) => {
                // 已发布的旧 manifest 必须删除（否则下次加载会拿到过期图）。
                // 清理本身失败也只是「残留垃圾」，不升级为 Err。
                if let Err(ce) = crate::storage::remove_sidecars(path) {
                    eprintln!("[警告] 图 sidecar 清理失败（残留文件不影响正确性）: {ce}");
                }
                eprintln!("[警告] 向量图落盘失败，已跳过（缓存，快照本身完好）: {e}");
                Ok(GraphStatus::PersistFailed(format!("{e}")))
            }
        }
    }

    /// 最近一次图 sidecar 的状态（NFR-07 可观测性：降级必须可见）。
    pub fn graph_status(&self) -> &GraphStatus {
        &self.graph_status
    }

    // ---- compaction（V2 Step 4 / T7-12 / FR-30，墓碑物理回收）----

    /// 墓碑统计（**只读、无副作用**）。`helix compact --dry-run` 的决策依据。
    ///
    /// `graph_points` 是向量索引中的点数（含墓碑——hnsw_rs 无 remove，墓碑留图，
    /// 由存活位图在检索期挡掉）；`raw_vectors` 已由 `remove` 的 `retain` 摘除墓碑。
    pub fn tombstone_stats(&self) -> TombstoneStats {
        let view = self.shared.snapshot();
        // ⚠️ **跨段求和**（设计 §4.9.5）：`main` 之外还有 `deltas`（`S8-02` 期恒空）。
        //
        // 🔑 求和逻辑在 `View::sums()`（`view.rs`）—— 抽成纯函数才能直接喂一个
        // **手工构造的、带 delta 的 `View`** 去测它（`S8-02` 期 `deltas` 恒空 ⇒
        // 内联写法「非空时是否正确累加」永远测不到）。
        //
        // 🔴 `S8-02` 评审 P3-2：先前此处只有一条 `debug_assert!(deltas.is_empty())`
        // 而**没有任何求和**，与本注释的声明不符 ⇒ `S8-03` 一旦 `deltas` 非空：
        // debug 下断言红、**release 下静默少算**。现改为真求和。
        let sums = view.sums();
        // 跨段墓碑指向的是**既往段里仍存活**的 doc ⇒ 必须从「存活文档数」扣掉，
        // 否则该数会偏大。⚠️ `S8-03` 起墓碑是**正常状态**（`remove` 命中既往段即记），
        // 合并时**物理化**（§4.9.2）⇒ 不再有「S8-02 期恒空」的断言。
        let docs_alive = sums.docs_alive.saturating_sub(view.tombstones.len());
        TombstoneStats {
            chunks_total: sums.chunks_total,
            chunks_alive: sums.chunks_alive,
            docs_total: sums.docs_total,
            docs_alive,
            graph_points: sums.graph_points,
            raw_vectors: sums.raw_vectors,
            tombstone_ratio: if sums.chunks_total == 0 {
                0.0
            } else {
                1.0 - sums.chunks_alive as f64 / sums.chunks_total as f64
            },
        }
    }

    /// 把**全部**未合并段（`deltas`）按 FIFO 合并进主段 —— `save()` / `compact()` 的**前置**。
    ///
    /// # 这是 `S8-06`（合并器）的**最小文本侧形态**
    ///
    /// 只做「文本侧 append + 跨段墓碑**物理化**（§4.9.2）+ 向量侧重建」，**不含**增量式
    /// 向量合并、`MergeReport`、`merge_pending` / `merge_all` 的公开面、字段索引等价性用例
    /// （那些留给 `S8-06`）。
    ///
    /// ⇒ 它存在的**唯一理由**：`D-S8-01` / `D-S8-12` 要求 `save` / `compact` 的**第一步**
    ///    就是合并 —— 否则落盘只有 `main`，会**静默丢内容**。⚠️ 与设计 §8 的 PR 切分表
    ///    （合并器属 `S8-06`）的偏差见 PR 正文。
    ///
    /// # 与并发读的关系
    ///
    /// 段一旦进过 `View` 就不可变（`I8-2`）⇒ 合并**不改旧段**，而是**克隆**主段内容后
    /// 追加、再发布一个**新** `View`（一次原子指针替换）⇒ **读端全程无感**
    /// （`FR-17` 的「合并期间不得阻塞读请求」在结构层面成立）。代价是一次 `O(N)` 克隆
    /// —— 与合并本身的量级相同。
    ///
    /// # 返回
    ///
    /// 本次合并掉的 delta 段数。
    fn fold_deltas(&mut self) -> Result<usize> {
        // 🔴 **顺序是硬要求**（`S8-03` 评审 P2-4）：**先记世代、后取视图快照**。
        //    反过来的话，若 `compact()` 恰好落在两次读之间，我们会拿到「旧进程内的视图 +
        //    新世代的 epoch」⇒ `commit_view` 的世代对账**通过** ⇒ 把基于旧 `main` 克隆出来的
        //    `merged` 发布进新视图（旧 ID 空间覆盖新 ID 空间）。
        //    先记世代则相反：期间任何 `compact()` 都会让 epoch 变化 ⇒ 发布被拒（保守但正确）。
        let snapshot_epoch = self.shared.epoch();
        let view = self.view();
        if view.deltas.is_empty() {
            return Ok(0);
        }
        let cfg = Arc::clone(&self.shared.cfg);

        // ① 文本侧：**克隆**主段内容后 append。
        //
        // 🔑 这里**刻意不用 `Arc::try_unwrap` 取独占**：`shared.view` 自己就挂着那个
        //    `Arc<View>`（其 `.main` 也被计数）⇒ `try_unwrap` **永远失败**，除非先把
        //    视图换掉（那就成了先发布再合并的循环）。⇒ 取形 = **克隆**（`O(N)`，
        //    与合并本身的量级相同）：段不可变（`I8-2`）⇒ 不改旧段、不需要「无读者」，
        //    因此合并**与并发读并存**（`FR-17` 的「合并不阻塞读」在结构层面成立）。
        let mut merged = view.main.index.clone();
        let mut raw_all: Vec<(ChunkId, Vec<f32>)> =
            view.main.raw_vectors.as_deref().unwrap_or(&[]).to_vec();
        let had_vectors = view.main.vector_index.is_some();
        let segment_generation = view.main.generation;
        let merged_segments = view.deltas.len();
        // ⚠️ delta 段里的 `raw_vectors` 记的是**段内本地** `chunk_id`（builder 侧分配的），
        //    而 `main.raw_vectors` 记的是全局（`base = 0`）⇒ 拼接时必须**全局化**，
        //    否则落盘的原始向量带着错位的 ID（`load` 后向量与分片对不上）。
        let mut delta_raw: Vec<(ChunkId, Vec<f32>)> = Vec::new();
        for seg in view.deltas.iter() {
            if let Some(raw) = seg.raw_vectors.as_deref() {
                for (local, v) in raw {
                    delta_raw.push((seg.base_chunk + local, v.clone()));
                }
            }
            merged.merge_from(seg.index.clone())?;
        }
        raw_all.extend_from_slice(&delta_raw);

        // ② 跨段墓碑**物理化**（§4.9.2，评审 P3-1 的取形）：FIFO 保证「墓碑的目标段必已
        //    先被合入」⇒ 直接走既有 `Index::remove` 语义（摘 postings + 清
        //    `content_hashes` + 回滚统计量 + 同步字段索引），**然后**从集合里移除墓碑
        //    ⇒ 热路径回到零谓词（§4.7.3，R52 的回归**可逆**）。
        //    主段的 `base_doc` 恒 0 ⇒ 全局 `doc_id` == 本地 `doc_id`。
        debug_assert_eq!(view.main.base_doc, 0, "主段的 base_doc 恒为 0");
        for doc in view.tombstones.iter() {
            merged.remove(doc, cfg.analyzer.as_ref())?;
        }

        // ②b `raw_vectors` 只保留**仍存活**的 chunk（与既有 `remove` 的 `retain` 同语义）：
        //     ① delta 段内部的墓碑位（就地删过的 chunk）与 ② 刚被物理化的跨段墓碑目标
        //     都可能留下向量 ⇒ 不滤掉，快照就会带着死 chunk 的向量
        //     （既有用例 `T5b_remove在flush后raw_vectors被retain摘净` 正是钉这个）。
        raw_all.retain(|(id, _)| merged.is_live_chunk(*id));

        // ③ 向量侧：**全量重建**（本 PR 暂取的保守取形，= 设计 §4.6.2 的「方案 B」）。
        //
        // ⚠️ **如实陈述**（`S8-03` 评审 P2-3 更正了本文原有的自相矛盾与一个绝对命题）：
        //
        // - **为什么现在只能重建**：合并**拿不到**主段图的所有权 —— `main` 是
        //   `Arc<Segment>` 共享（段不可变，`I8-2`）、`Box<dyn VectorIndex>` 也**不是
        //   `Clone`** ⇒ 「把 delta 的点直接 add 进旧图」这条就地路径在本 PR 的取形下**走不通**。
        // - 🔴 **但「所有权」不构成「只能重建」的证明**：`D-S8-09` 的**默认方案 A**
        //   （dump→load→增量 insert）**根本不需要旧图所有权** —— `dump_graph` 是 `&self`
        //   （设计 §4.9.3 已写明「对内存图也能做」），load 产出**新**图、再往新图 insert
        //   delta 的点 ⇒ 保住旧点拓扑，成本 O(delta) 而非 O(N·logN)。
        //   本 PR **不**实现方案 A（留给 `S8-06`），也**不**声称它不可行。
        // - **代价（方案 B）**：每次 `save`/`compact` 只要有 delta 就 O(N·logN) 重建 +
        //   **拓扑被重写** ⇒ ANN 排名会漂移。实测已显形：`atomic_snapshot::TI5…` 的检索手段
        //   被迫从 hybrid 改为 bm25（`CHANGELOG` 同条目）。
        // - ⚠️ **spike S8-S1 一条判据都还没测**（本 PR 只落地、未标定）⇒ 这个取形是
        //   **暂定**的，不是标定结论。
        // - `MergeReport.vector_strategy` / spike 结论 → `S8-06`。
        let vector_index = if had_vectors {
            Some(rebuild_vector_index(
                self.shared.backend,
                &raw_all,
                cfg.ef_search,
                cfg.parallel_build,
            )?)
        } else {
            None
        };

        // ⑤ 原子发布：`main` 吸收全部内容、`deltas` 清空、墓碑清空（已物理化）。
        //    ⚠️ 合并**不改 ID** ⇒ 已用长度不变（后续段的基址继续有效）。
        let new_main = Arc::new(Segment {
            index: merged,
            vector_index,
            raw_vectors: Some(raw_all),
            base_doc: 0,
            base_chunk: 0,
            generation: segment_generation,
        });
        let used_doc = new_main.index.total_docs() as DocId;
        let used_chunk = new_main.index.total_chunks() as ChunkId;
        // 🔴 **P1-1 的修复**（`S8-03` 第 1 轮评审）：发布闭包**必须消费锁内的 `cur`**。
        //
        // 原写法 `move |_cur, …|` 恒发布 `deltas = []` + 锁外算出的 `used_*` ⇒ 若另一个写端
        // 在「取快照」与「拿这把锁」之间提交了新 delta，那个 delta 会**从视图里消失**、
        // 且 `ids.next_*` **回缩**（⇒ 后续内容复用已发号的 ID）。
        // 现在改为把窗口内新提交的 delta **无损吸收**（合并不改 ID ⇒ 基址仍有效），
        // 并只摘掉**本次已物理化**的墓碑（`Shared::absorb_window`，纯函数 + 单测）。
        //
        // 🔴 **P1-2 的修复**（`S8-03` 第 2 轮评审）：吸收**必须校验前缀身份**，不能只看长度。
        //    `fold` **不改 `epoch`** ⇒ 世代对账拦不住并发 fold；而「B fold 过（D1 → main、
        //    `deltas` 清空）⇒ B 又提交 D2」之后，`cur.deltas = [D2]` 与快照 `[D1]` **长度相等**
        //    ⇒ 长度判据放行 ⇒ `skip(1)` 跳过 D2 ⇒ **D2 静默消失**（`ids.next_*` 却已记账 = 永久孤儿）。
        //    现在不符即 `Err(Busy)`（与 epoch 同型：不记账、不发布、如实声明丢弃）。
        // 🔴 **快照必须连 `Arc` 一起带进闭包**（`S8-03` 第 2 轮评审 **P1-2**）：只带**长度**的话，
        //    「另一个写端 `fold` 过（D1 进 `main`、`deltas` 清空）⇒ 它又 `add` + `commit` 了 D2」
        //    这一格会被 D2 **顶替**，而长度恰好相等 ⇒ 长度判据**看不出来** ⇒ **D2 静默消失**
        //    （而 `ids.next_*` 已把它的槽位记账）。段不可变（`I8-2`）⇒ 用 `Arc::ptr_eq` 判**身份**。
        //    `Vec<Arc<_>>` 的 clone 是 O(1)（只加引用计数）。
        let snapshot_deltas: Vec<Arc<Segment>> = view.deltas.iter().cloned().collect();
        let physicalized = (*view.tombstones).clone();
        self.shared.commit_view(
            snapshot_epoch,
            move |cur, generation, base_doc, base_chunk| {
                let Some((carried, tombstones)) = Shared::absorb_window(
                    &cur.deltas,
                    &snapshot_deltas,
                    &cur.tombstones,
                    &physicalized,
                ) else {
                    // 与 `expected_epoch` 同型处置：**拒绝发布**（本次不记账、不发布）。
                    // ⚠️ 但**声明的内容与另外两条不同**：合并是**维护性**操作，被拒时
                    //    「增量段仍在视图里、内容**没有**丢失」，丢掉的只是本次合并的**计算**
                    //    ⇒ 恢复指引必须是「重试本次 `save`/`compact`」，**不是**「重新 add/remove」。
                    //    （`add` 那条路径才是真的丢内容：builder 已被换掉。）
                    return Err(Error::Busy(
                        "本次合并依据的增量段快照已失效（窗口内有另一个写端 `fold` 过同一个 \
                         `Arc<Shared>` ⇒ 视图的增量段列表已不是当初那一批；长度可能相同，\
                         但段的身份不同）⇒ 本次发布已拒绝。\
                         ✅ **内容没有丢失**：那些增量段仍然在视图里、照旧可检索；\
                         丢掉的只是本次合并的计算结果 —— **重试本次 `save()` / `compact()` 即可**，\
                         不需要重新执行 `add` / `remove`"
                            .to_string(),
                    ));
                };
                Ok((
                    View {
                        main: Arc::clone(&new_main),
                        deltas: carried,
                        tombstones,
                        generation,
                    },
                    // 吸收后「已用长度」= 锁内真实值（新 delta 的末端）⇒ 不得回退
                    used_doc.max(base_doc),
                    used_chunk.max(base_chunk),
                ))
            },
        )?;
        Ok(merged_segments)
    }

    /// 在**内存**里按存活集重新物化一次并重建向量图。**不落盘**。
    ///
    /// - 内部第一步是 `self.commit()?`（D-S4-10）：先把写缓冲 flush 掉再重编号，
    ///   否则 `pending` 中的旧 `chunk_id` 会污染 `raw_vectors` 与新建的图（幽灵点）。
    /// - 不变式（§4.9）：I1 存活集不变、I2 BM25 统计量不变、I3 BM25 逐位一致、
    ///   I5 失败不留半压实（三者一起原子替换）、I7 `raw_vectors.len() ≤ 存活数`、
    ///   I8 返回时 `pending` 为空。
    /// - **`graph_status` 保持磁盘旧值**（描述的是 sidecar 状态，磁盘尚未更新，§4.6）。
    /// - ⚠️ **ID 可能变更**（D-S4-01 重编号）：跨 compaction 的持久引用请用
    ///   `source` / `content_hash`，不要用 `doc_id` / `chunk_id`。
    /// - `bytes_before` / `bytes_after` 均为 `None`（无路径可 stat）；要持久化请用
    ///   [`Self::compact_and_save`]。
    /// - ⚠️ **ID 会重编号**（`D-S4-01`）：`compact` 是 `I8-7`（段 ID 空间不重叠）的
    ///   **唯一合法例外** —— 它把存活内容稠密化 ⇒ **已用长度变小** ⇒ 走 `Shared::publish_reset`
    ///   （允许回退）而不是 `commit_view`（要求只增），并**清空 `deltas` 与跨段墓碑**
    ///   （它们引用的是旧 ID 空间）⇒ 之后本写端会换一个与新「已用长度」对齐的 builder。
    /// - `S8-02` 期那条「末步 `with_main_mut` 失败 ⇒ 丢弃整套重建结果 / 请在无活动读者时调用」
    ///   的说明随该入口退出生产路径而**作废**（`S8-03` 评审 P3-4.3）。
    pub fn compact(&mut self) -> Result<CompactionReport> {
        self.compact_with_bytes(None)
    }

    /// `compact()` + 既有 `save()`——**落盘的唯一入口**。
    ///
    /// `save()` 内部 commit（此时已 no-op）→ `save_with_crc`（atomic_write）→
    /// dump 新图 → CRC → 发布新 manifest——架构 §7.5.2 的「重建图后必须重发 manifest」
    /// 铁律由这条既有链路**自动满足**（§4.6），本方法不另开落盘路径。
    ///
    /// 落盘成功后 `graph_status` 更新为实际状态（`Loaded` / `PersistFailed`），
    /// `bytes_before` / `bytes_after` 为落盘前后的三体积。
    ///
    /// **失败语义（评审建议 3）**：`save` 失败（如 Strict 下图 dump 升级为 Err）时本方法
    /// 返回 `Err`，但**内存已在 `compact()` 阶段压实（I5 已原子替换主段）**——
    /// 返回的 `Err` 与内存状态不一致，磁盘仍是旧档。这是设计内行为：compaction 的核心
    /// 价值就是内存压实，落盘失败不回滚内存（I5 无 undo 路径）；调用方拿到 `Err` 后
    /// 可对同一 `path` 重试 `save()` 续写，不必重跑 `compact()`。
    pub fn compact_and_save(&mut self, path: &std::path::Path) -> Result<CompactionReport> {
        // 首次落盘（目标文件尚不存在）时 `bytes_before` 应为 `None`（诚实表达「此前无
        // 快照」），而非 `Some(0,0,0)`——`snapshot_bytes` 对缺失文件 `unwrap_or(0)` 永不
        // 出错（评审建议 4）。
        let bytes_before = if path.exists() {
            snapshot_bytes(path).ok()
        } else {
            None
        };
        let mut report = self.compact_with_bytes(bytes_before)?;
        let t_save = std::time::Instant::now();
        // 落盘（隐含 commit → no-op；dump 新图 + 重发 manifest）
        self.save(path)?;
        // 落盘后补全：三体积 + 图状态 + after 统计 + 总耗时（含 save）
        report.bytes_after = snapshot_bytes(path).ok();
        report.graph_status = self.graph_status.clone();
        report.after = self.tombstone_stats();
        report.total_ms += t_save.elapsed().as_millis();
        Ok(report)
    }

    /// compaction 的实现主体（§4.1 步骤 0~5 + I5 原子替换）。
    fn compact_with_bytes(&mut self, bytes_before: Option<SizeBytes>) -> Result<CompactionReport> {
        let t0 = std::time::Instant::now();

        // 步骤 0（D-S4-10 / I8）：先清空写缓冲，再谈重编号
        self.commit()?;
        // `D-S8-12`：**先合并再重编号** —— `compact` 会重编号，而合并依赖「基址不变」
        // ⇒ 顺序反了会让 `deltas` 里段的 `base_*` 指向已重编号的旧空间
        self.fold_deltas()?;

        let before = self.tombstone_stats();
        let has_tombstones = before.chunks_total > before.chunks_alive;

        // 无墓碑早退（评审建议 2）：`compact` 的全部价值都在物理回收，没有墓碑就
        // **没有可回收物**——此时重物化 + 重建整张图（10~100s 级）是纯浪费
        // （`remapped=false` 只保证 ID 不变，旧实现仍全量重建）。D-S4-02 刚在 `save`
        // 里引导用户跑 compact，无墓碑时务必早退。返回 `before == after` 的空 report
        //（语义与正常无墓碑一致，ID 一个不变）；`compact_and_save` 仍会幂等落盘。
        if !has_tombstones {
            return Ok(CompactionReport {
                reclaimed_chunks: 0,
                reclaimed_docs: 0,
                reclaimed_terms: 0,
                reclaimed_graph_points: 0,
                vector_rebuild_ms: 0,
                total_ms: t0.elapsed().as_millis(),
                remapped: false,
                graph_status: self.graph_status.clone(),
                bytes_before,
                bytes_after: None,
                before,
                after: before,
            });
        }

        // 步骤 1~2：取存活集 + 建 ID 映射（重编号，D-S4-01）
        let seg = Arc::clone(&self.view().main);
        let remap = seg.index.build_remap();

        // 步骤 3：Index 重新物化（返回新实例，self.inner.index 未动 → I5）
        let (new_index, reclaimed_terms) = seg.index.compacted(&remap);

        // 步骤 4：raw_vectors 过滤死 chunk + remap（I7）
        let new_raw = seg.raw_vectors.as_ref().map(|raw| {
            let mut out: Vec<(ChunkId, Vec<f32>)> = Vec::with_capacity(raw.len());
            for (old, v) in raw.iter() {
                if let Some(new_id) = remap.chunk.get(*old as usize).copied().flatten() {
                    out.push((new_id, v.clone()));
                }
            }
            out.sort_by_key(|(id, _)| *id); // 与插入顺序对齐（chunk_id 升序）
            out
        });

        // 步骤 5：向量索引重建（§4.5 / S4-06，与 load 降级同源）。
        // 判定维度是「是否有向量能力」（`had_vectors`），**与存活向量是否为空无关**
        // （D-S4-07 评审发现 1）：全删后 raw 为空，但只要装配有向量 lane（embedder 仍
        // 在配置里），就必须保留一个**空的**向量索引供后续 flush 灌入——否则落到
        // `vector_index = None`，flush 会误报 `NoEmbedder` 而砖死（索引不可恢复）。
        // `rebuild_vector_index` 对空 raw 天然安全（Hnsw `add_batch(&[])` no-op /
        // Brute `from_entries(&[])` 空索引），与设计 §4.5 的「无条件重建」一致。
        let had_vectors = seg.vector_index.is_some();
        let t_vec = std::time::Instant::now();
        let new_vi = match (&new_raw, had_vectors) {
            (Some(raw), true) => Some(rebuild_vector_index(
                self.shared.backend,
                raw,
                self.shared.cfg.ef_search,
                self.shared.cfg.parallel_build,
            )?),
            _ => None,
        };
        let vector_rebuild_ms = t_vec.elapsed().as_millis();

        // 全部构建成功 → **原子替换**（I5：任一 Err 都已 return，旧状态未动）。
        //
        // 🔴 `S8-03` 起走 [`Shared::commit_view`]（持 `ids` 锁的**唯一发布路径**）而不是
        //    就地改 `main`，因为 `compact` 会**重编号**（稠密化）⇒ 新主段**比原来短**
        //    ⇒ 必须把「已用长度」重置为新主段的长度。否则后续 `commit()` / 合并算出的
        //    `base_*` 会落在新主段之外（`commit_view` 的 `debug_assert` 会当场抓住 ——
        //    实测于 `step4_compaction` 的多条用例）。
        //    ⚠️ 旧实现（就地替换 `main`）在「单段 + 发号器恒 0」的骨架期恰好成立，
        //    多段之后不再够用；`deltas` / 跨段墓碑也必须一并清空（否则它们指向的 ID 空间
        //    已被重编号破坏）。
        let generation = seg.generation;
        let new_seg = Arc::new(Segment {
            index: new_index,
            vector_index: new_vi,
            raw_vectors: new_raw,
            base_doc: 0,
            base_chunk: 0,
            generation,
        });
        let used_doc = new_seg.index.total_docs() as DocId;
        let used_chunk = new_seg.index.total_chunks() as ChunkId;
        drop(seg);
        // ⚠️ 用 `publish_reset`（**允许**已用长度变小）而不是 `commit_view`（要求只增）：
        //    `compact` 会**重编号** ⇒ 新主段比原来短，这是 `I8-7` 的唯一合法例外。
        //    也正因为重编号，`deltas` 与跨段墓碑**必须一并清空**（它们引用的是旧 ID 空间）。
        self.shared.publish_reset(
            View {
                main: Arc::clone(&new_seg),
                deltas: Arc::from(Vec::<Arc<Segment>>::new()),
                tombstones: Arc::new(Tombstones::default()),
                // 占位：`publish_reset` 会用锁内推进出的序号覆盖它
                generation: 0,
            },
            used_doc,
            used_chunk,
        );
        // ⚠️ 重编号后「已用长度」变小 ⇒ **必须换掉 builder**（它记着旧基址）：
        //    否则后续任何 `commit()` 的基址对账都会不符并报 `Busy`
        //    （实测：`step4_compaction` 的 7 条用例全由此而来）。
        self.builder = Self::new_builder(&self.shared);

        let after = self.tombstone_stats();

        Ok(CompactionReport {
            reclaimed_chunks: before.chunks_total.saturating_sub(after.chunks_total),
            reclaimed_docs: before.docs_total.saturating_sub(after.docs_total),
            reclaimed_terms,
            reclaimed_graph_points: before.graph_points.saturating_sub(
                self.view()
                    .main
                    .vector_index
                    .as_ref()
                    .map(|v| v.len())
                    .unwrap_or(0),
            ),
            vector_rebuild_ms,
            total_ms: t0.elapsed().as_millis(),
            // 能走到这里必有墓碑（无墓碑已被上方 early-return 分流）
            remapped: true,
            graph_status: self.graph_status.clone(),
            bytes_before,
            bytes_after: None,
            before,
            after,
        })
    }

    /// 从快照加载（默认装配）。
    ///
    /// - 快照存**原始数据 + 原始向量**（D1），加载后重建倒排 + 向量索引
    /// - **配置指纹校验**（p6-design 8.2 / 修 B1/B2）：快照记录的指纹与当前
    ///   默认装配不一致时报 [`Error::ConfigMismatch`]，绝不静默换分词器/模型
    /// - 若快照含向量但当前装配无 embedder，向量不重建（纯 BM25）
    ///
    /// 加载**非默认装配**的快照（如 charabia）请用
    /// `SearchIndexBuilder::load(path)`（先按已知配置装配、再 load）。
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let builder = SearchIndexBuilder::default();
        let cfg = builder.build_config();
        let backend = builder.backend();
        Self::load_with(cfg, backend, builder.graph_opts(), path)
    }

    /// 按给定装配从快照加载（`load` 的实现主体）。
    ///
    /// 图 sidecar 的读取与校验见 `try_load_graph`（v2-step2-design §4.6）；
    /// **任何不符都降级为从 `raw_vectors` 全量重建**（图是派生缓存，丢弃不丢功能）。
    pub(crate) fn load_with(
        cfg: Config,
        backend: VectorBackend,
        graph: super::config::GraphOpts,
        path: &std::path::Path,
    ) -> Result<Self> {
        let (index, raw_vectors, fingerprint, body_crc) = crate::storage::load_with_crc(path)?;

        // 配置指纹校验（修 B1/B2），分维度严格度（见下方说明）：
        let actual = cfg.fingerprint();
        let analyzer_ok = fingerprint.analyzer_id == actual.analyzer_id;
        let chunker_ok = fingerprint.chunker == actual.chunker;
        // embedder：仅当**快照含向量**时才严格校验（id + dim 必须一致，否则
        // 用错模型重建向量索引会维度/语义错）；快照纯 BM25 时无向量可重建，
        // 装配有无 embedder 都是正确的（纯 BM25 检索不碰向量），故不校验。
        // 这使 CLI `search --index` 能加载 build 产出的两种快照（含向量 / 纯 BM25）。
        let embedder_ok = fingerprint.embedder_id.is_empty()
            || (fingerprint.embedder_id == actual.embedder_id && fingerprint.dim == actual.dim);
        if !(analyzer_ok && chunker_ok && embedder_ok) {
            return Err(Error::ConfigMismatch {
                expected: fingerprint.to_string(),
                actual: actual.to_string(),
            });
        }

        // 重建向量索引（V2 Step 2：优先从图 sidecar 加载，失败则降级重建）。
        //
        // 判定维度是「装配是否有向量能力」（`cfg.embedder`），**与快照向量是否为空
        // 无关**（评审发现 1 的 load 侧对齐）：装配配了 embedder 时，即使快照向量已
        // 删空（如全删后 compact_and_save），也必须保留**空**向量索引供后续写入——
        // 否则落 `vector_index = None`，flush 误报 `NoEmbedder` 砖死。重建对空向量
        // 天然安全（Hnsw `add_batch(&[])` no-op / Brute `from_entries(&[])` 空索引）。
        let mut graph_status = GraphStatus::NotApplicable;
        let vector_index = match cfg.embedder.as_ref() {
            Some(_) => Some(match backend {
                VectorBackend::Brute => {
                    // Brute 逃生舱：忽略图（精确扫描是确定性的，无需缓存）。
                    // V2 Step 4（S4-06）：与 Hnsw 降级重建共用 `rebuild_vector_index`。
                    rebuild_vector_index(backend, &raw_vectors, None, cfg.parallel_build)?
                }
                VectorBackend::Hnsw => {
                    let ef_search = graph.ef_search.unwrap_or(HnswRsIndex::default_ef_search());
                    if raw_vectors.is_empty() {
                        // 快照**无向量**（纯 BM25，或全删后 compact）——save 侧对空
                        // vectors 会清 sidecar 并落 `NotApplicable`（persist_graph 623 行），
                        // 故此处 try_load_graph 必然失败；直接静默建**空**向量 lane（评审
                        // 建议 6）：不 try_load_graph、不 warn_graph_degraded（无图可读，
                        // 不是「降级」）。PR30 不变式保留：装配有 embedder ⇒ 仍建
                        // `Some(空)` 向量索引供后续 flush 灌入，不落 None。
                        graph_status = GraphStatus::NotApplicable;
                        rebuild_vector_index(
                            backend,
                            &raw_vectors,
                            Some(ef_search),
                            cfg.parallel_build,
                        )?
                    } else {
                        match try_load_graph(
                            path,
                            &body_crc,
                            &fingerprint,
                            &raw_vectors,
                            graph,
                            ef_search,
                            cfg.parallel_build,
                        ) {
                            Ok(loaded) => {
                                graph_status = GraphStatus::Loaded;
                                Box::new(loaded) as Box<dyn VectorIndex>
                            }
                            Err(reason) => {
                                // 降级：图是缓存，丢弃只影响冷启动耗时
                                warn_graph_degraded(&reason, graph.mode)?;
                                graph_status = GraphStatus::Rebuilt(reason);
                                rebuild_vector_index(
                                    backend,
                                    &raw_vectors,
                                    Some(ef_search),
                                    cfg.parallel_build,
                                )?
                            }
                        }
                    }
                }
            }),
            // 纯 BM25 装配（无 embedder）：快照即便带向量也无从检索，向量 lane 保持
            // None（与 flush 421 行 embedder None 短路一致）。
            None => None,
        };

        let main = Arc::new(Segment {
            index,
            vector_index,
            raw_vectors: Some(raw_vectors),
            // 单段快照：基址归零、`deltas` 空、发号器归零（设计 §4.10）
            base_doc: 0,
            base_chunk: 0,
            generation: 0,
        });
        let shared = Arc::new(Shared::new(Arc::new(cfg), graph, backend, main));
        let builder = Self::new_builder(&shared);
        Ok(Self {
            shared,
            builder,
            embed_elapsed: std::time::Duration::ZERO,
            embed_count: 0,
            graph_status,
            graph_dump_elapsed: None,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;

    fn bm25_index() -> SearchIndex {
        let cfg = SearchIndexBuilder::default().embedder(None).build_config();
        SearchIndex::from_config(cfg, VectorBackend::Brute, Default::default())
    }

    /// **`S8-02`**：`commit()` 必须**发布新视图**（`generation` 递增）。
    ///
    /// 🔴 **为什么这条必须是单测**（变异实测，2026-09-18）：`S8-02` 期内容与视图**同源**
    /// （写端就地改 `main`）⇒「`commit()` 不做 publish」在**外部 API 层面不可观测**
    /// （读端读的还是那一个段，照样看得到新内容）⇒ **任何集成测试都抓不住它**
    /// （**M5 变异实测**：注入后 `tests/step8_segments.rs` 的 8 条**全绿**）。
    /// 能抓住的只有这里（`shared` 是 `pub(crate)`）。
    /// `S8-03` 起内容落进 `deltas` 后，可见性用例（§1.3 第 7 条）才会从外部覆盖它。
    #[test]
    fn S8_02_commit递增视图序号() {
        let mut idx = bm25_index();
        let g0 = idx.shared.snapshot().generation;
        idx.commit().unwrap();
        assert_eq!(
            idx.shared.snapshot().generation,
            g0 + 1,
            "commit() 必须恰好发布一次新视图"
        );
        idx.commit().unwrap();
        assert_eq!(
            idx.shared.snapshot().generation,
            g0 + 2,
            "每次 commit() 都必须发布（空 commit 也发布 ⇒ 幂等性只体现在内容上）"
        );
        // 骨架期不变式：发布不产生新段
        assert!(idx.shared.snapshot().deltas.is_empty());
    }

    /// **`S8-02`**：**并发 `commit()` 不得丢失视图序号**（`generation` 只增不减，NFR-07）。
    ///
    /// # 这条锁的是一个真实缺陷（`S8-02` 评审 P2，`2026-09-18`）
    ///
    /// `commit()` 原本是「`snapshot` → `advance` → `publish`」**三段分离**（`ids` 锁只覆盖
    /// `advance` 内部），而 `into_index()` 起总是成功 ⇒ 同一 `Arc<Shared>` 上可有多个写端
    /// 句柄（`SearchIndex: Send`）⇒ 两写端交错时**后发布的可能是更早 `advance` 的序号**。
    ///
    /// **修复前实测**（本条就是当时的复现用例）：4 写端 × 150 次 `commit()` ⇒ 观察线程
    /// 看到 **70 次回退**（样本 `(65,64)` / `(82,81)` / `(134,127)`）。
    ///
    /// # 判据为什么看「过程」而不是「最终值」
    ///
    /// 只看终点会**漏**：最后 `publish` 的恰好是最后 `advance` 的线程时终值仍然正确
    /// （实测：同一压力下用「终值 == 总数」判据跑 3 轮**全绿**）。⇒ 用观察线程持续快照，
    /// 一旦看到比历史最大值更小的序号即记录。
    ///
    /// # 修复后的性质（诚实标注）
    ///
    /// 四步在 `Shared::commit_view` 的 `ids` 锁内 ⇒ `publish` 顺序 == `advance` 顺序
    /// ⇒ **结构性成立**。⇒ 本用例是**回归锁**：它的牙齿来自**修复前的复现**（70 次），
    /// 而不是当前代码里某个可变点；删掉它不会有别的用例变红。
    #[test]
    #[allow(deprecated)] // `into_index()`（deprecated）是构造**多个写端**的唯一途径
    fn S8_02_并发commit不丢失视图序号() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let a = bm25_index();
        let b = a.searcher().into_index().unwrap();
        let c = a.searcher().into_index().unwrap();
        let d = a.searcher().into_index().unwrap();
        let sh = Arc::clone(&a.shared);
        const N: usize = 150;

        let stop = Arc::new(AtomicBool::new(false));
        let drops = Arc::new(std::sync::Mutex::new(Vec::<(u64, u64)>::new()));
        {
            let sh = Arc::clone(&sh);
            let stop = Arc::clone(&stop);
            let drops = Arc::clone(&drops);
            std::thread::spawn(move || {
                let mut max_seen = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let g = sh.snapshot().generation;
                    if g < max_seen {
                        drops.lock().unwrap().push((max_seen, g));
                    } else {
                        max_seen = g;
                    }
                }
            });
        }

        let spawn = |mut w: SearchIndex| {
            std::thread::spawn(move || {
                for _ in 0..N {
                    w.commit().unwrap();
                }
            })
        };
        for h in [spawn(a), spawn(b), spawn(c), spawn(d)] {
            h.join().unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        // 给观察线程最后一次读的机会（它可能正卡在读锁上）
        std::thread::sleep(std::time::Duration::from_millis(20));

        let observed = drops.lock().unwrap();
        assert!(
            observed.is_empty(),
            "generation 只增不减（NFR-07）：观察到 {} 次回退，样本 {:?}",
            observed.len(),
            observed.iter().take(3).collect::<Vec<_>>()
        );
    }

    #[test]
    fn 空索引可构造() {
        let idx = bm25_index();
        assert_eq!(idx.num_chunks(), 0);
        assert!(idx.builder.pending.is_empty());
    }

    /// **`S8-03` 口径反转**（原用例名：`add后未commit也能查到倒排`）。
    ///
    /// `delta` 分段后 `add` 写进**写端私有的 builder**，直到 `commit()` 才被冻结成新段并
    /// 发布 ⇒ 「**未 `commit` 即不可见**」成为**唯一**的可见性语义（`NFR-11` / §1.3 第 7 条）。
    /// ⚠️ 这是**有意的语义收紧**，不是行为回归 —— 设计早已写明 `S8-03` 落地时本用例
    /// **必须反转**（见 `v2-step8-design.md` §1.3 与 `tests/step8_segments.rs` 的模块文档）。
    #[test]
    fn add后未commit查不到() {
        let mut idx = bm25_index();
        let out = idx.add("BM25 检索算法").unwrap();
        assert!(!out.deduped);
        assert_eq!(out.chunk_ids.len(), 1);

        // 未 commit ⇒ 既不在已发布统计量里，也检索不到
        assert_eq!(idx.num_chunks(), 0, "未 commit 的内容不计入已发布统计量");
        assert!(
            idx.searcher().search("BM25").unwrap().hits.is_empty(),
            "未 commit ⇒ 查不到（NFR-11 的可见性边界）"
        );

        // commit ⇒ 立即可见
        idx.commit().unwrap();
        assert_eq!(idx.num_chunks(), 1);
        assert!(!idx.searcher().search("BM25").unwrap().hits.is_empty());
    }

    #[test]
    fn 幂等upsert返回deduped() {
        let mut idx = bm25_index();
        let a = idx.add("BM25 检索算法").unwrap();
        // ⚠️ `S8-03` 起必须 `commit()`：查重是**跨段**的（builder → deltas 逆序 → main），
        //    未发布的 builder 也能被查到 —— 这里显式 commit 是为了同时覆盖「已发布段」那一支
        idx.commit().unwrap();
        let b = idx.add("BM25 检索算法").unwrap();
        assert!(!a.deduped);
        assert!(b.deduped, "第二次同内容应 deduped");
        assert_eq!(a.doc_id, b.doc_id);
        assert!(b.chunk_ids.is_empty());
        assert_eq!(idx.num_chunks(), 1, "不应产生重复分片");
    }

    #[test]
    fn dedup_key替代text哈希() {
        let mut idx = bm25_index();
        let a = idx
            .add(Document::new("文本A").with_dedup_key("key-1"))
            .unwrap();
        // 不同 text 但同 dedup_key → 视为同一文档
        let b = idx
            .add(Document::new("文本B").with_dedup_key("key-1"))
            .unwrap();
        assert!(b.deduped);
        assert_eq!(a.doc_id, b.doc_id);
    }

    #[test]
    fn commit与flush均为noop无向量() {
        let mut idx = bm25_index();
        idx.add("文本一").unwrap();
        idx.commit().unwrap();
        idx.flush().unwrap();
        assert_eq!(idx.num_chunks(), 1);
    }

    #[test]
    fn 纯文本String与str均可add() {
        let mut idx = bm25_index();
        idx.add(String::from("字符串")).unwrap();
        idx.add("字符串字面量").unwrap();
        // `S8-03`：`num_chunks()` 只统计**已提交**的段 ⇒ 先 commit（可见性边界）
        idx.commit().unwrap();
        assert_eq!(idx.num_chunks(), 2);
    }

    #[test]
    fn save后load快照roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("search.idx");

        let mut idx = bm25_index();
        idx.add("BM25 是经典关键词检索算法").unwrap();
        idx.add("向量检索计算余弦相似度").unwrap();
        idx.save(&path).unwrap();

        // 纯 BM25 快照：按同装配（embedder=None）load，指纹一致
        let loaded = SearchIndexBuilder::default()
            .embedder(None)
            .load(&path)
            .unwrap();
        assert_eq!(loaded.num_chunks(), idx.num_chunks());

        // 检索能力保留：doc_freq 一致
        assert_eq!(
            loaded.view().main.index.doc_freq("检索"),
            idx.view().main.index.doc_freq("检索")
        );
    }

    /// **`S8-03` 口径变更**：`remove` 的目标若在**已发布段**里，不再就地物理删
    /// （段不可变 `I8-2`），而是记一条**跨段墓碑** ⇒ 段内计数**不变**，但对外
    /// （`tombstone_stats` / 检索）**立刻**表现为已删；**合并时物理化**（§4.9.2）。
    ///
    /// 原用例（`add` → `remove` → 断言 `num_chunks() == 0`）描述的是「就地物理删」，
    /// 那条路径现在只适用于**未发布的 builder**（分支 ①）⇒ 用例随之分成两段。
    #[test]
    fn remove删除文档并回滚统计量() {
        let mut idx = bm25_index();
        let out = idx.add("BM25 检索算法").unwrap();
        // ① 目标还在 **builder**（未发布）⇒ 就地物理删 ⇒ 活计数立刻回落
        // ⚠️ 判据必须是 `alive_count()`（**活**分片数）而不是 `total_chunks()`（**槽位**数）：
        //    删除只把槽位置成墓碑、不缩短 `Vec`（基址不变式的基石，§2.4）
        assert_eq!(idx.builder.index.alive_count(), 1);
        idx.remove(out.doc_id).unwrap();
        assert_eq!(idx.builder.index.alive_count(), 0, "builder 内就地物理删");
        assert_eq!(idx.builder.index.total_chunks(), 1, "槽位不缩短（墓碑位）");
        assert_eq!(idx.num_chunks(), 0);

        // ② 再来一轮：这次先 commit（目标进**已发布段**）⇒ 记墓碑，段内计数不变
        let out = idx.add("BM25 检索算法").unwrap();
        idx.commit().unwrap();
        assert_eq!(idx.num_chunks(), 1);

        idx.remove(out.doc_id).unwrap();
        // ⚠️ 墓碑记在**未发布的 builder** 上（设计 §4.8.3 分支 ②）⇒ 与内容一样，
        //    要 `commit()` 才对外生效（**同一条可见性边界**，不搞两套语义）
        assert_eq!(
            idx.num_chunks(),
            1,
            "跨段墓碑不改变段内计数（物理化在合并时）"
        );
        idx.commit().unwrap();
        assert_eq!(
            idx.tombstone_stats().docs_alive,
            0,
            "commit 后对外是「已删」"
        );
        assert!(
            idx.searcher().search("BM25").unwrap().hits.is_empty(),
            "墓碑必须挡住检索（否则是数据错误）"
        );

        // ③ 合并（`save` 的第一步）⇒ 墓碑**物理化** ⇒ 段内计数才真正回落
        let dir = tempfile::tempdir().unwrap();
        idx.save(&dir.path().join("s.idx")).unwrap();
        assert_eq!(idx.num_chunks(), 0, "合并（物理化）后计数回落");
        assert_eq!(idx.tombstone_stats().docs_alive, 0);
    }

    #[test]
    fn 装配不一致load报ConfigMismatch() {
        use crate::analyze::Analyzer;

        // 用 id 非默认的自定义 analyzer 建库（模拟 charabia 等非默认装配）
        struct CustomAnalyzer;
        impl Analyzer for CustomAnalyzer {
            fn analyze_doc(&self, text: &str) -> Vec<crate::analyze::Token> {
                crate::analyze::MixedAnalyzer::new().analyze_doc(text)
            }
            fn id(&self) -> &'static str {
                "custom-analyzer"
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mismatch.idx");

        // 用 custom analyzer 建库 + 落盘
        let mut idx = SearchIndex::from_config(
            SearchIndexBuilder::default()
                .embedder(None)
                .analyzer(Arc::new(CustomAnalyzer))
                .build_config(),
            VectorBackend::Brute,
            Default::default(),
        );
        idx.add("BM25 检索算法").unwrap();
        idx.save(&path).unwrap();

        // 用默认（mixed）装配 load → 必须报 ConfigMismatch（修 B1）
        let err = match SearchIndex::load(&path) {
            Ok(_) => panic!("装配不一致应报 ConfigMismatch"),
            Err(e) => e,
        };
        assert!(
            matches!(err, Error::ConfigMismatch { .. }),
            "应为 ConfigMismatch，得到 {err:?}"
        );
    }

    /// S3-T6 ⚠️（验收 1 的直接对应）：在 `save` 的快照写窗口各注入点崩溃后，
    /// `load` 要么拿到旧快照、要么拿到新快照，**绝不 `SnapshotCorrupted`**。
    ///
    /// 判定口径（附录 B）：旧/新以 chunk 数区分（2 篇旧文档 = 2 chunks，
    /// 追加第 3 篇 = 3 chunks）——不依赖分词细节。cp1/cp2（rename 前）真源
    /// 必为旧快照且 tmp 孤儿残留、被 load 回收；cp3（rename 后）真源必为新
    /// 快照且无 tmp（进程崩溃态；掉电回滚态见设计 §4.3 注 1，等价于 cp1/cp2）。
    ///
    /// 注入范围声明（设计 §7）：`FAIL_AT` 单发命中，只覆盖快照写窗口
    /// （save 内首次 `atomic_write`）；图 dump / manifest 窗口是 Step 2
    /// 已覆盖的性能退化窗口，不重复注入。
    #[test]
    fn save崩溃注入后load恒可用_T6() {
        use crate::storage::atomic::fault::InjectionGuard;

        for cp in [1u8, 2, 3] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("t6.idx");

            // v1：两篇旧文档，先正常落盘
            let mut idx = bm25_index();
            idx.add("BM25 是经典关键词检索算法").unwrap();
            idx.add("向量检索计算余弦相似度").unwrap();
            idx.save(&path).unwrap();

            // v2：追加一篇新文档后重写，注入崩溃
            idx.add("崩溃注入新增文档").unwrap();
            let g = InjectionGuard::acquire();
            g.arm(cp);
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| idx.save(&path)));
            drop(g); // 无论成败都复位

            assert!(r.is_err(), "cp{cp} 注入应 panic");
            let tmp = crate::storage::atomic::tmp_path(&path);
            if cp < 3 {
                assert!(tmp.exists(), "cp{cp}: rename 未发生，应残留 tmp 孤儿");
            } else {
                assert!(!tmp.exists(), "cp3: rename 已消费本 save 的 tmp");
            }

            // 核心不变式：要么旧要么新，绝不 SnapshotCorrupted
            let loaded = SearchIndexBuilder::default()
                .embedder(None)
                .load(&path)
                .unwrap_or_else(|e| panic!("cp{cp}: load 必须成功，得到 {e:?}"));
            match cp {
                1 | 2 => assert_eq!(
                    loaded.num_chunks(),
                    2,
                    "cp{cp}: rename 前崩溃，load 必须得到旧快照"
                ),
                _ => assert_eq!(
                    loaded.num_chunks(),
                    3,
                    "cp3: rename 后崩溃，load 必须得到新快照"
                ),
            }

            // 验收 3（快照 tmp 口径）：load 成功后孤儿被回收
            assert!(!tmp.exists(), "cp{cp}: load 成功后 tmp 孤儿应被回收");
        }
    }
}
