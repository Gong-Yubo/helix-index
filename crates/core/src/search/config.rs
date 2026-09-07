//! 门面层配置：`Config`（不可变装配）+ `SearchIndexBuilder`（配置入口）。
//!
//! # 默认装配（零配置可用，G4 / 4.1）
//!
//! - 分词器：`MixedAnalyzer`（中英混合）
//! - 分块器：`Chunker::default()`（512 char / 64 overlap）
//! - Embedder：`LocalEmbedder`（bge-small-zh-v1.5，512 维，`local-embed` feature）
//! - 向量后端：HNSW（`HnswRsIndex`）
//! - 融合：`RrfFusion::default()`（k=60，weights=(1.0, 1.5)，P5 定稿）
//! - 精排：`NoOpReranker`
//! - BM25：`Bm25Params::default()`（k1=1.5 / b=0.75，P5 定稿）
//! - `batch_size`：64（**待 I-13 实测校准**，p6-design 6.2）

use std::sync::Arc;

use crate::analyze::{Analyzer, MixedAnalyzer};
use crate::chunk::Chunker;
use crate::embed::Embedder;
use crate::error::Result;
use crate::fusion::{FusionStrategy, RrfFusion};
use crate::rerank::{NoOpReranker, Reranker};
use crate::retriever::Bm25Params;

/// 写缓冲批量 embed 的默认阈值（p6-design 6.2；**待 I-13 实测校准**，
/// 32/64/128/256 四档在 T2Ranking 12K 语料上对比后定稿）。
pub const DEFAULT_BATCH_SIZE: usize = 64;

/// 不可变装配：门面层持有、检索全程共享的组件。
///
/// 六个 trait 现状均已 `Send + Sync`（逐一核实），故 `Config: Send + Sync`，
/// 使读端 `Searcher` 满足 `'static + Clone + Send + Sync`（G3）。
///
/// 注意：`VectorIndex` **不在** `Config`——它在 `Inner`（`Box<dyn VectorIndex>`），
/// 因为向量索引是可变的、且其生命周期随写缓冲 flush 而变。
pub struct Config {
    /// 分词器（索引侧与查询侧共用同一实例，R4）
    pub analyzer: Arc<dyn Analyzer>,
    /// 分块器
    pub chunker: Chunker,
    /// 向量化（`None` = 纯 BM25）
    pub embedder: Option<Arc<dyn Embedder>>,
    /// 融合策略
    pub fusion: Arc<dyn FusionStrategy>,
    /// 重排策略
    pub reranker: Arc<dyn Reranker>,
    /// BM25 参数（P5 定稿）
    pub bm25_params: Bm25Params,
    /// 写缓冲批量 embed 阈值
    pub batch_size: usize,
    /// HNSW ef_search（P0-5：图加载后必须回填，全 crate 无 set_ef*；
    /// `None` = 用内核默认 EF_SEARCH=200）
    pub ef_search: Option<usize>,
    /// 批量建图是否走 `parallel_insert`（D-S2-05，**默认 false** 保确定性）
    pub parallel_build: bool,
}

impl Config {
    /// 生成配置指纹（p6-design 8.2）：快照记录 + load 校验用。
    pub fn fingerprint(&self) -> crate::storage::ConfigFingerprint {
        crate::storage::ConfigFingerprint {
            analyzer_id: self.analyzer.id().to_string(),
            embedder_id: self
                .embedder
                .as_ref()
                .map(|e| e.id().to_string())
                .unwrap_or_default(),
            dim: self.embedder.as_ref().map(|e| e.dim() as u32).unwrap_or(0),
            chunker: (self.chunker.chunk_chars(), self.chunker.overlap_chars()),
        }
    }
}

/// 向量后端选择（逃生舱：诊断用 brute 精确对照，p6-design 7.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VectorBackend {
    /// 暴力线性扫描（确定性、精确，小规模/诊断用）
    Brute,
    /// HNSW 近似最近邻（生产默认，原生增量插入）
    #[default]
    Hnsw,
}

/// 图持久化相关的三个开关（V2 Step 2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GraphOpts {
    /// HNSW ef_search（图加载后回填，P0-5）
    pub ef_search: Option<usize>,
    /// 图 sidecar 行为（D-S2-04）
    pub mode: GraphPersistMode,
    /// 是否读写图 sidecar（`--no-graph-persist` 逃生舱）
    pub persist: bool,
}

impl Default for GraphOpts {
    /// **图持久化默认开**（`persist: true`）——注意 `bool` 的派生默认是 `false`，
    /// 与语义相反，故此处手写（曾因此导致 save 永远不落图，测试全降级）。
    fn default() -> Self {
        Self {
            ef_search: None,
            mode: GraphPersistMode::Lenient,
            persist: true,
        }
    }
}

/// 图 sidecar 不可用时的行为（D-S2-04）。
///
/// 图是派生缓存：默认「警告后降级重建」（降级必须可观测，NFR-07）；
/// `strict` 把降级与 dump 失败升级为 `Err`（诊断 / CI 用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphPersistMode {
    /// 警告后降级 / 跳过图（默认）
    #[default]
    Lenient,
    /// 图不可用或 dump 失败即 `Err`
    Strict,
}

