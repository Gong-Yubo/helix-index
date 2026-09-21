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
    /// 批量建图是否走 `parallel_insert`（D-S2-05；**默认 `true`**，T7-21 / D-J11 翻转）
    pub parallel_build: bool,
    /// **查询侧会话池**长度（`S8-08` / D-S8-11；**默认 1** ⇒ 与「单会话」逐位等价）。
    ///
    /// - 只被**默认本地 embedder 的构造**消费（`default_embedder`）；显式 `embedder(Some(..))`
    ///   时本值被忽略；
    /// - ⚠️ **不进 `ConfigFingerprint`**（会话数不改变索引内容 ⇒ 同一快照在 1 / N 下都合法）；
    /// - `0` 在 `build_config` 处归一为 **1**（空池不可用），故这里恒 `≥ 1`。
    pub embed_sessions: usize,
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
    /// 并行建图（D-S2-05；**T7-21 / D-J11 起默认开**）
    parallel_build: bool,
    /// 查询侧会话池长度（`S8-08` / `D-S8-11`；默认 **1**）
    embed_sessions: usize,
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
            // T7-21 / D-J11：**默认翻转为开**（原 `false`）。
            // ⚠️ 它**只对「单次全量建图」路径生效**，见 `parallel_build()` 的文档。
            parallel_build: true,
            // `Q4`：**库侧默认不动**（1 = 零收益零风险；>1 要付 N 份 ONNX 会话的内存）。
            // 是否给推荐值由 spike S8-S2 的三条判据决定（`D-S8-11`）。
            embed_sessions: 1,
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

    /// 覆写并行建图开关（D-S2-05；**T7-21 / D-J11 起默认开** ⇒ 本方法现在的主要用途是**关掉**它）。
    ///
    /// ⚠️ **生效范围**：并行分派的条件是
    /// `parallel_build && items.len() >= PARALLEL_INSERT_THRESHOLD(1000)`
    /// （常量在 `vector/hnsw_rs_index.rs` 的 `add_batch` 分支）。三类交付点的批量：
    ///
    /// | 交付点 | 每次 `add_batch` 的批量 | 默认开后 |
    /// | --- | --- | --- |
    /// | **`compact()` 的向量索引重建**（`rebuild_vector_index`） | 全部存活向量（**一次**） | ✅ **走并行** |
    /// | **`load_with` 的降级重建**（`GraphStatus::Rebuilt` 分支，同一函数） | 全部向量（**一次**） | ✅ **走并行** |
    /// | **增量 `flush`**（`add()` 攒够 `batch_size` 触发） | 存量（≤ `batch_size` − 1）+ **当前文档 chunk 数 k** | ⚠️ **不是恒串行**，见下 |
    ///
    /// ⚠️ **增量 `flush` 的批量不是「≤ batch_size」**（评审 #49 P2-1 勘误）：`flush` 交给
    /// `add_batch` 的是**整个 `pending`**，而 `add()` 是「先把整篇文档的全部 chunk 推进
    /// `pending`、**再**检查阈值」⇒ 批量上界 = `(batch_size − 1) + k`（默认 64 ⇒ ≤ 63 + k）。
    /// 故 `k ≪ 937` 时确实远低于 1000（**常规短文档都是这种情形**），但
    /// **k ≥ 937（缓冲已满）或 k ≥ 1000（保证）时同样走并行** —— 默认 `Chunker(512/64)`
    /// 步长约 448 字符 ⇒ **单篇约 42~45 万字符**的长文档就可能触发
    /// （`Chunker` **无单文档 chunk 数上限**；段落边界切分还会让所需长度更短）。
    /// ⇒ **「增量建库一定可复现拓扑」不成立**：大单文档的增量建库同样不可复现。
    ///
    /// ⇒ 净收益仍主要在**一次性全量建图**（`compact()` 重建 / 冷启动降级重建）：
    /// 实测 12K 真实语料 **11.676s → 2.182s（5.35×）**、50K 合成 **5.26×**，
    /// oracle 重合率无差异（0.995 / 0.995）—— 见 issue #24。
    /// （⚠️ 另注：`bench --input` 的「内存直建」路径是**逐点 `add()`**、不交付批量 ⇒ **不受本开关影响**。）
    ///
    /// ⚠️ 代价：`parallel_insert_slice` 走 rayon ⇒ 插入顺序不确定 ⇒
    /// **建图拓扑不可复现**（C8）。图落盘后即被冻结，故「同快照两次加载」
    /// 仍逐位一致（**NFR-06 不受影响**）；但「同一批向量两次建库」不再逐位一致。
    /// D-J11 已拍板接受该代价（NFR-06 的口径是「同快照两次**加载**」，不约束建库过程）；
    /// 需要可复现拓扑时用 `.parallel_build(false)` 关掉。
    ///
    /// `HnswRsIndex::parallel_inserts()` 可观测后端实际分派（**门面不暴露该计数**，
    /// 覆盖边界见 `crates/core/tests/graph_persist.rs` 里 T22 的说明）。
    pub fn parallel_build(mut self, parallel_build: bool) -> Self {
        self.parallel_build = parallel_build;
        self
    }

    /// 查询侧**会话池**长度（`S8-08` / `D-S8-11` / T7-26；**默认 1**）。
    ///
    /// # 它影响什么
    ///
    /// 只影响**默认本地 embedder 的会话数**（`Config::embed_sessions`）：池里每个槽是一份
    /// 独立的 ONNX `Session`，`embed_query` / `embed_documents` 轮转取槽
    /// ⇒ 不同线程可以并行编码，解开 `R43` 的「查询侧编码被单 session 串行化」。
    ///
    /// # 它**不**影响什么
    ///
    /// - **索引内容与配置指纹**：`ConfigFingerprint` **不扩**（会话数不改变向量内容，
    ///   只改变「谁来算」）⇒ 同一个快照在 `sessions = 1` 与 `4` 下**都合法**；
    /// - **`dim` / `id()` / `is_normalized()`**：一字不变。
    ///
    /// # 代价与前置（如实登记）
    ///
    /// - 构造耗时与常驻内存均随 N **线性增长**（N 份会话各自的运行时缓冲；权重文件共享缓存目录）；
    /// - ⚠️ **槽同构是硬要求**：所有槽用同一份 `InitOptions`。若给不同槽设不同
    ///   `intra_threads`，同一 query 在不同并发度下可能拿到**不同的向量**
    ///   ⇒ 破坏 `NFR-10` ① 的「逐位一致」前置（`Q3′` 由 spike **S8-S2** 实测）。
    ///
    /// ⇒ **默认 1**（`Q4`：库侧默认不动）；`0` 归一为 `1`（见 `LocalEmbedder`）。
    ///
    /// ⚠️ 只对**默认装配路径**生效：显式 `embedder(Some(..))` 时本值被忽略
    /// （那种情况下请自行 `LocalEmbedder::new_with_sessions(n)`）。
    pub fn embed_sessions(mut self, embed_sessions: usize) -> Self {
        self.embed_sessions = embed_sessions;
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

    /// 用默认值 + 覆盖项组装出一个 `Config`（**生产入口**：注入真实的默认 embedder 构造器）。
    ///
    /// # ⚠️ 写法：先建 `cfg`（`embedder` 留 `None`），**再**按 `cfg.embed_sessions` 决定它
    ///
    /// 「会话数」是**默认 embedder 构造的输入**，不是事后的记录。若按常规写成一句
    /// 结构体字面量（`embedder: None => default_embedder(self.embed_sessions)`），
    /// `Config.embed_sessions` 就成了一个**写进去没人读**的字段 —— 那既会触发
    /// `dead_code`，也说明「字段是装饰」。⇒ **让它当真正的输入**：先归一、写进 `cfg`，
    /// 紧接着**读 `cfg.embed_sessions`** 去造默认 embedder。
    ///
    /// ⚠️ 真正的实现体在 [`Self::build_config_with`]（多一个**构造器注入点**，只为可测）。
    pub(crate) fn build_config(&self) -> Config {
        // 生产路径 = 注入**真实**构造器；抽 `build_config_with` 的唯一理由是让
        // 「会话数真的到达了构造器」这件事**有判据**（同 `EmbedderCtor` 的既有动机）。
        self.build_config_with(local_embedder_ctor)
    }

    /// 与 [`Self::build_config`] **逐字相同**，只多一个**默认 embedder 构造器**的注入点。
    ///
    /// # 为什么必须把它抽出来（`S8-08` 实测）
    ///
    /// `embed_sessions` 要穿过三层才到达构造器：
    /// `SearchIndexBuilder` → `build_config` → `default_embedder` → `EmbedderCtor`。
    /// 若判据只直接调 `resolve_embedder(...)`，那**中间那一次传递**就没有任何判据 ——
    /// **变异实测**：把 `default_embedder(cfg.embed_sessions, ctor)` 改成
    /// `default_embedder(1, ctor)` 时**全部单测照样绿**（会话数被静默吞掉、退回单槽，
    /// 而「静默退回单槽」恰好是一个**合法配置** ⇒ 行为判据看不见它）。
    /// ⇒ 把构造器变成形参，判据就能**从 `build_config` 这一层**断言它收到了什么。
    pub(crate) fn build_config_with(&self, ctor: EmbedderCtor) -> Config {
        let mut cfg = Config {
            analyzer: self
                .analyzer
                .clone()
                .unwrap_or_else(|| Arc::new(MixedAnalyzer::new())),
            chunker: self.chunker.unwrap_or_default(),
            // 占位：下面按 `cfg.embed_sessions` 决定（见本方法的文档）
            embedder: None,
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
            // `0` 是无意义输入（空池会在第一次 `embed_query` 时才炸）⇒ **在装配处**归一为 1，
            // 使 `Config.embed_sessions >= 1` 成为**类型级之外的不变式**（构造点唯一 ⇒ 易守）。
            embed_sessions: crate::embed::normalize_sessions(self.embed_sessions),
        };
        cfg.embedder = match &self.embedder {
            // 显式指定（含 `Some(None)` = 纯 BM25）
            Some(e) => e.clone(),
            // 未指定 → 默认本地 embedder，**会话数取自 cfg**（`local-embed` feature 下）
            None => default_embedder(cfg.embed_sessions, ctor),
        };
        cfg
    }

    /// 向量后端（`build_config` 之外的独立读取，供 `SearchIndex` 初始化 `Inner`）。
    pub(crate) fn backend(&self) -> VectorBackend {
        self.backend
    }
}

