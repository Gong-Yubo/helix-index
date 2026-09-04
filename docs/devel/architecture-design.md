# HelixIndex 架构设计说明书

### —— 面向 Agent 场景的通用检索引擎内核

| 项目   | 内容                   |
| ---- | -------------------- |
| 文档版本 | v1.4                 |
| 创建日期 | 2026-09-02           |
| 状态   | P6 接口重构设计已并入（对外接口门面层，见第 10 章） |
| 技术栈  | Rust 1.90+ / 2021 edition |
| 文件名   | `architecture-design.md` |
| 配套文档 | `requirements-spec.md`（需求分析说明书） |

---

## 1. 文档信息

### 1.1 版本与状态

| 版本   | 日期         | 状态  | 说明                          |
| ---- | ---------- | --- | --------------------------- |
| v1.0 | 2026-09-02 | 待评审 | 由 `archive/requirements-and-design_v1.0.md` v1.0 拆分重构而来 |

### 1.2 读者

实现者与代码评审者。阅读前应先了解需求背景，见 `requirements-spec.md` 第 2、4 章。

### 1.3 与需求文档的关系

```
需求分析说明书
    │  FR-xx / NFR-xx 的唯一权威来源
    │  术语表的唯一定义来源
    ▼
架构设计说明书（本文档）
    只引用编号，不重复定义需求
```

本文出现的每个 `FR-xx` / `NFR-xx` 均指向需求文档。需求变更时只改需求文档，本文通过编号保持引用有效。

### 1.4 修订记录

见文末「15.2 变更记录」。

---

## 2. 架构概述

### 2.1 文档目的与范围

本文档定义 HelixIndex 检索内核的模块结构、核心抽象、数据结构、关键算法与技术选型。

**覆盖的需求**：FR-01 ~ FR-23、NFR-01 ~ NFR-09。FR-24（结果去重）、FR-25（token budget 裁剪）为 v2 需求，本版不做设计。

**需求覆盖矩阵**见第 12.2 节。

### 2.2 架构目标与约束

| 来源     | 目标 / 约束                                    | 架构体现                          |
| ------ | ------------------------------------------ | ----------------------------- |
| G2     | 全部核心能力可替换                                  | 第 5 章，六大 trait 抽象             |
| G5     | 为 Rerank / 增量 / 分布式留位                      | FR-18 NoOp 实现、7.5 增量方案、模块边界   |
| NFR-02 | 混合 P99 < 20ms                              | 第 8 章：两路并行、零 IO、缓存            |
| NFR-06 | 结果确定可复现                                    | 第 8.2 节：seed 固定 + tie-break   |
| 需求文档 8.1 | BM25 手写、内存 + 快照、默认本地 embedding           | ADR-001、ADR-005、ADR-003       |

### 2.3 关键架构决策摘要

| 编号      | 决策                                    | 一句话理由                             | 相关需求   |
| ------- | ------------------------------------- | --------------------------------- | ------ |
| ADR-001 | BM25 倒排索引**手写**，不用 tantivy            | 符合"从 0 开发"目标，且中文调优与扩展完全可控         | FR-03、FR-04 |
| ADR-002 | 向量索引用 `instant-distance`，以主索引 + delta 绕开无 insert API | 纯 Rust 无 C 依赖，增量限制可用工程手段绕过。**P4 与 `hnsw_rs` A/B 复核，达标则删除 delta 区** | FR-08、FR-17 |
| ADR-003 | Embedding 默认本地 `fastembed`，远程作 feature | 无需联网与 API Key，中文效果好，远程作为兜底         | FR-06、FR-07 |
| ADR-004 | 融合**默认 RRF（k=60）**，加权归一仅作备选           | 两路量纲不可比，RRF 只用排名、免疫量纲              | FR-10、FR-11 |
| ADR-005 | 持久化用**内存索引 + 自研 bincode 快照**，不引入嵌入式 KV | 秒级加载、零额外依赖；bincode 3.0.0，**P4 与 `rkyv` A/B** | FR-16  |
| ADR-006 | BM25 检索用 **TAAT** 而非 DAAT/WAND        | 万级规模下实现简单、缓存友好，天然支持 OR 语义          | FR-04  |
| ADR-007 | **`tantivy` 仅作 dev 依赖的 BM25 正确性基线** | 不用它实现功能，**用它证明自己写得对**；自研最大风险是"写错了看不出来" | FR-03、FR-04 |
| ADR-008 | **MSRV 提到 1.90 并用 `cargo-deny` 强制 License 白名单** | NFR-08 的 1.80 已被主流依赖突破；License 友好是硬要求，人工审查不可靠 | NFR-08 |
| ADR-009 | **对外接口采用「门面层 + 共用编排内核」**（`SearchIndex` 写端 / owned `Searcher` 读端） | 复用同一编排实现避免逻辑漂移；重构是"加法"，bench/测试零改动，逃生舱保证能力不丢 | issue #1 |

---

## 3. 系统上下文与整体数据流

### 3.1 系统上下文

```
┌────────────────────────────────────────────┐
│  Agent 应用层                                │
│  知识库 RAG / 长期记忆 / 代码助手                    │
│  （查询改写、prompt 组装、多轮对话管理 —— 不在本内核）        │
└───────────────┬────────────────────────────┘
                │  SearchRequest / SearchResponse
┌───────────────▼────────────────────────────┐
│  helix-core（本内核）                          │
│  摄入 → 分析 → 索引 → 召回 → 融合 → 精排(留位) → 输出  │
└────────────────────────────────────────────┘
```

内核边界：**不做查询改写、不做 prompt 组装、不管多轮对话**。这些属于 Agent 应用层。内核只提供 FR-21 的多 query 接口作为承载。

### 3.2 写入路径

```
Document
   │
   ├─► Chunker ──► Vec<Chunk>
   │                  │
   │                  ├─► Analyzer ──► Vec<Token> ──► 倒排索引（可变）
   │                  │                                  └─► 正排存储
   │                  │
   │                  └─► Embedder::embed_documents ──► Vec<Vec<f32>> (L2 归一化)
   │                                                       │
   │                                     ┌─────────────────┴──────────────┐
   │                                     ▼                                ▼
   │                            主 HNSW（不可变）                    delta 区（暴力）
   │                                     └──────── commit() 合并 ─────────┘
   ▼
Snapshot (可选持久化)
```

### 3.3 查询路径

```
Query(raw text)
   │
   ├────────────────► Analyzer::analyze_query ──► Vec<Token>
   │                                                  │
   │                          ┌───────────────────────┴────────────┐
   │                          ▼                                    ▼
   │              [Lane A] BM25Retriever            [Lane B] VectorRetriever
   │                          │                        Embedder::embed_query
   │                          │                              ▼
   │                          │                      VectorIndex::search
   │                          ▼                              ▼
   │                   Vec<ScoredHit>                 Vec<ScoredHit>
   │                          └──────────┬───────────────────┘
   │                          （两路并行，rayon）
   │                                     ▼
   │                            FusionStrategy::fuse (RRF)
   │                                     ▼
   │                            Reranker（v1 = NoOp）
   │                                     ▼
   │                      正排回捞 ──► Vec<Hit> (含 Explain)
   │                                     ▼
   └──────────────────────────► SearchResponse (含 EmptyReason / took)
```

> **融合层不回捞正文**。融合只操作 `(chunk_id, score)`，正文回捞统一在融合与精排之后做一次，避免对候选集做无谓的字符串拷贝。

---

## 4. 模块架构

### 4.1 Cargo workspace 目录结构

```
HelixIndex/
├── Cargo.toml                 # workspace
├── crates/
│   ├── core/                  # helix-core（库，对外交付主体）
│   │   └── src/
│   │       ├── lib.rs          # 顶层 API：SearchIndex / Searcher 门面 + prelude
│   │       ├── error.rs        # Error / Result
│   │       ├── types.rs        # DocId / ChunkId / TermId / Score
│   │       ├── document.rs     # Document（输入 DTO）/ DocRecord（存储记录）/ Chunk
│   │       ├── schema.rs       # 字段定义、权重、Filter
│   │       ├── analyze/        # Analyzer trait / 中英混合分词 / 过滤器 / 停用词
│   │       ├── index/          # Index / inverted / posting / forward / stats
│   │       ├── retriever/      # Retriever trait / bm25 / vector
│   │       ├── vector/         # VectorIndex trait / hnsw / brute
│   │       ├── embed/          # Embedder trait / local / remote / cached
│   │       ├── fusion/         # FusionStrategy trait / rrf / weighted
│   │       ├── rerank/         # Reranker trait / noop
│   │       ├── query/          # SearchIndex / Searcher 门面 + search_parts 编排 + parse / explain
│   │       ├── storage/        # Snapshot 读写 / codec
│   │       └── chunk/          # Chunker 分块策略
│   └── cli/                   # helix（二进制）
├── data/                      # 30 篇 demo 语料 + T2Ranking 评测集转换产物（t2-corpus / t2-queries，见需求文档 9.4）
├── examples/
└── docs/
```

