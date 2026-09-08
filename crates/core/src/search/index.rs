//! 写端门面：`SearchIndex`（拥有型，持有全部装配与状态）。
//!
//! # 可见性语义（p6-design 6.3）
//!
//! `add` 之后必须 `commit()` 才对检索可见（对齐 Lucene / tantivy）。
//! `into_searcher()` 隐含 flush（把 `pending` 刷进 `Inner`，p6-design 6.4 评审 P2）。

use std::sync::Arc;

use crate::document::{content_hash, DocRecord, Document};
use crate::error::{Error, Result};
use crate::index::Index;
use crate::types::{ChunkId, DocId};
use crate::vector::{
    BruteForceIndex, HnswRsIndex, NormalizedVector, VectorGraphPersist, VectorIndex,
};

use super::config::{Config, GraphPersistMode, SearchIndexBuilder, VectorBackend};

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

/// 已提交状态（`Arc` 共享：`into_searcher` 零拷贝移交给读端）。
pub(crate) struct Inner {
    /// 倒排 + 正排 + 统计量
    pub index: Index,
    /// 向量索引（`None` = 纯 BM25）
    pub vector_index: Option<Box<dyn VectorIndex>>,
    /// 原始向量（快照策略 D1：存原始向量不存 HNSW 图；`None` = 未启用 keep_raw）
    pub raw_vectors: Option<Vec<(ChunkId, Vec<f32>)>>,
}

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

