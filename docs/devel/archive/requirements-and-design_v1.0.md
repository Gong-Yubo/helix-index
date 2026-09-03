# index-demo 需求分析与设计文档

> ⚠️ **历史文档，正文保留原名未改写**：项目已于 2026-09-03 正式定名 **HelixIndex**
> （库 crate `index-core` → `helix-core`，CLI 命令 `idx` → `helix`）。
> 文中出现的旧名均指改名前的同一项目，未逐处改写以保留历史原貌（D-E4）。

### —— 面向 Agent 场景的通用检索引擎内核

| 项目   | 内容                   |
| ---- | -------------------- |
| 文档版本 | v1.0                 |
| 创建日期 | 2026-09-01           |
| 状态   | 待评审                  |
| 项目定位 | 通用检索内核（场景层后接），单机 MVP |
| 技术栈  | Rust                 |

---

## 1. 项目背景与目标

### 1.1 背景

Agent 应用（RAG 知识库、长期记忆、代码助手）的核心瓶颈往往不在大模型本身，而在**检索**：召回错了，后面生成得再好也是幻觉。市面上现成的检索方案（Elasticsearch、Milvus、各类向量库）各有定位，但它们要么是给人用的搜索引擎，要么是纯向量数据库，缺少一层专门针对 **Agent 作为调用方** 而设计的内核。

本项目从零构建一个检索内核，目的有二：

1. **工程目的**：得到一套可控、可插拔、可演进的检索内核，供上层各类 Agent 场景接入。
2. **认知目的**：把检索链路（分词 → 索引 → 召回 → 融合 → 排序）的每一个环节都写一遍，掌握真实的调优抓手，而不是停留在调库的层面。

### 1.2 项目目标

| 目标 | 说明                                               |
| -- | ------------------------------------------------ |
| G1 | 实现 BM25 关键词检索与向量语义检索双路召回，并完成融合                   |
| G2 | 内核全部抽象为 trait，分词器 / 嵌入模型 / 向量索引 / 融合策略 / 精排器均可替换 |
| G3 | 接口设计上充分照顾 Agent 调用方的特殊性（见第 3 章）                  |
| G4 | 单机万级文档下跑通全链路，效果与延迟可量化                            |
| G5 | 架构为后续扩展（Rerank、增量、分布式）留出明确位置                     |

### 1.3 成功标准

- [ ] 在示例中文语料上，混合检索的 Recall@10 与 MRR@10 **优于** 单独的 BM25 与单独的向量检索
- [ ] 单次混合检索 P99 延迟（不含模型冷启动）< 20ms @ 1 万 chunk
- [ ] 索引快照加载时间 < 2s，显著优于重建索引
- [ ] 全部核心 trait 均有 ≥2 个实现或明确的替换路径

---

## 2. 术语表

| 术语            | 含义                                          |
| ------------- | ------------------------------------------- |
| Document      | 摄入的原始文档，一个文档可切分为多个 Chunk                    |
| Chunk         | 检索与返回的最小单位，通常 200~500 字                     |
| Term          | 分词后归一化得到的索引词                                |
| Posting       | 倒排表中一条记录：`(chunk_id, term_freq, positions)` |
| 倒排索引          | Term → Posting 列表 的映射                       |
| 正排存储          | ChunkId → 原文、元数据 的映射                        |
| df            | document frequency，包含某 Term 的 Chunk 数量      |
| avgdl         | 平均 Chunk 长度（以 Term 数计）                      |
| 召回（Retrieval） | 从全量中快速找出候选集，重速度                             |
| 精排（Rerank）    | 对候选集精细打分重排，重精度                              |
| 融合（Fusion）    | 把多路召回结果合并为统一排序                              |
| RRF           | Reciprocal Rank Fusion，倒数排名融合               |
| Lane          | 一路独立的召回通道（如 BM25 lane、向量 lane）              |

---

## 3. Agent 场景特殊性分析（核心章节）

> 本章是整个项目的立论基础。**如果这个引擎和 Elasticsearch 没有区别，那它就不该存在。**

### 3.1 一个前提性的张力

"通用内核"与"Agent 场景"之间存在张力：通用意味着不做场景假设，Agent 场景意味着要为 Agent 做优化。

本项目的解法是：**把 Agent 场景的特殊性翻译为「接口约束」与「质量属性约束」，而不是翻译为「领域逻辑」。**

具体而言：

- ✅ 内核负责：结构化输出、可解释性、防空召回、低延迟、幂等写入、元数据过滤
- ❌ 内核不负责：知识库的 schema、记忆的时序衰减公式、代码检索的符号关系

这样场景层（知识库 / 记忆 / 代码）才能真正接进来，而不是被内核的假设绑死。

### 3.2 五个根本变化

#### 变化一：查询的发出方从人类变成 LLM