### 4.2 模块职责与边界

| 模块          | 职责            | 禁止                  |
| ----------- | ------------- | ------------------- |
| `analyze`   | 文本 → Token 序列 | 不知道索引的存在            |
| `index`     | 倒排 / 正排 / 统计量 | 不知道融合与排序            |
| `vector`    | 向量存储与近邻检索     | 不知道文本，只认 `Vec<f32>` |
| `embed`     | 文本 → 向量       | 不知道索引的存在            |
| `retriever` | 单路召回          | 不与其他 lane 交互        |
| `fusion`    | 多路合并          | 不回捞正文               |
| `query`     | 编排以上全部；门面层（`SearchIndex` 写端 / `Searcher` 读端）只做装配与生命周期管理 | 不自己实现算法             |

> 这些「禁止」是**代码评审的硬性检查项**。违反边界不会导致编译失败，但会让 trait 抽象失去意义——一旦 `fusion` 开始回捞正文，换融合策略就再也不能独立测试。

> **门面层边界（P6 新增）**：`SearchIndex` / `Searcher` 是 `query` 模块上的门面，只做装配与生命周期管理，**不实现任何算法**——`add` 内部依次调 `Chunker::chunk` → `Index::add` → `Embedder::embed_documents` → `VectorIndex::add`，不碰 postings / BM25 公式 / HNSW 图。底层 `QueryExecutor<'a>`（旧 `Searcher`）原样保留作逃生舱，见第 10 章。

### 4.3 依赖规则

```
query  ──►  fusion / rerank / retriever / index / embed / analyze
retriever ─►  index | vector | embed | analyze
fusion  ──►  (无下游依赖，纯函数)
index   ──►  analyze / storage
vector  ──►  (无下游依赖)
embed   ──►  (无下游依赖)
```

规则：**只允许上层依赖下层，禁止反向依赖与同层横向依赖**（`retriever` 的两路实现之间尤其禁止互相引用）。

---

## 5. 核心抽象设计

### 5.1 基础类型

```rust
// ---------- types.rs ----------
pub type DocId   = u32;
pub type ChunkId = u32;
pub type TermId  = u32;
pub type Score   = f32;

pub struct Document {                // 输入 DTO（对外门面 add 的参数，P6 起才含 text）
    pub text: String,                // 原始文本，由门面层分块
    pub source: String,              // 文件路径 / URL / 标题 —— 溯源用（FR-12）
    pub metadata: serde_json::Value, // 业务自定义 —— 过滤用（FR-14）
    pub dedup_key: Option<String>,   // 幂等 upsert 键（FR-15）；缺省用 xxh64(text)
}

pub struct DocRecord {               // 存储记录（内部），由 Document 派生
    pub doc_id: DocId,
    pub source: String,
    pub metadata: serde_json::Value,
    pub content_hash: u64,           // xxh64(dedup_key.unwrap_or(text))
}

pub struct Chunk {
    pub chunk_id: ChunkId,
    pub doc_id: DocId,
    pub ordinal: u32,       // 在文档内的序号，用于相邻 chunk 合并（FR-24 v2）
    pub text: String,
    pub char_start: usize,  // 原文偏移，用于精确引用（FR-12）
    pub char_end: usize,
}
```

### 5.2 Token 与 Analyzer

```rust
pub struct Token {
    pub term: SmolStr,   // 归一化后的索引词
    pub position: u32,   // 位置，供未来短语查询使用
    pub start: usize,
    pub end: usize,
}

pub trait Analyzer: Send + Sync {
    fn analyze_doc(&self, text: &str) -> Vec<Token>;
    /// 查询侧分词。默认转发 analyze_doc —— 强制两侧一致，避免最常见的坑。
    fn analyze_query(&self, text: &str) -> Vec<Token> {
        self.analyze_doc(text)
    }
}
```

> 拆成两个方法而非一个，是为了**未来允许**查询侧差异化（如同义词扩展、保留停用词）；但默认实现强制一致，从 API 层面杜绝"索引侧和查询侧分词不一致"这类隐蔽 bug。对应风险 R4。

### 5.3 Embedder

```rust
pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize;
    /// 入库侧。BGE 系列不加前缀。
    fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    /// 查询侧。BGE 系列**必须**加 instruction 前缀。
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
    /// 输出是否已 L2 归一化
    fn is_normalized(&self) -> bool { true }
}
```

> `embed_documents` 与 `embed_query` **必须分开**。BGE 中文模型要求查询侧加前缀 `为这个句子生成表示以用于检索相关文章：`，入库侧不加。这一步漏掉，向量检索效果会明显下降——**是最容易踩也最难自查的坑**（代码能跑、分数看着也正常）。对应风险 R2。

### 5.4 VectorIndex

```rust
pub trait VectorIndex: Send + Sync {
    fn dim(&self) -> usize;
    fn len(&self) -> usize;
    fn build(vectors: Vec<Vec<f32>>, ids: Vec<ChunkId>) -> Result<Self>
    where
        Self: Sized;
    /// 契约：返回按 Score **降序**（相似度，越大越相似）
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(ChunkId, Score)>>;
}
```

> trait 契约固定为**相似度降序**，而底层 HNSW 返回的是距离升序。转换责任放在实现内部（见 7.3），业务层永远只看相似度。

### 5.5 Retriever

```rust
pub struct RetrieveRequest<'a> {
    pub query: &'a AnalyzedQuery,
    pub k: usize,
    pub filter: Option<&'a Filter>,
}

pub struct ScoredHit {
    pub chunk_id: ChunkId,
    pub score: Score,
}

pub trait Retriever: Send + Sync {
    fn name(&self) -> &'static str;
    fn retrieve(&self, req: &RetrieveRequest) -> Result<Vec<ScoredHit>>;
}
```

`name()` 用于 `Explain` 中标识来源 lane，是 FR-13 可解释性的基础。

### 5.6 FusionStrategy

```rust
pub trait FusionStrategy: Send + Sync {
    /// lanes: (lane 名称, 该 lane 的降序结果)
    /// weights: 与 lanes 一一对应的权重
    fn fuse(&self, lanes: &[(&'static str, Vec<ScoredHit>)], weights: &[f32])
        -> Vec<ScoredHit>;
}
```

### 5.7 Reranker（v1 仅留位，FR-18）

```rust
pub trait Reranker: Send + Sync {
    fn rerank(&self, query: &str, hits: Vec<Hit>, top_n: usize) -> Result<Vec<Hit>>;
}
```

### 5.8 返回类型

```rust
pub struct Hit {
    pub chunk_id: ChunkId,
    pub doc_id: DocId,
    pub score: Score,
    pub text: String,
    pub source: String,
    pub metadata: serde_json::Value,
    pub explain: Explain,
}

impl Hit {
    /// 直接产出可拼进 prompt 的上下文块，带出处标注（FR-12）
    pub fn to_context_block(&self) -> String { /* ... */ }
}

pub struct Explain {
    pub matched_terms: Vec<String>,
    pub bm25_score:  Option<Score>,
    pub bm25_rank:   Option<u32>,
    pub vector_score: Option<Score>,
    pub vector_rank:  Option<u32>,
    pub fused_score: Score,
}

pub struct SearchResponse {
    pub hits: Vec<Hit>,
    pub total_candidates: usize,
    /// 空结果时说明原因，供 Agent 决策（FR-13）
    pub empty_reason: Option<EmptyReason>,
    pub took: Duration,
}

pub enum EmptyReason {
    NoDocuments,        // 索引为空
    AllTermsUnmatched,  // 词全部未命中 —— 提示 query 可能含幻觉词
    FilteredOut,        // 有候选但被 filter 全部过滤 —— 提示放宽 scope
}
```

> `bm25_rank` / `vector_rank` 用 `Option` 而非默认值：`None` 明确表示"该 lane 未召回此文档"，这正是 4.5 节所说的诊断信号。用 0 或 `usize::MAX` 会丢失这层语义。

---

## 6. 数据结构设计

### 6.1 倒排索引

```rust
pub struct InvertedIndex {
    term_dict:   HashMap<String, TermId>,  // Term → TermId
    terms:       Vec<String>,              // TermId → Term（反查，快照与调试用）
    postings:    Vec<PostingList>,         // TermId → posting 列表
    df:          Vec<u32>,                 // TermId → document frequency
}

pub struct PostingList {
    chunks: Vec<Posting>,   // 按 chunk_id 升序
}

pub struct Posting {
    chunk_id:  ChunkId,
    term_freq: u32,
    positions: Vec<u32>,    // MVP 即存储，供未来短语查询；feature = "positions" 控制
}
```

**设计说明**