/// `SearchIndex` 的配置入口。**所有方法可选，不调即用默认值**（零配置可用）。
pub struct SearchIndexBuilder {
    analyzer: Option<Arc<dyn Analyzer>>,
    chunker: Option<Chunker>,
    embedder: Option<Option<Arc<dyn Embedder>>>,
    fusion: Option<Arc<dyn FusionStrategy>>,
    reranker: Option<Arc<dyn Reranker>>,
    bm25_params: Option<Bm25Params>,
    batch_size: Option<usize>,
    backend: VectorBackend,
    /// HNSW ef_search（P0-5：图加载后必须回填，全 crate 无 set_ef*）
    ef_search: Option<usize>,
    /// 图 sidecar 行为（D-S2-04，默认 Lenient）
    graph_mode: GraphPersistMode,
    /// 是否持久化图（S2-08 的 `--no-graph-persist` 逃生舱；默认开）
    graph_persist: bool,
    /// 并行建图（D-S2-05，默认关）
    parallel_build: bool,
}

impl Default for SearchIndexBuilder {
    fn default() -> Self {
        Self {
            analyzer: None,
            chunker: None,
            embedder: None,
            fusion: None,
            reranker: None,
            bm25_params: None,
            batch_size: None,
            backend: VectorBackend::Hnsw,
            ef_search: None,
            graph_mode: GraphPersistMode::Lenient,
            graph_persist: true,
            parallel_build: false,
        }
    }
}

impl SearchIndexBuilder {
    /// 覆盖分词器（逃生舱：charabia 对照，p6-design 7.3）。
    pub fn analyzer(mut self, analyzer: Arc<dyn Analyzer>) -> Self {
        self.analyzer = Some(analyzer);
        self
    }

    /// 覆盖分块器（逃生舱：评测口径 `Chunker::new(200_000, 0)` 强制单 chunk）。
    pub fn chunker(mut self, chunker: Chunker) -> Self {
        self.chunker = Some(chunker);
        self
    }