|    | 人类查询                   | Agent 查询                                  |
| -- | ---------------------- | ----------------------------------------- |
| 形态 | 短、关键词化（"rust bm25 实现"） | 完整自然语言问句（"在 Rust 里怎么实现一个支持中文的 BM25 检索？"）  |
| 长度 | 2~5 词                  | 10~40 词，含大量虚词                             |
| 质量 | 意图明确                   | 可能含**指代**（"它的默认值是多少"）、可能含**幻觉词**（模型编造的术语） |

**工程后果**：

- 长 query 直接喂 BM25，虚词会稀释信号
- 含幻觉词时该词 `df = 0`。若用 AND 语义 → **整体零召回**，这是 Agent 场景最典型的翻车方式
- 含指代时，语义向量会漂移，向量路也可能失效

**设计应对**：

- FR-04：BM25 **默认 OR 语义**，禁止 AND。单个 term 未命中只损失该 term 的贡献，不影响整体
- FR-13：空结果必须携带 `EmptyReason`（是库里没文档？还是词全没命中？还是被过滤掉了？），让 Agent 能据此决定"改写 query / 换数据源 / 直接回答"
- FR-21：支持多 query 输入后融合，为后续的 HyDE、multi-query 改写留出接口（内核不做改写，只做承载）

#### 变化二：检索结果是 LLM 的输入，不是给人看的列表

人可以翻页、扫标题、自行判断 relevance；Agent 是 Top-K 直接进 prompt，**一次机会，没有翻页这个交互**。

**工程后果**：

- **前几条的精确率** 比 "有没有涵盖全部相关文档" 更关键
- **重复内容极度有害**：同一句话出现在 3 个 chunk 里 → 浪费 3 倍 token，且污染上下文、干扰注意力
- 结果必须**可溯源**，否则 Agent 无法标注引用，用户无法验证

**设计应对**：

- FR-12：`Hit` 必须携带 `doc_id / source / char_start / char_end / metadata`，并提供 `to_context_block()` 直接产出可拼进 prompt 的文本块
- FR-24（v2）：结果去重，MMR 或同文档相邻 chunk 合并
- FR-25（v2）：按 token budget 裁剪，防止撑爆上下文窗口

#### 变化三：检索位于 Agent 循环的关键路径，且被高频调用

一次用户提问，Agent 可能跑 5~~20 轮，每轮调 1~~3 次检索 → **单次会话几十次调用**。

人类搜索等 200ms 无感；Agent 场景下 50 次 × 100ms = 5s，直接体现在端到端延迟上，用户能明确感知。

**工程后果**：

- **P99 比均值重要**。查询路径上不允许有隐藏的 O(n) 扫描或磁盘 IO
- Embedding 是最大延迟源：本地 bge-small 单条约 5~~15ms，远程 API 则 50~~200ms

**设计应对**：

- 两路召回并行执行（rayon）
- 全内存索引，查询路径零磁盘 IO
- FR-20：query embedding 缓存（Agent 多轮循环中大量 query 是重复的）
- 复用 `instant_distance::Search` 对象，避免每次查询重新分配堆内存
- 内核暴露 `SearchResponse.took`，让上层能观测与告警

#### 变化四：Agent 需要能"自查"检索质量

人看到结果不对会自己改 query。Agent 需要**信号**来判断"这次召回靠谱吗"，才能决定是改写 query、换工具、还是直接回答。

**一个高价值的诊断信号**：BM25 命中但向量路未命中，通常意味着"词汇匹配但语义不相关"（如 query 里出现的词恰好是文档里的高频噪音词）；反之，向量路命中但 BM25 零命中，意味着"语义相关但用词完全不同"。

**设计应对**：

- FR-13：每条 `Hit` 携带 `Explain`，包含各 lane 的 rank 与 score、`matched_terms`、融合分
- 上层可据此构造策略：两路都命中 → 高置信；仅一路命中 → 降低置信或触发重试

#### 变化五：写入模式不同

传统搜索系统的索引由离线管道批量构建。Agent 场景下，Agent 自己会往里写（新记忆、新经验、刚抓取的网页），写入是**零散、持续、与主流程异步**的。

**工程后果**：

- 必须幂等（同一个文档反复灌入不能产生重复结果）
- 增量写入不能阻塞读

**设计应对**：

- FR-15：doc 级 upsert，按 `content_hash` 或 `source` 去重
- FR-17：主索引 + delta 区，delta 异步合并，读不阻塞（详见 7.5）

### 3.3 约束推导汇总