- **term dict 与 postings 分离**：`HashMap<String, TermId>` + `Vec<PostingList>`，而不是 `HashMap<String, Vec<Posting>>`。后者在序列化时每个 key 都要重复存字符串，前者只需存一次字符串表，快照体积与加载速度都更优。
- **MVP 就存 positions**：虽然第一版不用，但**事后补需要重建全部索引**。存储成本可接受，收益是未来加短语查询零迁移成本。
- **MVP 不压缩**：posting 用朴素 `Vec<Posting>`。varint / roaring bitmap 列为 P4 优化项——过早优化会掩盖算法本身的正确性验证。

### 6.2 正排存储

```rust
pub struct ForwardStore {
    chunks:   Vec<Chunk>,          // chunk_id 即下标，O(1) 回捞
    docs:     HashMap<DocId, Document>,
    deleted:  RoaringBitmap,       // 墓碑标记
    by_source: HashMap<String, DocId>,  // 幂等 upsert 用（FR-15）
    by_hash:   HashMap<u64, DocId>,     // 幂等 upsert 用（FR-15）
}
```

### 6.3 统计量维护（增量友好，FR-17）

```rust
pub struct IndexStats {
    num_chunks: usize,  // 有效 chunk 数（已扣除墓碑）
    total_len:  u64,    // 所有 chunk 的 term 数总和
}
// avgdl = total_len as f32 / num_chunks as f32
```

**增量维护而非重算**

- 插入 chunk：对每个 term 的 `df += 1`（仅当该 term 首次出现于此 chunk），`total_len += dl`，`num_chunks += 1`
- 删除 chunk：从正排取回原文，**重新分析一次**得到 term 列表，逐个 `df -= 1`，再减 `total_len` 与 `num_chunks`

> 删除时重新分析而非维护反向映射，是用 CPU 换内存：省掉一份 `chunk → terms` 的常驻映射（百万级规模下这是显著开销），而删除是低频操作。对应风险 R5，必须由单测兜住。

### 6.4 向量存储（FR-08 / FR-17）

```rust
pub struct VectorStore {
    sealed: Option<HnswMap<NormalizedVector, ChunkId>>,  // 主 HNSW，不可变
    delta:  Vec<(ChunkId, NormalizedVector)>,            // 未建图的新向量
}
```

详见 7.5 节。

---

## 7. 关键算法设计

### 7.1 中英混合分词（FR-02）

**处理流程**

```
原始文本
  → 按 Unicode 属性切分为「中文段」与「拉丁段」
  → 中文段：jieba-rs 分词
  → 拉丁段：正则切词（[A-Za-z0-9]+）
  → Token 过滤器链：小写化 → 停用词过滤 → 英文词干化（可选）→ 长度过滤
  → Vec<Token>
```

**决策点**

| 项      | 决策                                    | 理由                                                                     |
| ------ | ------------------------------------- | ---------------------------------------------------------------------- |
| 中文分词   | jieba-rs                              | 生态成熟、词典可定制                                                             |
| 中英分段   | `unicode-segmentation`                | **不手写 Unicode 边界判断**：全角符号、假名、emoji 的边界自己判极易出错，一个轻量纯 Rust 依赖换正确性          |
| 文本归一化  | `unicode-normalization`（NFC）          | 避免"é"与"e+́"被当成两个 term，导致索引侧与查询侧不一致                                  |
| 英文词干化  | **第一版不做**       | 词干化（rust-stemmers）会损害专有名词与术语的精确匹配；检索场景宁可牺牲一点召回换精确率。留作可插拔 filter        |
| 停用词    | 中英文合并词表，保守裁剪    | 只去掉真正无意义的虚词。⚠️ 注意"的/了"这类词**不能**在向量路去掉——向量路用的是原文，不受影响；但 BM25 路去掉可显著减小索引 |
| 单字词    | 保留              | 中文单字词（"云""网""库"）常是关键术语                                                 |
| 查询侧一致性 | 强制同一 `Analyzer` | 通过默认实现 `analyze_query → analyze_doc` 保证（见 5.2）                          |

### 7.2 BM25（FR-04）

**公式**

```
score(D, Q) = Σ_{t ∈ Q}  idf(t) · (tf(t,D) · (k1 + 1)) / (tf(t,D) + k1 · (1 - b + b · dl(D)/avgdl))

idf(t) = ln(1 + (N - df(t) + 0.5) / (df(t) + 0.5))
```

**参数**：`k1 = 1.2`，`b = 0.75`（Lucene 默认值，作为起点）

> ⚠️ **中文场景的重要差异**：`dl` 是 chunk 的 **term 数**。中文分词后 term 数显著少于英文（同样一段话，英文按空格切出 200 词，中文分词后可能只有 120 词），导致 `avgdl` 系统性地偏小，`b` 的作用被放大。**1.2 / 0.75 只能作为起点，必须在 P5 阶段用评测集网格搜索重新调参，不能盲信英文经验值。** 对应风险 R6。

**检索语义**：OR（FR-04）。query 中未命中的 term 贡献 0 分，其余 term 正常累加。

**执行方式**：TAAT（Term-At-A-Time），理由见 ADR-006。

### 7.3 向量检索与相似度对齐（FR-09）

**核心矛盾**：`instant-distance` 的 `Point::distance()` 语义是**距离（越小越相似）**，而业务层要的是**相似度（越大越相似）**，且 BGE 模型用的是余弦相似度。

**解法：入库前统一 L2 归一化**

归一化后，欧氏距离 `d` 与余弦相似度 `cos` 满足：

```
d² = |u - v|² = |u|² + |v|² - 2·u·v = 2 - 2·cos(u,v)
⟹ cos = 1 - d² / 2
```

由于 `d ≥ 0` 时 `cos` 关于 `d` **单调递减**，按 `d` 升序排列与按 `cos` 降序排列**完全等价**——排序结果无需转换即可使用，仅在需要展示真实余弦值时做一次换算即可。

附带好处：归一化后**内积等于余弦**，为未来换用内积索引（如 Faiss 的 `IndexFlatIP`）留好前提。

```rust
#[derive(Clone)]
pub struct NormalizedVector(pub Vec<f32>);

impl instant_distance::Point for NormalizedVector {
    fn distance(&self, other: &Self) -> f32 {
        self.0.iter()
            .zip(other.0.iter())
            .map(|(a, b)| { let d = a - b; d * d })
            .sum::<f32>()
            .sqrt()
    }
}
```

**模型选择**：`BAAI/bge-small-zh-v1.5`，512 维。理由：中文效果与体积平衡（ONNX 约 90MB），CPU 推理速度可接受。备选 `BAAI/bge-m3`（1024 维、多语言、同时支持稀疏与稠密），在需要多语言或稀疏检索时切换。

### 7.4 混合融合（FR-10 / FR-11）

**为什么默认 RRF 而不是加权归一**

两路分数的**量纲与分布完全不同**：

|       | 取值范围                                  | 分布特征                  |
| ----- | ------------------------------------- | --------------------- |
| BM25  | [0, +∞) 无上界，中文常见 5~30                 | 长尾、随 query term 数线性增长 |
| 余弦相似度 | [0, 1]，中文 embedding 实际集中在 [0.6, 0.95] | 极度集中、区分度低             |

这种不可比性使归一化**既损失信息又不稳定**：

- **min-max 归一化**：Top-K 内最大值一变，其余所有分数全部改变；且只有一个候选时分母为 0
- **除以 Top-1 分数**：比 min-max 稳定，但 Top-1 分数本身随 query 波动

RRF **只使用排名，完全免疫量纲问题**：

```
score(d) = Σ_{lane i}  w_i / (k + rank_i(d))
```

- `k = 60`（Cormack et al. 2009 原论文推荐值，empirically robust）
- `rank` 从 1 开始；某 lane 未命中该文档则不贡献
- 默认 `w_bm25 = w_vector = 1.0`

**FR-11 加权归一作为可选策略**：若业务确实需要，采用「除以该 lane Top-1 分数」而非 min-max，并在单候选时退化为 1.0。

### 7.5 增量写入（FR-17）

**约束**：`instant-distance` 的 `Builder::build()` 一次性消费 points，**没有增量插入 API**。

**解法：主索引 + delta 区**

```rust
pub struct VectorStore {
    sealed: Option<HnswMap<NormalizedVector, ChunkId>>,  // 主 HNSW，不可变
    delta:  Vec<(ChunkId, NormalizedVector)>,            // 未建图的新向量
}

// 检索：两处都查，合并取 Top-K
fn search(&self, q: &[f32], k: usize) -> Vec<(ChunkId, Score)> {
    let mut out = self.sealed.as_ref().map(|h| h.search(...)).unwrap_or_default();
    out.extend(brute_force(&self.delta, q, k));
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out.truncate(k);
    out
}

// 合并触发条件（满足任一）
fn should_rebuild(&self) -> bool {
    self.delta.len() >= 1000
        || self.delta.len() as f32
            >= self.sealed_len() as f32 * 0.1
}
```