/// 本地 embedder 的**构造器**签名——S6-T7 的**内部测试接缝**，不是公开扩展点。
///
/// 存在的唯一理由是**可测性**（设计 §7 的可测性前提）：`LocalEmbedder::new()` 要拉
/// ONNX 模型 + 推理，CI 里跑不动。把「构造」抽成函数指针之后，「显式请求向量却拿不到
/// 向量」这条策略就能用**必失败的构造器**在秒级单测里钉住。
///
/// ⚠️ **按 PR #43 评审 P3-5 收敛为私有**（原为 `pub`）：它只为单测注入而存在，
/// 公开出去会被误当成受稳定承诺保护的 API（原形态一次导出 3 个公开项，而生产只用
/// 到其中 1 个）。入口收敛为 [`required_local_embedder`]（**公开**，供 CLI 的
/// `--vectors`）+ [`default_embedder`]（**私有**，供零配置装配）两处
/// —— 即**公开面只有 1 个**（PR #43 第 2 轮评审 P3 的措辞更正）。
type EmbedderCtor = fn(usize) -> Result<Arc<dyn Embedder>>;

/// 默认本地 embedder 的真实构造器（`local-embed` feature）。
///
/// 模型首次下载约 49 s；**失败不 panic**，把错误原样交给调用方，
/// 由 [`resolve_embedder`] 按「是否显式请求」决定上抛还是退化。
#[cfg(feature = "local-embed")]
fn local_embedder_ctor(sessions: usize) -> Result<Arc<dyn Embedder>> {
    use crate::embed::LocalEmbedder;
    Ok(Arc::new(LocalEmbedder::new_with_sessions(sessions)?) as Arc<dyn Embedder>)
}