| Agent 场景特征           | 对内核的要求             | 落到需求          | 落到模块                             |
| -------------------- | ------------------ | ------------- | -------------------------------- |
| query 由 LLM 生成，质量不稳定 | OR 语义、防空召回、显式空原因   | FR-04 / FR-13 | `retriever/bm25`、`query/explain` |
| 结果直接进 prompt，无翻页     | 结构化 + 可溯源 + 去重     | FR-12 / FR-24 | `types`、`fusion`                 |
| 高频调用，关键路径            | 低 P99、并行、缓存、零 IO   | FR-19 / FR-20 | `query/searcher`、`embed/cached`  |
| 需要自查检索质量             | 可解释性、双路命中信号        | FR-13         | `query/explain`                  |
| Agent 自己写入           | 幂等 upsert、非阻塞增量    | FR-15 / FR-17 | `index`、`vector`                 |
| 多场景共享内核              | 全 trait 抽象，不内置领域逻辑 | 5.3 节         | 全部                               |
| 需要按 scope 限定范围       | 元数据过滤              | FR-14         | `schema`                         |

---

## 4. 需求范围

### 4.1 功能性需求

优先级采用 MoSCoW：**Must** 第一版必做 / **Should** 第一版尽量做 / **Could** 第二版 / **Won't** 本期不做。

| 编号    | 需求                   | 优先级    | 说明                                       |
| ----- | -------------------- | ------ | ---------------------------------------- |
| FR-01 | 文档摄入与分块              | Must   | 支持按固定长度 + 重叠切分；`Document` → `Vec<Chunk>` |
| FR-02 | 中英混合分词               | Must   | jieba-rs 处理中文，正则/空白处理拉丁文本                |
| FR-03 | 倒排索引构建               | Must   | term dict + postings，手写实现                |
| FR-04 | BM25 检索              | Must   | OR 语义，参数可调                               |
| FR-05 | `Embedder` 抽象        | Must   | trait 定义                                 |
| FR-06 | 本地 embedding 实现      | Must   | fastembed + bge-small-zh-v1.5            |
| FR-07 | 远程 embedding 实现      | Should | HTTP API，feature flag 隔离                 |
| FR-08 | 向量索引                 | Must   | instant-distance HNSW                    |
| FR-09 | 向量检索                 | Must   | Top-K 相似度检索                              |
| FR-10 | RRF 融合               | Must   | 默认融合策略                                   |
| FR-11 | 加权归一融合               | Should | 备选策略，需规避归一化陷阱                            |
| FR-12 | 结构化结果返回              | Must   | 含出处、偏移、元数据、上下文块生成                        |
| FR-13 | 检索可解释性               | Must   | `Explain` + `EmptyReason`                |
| FR-14 | 元数据过滤                | Should | tag 等值匹配 + 数值范围                          |
| FR-15 | 幂等 upsert 与删除        | Must   | content_hash / source 去重；墓碑删除            |
| FR-16 | 索引二进制快照              | Must   | save / load                              |
| FR-17 | 增量写入                 | Must   | 主索引 + delta 区                            |
| FR-18 | `Reranker` 抽象 + NoOp | Must   | **第一版只留位**，不接模型                          |
| FR-19 | 批量摄入并行化              | Should | rayon                                    |
| FR-20 | query embedding 缓存   | Should | LRU                                      |
| FR-21 | 多 query 融合接口         | Could  | 为查询改写留口子                                 |
| FR-22 | CLI                  | Must   | 建索引 / 检索 / 三种模式对比                        |
| FR-23 | 示例中文语料               | Must   | `data/` 下自带小样本                           |
| FR-24 | 结果去重（MMR）            | Could  | v2                                       |
| FR-25 | token budget 裁剪      | Could  | v2                                       |

### 4.2 非功能性需求

| 编号     | 类别   | 指标                                               | 备注                                 |
| ------ | ---- | ------------------------------------------------ | ---------------------------------- |
| NFR-01 | 规模   | 1 万 chunk（MVP 目标），10 万（架构上限）                     | 单机内存                               |
| NFR-02 | 查询延迟 | BM25 < 5ms；向量 < 10ms；混合 P99 < 20ms               | 不含模型冷启动，待实测校准                      |
| NFR-03 | 索引构建 | 1 万 chunk 含 embedding 构建 < 120s                  | CPU 本地推理，待实测                       |
| NFR-04 | 冷启动  | 快照加载 < 2s                                        | 对比重建索引 30s+                        |
| NFR-05 | 内存   | 1 万 chunk 向量部分约 20MB（512 维 × 4B）                 | 倒排另计，待实测                           |
| NFR-06 | 确定性  | 相同 query + 相同索引 → 完全相同结果                         | HNSW seed 固定，tie-break by chunk_id |
| NFR-07 | 可观测  | 每次检索输出 `took`、各 lane 耗时、候选数                      |                                    |
| NFR-08 | 兼容性  | Rust 1.80+（instant-distance 的 MSRV），2021 edition |                                    |

> ⚠️ NFR-02~NFR-05 均为**目标值**，必须在 P5 阶段用实测数据校准，实测前不得作为对外承诺。

### 4.3 明确不做（Out of Scope）

以下内容**第一版明确不做**，避免范围蔓延：