**代价分析**（须写入文档，避免误用）

- delta 区是**线性扫描**，复杂度 O(|delta| × dim)。将其规模限制在总量的 10% 以内，可保证其对延迟的影响可控
- 重建是 O(n log n)，但可后台异步执行，读请求由旧主索引 + delta 继续服务
- 倒排索引本身可增量写入（HashMap 插入），不受此限制

**⚠️ 本方案待 P4 复核（见 9.2 与 ADR-002）**

`hnsw_rs` 0.3.4（MIT/Apache-2.0，**纯 Rust**）原生支持增量插入，且与本设计的归一化方案天然契合：

- `pub fn insert(&self, datav_with_id: (&[T], usize))` —— 构造后可随时单条插入，无需 delta 区与后台重建
- `pub fn search_filter(&self, data, knbn, ef, filter: Option<&dyn FilterT>)` —— 内建查询时过滤，直接服务 FR-14
- `DistDot` 的语义与 7.3 完全一致：**要求向量入库前 L2 归一化**（文档原文：*"essentially the Cosine distance but we suppose all vectors have been l2 normalized to unity BEFORE INSERTING in HNSW"*），并自带 `l2_normalize` 辅助函数

因此 R1 不一定是"必须绕开的约束"，也可能是"换选型即可消除的约束"。**P4 实现 T4-06 之前，先用 `hnsw_rs` 做一次 A/B 原型**（第二个 `VectorIndex` 实现）：

| 对比项     | instant-distance + delta | hnsw_rs 原生增量 |
| ------- | ------------------------ | -------------- |
| 增量写入延迟  | O(1) 写 delta，但重建时 O(n log n) | 直接入图          |
| 检索路径    | 主索引 + delta 两处扫描后合并      | 单次检索           |
| 过滤支持    | 检索后过滤                    | `search_filter` 原生支持 |
| 删除      | 墓碑 + 过滤（本方案）             | 同样只能墓碑（**无 remove API**，与本方案一致，不构成劣势） |
| 依赖      | 纯 Rust                   | 纯 Rust         |

若 `hnsw_rs` 在召回一致性与延迟上达标，**删除 delta 区设计**（T4-06 一并取消），代码量与复杂度均显著下降。

> 若后续需要**物理删除**（而非墓碑），`hnsw_rs` 与 `instant-distance` 都不满足，此时路径为 `usearch`（Apache-2.0，支持 `add` / `remove` / `filtered_search` / `exact_search`，但通过 `cxx` 引入 C++ 依赖）；`arroy`（LMDB 后端）因与"内存 + 快照"路线冲突而排除。

### 7.6 快照格式（FR-16）

```
┌────────────────────────────────────────┐
│ Header: magic "IDX1" (4B)              │
│         format_version: u32            │
│         crc32: u32                     │
├────────────────────────────────────────┤
│ Section: term_dict   (bincode)         │
│ Section: postings    (bincode)         │
│ Section: forward     (bincode)         │
│ Section: stats       (bincode)         │
│ Section: vectors     (bincode, 可选)   │
└────────────────────────────────────────┘
```

- 序列化用 **bincode 3.0.0**（MIT）。
  > 修正：初版文档称"bincode 1.3 与 `instant-distance` 的 `with-serde` 内部 bincode 对齐"——**该说法有误**。`instant-distance` 0.6.1 的 `with-serde` 只引入 `serde` 与 `serde-big-array`，**不含 bincode**。bincode 版本无外部约束，可自由选择。
- **P4 阶段与 `rkyv` 实测对比**：`rkyv` 0.8.18（MIT）为零反序列化格式，配合 `memmap2` 可使加载接近 O(1)，对 NFR-04（冷启动 < 2s）极具吸引力。代价是所有落盘结构需 `derive(Archive)`，格式升级要额外处理版本兼容。**取舍规则：若 bincode 在 1 万 chunk 快照上已满足 NFR-04，则不引入 rkyv。** 快照结构已在 `storage/codec.rs` 抽象，切换不影响上层。
- `format_version` 用于向后兼容检测：版本不匹配时直接报错而非静默读错
- CRC32 校验防止读到半截文件（进程被 kill 的常见后果）

---

## 8. 质量属性设计

### 8.1 性能（NFR-02 / NFR-03）

| 手段                                    | 说明                                      | 对应需求   |
| -------------------------------------- | --------------------------------------- | ------ |
| 两路召回并行                                 | BM25 lane 与向量 lane 用 rayon 并行，取较慢者耗时    | NFR-02 |
| 查询路径零磁盘 IO                             | 全内存索引；过滤在融合前用 bitmap 做，不触碰正排            | NFR-02 |
| query embedding 缓存                     | **`moka`** 并发缓存（带 TTL），Agent 多轮循环中大量 query 是重复的 | FR-20  |
| 复用 `instant_distance::Search` 对象       | 避免每次查询重新分配堆内存                           | NFR-02 |
| 批量摄入并行                                 | rayon 并行分词与 embedding                   | FR-19  |
| 融合层不回捞正文                               | 只在最后对 Top-K 做一次正排回捞                     | NFR-02 |

> Embedding 是最大延迟源：本地 bge-small 单条约 5~15ms，远程 API 则 50~200ms。**若使用远程 embedding，NFR-02 的 20ms 上限不可能达成**，此时应改为监控 P99 并单独上报 embedding 耗时。

### 8.2 确定性（NFR-06）

| 来源         | 处理                                          |
| ---------- | ------------------------------------------- |
| HNSW 图构建随机性 | 固定随机种子                                      |
| 同分排序不稳定    | tie-break by `chunk_id` 升序                  |
| HashMap 遍历序 | 凡是进入排序的路径，必须先收集再按 `chunk_id` 排序，不直接依赖 HashMap 迭代顺序 |
| 并行执行       | 两路并行不影响各自结果；融合前按 lane 名固定顺序收集               |

验收：同一 query 连续检索 100 次，结果顺序完全一致（见 13.1 单测清单）。

### 8.3 可观测性（NFR-07）

`SearchResponse.took` 为端到端耗时。此外内核内部记录各 lane 耗时与候选数，通过 tracing 输出：

```
search: took=8.2ms bm25=1.4ms(vector=6.1ms parallel) candidates=187 fused=50 took_total=8.2ms
```

---

## 9. 技术选型与决策记录

### 9.1 选型表

> 全部版本于 2026-09-02 经 crates.io API 实测；License 与 MSRV 核查见 `thirdparty.md`。

| 用途             | 选型                     | 版本        | License            | 理由                                                                     |
| -------------- | ---------------------- | --------- | ------------------ | ---------------------------------------------------------------------- |
| 语言             | Rust                   | **1.90+** / 2021 edition | —        | MSRV 由依赖决定（bincode 1.85 / clap 1.85 / smol_str 1.89），见 ADR-008           |
| 向量索引           | `instant-distance`     | 0.6.1     | MIT OR Apache-2.0  | 纯 Rust HNSW、rayon 并行构建、`with-serde` 支持序列化。**P4 与 `hnsw_rs` A/B 复核**（见 7.5、ADR-002） |
| 本地 embedding   | `fastembed`            | **6.0.2** | Apache-2.0         | 基于 ort(ONNX Runtime)、同步无 tokio 依赖。⚠️ ① 传递依赖 `ort =2.0.0-rc.13` 为预发布，`Cargo.lock` 必须入库；② **必须 `default-features = false`**，否则默认 feature 会带来 `image → rav1e → libfuzzer-sys`（NCSA 许可）；③ 实际下载的模型仓为 `Xenova/bge-small-zh-v1.5`（HF 未声明 License，**已决策接受**，沿用上游 MIT；残留风险见需求文档 8.3 P5） |
| 中文分词           | `jieba-rs`             | 0.10.3    | MIT                | 生态成熟、词典可定制                                                             |
| 中英分段           | `unicode-segmentation` | 1.13.3    | MIT OR Apache-2.0  | 不手写 Unicode 边界判断                                                       |
| 文本归一化          | `unicode-normalization` | 0.1.25   | MIT OR Apache-2.0  | NFC 归一化，保证索引侧与查询侧一致                                                    |
| 并行             | `rayon`                | 1.12      | MIT OR Apache-2.0  | 两路召回并行、批量 embedding                                                    |
| 序列化            | `bincode`              | **2.0.1** | MIT                | 主流、够快。**⚠️ 不可用 3.0.0**：crates.io 上的 3.0.0 是玩笑发布（源码仅 `compile_error!("https://xkcd.com/2347/")`）。版本无外部约束（`instant-distance` 的 `with-serde` 只含 serde）。P4 与 `rkyv` A/B |
| 小字符串           | `smol_str`             | 0.3.6     | MIT OR Apache-2.0  | Term 存储的 inline 优化，减少堆分配                                               |
| 缓存             | **`moka`**             | 0.12.16   | (MIT OR Apache-2.0) AND Apache-2.0 | **并发安全 + TTL**；替代原计划的 `lru`（非并发安全，需套 `Mutex`，会抵消两路并行收益）。⚠️ 必须显式启用 `sync` 或 `future` feature，否则 `compile_error!` |
| CLI            | `clap`                 | 4.6       | MIT OR Apache-2.0  | derive API                                                             |
| 错误处理           | `thiserror` / `anyhow` | 2.0 / 1.0 | MIT OR Apache-2.0  | 库侧 / CLI 侧                                                             |
| 日志             | `tracing`（+ subscriber） | 0.1 / 0.3 | MIT             | 可观测性（NFR-07）                                                           |
| 校验             | `crc32fast`            | 1.5.1     | MIT OR Apache-2.0  | 快照 CRC                                                                 |
| **测试基线（dev）**  | **`tantivy`**          | 0.26.1    | MIT                | **仅作 BM25 正确性对照，不参与构建产物**，见 ADR-007                                    |
| 基准测试（dev）      | `criterion`            | 0.8.2     | Apache-2.0 OR MIT  | P5 延迟实测                                                                |
| 属性测试（dev）      | `proptest`             | 1.11      | MIT OR Apache-2.0  | 分词一致性、快照 round-trip                                                    |
| **依赖合规（工具）**   | **`cargo-deny`**       | 0.20.2    | MIT OR Apache-2.0  | License 白名单强制校验，见 ADR-008                                              |