/// 写端门面（拥有型）。持有 analyzer / chunker / embedder / fusion / reranker /
/// 倒排 / 向量索引 / 写缓冲，暴露 `add(doc)` / `commit()` / `into_searcher()`。
///
/// 注意：`SearchIndex` 是 **`!Sync`**（含可变 `pending`，`add(&mut self)`）。
/// 它可 `Send`（能整体 move 到别的线程），但不可跨线程共享（p6-design 5.3）。
///
/// # 所有权切换（p6-design 6.4 方案 A 的实现细化）
///
/// `inner` 直接持有 `Inner`（**非** `Arc`）。`into_searcher()` 时包成 `Arc` 移交；
/// `Searcher::into_index()` 用 `Arc::try_unwrap` 解包——refcount==1 零拷贝取出，
/// refcount>1（有 `Searcher` clone 残留）时**明确报错**而非静默深拷贝。
/// （设计 6.4 的 P3 修正假设 `Arc::make_mut` 静默深拷贝，但 `Box<dyn VectorIndex>`
/// 不可 `Clone` 使该路径无法编译；`try_unwrap` 是更诚实的等价实现。）
pub struct SearchIndex {
    pub(crate) cfg: Arc<Config>,
    pub(crate) inner: Inner,
    pub(crate) pending: Vec<PendingChunk>,
    /// 累计 embed 推理耗时（`flush` 中累加，供 NFR-03 构建耗时口径观测）。
    /// 只计 `embed_documents` 推理本身，不含归一化 / 灌向量索引。
    pub(crate) embed_elapsed: std::time::Duration,
    /// 图持久化开关（ef_search 回填 / strict 模式 / 逃生舱）
    pub(crate) graph: super::config::GraphOpts,
    /// 最近一次图 sidecar 的状态（NFR-07：降级必须可观测）
    pub(crate) graph_status: GraphStatus,
    /// 最近一次 `save` 中图 sidecar 落盘（dump + CRC + manifest 发布）的耗时。
    /// `None` = 本次 `save` 未走图持久化（验收 7：dump 耗时要有实测记录）。
    pub(crate) graph_dump_elapsed: Option<std::time::Duration>,
    /// 向量后端（V2 Step 4：compaction 重建向量索引需按后端重建同类型，故留档）
    pub(crate) backend: VectorBackend,
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
        let ef_search = cfg.ef_search;
        let vector_index = match cfg.embedder.as_ref() {
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
        };
        let inner = Inner {
            index: Index::new(),
            vector_index,
            raw_vectors: Some(Vec::new()),
        };
        Self {
            cfg: Arc::new(cfg),
            inner,
            pending: Vec::new(),
            embed_elapsed: std::time::Duration::ZERO,
            graph,
            graph_status: GraphStatus::NotApplicable,
            graph_dump_elapsed: None,
            backend,
        }
    }

    /// 摄入一篇文档：查重 → 分块 → 倒排 → 写缓冲（p6-design 6.1）。
    ///
    /// - **幂等 upsert（FR-15）**：`content_hash = xxh64(dedup_key.unwrap_or(text))`
    ///   命中已存在文档时直接返回 `deduped = true`，不重复分块/插入。
    /// - 向量化**延后到 `flush`**（批量 embed，NFR-03）；此刻只进倒排与写缓冲。
    /// - 缓冲满（`batch_size`）时自动同步 flush。
    pub fn add(&mut self, doc: impl Into<Document>) -> Result<AddOutcome> {
        let doc = doc.into();
        let text = doc.text.clone();
        let hash = content_hash(doc.dedup_key.as_deref().unwrap_or(&text));

        // 短路查重（与 Index::add 内部去重语义一致）
        if let Some(existing) = self.inner.index.doc_id_by_hash(hash) {
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

        // 分块 → 倒排（立即完成）
        let chunks = self.cfg.chunker.chunk(0, &text);
        let chunk_texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let (doc_id, chunk_ids) =
            self.inner
                .index
                .add(record, chunks, self.cfg.analyzer.as_ref())?;

        // 进写缓冲（有向量侧时才需要 embed）
        if self.cfg.embedder.is_some() {
            for (chunk_id, text) in chunk_ids.iter().zip(chunk_texts) {
                self.pending.push(PendingChunk {
                    chunk_id: *chunk_id,
                    text,
                });
            }
            if self.pending.len() >= self.cfg.batch_size {
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
        if self.pending.is_empty() {
            return Ok(());
        }
        let Some(embedder) = self.cfg.embedder.as_ref() else {
            // 无向量侧：缓冲不应存在（add 里已短路）；防御性清空
            self.pending.clear();
            return Ok(());
        };

        let texts: Vec<String> = self.pending.iter().map(|p| p.text.clone()).collect();
        let t = std::time::Instant::now();
        let vecs = embedder.embed_documents(&texts)?;
        self.embed_elapsed += t.elapsed();
        debug_assert_eq!(vecs.len(), texts.len(), "embed 输出条数应与输入一致");

        // 归一化 + 灌向量索引 + 记录原始向量（快照用）。
        // V2 Step 2：改用 `add_batch`——达阈值且开了并行建图开关时由实现走
        // `parallel_insert_slice`，否则串行（默认，保确定性）。小批量下批量路径
        // 与逐条 add 行为等价，故无需分支。
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
        let vi = self.inner.vector_index.as_mut().ok_or(Error::NoEmbedder)?;
        let mut items: Vec<(ChunkId, NormalizedVector)> = Vec::new();
        for (pending, v) in self.pending.iter().zip(vecs) {
            if !self.inner.index.is_live_chunk(pending.chunk_id) {
                continue; // 墓碑 chunk：向量丢弃，不落 raw_vectors、不进图
            }
            let nv = NormalizedVector::new(v);
            if let Some(raw) = self.inner.raw_vectors.as_mut() {
                raw.push((pending.chunk_id, nv.as_slice().to_vec()));
            }
            items.push((pending.chunk_id, nv));
        }
        vi.add_batch(&items)?;
        self.pending.clear();
        Ok(())
    }

    /// 使已摄入的文档对检索可见（对齐 Lucene `commit`，p6-design 6.3）。
    ///
    /// 本轮实现中 `commit()` 与 `flush()` 等价（都是刷写缓冲）；真正的
    /// delta 分段（`commit` = 刷成新 segment）留 P7，届时 API 形态不变。
    pub fn commit(&mut self) -> Result<()> {
        self.flush()
    }

    /// 当前已提交的分片数（不含 pending）。
    pub fn num_chunks(&self) -> u32 {
        self.inner.index.num_chunks()
    }

    /// 当前已提交的文档数（不含墓碑）。
    pub fn num_docs(&self) -> usize {
        self.inner.index.num_docs()
    }

    /// 全语料词项总数（所有分片的分词数之和）。
    pub fn total_len(&self) -> u64 {
        self.inner.index.total_len()
    }

    /// 平均分片长度（BM25 长度归一化用）。
    pub fn avgdl(&self) -> f32 {
        self.inner.index.avgdl()
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

    /// 交出所有权，产出只读 `Searcher`（`'static + Clone + Send + Sync`）。
    ///
    /// **隐含 flush**（p6-design 6.4 评审 P2）：把 `pending` 刷进 `Inner` 后再移交，
    /// 否则未 embed 的分片在检索侧不可见。`Inner` 包成 `Arc` 移交给读端，零拷贝。
    pub fn into_searcher(mut self) -> Result<crate::search::Searcher> {
        self.flush()?;
        Ok(crate::search::Searcher {
            cfg: self.cfg,
            inner: Arc::new(self.inner),
            graph: self.graph,
            graph_status: self.graph_status,
            backend: self.backend,
        })
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
        self.inner
            .index
            .remove(doc_id, self.cfg.analyzer.as_ref())?;

        // 顺带摘除原始向量（O(len)，与 `Index::remove` 的 O(N) 同量级）：
        // 这既缩小快照体积，也断掉「删除 → save → load → 幽灵候选复活」的路径。
        // 内存中的 HNSW 图仍需靠存活位图过滤（物理回收归 Step 4 的 compaction，S4-03~S4-07）。
        let Inner {
            index, raw_vectors, ..
        } = &mut self.inner;
        if let Some(raw) = raw_vectors.as_mut() {
            raw.retain(|(id, _)| index.is_live_chunk(*id));
        }
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
        self.commit()?;
        let vectors: Vec<(ChunkId, Vec<f32>)> =
            self.inner.raw_vectors.as_deref().unwrap_or(&[]).to_vec();
        let body_crc = crate::storage::save_with_crc(
            path,
            &self.inner.index,
            &vectors,
            &self.cfg.fingerprint(),
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
        let Some(vi) = self.inner.vector_index.as_ref() else {
            crate::storage::remove_sidecars(path)?;
            return Ok(GraphStatus::NotApplicable);
        };
        if !self.graph.persist || vectors.is_empty() {
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
        match write_graph_sidecar(path, g, body_crc, self.cfg.fingerprint().dim) {
            Ok(()) => Ok(GraphStatus::Loaded),
            Err(e) if self.graph.mode == GraphPersistMode::Strict => {
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
        let index = &self.inner.index;
        let chunks_total = index.total_chunks();
        let chunks_alive = index.alive_count();
        TombstoneStats {
            chunks_total,
            chunks_alive,
            docs_total: index.total_docs(),
            docs_alive: index.num_docs(),
            graph_points: self
                .inner
                .vector_index
                .as_ref()
                .map(|v| v.len())
                .unwrap_or(0),
            raw_vectors: self
                .inner
                .raw_vectors
                .as_ref()
                .map(|v| v.len())
                .unwrap_or(0),
            tombstone_ratio: if chunks_total == 0 {
                0.0
            } else {
                1.0 - chunks_alive as f64 / chunks_total as f64
            },
        }
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
    pub fn compact_and_save(&mut self, path: &std::path::Path) -> Result<CompactionReport> {
        let bytes_before = snapshot_bytes(path).ok();
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

        let before = self.tombstone_stats();
        let has_tombstones = before.chunks_total > before.chunks_alive;

        // 步骤 1~2：取存活集 + 建 ID 映射（重编号，D-S4-01）
        let remap = self.inner.index.build_remap();

        // 步骤 3：Index 重新物化（返回新实例，self.inner.index 未动 → I5）
        let (new_index, reclaimed_terms) = self.inner.index.compacted(&remap);

        // 步骤 4：raw_vectors 过滤死 chunk + remap（I7）
        let new_raw = self.inner.raw_vectors.as_ref().map(|raw| {
            let mut out: Vec<(ChunkId, Vec<f32>)> = Vec::with_capacity(raw.len());
            for (old, v) in raw.iter() {
                if let Some(new_id) = remap.chunk.get(*old as usize).copied().flatten() {
                    out.push((new_id, v.clone()));
                }
            }
            out.sort_by_key(|(id, _)| *id); // 与插入顺序对齐（chunk_id 升序）
            out
        });

        // 步骤 5：向量索引重建（§4.5 / S4-06，与 load 降级同源）
        let had_vectors = self.inner.vector_index.is_some();
        let t_vec = std::time::Instant::now();
        let new_vi = match (&new_raw, had_vectors) {
            (Some(raw), true) if !raw.is_empty() => Some(rebuild_vector_index(
                self.backend,
                raw,
                self.cfg.ef_search,
                self.cfg.parallel_build,
            )?),
            _ => None,
        };
        let vector_rebuild_ms = t_vec.elapsed().as_millis();

        // 全部构建成功 → 一次性原子替换（I5：任一 Err 都已 return，旧状态未动）
        self.inner = Inner {
            index: new_index,
            raw_vectors: new_raw,
            vector_index: new_vi,
        };

        let after = self.tombstone_stats();

        Ok(CompactionReport {
            reclaimed_chunks: before.chunks_total.saturating_sub(after.chunks_total),
            reclaimed_docs: before.docs_total.saturating_sub(after.docs_total),
            reclaimed_terms,
            reclaimed_graph_points: before.graph_points.saturating_sub(
                self.inner
                    .vector_index
                    .as_ref()
                    .map(|v| v.len())
                    .unwrap_or(0),
            ),
            vector_rebuild_ms,
            total_ms: t0.elapsed().as_millis(),
            remapped: has_tombstones,
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

        // 重建向量索引（V2 Step 2：优先从图 sidecar 加载，失败则降级重建）
        let mut graph_status = GraphStatus::NotApplicable;
        let vector_index = match (cfg.embedder.as_ref(), raw_vectors.is_empty()) {
            (Some(_), false) => Some(match backend {
                VectorBackend::Brute => {
                    // Brute 逃生舱：忽略图（精确扫描是确定性的，无需缓存）。
                    // V2 Step 4（S4-06）：与 Hnsw 降级重建共用 `rebuild_vector_index`。
                    rebuild_vector_index(backend, &raw_vectors, None, cfg.parallel_build)?
                }
                VectorBackend::Hnsw => {
                    let ef_search = graph.ef_search.unwrap_or(HnswRsIndex::default_ef_search());
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
            }),
            _ => None,
        };

        Ok(Self {
            cfg: Arc::new(cfg),
            inner: Inner {
                index,
                vector_index,
                raw_vectors: Some(raw_vectors),
            },
            pending: Vec::new(),
            embed_elapsed: std::time::Duration::ZERO,
            graph,
            graph_status,
            graph_dump_elapsed: None,
            backend,
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

    #[test]
    fn 空索引可构造() {
        let idx = bm25_index();
        assert_eq!(idx.num_chunks(), 0);
        assert!(idx.pending.is_empty());
    }

    #[test]
    fn add后未commit也能查到倒排() {
        // 倒排在 add 时立即写入（可见性语义只影响向量侧 flush）
        let mut idx = bm25_index();
        let out = idx.add("BM25 检索算法").unwrap();
        assert!(!out.deduped);
        assert_eq!(out.chunk_ids.len(), 1);
        assert_eq!(idx.num_chunks(), 1);
    }

    #[test]
    fn 幂等upsert返回deduped() {
        let mut idx = bm25_index();
        let a = idx.add("BM25 检索算法").unwrap();
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
            loaded.inner.index.doc_freq("检索"),
            idx.inner.index.doc_freq("检索")
        );
    }

    #[test]
    fn remove删除文档并回滚统计量() {
        let mut idx = bm25_index();
        let out = idx.add("BM25 检索算法").unwrap();
        assert_eq!(idx.num_chunks(), 1);
        idx.remove(out.doc_id).unwrap();
        assert_eq!(idx.num_chunks(), 0);
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