| 不做                                  | 原因                         |
| ----------------------------------- | -------------------------- |
| 分布式 / 分片 / 副本                       | 单机 MVP 优先，架构留位             |
| HTTP / gRPC 服务层                     | 先以库 + CLI 形态交付             |
| PDF / Word / HTML 文档解析              | 属于摄入层，非检索内核职责              |
| 查询改写 / HyDE / 多轮对话管理                | Agent 应用层职责，内核只留多 query 接口 |
| Rerank 模型接入                         | FR-18 只定义 trait + NoOp 实现  |
| 权限 / 多租户隔离                          | v2                         |
| 图检索 / 关系推理                          | 不在本期范围                     |
| posting 压缩（varint / roaring bitmap） | MVP 用朴素 `Vec<u32>`，P4 优化项  |
| 查询语法（布尔、短语、通配符）                     | MVP 只用 OR 语义，位置信息先存好备用     |

---

## 5. 系统架构

### 5.1 整体数据流

**写入路径**

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

**查询路径**

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

### 5.2 模块划分

采用 Cargo workspace，库与 CLI 分离：

```
index-demo/
├── Cargo.toml                 # workspace
├── crates/
│   ├── core/                  # index-core（库，对外交付主体）
│   │   └── src/
│   │       ├── lib.rs          # 顶层 API：Index / Searcher 组装
│   │       ├── error.rs        # Error / Result
│   │       ├── types.rs        # DocId / ChunkId / TermId / Score
│   │       ├── document.rs     # Document / Chunk
│   │       ├── schema.rs       # 字段定义、权重、Filter
│   │       ├── analyze/        # Analyzer trait / 中英混合分词 / 过滤器 / 停用词
│   │       ├── index/          # Index / inverted / posting / forward / stats
│   │       ├── retriever/      # Retriever trait / bm25 / vector
│   │       ├── vector/         # VectorIndex trait / hnsw / brute
│   │       ├── embed/          # Embedder trait / local / remote / cached
│   │       ├── fusion/         # FusionStrategy trait / rrf / weighted
│   │       ├── rerank/         # Reranker trait / noop
│   │       ├── query/          # Searcher / parse / explain
│   │       ├── storage/        # Snapshot 读写 / codec
│   │       └── chunk/          # Chunker 分块策略
│   └── cli/                   # idx（二进制）
├── data/                      # 示例中文语料
├── examples/
└── docs/
```

**模块职责边界**（关键约束）：

| 模块          | 职责            | 禁止                  |
| ----------- | ------------- | ------------------- |
| `analyze`   | 文本 → Token 序列 | 不知道索引的存在            |
| `index`     | 倒排 / 正排 / 统计量 | 不知道融合与排序            |
| `vector`    | 向量存储与近邻检索     | 不知道文本，只认 `Vec<f32>` |
| `embed`     | 文本 → 向量       | 不知道索引的存在            |
| `retriever` | 单路召回          | 不与其他 lane 交互        |
| `fusion`    | 多路合并          | 不回捞正文               |
| `query`     | 编排以上全部        | 不自己实现算法             |

### 5.3 核心抽象

```rust
// ---------- types.rs ----------
pub type DocId   = u32;
pub type ChunkId = u32;
pub type TermId  = u32;
pub type Score   = f32;

pub struct Document {
    pub doc_id: DocId,
    pub source: String,              // 文件路径 / URL / 标题 —— 溯源用
    pub metadata: serde_json::Value, // 业务自定义 —— 过滤用
    pub content_hash: u64,           // 幂等 upsert 用
}

pub struct Chunk {
    pub chunk_id: ChunkId,
    pub doc_id: DocId,
    pub ordinal: u32,       // 在文档内的序号，用于相邻 chunk 合并
    pub text: String,
    pub char_start: usize,  // 原文偏移，用于精确引用
    pub char_end: usize,
}
```

```rust
// ---------- analyze ----------
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

> 拆成两个方法而非一个，是为了**未来允许**查询侧差异化（如同义词扩展、保留停用词）；但默认实现强制一致，从 API 层面杜绝"索引侧和查询侧分词不一致"这类隐蔽 bug。

```rust
// ---------- embed ----------
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

> `embed_documents` 与 `embed_query` **必须分开**。BGE 中文模型对查询侧要求加前缀 `为这个句子生成表示以用于检索相关文章：`，入库侧不加。这一步漏掉，向量检索效果会明显下降——是最容易踩也最难自查的坑。

```rust
// ---------- vector ----------
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

```rust
// ---------- retriever ----------
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

```rust
// ---------- fusion ----------
pub trait FusionStrategy: Send + Sync {
    /// lanes: (lane 名称, 该 lane 的降序结果)
    /// weights: 与 lanes 一一对应的权重
    fn fuse(&self, lanes: &[(&'static str, Vec<ScoredHit>)], weights: &[f32])
        -> Vec<ScoredHit>;
}
```

```rust
// ---------- rerank（v1 仅留位）----------
pub trait Reranker: Send + Sync {
    fn rerank(&self, query: &str, hits: Vec<Hit>, top_n: usize) -> Result<Vec<Hit>>;
}
```