**不引入**：`arroy`（耦合 LMDB，与内存+快照冲突）、`redb` / `sled` / `heed`（持久化路线为快照）、`dashmap`（7.x 仍 rc，v1 单写者 + `RwLock` 足够）、`stop-words`（词表不值得一个依赖，且中文需自维护）、`lindera`（词典数十 MB）、`axum` / `tokio`（不做服务层）。完整评估见 `thirdparty.md`。

### 9.2 备选方案对比

**向量索引**

> 2026-09-02 复核后新增 `hnsw_rs` 与 `usearch` 两个候选，并修正 `arroy` 的版本与结论依据。

| 候选                 | 增量 insert | 删除    | 查询时过滤                   | License            | 依赖            | 结论                                              |
| ------------------ | --------- | ----- | ----------------------- | ------------------ | ------------- | ----------------------------------------------- |
| `instant-distance` 0.6.1 | ❌ 无 insert API | ❌ | ❌                  | MIT OR Apache-2.0  | 纯 Rust，无 C 依赖 | ✅ **第一版选用**（最轻）。用主索引 + delta 区绕开限制，**P4 复核** |
| **`hnsw_rs`** 0.3.4 | **✅**     | ❌     | **✅ `search_filter`**   | **MIT/Apache-2.0** | **纯 Rust**    | ⏳ **P4 A/B 原型**。达标则取代前者并删除 delta 区；`DistDot` 要求入库前归一化，与 7.3 方案天然契合 |
| `usearch` 2.26.2   | ✅         | **✅** | ✅ `filtered_search`     | Apache-2.0         | ❌ C++（`cxx`）  | ⏳ P6 备选。仅当**物理删除**成为硬需求时引入（前两者均不支持）             |
| `arroy` 0.8.0      | ✅         | ✅     | ✅                       | MIT                | ❌ LMDB（`heed`） | ❌ 与"内存 + 快照"路线冲突                                |
| 自研 HNSW            | ✅ 可控      | ✅     | ✅                       | —                  | 无             | ❌ 第一版不做。待内核稳定后作为 P6 优化项                         |

**BM25 实现**：见 ADR-001。

**Embedding**：见 ADR-003。

### 9.3 Feature Flags

```toml
[features]
default       = ["local-embed"]
local-embed   = ["dep:fastembed"]         # 本地 ONNX 推理
remote-embed  = ["dep:reqwest", "dep:tokio"]  # 远程 HTTP API
positions     = []                        # posting 存储位置信息
```

`local-embed` 与 `remote-embed` 可同时开启，运行时通过配置选择；都不开启时 `Embedder` 无可用实现，编译期即暴露（见 11.3 `NoEmbedder`）。

### 9.4 决策记录

#### ADR-001：BM25 倒排索引手写，不用 tantivy

- **状态**：已接受
- **背景**：需要 BM25 关键词检索（FR-03、FR-04）。tantivy 是成熟的工业级全文检索库。
- **决策**：手写分词 → 倒排表 → postings → BM25 打分 → 检索的完整链路，只用 `jieba-rs` 处理分词。
- **理由**：① 项目目标包含"把检索链路每个环节写一遍"（G2 的认知目的）；② 中文场景的 `avgdl` 与参数调优需要完全可控；③ 字段权重、查询语法扩展自由。
- **代价**：需自己处理段合并、posting 压缩等工程问题；MVP 阶段不做（见 7.1、7.6）。
- **相关需求**：FR-02、FR-03、FR-04

#### ADR-002：向量索引用 instant-distance，以主索引 + delta 绕开无 insert API

- **状态**：已接受
- **背景**：需要 HNSW 向量索引（FR-08）与增量写入（FR-17），但 `instant-distance` 的 `build()` 一次性消费 points，无增量插入。
- **决策**：选用 `instant-distance`，用「不可变主 HNSW + 暴力 delta 区」支持增量；delta 达 1000 条或总量 10% 时后台重建。
- **理由**：纯 Rust 无 C 依赖、Apache-2.0、rayon 并行构建、`with-serde` 支持序列化。增量限制可用工程手段绕过，代价可控（见 7.5）。
- **代价**：delta 区为线性扫描；重建期间有额外内存占用。
- **相关需求**：FR-08、FR-09、FR-17

#### ADR-003：Embedding 默认本地 fastembed，远程作 feature

- **状态**：已接受
- **背景**：需要中文 embedding（FR-05、FR-06），可选远程 API（FR-07）。
- **决策**：`Embedder` trait 抽象；默认本地 `fastembed` + `BAAI/bge-small-zh-v1.5`（512 维）；远程 HTTP 实现置于 `remote-embed` feature 之后。
- **理由**：本地方案无需联网与 API Key，demo 可离线运行；`fastembed` 基于 ort、同步无 tokio 依赖，不污染运行时。
- **代价**：首次运行需下载 ONNX 模型（国内可能失败，对应风险 R3）；CPU 推理是主要延迟源。
- **相关需求**：FR-05、FR-06、FR-07

#### ADR-004：融合默认 RRF（k=60），加权归一仅作备选

- **状态**：已接受
- **背景**：需要把 BM25 与向量两路结果合并（FR-10、FR-11）。
- **决策**：默认 RRF，`k=60`，两路权重均为 1.0；加权归一作为可选策略实现，采用「除以 lane Top-1 分数」而非 min-max。
- **理由**：两路量纲不可比（BM25 无上界 vs 余弦集中在 [0.6, 0.95]），归一化既损失信息又不稳定；RRF 只用排名，免疫量纲。
- **代价**：RRF 丢弃了分数绝对大小信息，无法表达"这一路明显更可信"。需要时由权重 `w_i` 调节。
- **相关需求**：FR-10、FR-11

#### ADR-005：持久化用内存索引 + 自研 bincode 快照

- **状态**：已接受
- **背景**：需要快照 save/load（FR-16），且需求文档 8.1 明确不引入嵌入式 KV。
- **决策**：平时全内存，save/load 到自定义二进制格式（magic + version + crc32 + 若干 bincode section）。
- **理由**：① 秒级加载，满足 NFR-04；② 零额外依赖；③ bincode 版本无外部约束（`instant-distance` 的 `with-serde` 只含 serde，不含 bincode）。
- **修正（2026-09-02，P0 实测）**：① 初版称"bincode 1.3 与 instant-distance 内部版本对齐"属事实错误；② 中间曾改选 3.0.0，但 **3.0.0 是玩笑发布**（源码仅一行 `compile_error!`），最终定为 **`bincode 2.0.1`**。P4 末与 `rkyv` 实测对比后再定稿。
- **代价**：不支持单条文档的持久化更新，必须整体重写快照（MVP 规模可接受）。
- **相关需求**：FR-16

#### ADR-006：BM25 检索用 TAAT 而非 DAAT/WAND

- **状态**：已接受
- **背景**：需要实现 BM25 检索执行（FR-04）。
- **决策**：TAAT（Term-At-A-Time）——逐个 term 遍历 posting 列表，累加分数到 `HashMap<ChunkId, f32>`，最后取 Top-K。
- **理由**：万级规模下实现简单、缓存友好，且天然支持 OR 语义（FR-04 的硬要求）。
- **代价**：长尾 term 的 posting 长时无剪枝，无法提前终止。DAAT（WAND / Block-Max WAND）列为 P4 优化项。
- **相关需求**：FR-04

#### ADR-007：`tantivy` 作为 dev 依赖的 BM25 正确性基线