/// 默认本地 embedder 的构造器（未启用 `local-embed`：**恒失败**，无本地推理能力）。
#[cfg(not(feature = "local-embed"))]
fn local_embedder_ctor(_sessions: usize) -> Result<Arc<dyn Embedder>> {
    Err(crate::error::Error::NoEmbedder)
}

/// 解析**默认** embedder：按「是否显式请求向量」决定失败语义（D-S6-05 方案 A）。
///
/// | `require` | 场景 | 构造失败时 |
/// | --- | --- | --- |
/// | `true` | 显式请求向量（`helix build --vectors`） | **上抛 `Err`** |
/// | `false` | 零配置默认装配（`SearchIndex::builder().build()`） | 退化 `Ok(None)`（纯 BM25）+ **stderr 告警** |
///
/// # 为什么要把「策略」与「构造」分开
///
/// 「要了向量却拿到纯 BM25」是典型的**静默降级**：检索不报错，只是召回悄悄变差，
/// 用户往往在结果不对时才发现——这与 Step 2 用 `GraphStatus` 消灭「图降级无人知」
/// 是同一条纪律（NFR-07）。而 `require == false` 时必须保留退化，否则
/// `SearchIndex::builder().build()` 的「零配置可用」契约（G4）就破了。
///
/// 两者是不同的人机界面，因此**不能**用一句 `.ok()` 同时糊过去。
/// 分离之后，测试注入一个必失败的 [`EmbedderCtor`] 即可覆盖完整策略，
/// 不必依赖真实模型。
///
/// ⚠️ **退化分支必须保留根因**（PR #43 评审 P2-1）：设计 §7 对 S6-T7 的验收判据是
/// 「要么 `Err`、要么**可观测标志**为真，**绝不静默**」——只丢一句 `Ok(None)` 满足
/// 「不报错」，却不满足「可观测」。更关键的是下游代价：`search --mode vector` /
/// `compare` 只能看到 `None`，于是报「未启用任何 Embedder 实现，请开启 local-embed
/// 或 remote-embed feature」——而 feature 明明是开着的，真因是**模型没拿到** ⇒
/// 把用户指向错的方向。这里与 `GraphStatus::Rebuilt(reason)` 保留 reason 的做法对齐。
fn resolve_embedder(
    require: bool,
    ctor: EmbedderCtor,
    sessions: usize,
) -> Result<Option<Arc<dyn Embedder>>> {
    match ctor(sessions) {
        Ok(e) => Ok(Some(e)),
        // 显式请求：错就是错，不许静默换轨
        Err(err) if require => Err(err),
        // 零配置路径：退化而非报错（契约见 rustdoc 上表），但**不静默**
        Err(err) => {
            eprintln!(
                "[警告] 本地 embedder 初始化失败，本次装配退化为纯 BM25（原因: {err}）；\
                 要向量检索请检查模型下载与缓存目录"
            );
            Ok(None)
        }
    }
}