```rust
// ---------- 返回结果 ----------
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
    /// 直接产出可拼进 prompt 的上下文块，带出处标注
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

---

## 6. 核心数据结构

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

**设计说明**：

- **term dict 与 postings 分离**：`HashMap<String, TermId>` + `Vec<PostingList>`，而不是 `HashMap<String, Vec<Posting>>`。后者在序列化时每个 key 都要重复存字符串，前者只需存一次字符串表，快照体积与加载速度都更优。
- **MVP 就存 positions**：虽然第一版不用，但**事后补需要重建全部索引**。存储成本可接受，收益是未来加短语查询零迁移成本。
- **MVP 不压缩**：posting 用朴素 `Vec<Posting>`。varint / roaring bitmap 列为 P4 优化项——过早优化会掩盖算法本身的正确性验证。

### 6.2 正排存储

```rust
pub struct ForwardStore {
    chunks:   Vec<Chunk>,          // chunk_id 即下标，O(1) 回捞
    docs:     HashMap<DocId, Document>,
    deleted:  RoaringBitmap,       // 墓碑标记
    by_source: HashMap<String, DocId>,  // 幂等 upsert 用
    by_hash:   HashMap<u64, DocId>,     // 幂等 upsert 用
}
```

### 6.3 统计量维护（增量友好）

```rust
pub struct IndexStats {
    num_chunks: usize,  // 有效 chunk 数（已扣除墓碑）
    total_len:  u64,    // 所有 chunk 的 term 数总和
}
// avgdl = total_len as f32 / num_chunks as f32
```

**增量维护而非重算**：

- 插入 chunk：对每个 term 的 `df += 1`（仅当该 term 首次出现于此 chunk），`total_len += dl`，`num_chunks += 1`
- 删除 chunk：从正排取回原文，**重新分析一次**得到 term 列表，逐个 `df -= 1`，再减 `total_len` 与 `num_chunks`

> 删除时重新分析而非维护反向映射，是用 CPU 换内存：省掉一份 `chunk → terms` 的常驻映射（在百万级规模下这是显著开销），而删除是低频操作。

---

## 7. 关键算法设计

### 7.1 中英混合分词

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

| 项      | 决策              | 理由                                                                     |
| ------ | --------------- | ---------------------------------------------------------------------- |
| 中文分词   | jieba-rs        | 生态成熟、词典可定制                                                             |
| 英文词干化  | **第一版不做**       | 词干化（rust-stemmers）会损害专有名词与术语的精确匹配；检索场景宁可牺牲一点召回换精确率。留作可插拔 filter        |
| 停用词    | 中英文合并词表，保守裁剪    | 只去掉真正无意义的虚词。⚠️ 注意"的/了"这类词**不能**在向量路去掉——向量路用的是原文，不受影响；但 BM25 路去掉可显著减小索引 |
| 单字词    | 保留              | 中文单字词（"云""网""库"）常是关键术语                                                 |
| 查询侧一致性 | 强制同一 `Analyzer` | 通过默认实现 `analyze_query → analyze_doc` 保证                                |

### 7.2 BM25

**公式**

```
score(D, Q) = Σ_{t ∈ Q}  idf(t) · (tf(t,D) · (k1 + 1)) / (tf(t,D) + k1 · (1 - b + b · dl(D)/avgdl))