- **状态**：已接受（2026-09-02 新增）
- **背景**：手写 BM25 的最大风险不是写不出来，而是**实现有偏差但看不出来**。现有防护是"手算 5 篇文档比对"，覆盖率太低（覆盖不到空 query、全停用词 query、超长 query、df=0 等边界）。
- **决策**：`tantivy` 0.26.1（MIT）**仅作为 dev-dependency 引入**，用于：① P1 阶段建同语料索引，逐条比对 BM25 排序；② P5 阶段作为第三路基线加入对照实验。**不参与构建产物**。
- **理由**：① 工业实现是最现成的 oracle，成本极低而价值极高；② 仅 dev 依赖，不影响分发与 License；③ 不违背 ADR-001——不用它**实现功能**，只用它**验证实现**。
- **代价**：多 50+ 个 dev 依赖，拉长 `cargo test` 的首次编译时间；需容忍 `k1`/`b` 与平均长度口径差异（排序可比，分值不必全等）。
- **验收**：自研 BM25 与 tantivy 在同一评测集上的 NDCG@10 差距**不得超过 5%**，否则必须查清原因再发布 P5 结论。
- **相关需求**：FR-03、FR-04；对应 plan.md T1-15、T5-04

#### ADR-008：MSRV 提到 1.90，并用 `cargo-deny` 强制 License 白名单

- **状态**：已接受（2026-09-02 新增）
- **背景**：① NFR-08 原定 Rust 1.80+，但实测主流依赖的 MSRV 已突破：`bincode` 3.0.0 = 1.85、`clap` 4.6 = 1.85、`smol_str` = 1.89、`criterion` 0.8 = 1.86、`roaring` = 1.90；坚守 1.80 意味着全面锁旧版本。② 项目要求依赖 License 友好，但传递依赖可达数十个，人工审查 `Cargo.toml` 不可靠。
- **决策**：① MSRV 定为 **1.90+**，并在 `rust-toolchain.toml` 固定；② 引入 `cargo-deny` 0.20.2（MIT OR Apache-2.0），配置白名单 `MIT / Apache-2.0 / BSD-3-Clause / BSL-1.0 / CC0-1.0 / Zlib / ISC / Unicode-DFS-2016`，出现其他 License（尤其 GPL 系列）直接失败。
- **理由**：① 1.90 覆盖除 `croaring`（1.95，已排除）外的所有候选依赖；当前 stable 为 1.98.0，1.90 对使用者不算苛刻；② 合规检查自动化，避免"某个传递依赖换成 GPL 而无人发现"。
- **代价**：要求使用者的工具链 ≥ 1.90。
- **相关需求**：NFR-08；对应 plan.md T0-03、T0-06

#### ADR-009：对外接口采用「门面层 + 共用编排内核」，而非重写检索路径

- **状态**：已接受（2026-09-04 新增，依据 `p6-design.md` v2.0 决策点 D-I1~D-I8）
- **背景**：issue #1 要求把接口重构为 `index.add(doc)` / `searcher.search(query)`。现状 `Searcher<'a>` 借 4 个外部引用无法存 struct，最小闭环 79 行含 3 个静默陷阱；但它的编排逻辑（两路召回→融合→回捞→精排）已稳定并被评测验证，**不应重写**。
- **决策**：新增 `SearchIndex`（写端）+ owned `Searcher`（读端，`'static + Clone + Send + Sync`）门面；把原 `Searcher<'a>` 的编排抽成自由函数 `search_parts`，旧类型改名 `QueryExecutor<'a>`（签名不变）作底层逃生舱。门面只做装配与生命周期管理，不实现算法。
- **理由**：① 新旧共用同一 `search_parts`，避免两份编排逻辑漂移；② 重构是"加法"，bench / 集成测试 / 单测零改动，评测可复现性不受威胁；③ 逃生舱保证现有可调能力（brute 后端、BM25/RRF 网格、charabia、裸 embedder）一个不丢。
- **代价**：`Document` 拆为输入 DTO + `DocRecord`（breaking）；快照升 `FORMAT_VERSION` 2 存配置指纹（旧 `.idx` 需重建）；`raw_vectors` 使向量内存翻倍（NFR-05 重测）。
- **相关需求**：FR-12 / FR-14 / FR-15 / FR-17 / NFR-03 / NFR-05；对应 plan.md P6（I-01~I-14）

---

## 10. 对外接口设计（门面层）

> 架构级概要。任务级拆分见 `plan.md` 第 11 章；完整设计（业界调研、Before/After、决策点 D-I1~D-I8）见 `p6-design.md`（v2.0 已拍板）。

### 10.1 设计动机

对外接口重构的三个动因（均实测，见 `p6-design.md` 第 1 章）：

1. **静默正确性陷阱**：现状最小闭环 79 行、手接 8 对象、7 步，其中 3 步写错不报错只变差——分词器不一致（R4）、忘算 `content_hash`（FR-15 幂等失效）、忘 `NormalizedVector`（余弦分全错）。这与 11.3 节「配置错误尽早暴露」的原则直接矛盾。
2. **生命周期逼用户当库作者**：`Searcher<'a>` 借 4 个外部引用，无法存 struct / 无法跨线程；仓库自己的 CLI 与 bench 都手搓了「引擎对象」。
3. **批量 embed 不可见**：向量化是批接口，逐条 `add` 会打穿 NFR-03。

门面层的修复方式：把易错步骤移进库「只做对一次」，把生命周期收进 owned 对象，把批量 embed 收进写缓冲。

### 10.2 目标 API 形态

```rust
use helix_core::prelude::*;

let mut index = SearchIndex::builder().build()?;   // 零配置可用
index.add(Document::new("文本").with_source("src").with_metadata(json!({})))?;
index.add("纯文本")?;                               // impl Into<Document> for &str
index.commit()?;                                    // 刷写缓冲，此后可见

let searcher = index.into_searcher();               // 'static + Clone + Send + Sync
let resp = searcher.search("查询")?;                // 只有 query 必选；mode=Hybrid(自动)、top_n=10
let resp = searcher.search_with("查询")
    .mode(SearchMode::Hybrid)   // 也接受 .mode("hybrid")
    .top_n(10)
    .filter(&Filter::eq("lang", "zh"))              // FR-14
    .explain(true)                                  // FR-13
    .exec()?;
```

### 10.3 写端 `SearchIndex`

| 方法 | 说明 |
| --- | --- |
| `SearchIndex::builder()` → `SearchIndexBuilder` | 全部配置点（analyzer / chunker / embedder / fusion / reranker / bm25 / batch_size）；**不调即用默认值** |
| `add(doc) -> Result<AddOutcome>` | 分块 → 倒排 → 写缓冲；返回 `{ doc_id, chunk_ids, deduped }` |
| `add_documents(iter)` | 批量路径（吞吐最优，`helix build` 走这条） |
| `remove(doc_id)` | 墓碑删除 + 统计量回滚 |
| `commit()` | 刷写缓冲（批量 embed + 灌向量索引），此后数据可见（对齐 Lucene） |
| `into_searcher()` | 交出所有权，产出 owned `Searcher`（隐含 flush） |
| `search(&mut self, q)` | 便利法：内部 `commit()` 后立即检索 |
| `save(path)` / `load(path)` | 落盘 / 加载（含配置指纹校验，见 10.6） |

**写缓冲与批量 embed**：`Embedder::embed_documents` 是批接口，`add` 逐条 embed 会打穿 NFR-03。因此 `add` 只把 chunk 文本 push 进 `pending`，满 `batch_size`（默认 64，**实测校准** 32/64/128/256）时同步 flush。L2 归一化由 flush 路径内部保证。

**可见性语义**：`add` 后必须 `commit()` 才对检索可见（对齐 Lucene/tantivy），为 P7 的 delta 分段留出语义空间。

**所有权切换**：`into_searcher()` / `into_index()` 只移动 `Arc<Inner>`，零拷贝零锁。⚠️ 若 `Searcher` 被 `clone()` 后再 `into_index().add()`，`Arc::make_mut` 会静默深拷贝整个 `Inner`（快照隔离语义正确，但非零拷贝）——`add` 内加 `debug_assert!(Arc::strong_count == 1)` 提示。

### 10.4 读端 `Searcher`

| 方法 | 说明 |
| --- | --- |
| `search(query)` | 主形态：**只有 query 必选**；`top_n=10`，`mode` 按装配自动推断（有向量→Hybrid、无→Lexical） |
| `search_with(query)` → `SearchRequest` | builder：`.mode(m)` `.top_n(n)` `.filter(&f)` `.explain(b)` `.exec()` |
| `into_index()` | 换回写端继续增量写入 |
| `config_report()` | 打印 analyzer / chunker / embedder 实际取值（可观测性，对应风险 R7） |

`Searcher: 'static + Clone + Send + Sync`（六 trait 均已 `Send + Sync`），可直接放进 axum 的 `AppState`、`Arc<Searcher>` 分发、`move` 进 rayon。`SearchIndex` 本身是 `!Sync`（含可变 `pending`），可 `Send` 不可共享。