    /// 覆盖 Embedder。传 `None` = 纯 BM25（不建向量侧）。
    pub fn embedder(mut self, embedder: Option<Arc<dyn Embedder>>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// 覆盖融合策略（逃生舱：RRF k / weights 网格）。
    pub fn fusion(mut self, fusion: Arc<dyn FusionStrategy>) -> Self {
        self.fusion = Some(fusion);
        self
    }

    /// 覆盖精排策略（默认 `NoOpReranker`）。
    pub fn reranker(mut self, reranker: Arc<dyn Reranker>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    /// 覆盖 BM25 参数（逃生舱：BM25 网格搜索）。
    pub fn bm25_params(mut self, params: Bm25Params) -> Self {
        self.bm25_params = Some(params);
        self
    }

    /// 覆盖写缓冲批量 embed 阈值。
    pub fn batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = Some(batch_size);
        self
    }

    /// 选择向量后端（默认 HNSW；诊断用 brute）。
    pub fn vector_backend(mut self, backend: VectorBackend) -> Self {
        self.backend = backend;
        self
    }

    /// 覆盖 HNSW ef_search（图加载后回填用，P0-5；默认 200）。
    /// bench 的 `--ef-search` 必须经这里流进来，否则逐位一致断言会因
    /// ef 不同产生假差异（v2-step2-design §5.3 坑 3）。
    pub fn ef_search(mut self, ef_search: usize) -> Self {
        self.ef_search = Some(ef_search);
        self
    }

    /// 图 sidecar 行为（D-S2-04：默认警告后降级；Strict 时图问题即 Err）。
    pub fn graph_mode(mut self, mode: GraphPersistMode) -> Self {
        self.graph_mode = mode;
        self
    }

    /// 关闭图持久化（`--no-graph-persist` 逃生舱：写库但不落图、
    /// 加载时也不尝试读图）。磁盘紧张 / 排查图问题用。
    pub fn without_graph_persist(mut self) -> Self {
        self.graph_persist = false;
        self
    }

    /// 打开并行建图（D-S2-05，**默认关**）。
    ///
    /// ⚠️ 代价：`parallel_insert_slice` 走 rayon ⇒ 插入顺序不确定 ⇒
    /// **建图拓扑不可复现**（C8）。图落盘后即被冻结，故「同快照两次加载」
    /// 仍逐位一致；但「同一批向量两次建库」不再一致，与 P5/P6 基线的可比性
    /// 会受影响。建议只在基准实测（S2-11 / T13）时打开。
    ///
    /// ⚠️ **必须配合 `batch_size >= 1000`**（并行阈值）才真的生效：
    /// 默认 `batch_size = 64` 时每次 `add_batch` 只有 64 条，会静默回落串行，
    /// 而「并行 vs 串行质量等价」的测试会退化成「串行 vs 串行」还全绿
    /// （评审 #13 发现 1）。`HnswRsIndex::parallel_inserts()` 可观测实际分派。
    pub fn parallel_build(mut self, parallel_build: bool) -> Self {
        self.parallel_build = parallel_build;
        self
    }

    /// 组装出一个空的 `SearchIndex`（消费 builder）。
    ///
    /// 这是默认装配的唯一入口：`SearchIndex::builder().build()` 零配置可用。
    /// 有向量侧时 `build()` 可能触发模型下载（首次 ~49s）；失败时退化为纯 BM25。
    pub fn build(self) -> crate::search::SearchIndex {
        let cfg = self.build_config();
        let backend = self.backend();
        crate::search::SearchIndex::from_config(cfg, backend, self.graph_opts())
    }

    /// 按**当前装配**从快照加载（p6-design 8.2 的"标准姿势"）。
    ///
    /// 加载非默认快照（如 charabia 建库）时，先
    /// `builder().analyzer(..).embedder(..).chunker(..)` 装配好再调用本方法；
    /// 装配与快照指纹不一致时报 `ConfigMismatch`（绝不静默换分词器）。
    pub fn load(self, path: &std::path::Path) -> Result<crate::search::SearchIndex> {
        let cfg = self.build_config();
        let backend = self.backend();
        crate::search::SearchIndex::load_with(cfg, backend, self.graph_opts(), path)
    }

    /// 图持久化相关的三个开关打包（供 `SearchIndex::load_with` 使用）。
    pub(crate) fn graph_opts(self) -> GraphOpts {
        GraphOpts {
            ef_search: self.ef_search,
            mode: self.graph_mode,
            persist: self.graph_persist,
        }
    }

    /// 用默认值 + 覆盖项组装出一个 `Config`。
    pub(crate) fn build_config(&self) -> Config {
        Config {
            analyzer: self
                .analyzer
                .clone()
                .unwrap_or_else(|| Arc::new(MixedAnalyzer::new())),
            chunker: self.chunker.unwrap_or_default(),
            embedder: match &self.embedder {
                // 显式指定（含 None = 纯 BM25）
                Some(e) => e.clone(),
                // 未指定 → 默认本地 embedder（local-embed feature 下）
                None => default_embedder(),
            },
            fusion: self
                .fusion
                .clone()
                .unwrap_or_else(|| Arc::new(RrfFusion::default())),
            reranker: self
                .reranker
                .clone()
                .unwrap_or_else(|| Arc::new(NoOpReranker)),
            bm25_params: self.bm25_params.unwrap_or_default(),
            batch_size: self.batch_size.unwrap_or(DEFAULT_BATCH_SIZE),
            ef_search: self.ef_search,
            parallel_build: self.parallel_build,
        }
    }

    /// 向量后端（`build_config` 之外的独立读取，供 `SearchIndex` 初始化 `Inner`）。
    pub(crate) fn backend(&self) -> VectorBackend {
        self.backend
    }
}

/// 默认 Embedder：`local-embed` feature 下用本地 bge-small-zh-v1.5；
/// 未启用时退化为 `None`（纯 BM25）。
#[cfg(feature = "local-embed")]
fn default_embedder() -> Option<Arc<dyn Embedder>> {
    use crate::embed::LocalEmbedder;
    // 模型首次下载约 49s；失败时退化为纯 BM25 而非 panic（零配置可用）。
    LocalEmbedder::new()
        .ok()
        .map(|e| Arc::new(e) as Arc<dyn Embedder>)
}

/// 默认 Embedder（未启用 `local-embed`）：无向量，纯 BM25。
#[cfg(not(feature = "local-embed"))]
fn default_embedder() -> Option<Arc<dyn Embedder>> {
    None
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;

    #[test]
    fn 默认配置零配置可用() {
        let cfg = SearchIndexBuilder::default().build_config();
        // 分词器/融合/精排均有默认实现（不 panic）
        assert!(cfg.embedder.is_some() || cfg.embedder.is_none()); // 有或没有均可
        assert!(!cfg.fusion.name().is_empty());
        assert_eq!(cfg.bm25_params.k1, 1.5);
        assert_eq!(cfg.bm25_params.b, 0.75);
        assert_eq!(cfg.batch_size, DEFAULT_BATCH_SIZE);
    }

    #[test]
    fn 显式覆盖生效() {
        let cfg = SearchIndexBuilder::default()
            .batch_size(128)
            .bm25_params(Bm25Params { k1: 1.2, b: 0.5 })
            .embedder(None)
            .build_config();
        assert_eq!(cfg.batch_size, 128);
        assert_eq!(cfg.bm25_params.k1, 1.2);
        assert!(cfg.embedder.is_none());
    }

    #[test]
    fn 默认后端为hnsw() {
        let b = SearchIndexBuilder::default();
        assert_eq!(b.backend(), VectorBackend::Hnsw);
        assert_eq!(
            b.vector_backend(VectorBackend::Brute).backend(),
            VectorBackend::Brute
        );
    }
}