idf(t) = ln(1 + (N - df(t) + 0.5) / (df(t) + 0.5))
```

**参数**：`k1 = 1.2`，`b = 0.75`（Lucene 默认值，作为起点）

> ⚠️ **中文场景的重要差异**：`dl` 是 chunk 的 **term 数**。中文分词后 term 数显著少于英文（同样一段话，英文按空格切出 200 词，中文分词后可能只有 120 词），导致 `avgdl` 系统性地偏小，`b` 的作用被放大。**1.2 / 0.75 只能作为起点，必须在 P5 阶段用评测集网格搜索重新调参，不能盲信英文经验值。**

**检索语义**：OR。query 中未命中的 term 贡献 0 分，其余 term 正常累加。

**实现要点**：采用 **TAAT（Term-At-A-Time）** 而非 DAAT——先对每个 term 遍历其 posting 列表累加分数到 `HashMap<ChunkId, f32>`，最后取 Top-K。万级规模下 TAAT 实现简单、缓存友好，且天然支持 OR 语义。DAAT（WAND / Block-Max WAND）列为 P4 优化项。

### 7.3 向量检索与相似度对齐

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

### 7.4 混合融合

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

- `k = 60`（Cormack et al. 2009 原论文推荐值， empirically robust）
- `rank` 从 1 开始；某 lane 未命中该文档则不贡献
- 默认 `w_bm25 = w_vector = 1.0`

**FR-11 加权归一作为可选策略**：若业务确实需要，采用「除以该 lane Top-1 分数」而非 min-max，并在单候选时退化为 1.0。

### 7.5 增量写入

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

**代价分析**（须写入文档，避免误用）：

- delta 区是**线性扫描**，复杂度 O(|delta| × dim)。将其规模限制在总量的 10% 以内，可保证其对延迟的影响可控
- 重建是 O(n log n)，但可后台异步执行，读请求由旧主索引 + delta 继续服务
- 倒排索引本身可增量写入（HashMap 插入），不受此限制

> 若后续需要真正的增量 HNSW，路径有二：① 自研支持 insert/delete 的 HNSW；② 换成 `arroy`（LMDB 后端，支持增量更新），但会引入 LMDB 依赖并与"内存 + 快照"路线冲突，需重新评估持久化方案。

### 7.6 快照格式

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

- 序列化用 **bincode 1.3**：与 `instant-distance` 的 `with-serde` feature 内部依赖的 bincode 版本保持一致，避免引入两套编解码
- `format_version` 用于向后兼容检测：版本不匹配时直接报错而非静默读错
- CRC32 校验防止读到半截文件（进程被 kill 的常见后果）

---

## 8. 技术选型

### 8.1 选型表

| 用途           | 选型                 | 版本                   | 理由                                                         |
| ------------ | ------------------ | -------------------- | ---------------------------------------------------------- |
| 语言           | Rust               | 1.80+ / 2021 edition | 用户指定；性能与内存可控                                               |
| 向量索引         | `instant-distance` | 0.6.1                | 纯 Rust HNSW、Apache-2.0、rayon 并行构建、`with-serde` 支持序列化       |
| 本地 embedding | `fastembed`        | 5.17.4               | 基于 ort(ONNX Runtime)、同步无 tokio 依赖、原生支持 `bge-small-zh-v1.5` |
| 中文分词         | `jieba-rs`         | latest               | 生态成熟、词典可定制                                                 |
| 并行           | `rayon`            | 1.x                  | 两路召回并行、批量 embedding                                        |
| 序列化          | `bincode`          | 1.3                  | 与 instant-distance 内部版本对齐                                  |
| 小字符串         | `smol_str`         | latest               | Term 存储的 inline 优化，减少堆分配                                   |
| CLI          | `clap`             | 4.x                  | derive API                                                 |
| 错误处理         | `thiserror`        | latest               | 库侧错误定义                                                     |
| 位图           | `roaring`          | latest               | 墓碑标记与过滤器                                                   |

### 8.2 备选方案对比

**向量索引**

| 候选                 | 增量写入           | 依赖            | 结论                                        |
| ------------------ | -------------- | ------------- | ----------------------------------------- |
| `instant-distance` | ❌ 无 insert API | 纯 Rust，无 C 依赖 | ✅ **选用**。用主索引 + delta 区绕开限制               |
| `arroy`            | ✅ 支持           | 耦合 LMDB       | ❌ 与"内存 + 快照"路线冲突，且随机投影树在 512 维以上表现弱于 HNSW |
| 自研 HNSW            | ✅ 可控           | 无             | ❌ 第一版不做。待内核稳定后作为 P6 优化项替换                 |

**BM25 实现**

| 候选        | 结论                                               |
| --------- | ------------------------------------------------ |
| 手写倒排索引    | ✅ **选用**。符合"从 0 开发"目标，且完全可控（中文调优、字段权重、查询语法扩展均自由） |
| `tantivy` | ❌ 工业级但细节被封装，达不到从零掌握的目的                           |

**Embedding**

| 候选                              | 结论                                                 |
| ------------------------------- | -------------------------------------------------- |
| `fastembed` + bge-small-zh-v1.5 | ✅ **默认**。无需联网与 API Key，512 维，中文效果好                 |
| 远程 API（OpenAI / 通义 / BGE 服务）    | ✅ 作为 `remote-embed` feature 保留。模型更大效果更好，但引入网络依赖与成本 |

### 8.3 Feature Flags

```toml
[features]
default       = ["local-embed"]
local-embed   = ["dep:fastembed"]         # 本地 ONNX 推理
remote-embed  = ["dep:reqwest", "dep:tokio"]  # 远程 HTTP API
positions     = []                        # posting 存储位置信息
```

`local-embed` 与 `remote-embed` 可同时开启，运行时通过配置选择；都不开启时 `Embedder` 无可用实现，编译期即暴露。

---

## 9. 接口设计

### 9.1 Rust API

```rust
use index_core::prelude::*;

// ---- 构建索引 ----
let analyzer = MixedAnalyzer::new()?;
let embedder = LocalEmbedder::new(EmbeddingModel::BGESmallZH)?;

let mut index = Index::builder()
    .analyzer(analyzer)
    .embedder(embedder)
    .build()?;

for doc in documents {
    index.upsert(doc)?;   // 幂等，按 content_hash 去重
}
index.commit()?;          // 合并 delta，构建 HNSW