/// **显式请求向量**的生产入口：拿不到本地 embedder 即 `Err`（`helix build --vectors` 用）。
///
/// 这是 **S6-09 / D-S6-05 方案 A** 的唯一公开入口。策略本体在 `resolve_embedder`，
/// 这里只固定 `require = true`，使调用方**不必**处理「`Ok` 却拿到 `None`」这条不可达
/// 分支——原形态把它暴露出去后，CLI 被迫多一个 `expect`（新 panic 分支、无测试覆盖，
/// 见 PR #43 评审 P3-5）。
///
/// 与零配置路径 `default_embedder` 的区别是**人机界面**：用户显式要了向量却拿不到，
/// 属于必须当场告知的错误；而未显式请求时的退化只需可观测（保 G4 契约）。
///
/// ⚠️ 0.x 阶段按评审 Q6 拍板**不做额外兼容开关**（D-S6-05），故这里不接受
/// 「允许半向量」之类的放行参数。
pub fn required_local_embedder() -> Result<Arc<dyn Embedder>> {
    // `require = true` ⇒ 必为 `Some`；用 `ok_or` 而非 `expect`，是为了不引入 panic 分支。
    // ⚠️ CLI 侧暂**固定单会话**：`--embed-sessions` 的口径与推荐值由 spike S8-S2 的
    //    结论决定（`Q4` 明确「库侧默认不动」，CLI 侧留待结论出来后另开），
    //    所以这里不给它开参数；需要多会话的调用方走 `SearchIndexBuilder::embed_sessions`。
    resolve_embedder(true, local_embedder_ctor, 1)?.ok_or(crate::error::Error::NoEmbedder)
}