### 10.5 Document 输入 DTO 与数据模型

`Document`（对外输入）与 `DocRecord`（内部存储）拆分，见 5.1。要点：

- `content_hash = xxh64(dedup_key.unwrap_or(text))`——`dedup_key` 存在时以它为准，是**替代**非并存
- `dedup_key` 持久化进 `DocRecord`，remove→re-add 幂等判定不变
- 同一逻辑文档应始终用同一 `dedup_key`（时有时无判为不同文档，是调用方责任）

### 10.6 持久化与配置指纹

`SearchIndex::load(path)` 校验快照中的 `ConfigFingerprint { analyzer_id, embedder_id, dim, chunker }` 与当前装配是否一致，不一致 → `Error::ConfigMismatch { expected, actual }`（**双面打印**，绝不静默）。这修掉了 `helix search --index` 写死 `MixedAnalyzer` 导致的静默换分词器（R4）隐患。

快照 `FORMAT_VERSION` 升 2；`positions` feature 通过 `effective_version()` 对 base +1（base 2 / positions 3）——**这是把 `codec.rs` 里「从未实现的注释约定」落成实现**。

### 10.7 逃生舱：底层永远可达

门面是**默认装配，不是唯一路径**。底层 `QueryExecutor<'a>`（旧 `Searcher`）原样保留，需要完全自定义 lane 组装（brute 后端、BM25/RRF 网格、裸 embedder、`--runs` 重建图）时直接用它。完整逃生舱映射见 `p6-design.md` 7.3。

---

## 11. 内部接口与 CLI

### 11.1 底层 API（trait 与逃生舱）

> 门面层（第 10 章）是默认路径；以下底层 trait 装配 API 保留作逃生舱（换 brute 后端、BM25/RRF 网格、charabia 对照等，完整映射见 `p6-design.md` 7.3）。底层 `Searcher<'a>` 已改名 `QueryExecutor<'a>`。

```rust
use helix_core::prelude::*;

// ---- 构建索引 ----
let analyzer = MixedAnalyzer::new()?;
let embedder = LocalEmbedder::new(EmbeddingModel::BGESmallZHV15)?;  // fastembed 6.x 变体名（非 BGESmallZH）

let mut index = Index::builder()
    .analyzer(analyzer)
    .embedder(embedder)
    .build()?;

for doc in documents {
    index.upsert(doc)?;   // 幂等，按 content_hash 去重（FR-15）
}
index.commit()?;          // 合并 delta，构建 HNSW（FR-17）

// ---- 持久化 ----
index.save_to_path("index.helix")?;
let index = Index::load_from_path("index.helix")?;   // 秒级加载（FR-16）

// ---- 检索 ----
let searcher = Searcher::new(index)
    .fusion(RrfFusion::new(60.0))
    .reranker(NoOpReranker);          // v1 留位（FR-18）

let resp = searcher.search(SearchRequest {
    query: "Rust 里怎么实现支持中文的 BM25？",
    k: 10,
    mode: SearchMode::Hybrid,          // Bm25 | Vector | Hybrid
    filter: Some(Filter::eq("lang", "zh")),
})?;

for hit in &resp.hits {
    println!("{:.4} [{}] {}", hit.score, hit.source, hit.explain.matched_terms.join(","));
    println!("{}", hit.to_context_block());   // 直接拼进 prompt（FR-12）
}

if let Some(reason) = &resp.empty_reason {
    eprintln!("空结果原因：{:?}", reason);   // Agent 据此决策（FR-13）
}
```

### 11.2 CLI

```bash
# 建索引
helix build --input data/corpus.jsonl --output index.helix

# 检索（三种模式）
helix search --index index.helix --mode hybrid -k 10 "查询文本"
helix search --index index.helix --mode bm25   -k 10 "查询文本"
helix search --index index.helix --mode vector -k 10 "查询文本"

# 对比三种模式的效果（调试用，最常用）
helix compare --index index.helix -k 10 "查询文本"

# 可解释性输出（FR-13）
helix search --index index.helix --mode hybrid --explain "查询文本"

# 评测（P5）
helix bench --index index.helix --queries data/t2-queries.jsonl
```