// ---- 持久化 ----
index.save_to_path("index.idx")?;
let index = Index::load_from_path("index.idx")?;   // 秒级加载

// ---- 检索 ----
let searcher = Searcher::new(index)
    .fusion(RrfFusion::new(60.0))
    .reranker(NoOpReranker);          // v1 留位

let resp = searcher.search(SearchRequest {
    query: "Rust 里怎么实现支持中文的 BM25？",
    k: 10,
    mode: SearchMode::Hybrid,          // Bm25 | Vector | Hybrid
    filter: Some(Filter::eq("lang", "zh")),
})?;

for hit in &resp.hits {
    println!("{:.4} [{}] {}", hit.score, hit.source, hit.explain.matched_terms.join(","));
    println!("{}", hit.to_context_block());
}

if let Some(reason) = &resp.empty_reason {
    eprintln!("空结果原因：{:?}", reason);   // Agent 据此决策
}
```

### 9.2 CLI

```bash
# 建索引
idx build --input data/corpus.jsonl --output index.idx

# 检索（三种模式对比）
idx search --index index.idx --mode hybrid -k 10 "查询文本"
idx search --index index.idx --mode bm25   -k 10 "查询文本"
idx search --index index.idx --mode vector -k 10 "查询文本"

# 对比三种模式的效果（调试用，最常用）
idx compare --index index.idx -k 10 "查询文本"

# 可解释性输出
idx search --index index.idx --mode hybrid --explain "查询文本"