/// 默认 Embedder：走 [`resolve_embedder`] 的**非显式请求**分支。
///
/// `local-embed` 下是本地 bge-small-zh-v1.5；未启用该 feature 时
/// `local_embedder_ctor` 恒失败 ⇒ 得到 `None`（纯 BM25）**并打一条 stderr 告警**
/// （退化可观测，见 §7 的 S6-T7 判据）。
fn default_embedder(sessions: usize, ctor: EmbedderCtor) -> Option<Arc<dyn Embedder>> {
    // `require = false` 时 resolve_embedder 不会返回 Err，故这里的 unwrap 不可能 panic。
    resolve_embedder(false, ctor, sessions).unwrap_or(None)
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
        // T7-21 / D-J11：并行建图**默认已翻转**（门面默认 = 用户可见的默认）
        assert!(cfg.parallel_build, "门面默认 parallel_build 应为 true");
    }

    /// **T7-21 / D-J11**：门面默认已翻转为「开」，且**显式关闭仍然有效**（逃生舱不失效）。
    ///
    /// ⚠️ 覆盖边界：本测试只钉**配置层**的默认值与覆写。「配置 → 后端真的分派并行」这条
    /// 端到端链路**不可观测**（并行计数器 `parallel_inserts()` 只在低层 `HnswRsIndex` 上，
    /// 门面不暴露）⇒ 后端分派那一段由 `tests/graph_persist.rs` 的 **T22** 钉住。
    #[test]
    fn 并行建图默认开且可显式关闭() {
        assert!(
            SearchIndexBuilder::default().build_config().parallel_build,
            "门面默认应为 true（T7-21 / D-J11 翻转）"
        );
        assert!(
            !SearchIndexBuilder::default()
                .parallel_build(false)
                .build_config()
                .parallel_build,
            "显式 false 必须能关掉（需要可复现拓扑时用）"
        );
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

    // ---- D-S6-05 / S6-T7：显式请求向量必须给向量（**不需要真实模型**） ----

    use crate::error::Error;

    /// 注入用：恒失败的构造器（模拟模型下载失败 / 未启用 `local-embed`）。
    ///
    /// ⚠️ `sessions` 形参是 `S8-08` 加的（ctor 现在要转交会话数）；本测试**不关心**它，
    /// 但**故意保留形参而不吞掉** —— 装配层若忘了把 `embed_sessions` 传下来，
    /// 「会话数真的到达了构造器」这件事就只能靠一个**记录型** ctor 证明（见下方 `recording_ctor`）。
    fn failing_ctor(_sessions: usize) -> Result<Arc<dyn Embedder>> {
        Err(Error::Embedding("注入的必失败构造器".to_string()))
    }

    /// 注入用：恒成功的构造器（最小 Embedder，不碰 ONNX）。
    fn ok_ctor(_sessions: usize) -> Result<Arc<dyn Embedder>> {
        Ok(Arc::new(ProbeEmbedder))
    }

    /// 注入用：**记录会话数**的构造器（`S8-08` 的接线判据用）。
    ///
    /// # 为什么需要它
    ///
    /// `embed_sessions` 要穿过三层才到达构造器：`SearchIndexBuilder` → `build_config`
    /// → `default_embedder` → `EmbedderCtor`。**只看「结果对不对」是抓不到漏传的** ——
    /// 漏传会静默退回**单槽**，而「单槽」本身是**合法配置**（默认值就是 1）⇒ 行为判据看不见它。
    /// ⇒ 用 `AtomicUsize` 记录 ctor **实收**的值，把接线本身变成断言。
    static SEEN_SESSIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn seen() -> usize {
        SEEN_SESSIONS.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn recording_ctor(sessions: usize) -> Result<Arc<dyn Embedder>> {
        SEEN_SESSIONS.store(sessions, std::sync::atomic::Ordering::SeqCst);
        Ok(Arc::new(ProbeEmbedder))
    }

    /// **`S8-08` 的接线判据**：`embed_sessions` 必须**真的到达构造器**，且默认是 1。
    ///
    /// # 判据为什么必须**从 `build_config_with` 这一层**进（而不是直接调 `resolve_embedder`）
    ///
    /// 直接调 `resolve_embedder` 只能证明「形参会往下转交」；而中间那次传递
    /// （`build_config` → `default_embedder(cfg.embed_sessions, ..)`）**没有任何判据** ——
    /// **变异实测**：把它改成 `default_embedder(1, ctor)` 时，只写直接调用的判据**全部依旧绿**。
    /// ⇒ 判据必须带上 `SearchIndexBuilder` 与 `build_config` 这两段。
    #[test]
    fn S8_08_会话数必须真的到达构造器且默认为一() {
        // ① 默认 1（Q4：库侧默认不动）
        let _ = SearchIndexBuilder::default().build_config_with(recording_ctor);
        assert_eq!(seen(), 1, "默认必须是 1");

        // ② 显式 4 ⇒ 原样到达（接线不断层）
        let _ = SearchIndexBuilder::default()
            .embed_sessions(4)
            .build_config_with(recording_ctor);
        assert_eq!(
            seen(),
            4,
            "🔴 会话数在装配链路里被吞掉了（构造器收到的是别的值）"
        );

        // ③ `0` 在**装配处**归一为 1 后才交给构造器 ⇒ 构造器与 `LocalEmbedder` 都不会看到 0
        //    （后者的 `normalize_sessions` 因此是**第二道防线**，见 `embed::local`）
        let _ = SearchIndexBuilder::default()
            .embed_sessions(0)
            .build_config_with(recording_ctor);
        assert_eq!(
            seen(),
            1,
            "0 必须在装配处归一（空池会在第一次 embed_query 时才炸）"
        );

        // ④ 显式装配 `embedder(..)` 时**不**得再去造默认 embedder（本值被忽略）
        SEEN_SESSIONS.store(0, std::sync::atomic::Ordering::SeqCst);
        let cfg = SearchIndexBuilder::default()
            .embedder(Some(Arc::new(ProbeEmbedder)))
            .embed_sessions(4)
            .build_config_with(recording_ctor);
        assert_eq!(
            seen(),
            0,
            "显式 embedder ⇒ 不得调默认构造器（否则白付一次模型加载）"
        );
        assert!(
            cfg.embedder.is_some(),
            "显式装配的 embedder 必须原样进入 `Config`"
        );
    }

    /// **`S8-08`**：`SearchIndexBuilder::embed_sessions` 的默认 / 显式 / 归一三态。
    ///
    /// ⚠️ 归一发生在 `build_config`（`Config.embed_sessions >= 1` 是要守住的不变式）；
    /// 而**构造器**拿到的是**已归一**的值（见 `default_embedder` 的调用点）。
    ///
    /// 🔴 **必须走 `build_config_with(ok_ctor)`，不能用 `build_config()`**：后者调**真实**
    /// 构造器 ⇒ **在 CI 里会去联网下载模型**（实测踩到：`feature isolation` 的 `charabia`
    /// 步骤红 —— 同一用例里「一次构造失败、一次成功」⇒ 结果**不确定**）。
    /// 本用例只关心装配出来的 `Config`，与 embedder 是怎么来的无关
    /// ⇒ 用注入的最小 embedder（不碰 ONNX / 网络）。
    #[test]
    fn S8_08_builder的会话数默认一且零归一为一() {
        let cfg = SearchIndexBuilder::default().build_config_with(ok_ctor);
        assert_eq!(cfg.embed_sessions, 1, "默认 1 ⇒ 零行为变化");

        let cfg = SearchIndexBuilder::default()
            .embed_sessions(4)
            .build_config_with(ok_ctor);
        assert_eq!(cfg.embed_sessions, 4, "显式值原样生效");

        let cfg = SearchIndexBuilder::default()
            .embed_sessions(0)
            .build_config_with(ok_ctor);
        assert_eq!(
            cfg.embed_sessions, 1,
            "0 是无意义输入 ⇒ 归一为 1（不造空池）"
        );
    }

    /// **`S8-08`**：会话数**不进配置指纹**（否则同一快照在 1 / 4 下会互不兼容）。
    ///
    /// 🔴 **同样必须注入最小 embedder**（理由见上一条）：用真实构造器时，`embedder`
    /// 是 `None` 还是 `Some` **取决于模型能不能拿到** ⇒ 两个 `Config` 的
    /// **唯一变量就变成了「有没有模型」而不是「会话数」** —— 那正是 CI 上这条用例首轮红的
    /// 原因（`left: embedder_id: ""` vs `right: "bge-small-zh-v1.5"`）。
    #[test]
    fn S8_08_会话数不进配置指纹() {
        let a = SearchIndexBuilder::default()
            .embed_sessions(1)
            .build_config_with(ok_ctor);
        let b = SearchIndexBuilder::default()
            .embed_sessions(8)
            .build_config_with(ok_ctor);
        // 前提（自证变量被钉住）：两侧都真的拿到了 embedder ⇒ 差异只可能来自会话数
        assert!(
            a.embedder.is_some() && b.embedder.is_some(),
            "前提：注入的构造器必须成功"
        );
        assert_eq!(
            format!("{:?}", a.fingerprint()),
            format!("{:?}", b.fingerprint()),
            "🔴 会话数改变了指纹 ⇒ 同一索引在 1 / N 会话下会被判为「配置不匹配」（不该发生）"
        );
    }

    struct ProbeEmbedder;

    impl Embedder for ProbeEmbedder {
        fn dim(&self) -> usize {
            4
        }
        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.0; 4]).collect())
        }
        fn embed_query(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.0; 4])
        }
        fn id(&self) -> &'static str {
            "probe-embedder"
        }
    }

    /// S6-T7 主断言：**显式请求向量 ⇒ 构造失败必须上抛**，不许悄悄退化成纯 BM25。
    ///
    /// 用 `match` 而非 `expect_err`：后者要求成功侧的 `Option<Arc<dyn Embedder>>`
    /// 实现 `Debug`，为一个测试给 trait 加 bound 得不偿失。
    #[test]
    fn 显式请求向量时构造失败必须报错() {
        let err = match resolve_embedder(true, failing_ctor, 1) {
            Ok(v) => panic!(
                "require=true 时必须上抛 Err，实际拿到 Ok({:?})",
                v.is_some()
            ),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("注入的必失败构造器"),
            "应原样上抛构造错误（而不是换成别的错），实际: {err}"
        );
    }

    /// S6-T7 对照面：**零配置路径**必须保住「零配置可用」契约——
    /// 构造失败退化为纯 BM25，而不是让 `builder().build()` 整体不可用。
    #[test]
    fn 未显式请求时构造失败退化为纯bm25() {
        assert!(resolve_embedder(false, failing_ctor, 1).unwrap().is_none());
    }

    /// 构造成功时两种模式都要拿到 embedder（防「require 分支写反」类回归）。
    #[test]
    fn 构造成功时两种模式都拿到向量() {
        for require in [true, false] {
            let got = resolve_embedder(require, ok_ctor, 1).unwrap();
            assert!(got.is_some(), "require={require} 构造成功应给出 embedder");
        }
    }
}