### 11.3 错误类型

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("序列化错误: {0}")]
    Codec(#[from] bincode::Error),

    #[error("快照版本不兼容: 文件版本 {found}, 支持版本 {expected}")]
    SnapshotVersionMismatch { found: u32, expected: u32 },

    #[error("配置不匹配: 快照指纹 {expected}, 当前装配 {actual}")]
    ConfigMismatch { expected: String, actual: String },   // P6 新增：load 时校验配置指纹（10.6）

    #[error("快照校验失败，文件可能损坏")]
    SnapshotCorrupted,

    #[error("向量维度不匹配: 期望 {expected}, 实际 {found}")]
    DimensionMismatch { expected: usize, found: usize },

    #[error("嵌入模型错误: {0}")]
    Embedding(String),

    #[error("未启用任何 Embedder 实现，请开启 local-embed 或 remote-embed feature")]
    NoEmbedder,

    #[error("分词错误: {0}")]
    Analyze(String),
}
```

> `NoEmbedder` 与 `DimensionMismatch` 是刻意设计的**运行期早期失败**：把配置错误在第一时间暴露，而不是让错误向量静默进入索引——后者会表现为"检索效果莫名其妙地差"，排查成本极高。

---

## 12. 实施计划与需求映射

### 12.1 阶段划分

| 阶段         | 内容                                                             | 验收标准                                                 | 依赖 |
| ---------- | -------------------------------------------------------------- | ---------------------------------------------------- | -- |
| **P0**     | 工程骨架 + 安装 Rust 工具链 + 验证依赖可获取性                                  | `cargo build` 通过空工程；`instant-distance` / `fastembed` / `jieba-rs` 均可拉取编译 | —  |
| **P1**     | `analyze` + `index` + `retriever/bm25` + 单测                    | CLI `helix search --mode bm25` 在示例语料上出结果；BM25 分值与手算值一致 | P0 |
| **P2**     | `embed`（本地）+ `vector`（HNSW）+ `retriever/vector`                | CLI 向量路出结果；同义改写能被召回                                  | P1 |
| **P3**     | `fusion`（RRF + 加权）+ `query/searcher` + `explain`               | `helix compare` 可对比三路；Explain 输出完整                     | P2 |
| **P4**     | `storage` 快照 + 增量写入（**先 `hnsw_rs` A/B 再定案**）+ 元数据过滤                | save/load 后检索结果完全一致；增量写入后立即可查                        | P3 |
| **P5**     | T2Ranking 评测集装配 + `helix bench` + README                        | Recall@K / MRR@10 / 分级 NDCG@10 / 延迟有实测数据；BM25 参数网格搜索调参 | P4 |
| **P6**     | **接口重构**：`SearchIndex` / `Searcher` 门面 + 写缓冲 commit + 配置指纹（issue #1，见第 10 章） | 最小闭环 ≤15 行；brute 后端新旧逐位一致；CLI 参数输出一字不改 | P5 |
| **P7**（v2） | Reranker 接入（bge-reranker-v2-m3）、MMR 去重、token budget 裁剪、自研 HNSW、delta 分段（FR-17 完整版） | —                                                    | P6 |

**关键路径**：P1 → P2 → P3。这三步完成即具备完整检索能力，P4/P5 是工程化与验证。

### 12.2 阶段 → 需求覆盖矩阵

| 需求    | 阶段           | 需求    | 阶段          |
| ----- | ------------ | ----- | ----------- |
| FR-01 | P1           | FR-14 | P4          |
| FR-02 | P1           | FR-15 | P4          |
| FR-03 | P1           | FR-16 | P4          |
| FR-04 | P1           | FR-17 | P4          |
| FR-05 | P2           | FR-18 | P3          |
| FR-06 | P2           | FR-19 | P2          |
| FR-07 | P2（可选 feature） | FR-20 | P3       |
| FR-08 | P2           | FR-21 | v2（P7）      |
| FR-09 | P2           | FR-22 | P1，随阶段演进   |
| FR-10 | P3           | FR-23 | P1，P5 完善   |
| FR-11 | P3           | FR-24 | v2（P7）      |
| FR-12 | P3           | FR-25 | v2（P7）      |
| FR-13 | P3           | —     | —           |

| NFR     | 验证阶段 |
| ------- | ---- |
| NFR-01  | P5   |
| NFR-02  | P5   |
| NFR-03  | P5   |
| NFR-04  | P4   |
| NFR-05  | P5   |
| NFR-06  | P3（单测） |
| NFR-07  | P3   |
| NFR-08  | P0   |
| NFR-09  | P0（`make deny`），每个阶段末复查 |

> 覆盖矩阵的作用是**反向检查**：每一行需求都必须有归属阶段，否则就是漏做。

---

## 13. 测试策略

### 13.1 单元测试（必须有）

| 测试               | 断言                                                        |
| ---------------- | --------------------------------------------------------- |
| BM25 公式正确性       | 构造 5 篇已知文档，手工计算期望分值，逐条比对                                  |
| **索引侧/查询侧分词一致性** | 对同一文本，`analyze_doc` 与 `analyze_query` 输出的 term 序列完全相同     |
| RRF 融合排序         | 构造两路已知 rank，验证融合后顺序与 `w/(k+rank)` 累加一致                    |
| 相似度对齐            | 归一化向量经 `cos = 1 - d²/2` 换算后，与直接点积结果误差 < 1e-5              |
| 快照 round-trip    | save → load 后，全部统计量、postings、检索结果完全一致                     |
| 幂等 upsert        | 同一文档 upsert 两次，chunk 数不翻倍，检索结果不变                          |
| 删除后统计量           | 删除 chunk 后 `df` / `total_len` / `num_chunks` 与全量重建的结果一致   |
| 确定性              | 同一 query 连续检索 100 次，结果顺序完全一致                              |
| BGE 前缀           | 断言 `embed_query` 输出含 instruction 前缀（通过 mock 或对比不同输入的向量差异） |
| **BM25 与 tantivy 对照**（dev 基线） | 同语料下自研实现与 `tantivy` 的 Top-K **排序**一致率 ≥ 95%（分值允许口径差异）；覆盖边界：空 query、全停用词 query、超长 query、`df = 0` 的 term。见 ADR-007 |

> **"删除后统计量" 与 "BM25 手算值" 这两个测试最关键**：增量维护的统计量漂移和公式实现偏差，都是不会报错、只会让效果慢慢变差的问题，必须靠测试兜住。
>
> **tantivy 对照测试是前三者的补强**：手算只能验证 5 个样本，对照测试能覆盖整份语料与边界情况。三者叠加才敢说"BM25 是对的"。

### 13.2 集成测试

- 端到端：摄入 → 提交 → 快照 → 加载 → 检索，结果一致
- 并发：多线程并发检索（验证 `Send + Sync` 与 `Search` 对象复用安全）

> 效果评测（指标定义、对照实验、数据集要求）见 `requirements-spec.md` 第 9 章，本架构文档不重复定义。

---

## 14. 技术风险与应对

| #  | 风险                                          | 影响                                 | 应对措施                                                                  |
| -- | ------------------------------------------- | ---------------------------------- | --------------------------------------------------------------------- |
| R1 | **`instant-distance` 无增量插入 API**            | 增量写入需全量重建                          | ① 主索引 + delta 暴力区（7.5）；② **P4 用 `hnsw_rs`（纯 Rust、原生增量 insert）做 A/B，达标则换选型、删除 delta 区，风险随之消除**；③ 需物理删除时走 `usearch`（引入 C++ 依赖） |
| R2 | **BGE 查询侧前缀遗漏**                             | 向量检索效果显著下降，且**极难自查**（代码能跑、分数看着也正常） | `Embedder` 强制区分 `embed_query` / `embed_documents`；写单测断言；README 显著位置标注 |
| R3 | **fastembed 首次下载模型失败/极慢**（国内访问 HuggingFace） | 阻塞 P2 阶段                           | ① 支持 `HF_ENDPOINT` 镜像环境变量；② 支持指定本地模型目录；③ `remote-embed` 作为兜底路径        |
| R4 | **索引侧/查询侧分词不一致**                            | 召回率莫名偏低，排查成本极高                     | 默认实现强制 `analyze_query → analyze_doc`；单测断言两侧一致                         |
| R5 | **BM25 增量统计量漂移**                            | df / avgdl 逐渐失真，效果缓慢劣化，**不会报错**    | 增量累加 + 墓碑；单测对比"增量删除后"与"全量重建"的统计量是否一致                                  |
| R6 | **中文 avgdl 偏小导致英文经验参数失效**                   | BM25 排序质量不佳                        | P5 阶段网格搜索调参，不盲信 k1=1.2 / b=0.75                                       |
| R7 | **RRF 的 k 值选择**                             | 融合效果次优                             | k=60 为论文推荐值且鲁棒；若效果不佳，在 {20, 60, 100} 中对比                              |
| R8 | **OR 语义下长 query 噪声稀释**                      | Agent 的长问句中虚词干扰排序                  | BM25 的 idf 天然抑制高频虚词；若仍不足，v2 增加 query term 裁剪（按 idf 阈值）                |
| R9 | **`fastembed` 精确锁定 `ort =2.0.0-rc.13`（预发布）** | 依赖树不可浮动；ort 2.0 正式版 API 变动需等上游跟进   | ① **`Cargo.lock` 必须入库**（plan.md T0-03）；② 5.x 与 6.x 依赖相同，换版本无法规避；③ 极端情况下走兜底路径：直接用 `ort 1.16.3` + `tokenizers` 自研 BGE 推理（约 150 行） |
| R10 | **传递依赖 License 漂移**                       | 某个传递依赖换成 GPL 系列而无人发现，破坏"License 友好"约束 | 引入 `cargo-deny` 白名单校验并接入 CI/本地 `make deny`（ADR-008）                    |

> 项目级风险（工具链未安装、语料与标注依赖、模型域不匹配）见 `requirements-spec.md` 第 8、9 章。

---

## 15. 附录

### 15.1 参考资料

- Robertson & Zaragoza, *The Probabilistic Relevance Framework: BM25 and Beyond*, 2009 — BM25 理论基础
- Malkov & Yashunin, *Efficient and Robust Approximate Nearest Neighbor Search Using HNSW*, 2016 — HNSW 原论文
- Cormack, Clarke & Büttcher, *Reciprocal Rank Fusion Outperforms Condorcet and Individual Rank Learning Methods*, SIGIR 2009 — RRF 与 k=60 的出处
- BAAI/bge-small-zh-v1.5 模型卡 — 查询侧 instruction 前缀的权威说明

### 15.2 变更记录

| 版本   | 日期         | 变更                                                                     |
| ---- | ---------- | ---------------------------------------------------------------------- |
| v1.0 | 2026-09-02 | 由 `archive/requirements-and-design_v1.0.md` v1.0 拆分而来。承接第 5、6、7、8、9、10、11.1、11.2、12 章内容；新增「文档信息」「架构概述与关键决策」「质量属性设计」「ADR 决策记录」「实施计划与需求映射」五节 |
| v1.2 | 2026-09-02 | **P0 执行后回写**（详见 `p0-design.md` 第 12 章）：① 序列化定为 **`bincode 2.0.1`**（3.0.0 为玩笑发布）；② `moka` 需显式启用 `sync` feature；③ `fastembed` 必须 `default-features = false` 以移除 NCSA 依赖链；④ 模型实际来源为 `Xenova/bge-small-zh-v1.5`（非 Qdrant）；⑤ 枚举变体确认为 `BGESmallZHV15`（非 `BGESmallZH`） |
| v1.1 | 2026-09-02 | 依据 `thirdparty.md` 调研结论回写：① 新增 ADR-007（tantivy 作 dev 基线）、ADR-008（MSRV 1.90 + cargo-deny）；② 7.1 引入 `unicode-segmentation` / `unicode-normalization`，不再手写 Unicode 分段；③ 7.5 补充 `hnsw_rs` A/B 复核路径；④ 7.6 修正 bincode 选型理由（instant-distance 不含 bincode）并新增 `rkyv` A/B 取舍规则；⑤ 8.1 缓存由 `lru` 改为 `moka`；⑥ 9.1/9.2 版本与候选同步至实测值（fastembed 6.0.2、arroy 0.8.0、新增 hnsw_rs/usearch）；⑦ 12.1 新增 tantivy 对照测试；⑧ 13 更新 R1、新增 R9/R10 |
| v1.4 | 2026-09-04 | 依据 `p6-design.md` v2.0（issue #1 接口重构，决策点 D-I1~D-I8 已拍板）：① 新增**第 10 章「对外接口设计（门面层）」**，原 10~14 章顺延为 11~15；② 新增 ADR-009（门面层 + 共用编排内核）；③ 5.1 `Document` 拆为输入 DTO + `DocRecord`；④ 4.2 补门面层边界；⑤ 11.3 错误类型新增 `ConfigMismatch`；⑥ 阶段表 P6 重定义为接口重构、原 v2 顺延 P7。详细设计见 `p6-design.md`，任务级见 `plan.md` |
| v1.3 | 2026-09-03 | 依据 `p5-design.md` v1.2（评测数据源切换 T2Ranking）同步：① 4.x 目录树 data/ 注释更新；② 10.2 CLI 示例改 `data/t2-queries.jsonl`；③ 11 阶段门槛 P5 行更新（T2Ranking 装配 + 分级 NDCG）。评测数据集的**需求定义**见需求文档 9.4（v1.2），本文档不复制 |