# 评测
idx bench --index index.idx --queries data/queries.jsonl
```

### 9.3 错误类型

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("序列化错误: {0}")]
    Codec(#[from] bincode::Error),

    #[error("快照版本不兼容: 文件版本 {found}, 支持版本 {expected}")]
    SnapshotVersionMismatch { found: u32, expected: u32 },

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

> `NoEmbedder` 与 `DimensionMismatch` 是刻意设计的**编译期/运行期早期失败**：把配置错误在第一时间暴露，而不是让错误向量静默进入索引。

---

## 10. 分阶段实施计划

| 阶段         | 内容                                                             | 验收标准                                                 | 依赖 |
| ---------- | -------------------------------------------------------------- | ---------------------------------------------------- | -- |
| **P0**     | 需求分析文档 + 工程骨架 + 安装 Rust 工具链                                    | `cargo build` 通过空工程                                  | —  |
| **P1**     | `analyze` + `index` + `retriever/bm25` + 单测                    | CLI `idx search --mode bm25` 在示例语料上出结果；BM25 分值与手算值一致 | P0 |
| **P2**     | `embed`（本地）+ `vector`（HNSW）+ `retriever/vector`                | CLI 向量路出结果；同义改写能被召回                                  | P1 |
| **P3**     | `fusion`（RRF + 加权）+ `query/searcher` + `explain`               | `idx compare` 可对比三路；Explain 输出完整                     | P2 |
| **P4**     | `storage` 快照 + 增量写入（delta 区）+ 元数据过滤                            | save/load 后检索结果完全一致；增量写入后立即可查                        | P3 |
| **P5**     | 示例语料 + 评测集 + `idx bench` + README                              | Recall@K / MRR@10 / 延迟有实测数据；BM25 参数网格搜索调参            | P4 |
| **P6**（v2） | Reranker 接入（bge-reranker-v2-m3）、MMR 去重、token budget 裁剪、自研 HNSW | —                                                    | P5 |

**关键路径**：P1 → P2 → P3。这三步完成即具备完整检索能力，P4/P5 是工程化与验证。

---

## 11. 测试与评测方案

### 11.1 单元测试（必须有）

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

> **"删除后统计量" 与 "BM25 手算值" 这两个测试最关键**：增量维护的统计量漂移和公式实现偏差，都是不会报错、只会让效果慢慢变差的问题，必须靠测试兜住。

### 11.2 集成测试

- 端到端：摄入 → 提交 → 快照 → 加载 → 检索，结果一致
- 并发：多线程并发检索（验证 `Send + Sync` 与 `Search` 对象复用安全）

### 11.3 效果评测（P5）

**数据集**：`data/queries.jsonl`，人工标注 query → 相关 chunk_id 列表，建议 30~50 条查询，覆盖：

- 精确术语匹配（BM25 应占优）
- 同义改写（向量应占优）
- 中英混合术语（混合应占优）
- 长句自然语言问句（Agent 典型输入）

**指标**：

| 指标           | 定义            | 用途                         |
| ------------ | ------------- | -------------------------- |
| Recall@10    | 前 10 条中相关文档占比 | 衡量召回能力                     |
| MRR@10       | 首个相关文档排名倒数的均值 | 衡量"第一条对不对"，**Agent 场景更重要** |
| NDCG@10      | 折损累积增益        | 综合排序质量                     |
| P50 / P99 延迟 | 端到端检索耗时       | NFR-02 验证                  |

**对照实验**（必须做，否则无法证明混合检索的价值）：

| 配置           | 预期                        |
| ------------ | ------------------------- |
| BM25 only    | 术语查询好，改写查询差               |
| Vector only  | 改写查询好，术语查询差               |
| Hybrid (RRF) | **两类查询都不差，整体最优** ← 需被数据证实 |

**参数调优**：对 `k1 ∈ {1.0, 1.2, 1.5, 2.0}` × `b ∈ {0.3, 0.5, 0.75, 0.9}` 做网格搜索，以 NDCG@10 为目标选优。

> 如调优后混合检索未能优于最优单路，需如实记录并分析原因（常见于：语料太小、query 标注质量差、或 embedding 模型与语料域不匹配），不得粉饰数据。

---

## 12. 风险与应对

| #  | 风险                                          | 影响                                 | 应对措施                                                                  |
| -- | ------------------------------------------- | ---------------------------------- | --------------------------------------------------------------------- |
| R1 | **`instant-distance` 无增量插入 API**            | 增量写入需全量重建                          | 主索引 + delta 暴力区，delta 限制在总量 10% 或 1000 条以内；后台异步重建。已在 7.5 设计           |
| R2 | **BGE 查询侧前缀遗漏**                             | 向量检索效果显著下降，且**极难自查**（代码能跑、分数看着也正常） | `Embedder` 强制区分 `embed_query` / `embed_documents`；写单测断言；README 显著位置标注 |
| R3 | **fastembed 首次下载模型失败/极慢**（国内访问 HuggingFace） | 阻塞 P2 阶段                           | ① 支持 `HF_ENDPOINT` 镜像环境变量；② 支持指定本地模型目录；③ `remote-embed` 作为兜底路径        |
| R4 | **索引侧/查询侧分词不一致**                            | 召回率莫名偏低，排查成本极高                     | 默认实现强制 `analyze_query → analyze_doc`；单测断言两侧一致                         |
| R5 | **BM25 增量统计量漂移**                            | df / avgdl 逐渐失真，效果缓慢劣化，**不会报错**    | 增量累加 + 墓碑；单测对比"增量删除后"与"全量重建"的统计量是否一致                                  |
| R6 | **中文 avgdl 偏小导致 English 经验参数失效**            | BM25 排序质量不佳                        | P5 阶段网格搜索调参，不盲信 k1=1.2 / b=0.75                                       |
| R7 | **RRF 的 k 值选择**                             | 融合效果次优                             | k=60 为论文推荐值且鲁棒；若效果不佳，在 {20, 60, 100} 中对比                              |
| R8 | **OR 语义下长 query 噪声稀释**                      | Agent 的长问句中虚词干扰排序                  | BM25 的 idf 天然抑制高频虚词；若仍不足，v2 增加 query term 裁剪（按 idf 阈值）                |
| R9 | **Rust 工具链未安装**                             | 阻塞全部编码工作                           | 见第 13 章前置条件，优先解决                                                      |

---

## 13. 前置条件与环境准备

### 13.1 当前状态

| 项                                          | 状态                                                        |
| ------------------------------------------ | --------------------------------------------------------- |
| 工作区 `/Users/gongyubo/Code/mine/index-demo` | ✅ 空目录，已创建 `docs/` 与 `data/`                               |
| Rust 工具链                                   | ❌ **未安装**（`rustc` / `cargo` 均不存在，`~/.cargo` 不存在，无 rustup） |
| 示例语料                                       | ❌ 待准备                                                     |

### 13.2 待办

1. **安装 Rust 工具链**
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```
   安装后需将 `~/.cargo/bin` 加入 PATH。建议同时配置国内镜像加速 crates.io 下载。
2. **验证依赖可获取性**：P0 阶段先建一个最小工程，验证 `instant-distance`、`fastembed`、`jieba-rs` 能否正常拉取与编译。⚠️ `fastembed` 依赖 ONNX Runtime（ort），在 Apple Silicon 上的编译耗时较长，需预留时间。
3. **准备示例语料**：`data/` 下放置中文语料（建议 50~200 篇短文档），并配套标注 `queries.jsonl`。

---

## 14. 附录

### 14.1 参考资料

- Robertson & Zaragoza, *The Probabilistic Relevance Framework: BM25 and Beyond*, 2009 — BM25 理论基础
- Malkov & Yashunin, *Efficient and Robust Approximate Nearest Neighbor Search Using HNSW*, 2016 — HNSW 原论文
- Cormack, Clarke & Büttcher, *Reciprocal Rank Fusion Outperforms Condorcet and Individual Rank Learning Methods*, SIGIR 2009 — RRF 与 k=60 的出处
- BAAI/bge-small-zh-v1.5 模型卡 — 查询侧 instruction 前缀的权威说明

### 14.2 变更记录

| 版本   | 日期         | 变更              |
| ---- | ---------- | --------------- |
| v1.0 | 2026-09-01 | 初稿，基于需求问答确认结果编写 |
