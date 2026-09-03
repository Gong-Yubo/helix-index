# P6 — 融合索引接口重新设计（设计说明）

| 项目 | 内容 |
| --- | --- |
| 版本 | **v1.0（待评审）** |
| 日期 | 2026-09-03 |
| 状态 | ⬜ **待拍板**（决策点 D-I1 ~ D-I6 未决，未动代码） |
| 来源 | [issue #1「融合索引接口重新设计」](https://github.com/Gong-Yubo/helix-index/issues/1) |
| 前提 | P0~P5 + v1 收尾全部完成（`v1-finish-design.md` V1-00~V1-16 已落地，CI 9/9 绿） |
| 上游 | `requirements-spec.md`（FR/NFR 定义源）、`architecture-design.md`（模块边界 4.2、ADR-001~008）、`plan.md` |
| 定位 | **不改算法、不改评测口径、不改 CLI 参数**，只重构库的**对外接口层**，让 `index.add(doc)` / `searcher.search(query, mode, top_n)` 成为默认路径 |

> 命名说明：本文件按 `p0~p5-design.md` 的既有约定命名，编号 P6。
> 但 `plan.md` 当前已把 **P6 定义为「v2：Rerank / MMR / 自研索引」**。
> 本文档建议**把「接口重构」提为 P6、原 v2 顺延 P7**，理由见第 10 节 D-I1。

---

## 1. 问题定义

### 1.1 issue 的原话

> 现有索引构建与检索接口不够友好，请从简单易用合理的角度重构接口。
> 重构后，接口如下易用：
> - 索引构建接口：`index.add(doc)`
> - 检索接口：`searcher.search(query, mode, top_n)`

### 1.2 现状实测：最小闭环要写 79 行、碰 8 个对象

`crates/core/examples/search_basic.rs` 是仓库里"库使用最小闭环"的官方示例，
**79 行**。拆开看，用户必须亲手做完以下 7 件事：

| # | 用户必须写的代码 | 涉及对象 | 忘了会怎样 |
| --- | --- | --- | --- |
| 1 | `MixedAnalyzer::new()` 并**一直活到检索时** | `Analyzer` | 索引/查询分词器不一致（R4）——**静默错** |
| 2 | `Chunker::default()` + 每篇 `chunker.chunk(0, &text)` | `Chunker` | 无法入库 |
| 3 | `content_hash(&text)` 手算 | `Document` | FR-15 幂等 upsert **静默失效**（hash=0 走跳过分支） |
| 4 | `Document { doc_id: 0, .. }` 填一个无意义的 0 | `Document` | 无（纯噪音） |
| 5 | `LocalEmbedder::new()` + 收集 `live_chunks()` + `embed_documents()` | `Embedder` | 向量模式不可用 |
| 6 | 逐条 `hnsw.add(id, NormalizedVector::new(v))` | `VectorIndex` | **不做 L2 归一化 → 余弦分全错，静默错** |
| 7 | `Searcher::new(&index, &analyzer).with_vector(&e, &vi)` | `Searcher<'a>` | 编译错误 |

其中 **第 1、3、6 步是"静默正确性陷阱"**：写错了不报错、不 panic，只是效果莫名其妙地差。
这跟 `error.rs` 开头写的原则（"配置错误要在最早时刻暴露"）是矛盾的——
**我们把最容易写错的三件事，外包给了每一个调用方，每人每次都重犯一遍。**

### 1.3 现状实测：生命周期把用户逼成库作者

`Searcher<'a>` 持有 4 个外部引用（`&'a Index` / `&'a dyn Analyzer` / `&'a dyn Embedder` / `&'a dyn VectorIndex`），
**无法存进任何 struct、无法 `move` 进线程/task**，只能在局部作用域里现搭现用。

后果已经在仓库内部显现——**CLI 自己造了本该由库提供的抽象**：

| 位置 | 自造的脚手架 |
| --- | --- |
| `crates/cli/src/bench.rs` | `struct Setup { index, analyzer, vectors, embedder, backend, vector_kind }` + `enum VectorBackend { Hnsw, Brute }` + `fn make_searcher<'a>(...) -> Result<Searcher<'a>>` |
| `crates/cli/src/main.rs` | `load_corpus()` / `load_corpus_with()` / `build_index()` / `embed_chunks()` / `rebuild_vector_index()` 五个自由函数拼装 |
| `crates/cli/src/bench.rs` | 为了 `--runs N` 重建 HNSW 图，不得不把 `vectors` 单独提出来在 `Setup` 里传来传去 |

**判据：一个库的官方 CLI 和官方评测工具都需要手搓一层"引擎对象"才能用，
说明这个库缺的正是这一层。** 这是本 issue 存在的根因，也是重构的核心靶心。

### 1.4 顺带发现的现存缺陷（重构应一并修掉）

| # | 缺陷 | 位置 | 严重度 |
| --- | --- | --- | --- |
| B1 | `helix search --index snap.idx` **写死** `MixedAnalyzer::new()`；若快照是用 charabia 建的，查询侧会静默换分词器 → **违反 R4** | `main.rs:301` | 高（静默错） |
| B2 | 快照不记录 analyzer / embedder / chunker 的任何信息，无法在 load 时校验 | `storage/snapshot.rs` | 中 |
| B3 | `Document` 不含 `text`，只有 chunk 里有；`content_hash` 由调用方算 | `document.rs` | 中（API 噪音） |
| B4 | FR-17「增量写入不阻塞读」在需求里是 Must，但现状是单个 `&mut Index`，**根本不支持** | 架构层面 | 已知未做 |

---

## 2. 业界做法调研

### 2.1 五个参照系

| 系统 | 写入 | 检索 | 关键取舍 |
| --- | --- | --- | --- |
| **Lucene / tantivy** | `IndexWriter::add_document(doc!(f => v))?` + **显式 `commit()`** | `reader.searcher()` → `searcher.search(&query, &TopDocs::with_limit(n))` | Writer/Reader 分离 + 不可变 segment；**commit 后文档才可见**；schema-first 显式声明字段；writer 有内存预算（如 50MB），满则自动刷段 |
| **Chroma** | `collection.add(ids, documents, metadatas)` — 集合绑定 `EmbeddingFunction` 时**自动向量化** | `collection.query(query_texts=[...], n_results=k, where={...})` | **EmbeddingFunction 挂在 collection 上，add/query 双侧自动调用**，用户从不接触向量；schemaless（metadata 是自由 dict） |
| **LanceDB** | `table.add(records)` — schema 用 `SourceField()` / `VectorField()` 声明，写入时经 `WithEmbeddingsScannable` **流式**自动向量化 | `table.search(q).limit(n).where(...).rerank(RRFReranker()).to_pandas()` | 声明式自动向量化且**流式**（边流入、边算向量、边落盘，不要求一次算完）；`EmbeddingFunction` 分 `compute_source_embeddings` / `compute_query_embeddings` **两路**；检索侧 builder 链 |
| **Meilisearch / Typesense** | `index.add_documents(docs, Some("id"))` — schemaless、主键**可自动推断**、写入是异步 task | `index.search().with_query(q).with_limit(n).execute()` | 面向"开箱即用"；可选参数一律 builder |
| **LlamaIndex / LangChain** | `VectorStoreIndex.from_documents(docs)` | `as_retriever()` / `query_engine.query(q)` | 场景层封装：chunk + embed + index 全藏进一个构造函数 |

### 2.2 五条共识（本方案直接采纳）

1. **写入端"一条路走完"**：分词 / 分块 / 向量化 / 插索引全部内聚在 `add` 背后，用户只给原始文档。
   Chroma、LanceDB、LlamaIndex 三家一致。代价是必须暴露 chunking / embedding 的可配置点，
   但**默认值必须能直接用**。
2. **Embedding 由"集合 / 表"持有，而不是每次调用传入**——Chroma 与 LanceDB 的共同选择。
   这是消除"查询侧忘了加 BGE instruction 前缀"（R2）、"入库侧忘了 L2 归一化"这类
   **静默正确性 bug** 的根本手段：把易错步骤移进库里，**只做对一次**。
   （LanceDB 把 query/source 两路拆成两个方法，正好对应本项目 `embed_query`/`embed_documents` 的既有设计——
   说明我们的 trait 分层是对的，只是挂错了位置：该挂在索引上，不该挂在调用方手上。）
3. **向量化必须批量 / 流式，不能逐条**：LanceDB 明确用 `WithEmbeddingsScannable` 流式处理，
   Chroma 内部 batch。这是 NFR-03（构建耗时）的硬约束。
   → **所以 `add(doc)` 表面是单条，内部必须有写缓冲 + 批量 flush**，
   这与 Lucene/tantivy「内存 buffer 满 / commit 时刷成 segment」是同一个机制。
4. **Writer / Reader 分离**（Lucene、tantivy）：写端可变、读端是不可变快照 → 天然满足 FR-17。
   它顺带解决 Rust 的生命周期问题：读端可以 `Arc` 化、可存储、可跨线程，
   不再需要 `Searcher<'a>` 去借 4 个外部对象。
5. **检索侧用 builder 承载可选参数**（LanceDB `.limit().where().rerank()`、
   Meilisearch `.with_query().with_limit()`），主参数保持最简形式。
   → 对应我们的 `search(query, mode, top_n)`：三个参数都是**必需**的，
   可选参数（filter / explain / BM25 参数）走 builder。

### 2.3 三条反模式（本方案明确不采纳）

| 反模式 | 出处 | 为何不采纳 |
| --- | --- | --- |
| **schema-first，字段必须预先声明类型与属性** | Lucene / tantivy | 我们的 metadata 是 schemaless JSON（对应 Chroma 的 metadata / LanceDB 的 dynamic field）。引入 `SchemaBuilder` 是纯负担，且会与"Agent 自己零散写入、字段随时加"的场景冲突 |
| **主键由用户强制提供（`ids=[...]`）** | Chroma | 我们是 content_hash 幂等（FR-15），更适合 Meilisearch 的"自动推断 + 可覆盖" |
| **写入异步 task 化（返回 taskId 轮询）** | Meilisearch | 那是 C/S 架构的产物。我们是嵌入式库，同步 + 显式 `commit()` 更简单也更诚实 |

---

## 3. 设计目标与非目标

### 3.1 目标

| # | 目标 | 可验证的判据 |
| --- | --- | --- |
| G1 | 最小闭环 ≤ 15 行、只碰 1 个对象 | `examples/search_basic.rs` 从 79 行降到 ≤15 行 |
| G2 | 三个静默陷阱（分词器不一致 / 忘算 hash / 忘归一化）在 API 上**不可能发生** | 用户代码里不再出现 `content_hash`、`NormalizedVector::new`、`&analyzer` |
| G3 | `searcher` 可存进 struct、可 `Arc`、可跨线程 | `Searcher: 'static + Clone + Send + Sync` |
| G4 | 默认路径零配置可用；**所有现有可调能力一个不丢** | 第 7.3 节「逃生舱映射表」逐项对账 |
| G5 | CLI 参数与行为**完全不变**；评测数字在已记录的抖动容差内不变 | 第 10 节验收标准 |

### 3.2 非目标（明确不做，防止范围蔓延）

- ❌ 不引入 schema builder（见 2.3）
- ❌ 不改任何算法（BM25 参数 k1/b、RRF k/weights、ef_search 全部沿用 P5 定稿值）
- ❌ 不实现真正的 delta 分段合并（FR-17 完整版）——本轮只把**接口形态**留好，见 6.4
- ❌ 不接 Rerank 模型（那是顺延后的 P7）
- ❌ 不换 CLI 参数、不改快照 magic

---

## 4. 目标 API

### 4.1 Before / After

**Before（现状，79 行，摘核心片段）**

```rust
let analyzer = MixedAnalyzer::new();              // ① 必须活到最后
let chunker = Chunker::default();                 // ②
let mut index = Index::new();
for line in corpus.lines() {
    let v: serde_json::Value = serde_json::from_str(line)?;
    let text = v["text"].as_str().unwrap().to_string();
    let doc = Document {
        doc_id: 0,                                // ③ 无意义的 0
        source: v["source"].as_str().unwrap().to_string(),
        metadata: v.get("metadata").cloned().unwrap_or(serde_json::json!({})),
        content_hash: content_hash(&text),        // ④ 手算，漏了就静默失效
    };
    index.add(doc, chunker.chunk(0, &text), &analyzer)?;
}

let embedder = LocalEmbedder::new()?;             // ⑤
let entries: Vec<_> = index.live_chunks().map(|c| (c.chunk_id, c.text.clone())).collect();
let texts: Vec<String> = entries.iter().map(|(_, t)| t.clone()).collect();
let vecs = embedder.embed_documents(&texts)?;
let mut hnsw = HnswRsIndex::with_capacity(entries.len().max(1024));
for ((id, _), v) in entries.iter().zip(vecs) {
    hnsw.add(*id, NormalizedVector::new(v))?;     // ⑥ 漏了归一化 = 静默错
}

let searcher = Searcher::new(&index, &analyzer)   // ⑦ 借 4 个对象，无法存储
    .with_vector(&embedder, &hnsw)
    .with_fusion(Box::new(RrfFusion::default()));
let resp = searcher.search("如何加快检索速度", SearchMode::Hybrid, 3)?;
```

**After（目标，15 行）**

```rust
use helix_core::prelude::*;

let mut index = SearchIndex::builder().build()?;   // ① 零配置：MixedAnalyzer + 512/64 + bge-small-zh + HNSW + RRF(60, 1:1.5)
for line in corpus.lines() {
    let v: serde_json::Value = serde_json::from_str(line)?;
    index.add(Document::new(v["text"].as_str().unwrap())
        .with_source(v["source"].as_str().unwrap_or("<unknown>"))
        .with_metadata(v.get("metadata").cloned().unwrap_or(serde_json::json!({}))))?;
}
index.commit()?;                                   // ② 刷写缓冲（批量 embed）；此后数据可见

let searcher = index.into_searcher();              // ③ 'static / Clone / Send+Sync，可存进 struct
let resp = searcher.search("如何加快检索速度", SearchMode::Hybrid, 3)?;   // ④ ← issue 目标形态
```

单文档场景（Agent 零散写入）可以更短：

```rust
index.add("这是一段要入库的文本")?;   // impl Into<Document> for &str / String
```

### 4.2 完整 API 表面（新增 / 改名）

**新增**

| 类型 / 方法 | 说明 |
| --- | --- |
| `SearchIndex` | 拥有型门面（写端）：持有 analyzer / chunker / embedder / fusion / reranker / 倒排 / 向量索引 / 写缓冲 |
| `SearchIndex::builder()` → `SearchIndexBuilder` | 全部配置点，见 7.3；**所有方法可选，不调即用默认值** |
| `SearchIndex::add(doc) -> Result<AddOutcome>` | 分块 → 倒排 → 写缓冲；**返回 `{ doc_id, chunk_ids, deduped }`** |
| `SearchIndex::add_documents(iter)` | 批量路径（吞吐最优，绕过逐条开销） |
| `SearchIndex::remove(doc_id)` | 墓碑删除 + 统计量回滚（复用现有 `Index::remove`） |
| `SearchIndex::commit()` | 刷写缓冲（批量 embed + 灌向量索引）；**此后数据对外可见** |
| `SearchIndex::into_searcher()` | 交出所有权，产出 `'static` 的 `Searcher` |
| `SearchIndex::search(q, mode, n)` | 便利法：内部 `commit()` 后直接在当前状态上检索（写完马上查的最常见路径） |
| `SearchIndex::save(path)` / `SearchIndex::load(path)` | 落盘 / 加载（含配置指纹校验，见第 8 节） |
| `Searcher`（新） | owned 只读检索器：`'static + Clone + Send + Sync` |
| `Searcher::search(query, mode, top_n)` | ← issue 目标形态 |
| `Searcher::search_with(query, mode)` → `SearchRequest` | builder：`.top_n(n)` `.filter(&f)` `.explain(bool)` `.exec()` |
| `Searcher::into_index()` | 换回写端（零拷贝），继续增量写入 |
| `document::Document`（重定义） | **输入 DTO**：`{ text, source, metadata, dedup_key }`，无 `doc_id` |
| `document::DocRecord`（新） | 存储记录：原 `Document`（`{ doc_id, source, metadata, content_hash }`，无 text） |
| `AddOutcome` | `{ doc_id: DocId, chunk_ids: Vec<ChunkId>, deduped: bool }` |

**改名**

| 原名 | 新名 | 理由 |
| --- | --- | --- |
| `query::Searcher<'a>` | `query::QueryExecutor<'a>` | 把 `Searcher` 让给 issue 要求的那个名字。旧类型**签名完全不变**，仅改类型名，内部实现抽成自由函数供两者共用（见 5.2） |

**保持不变**

- 六个 trait（`Analyzer` / `Embedder` / `VectorIndex` / `Retriever` / `FusionStrategy` / `Reranker`）一个不动
- `Index`（倒排 + 正排）作为**内部结构**保留，`add(doc, chunks, analyzer)` 签名仅把 `Document` 换成 `DocRecord`
- `SearchResponse` / `Hit` / `Explain` / `EmptyReason`：一个字段不改
- CLI 全部参数；快照 magic `IDX1`

---

## 5. 架构设计

### 5.1 分层（严格遵守架构文档 4.2 的模块边界）

```
                ┌─────────────────────────────────────────┐
   用户代码      │  SearchIndex (写端)      Searcher (读端)  │   ← 新增门面层，本轮唯一新增的层
                └────────────┬───────────────┬────────────┘
                             │               │
                ┌────────────▼───────────────▼────────────┐
   query 层     │  query::search_parts(&SearchParts, ...)   │   ← 自由函数（编排逻辑的唯一实现）
                │  QueryExecutor<'a>  （旧 Searcher，薄壳）  │
                └────────────┬─────────────────────────────┘
                             │  调用
        ┌────────────────────┼────────────────────┐
        ▼                    ▼                    ▼
   retriever            fusion              rerank        ← 六 trait 完全不动
        │
   ┌────┴─────┐
   ▼          ▼
 index     vector      ← 内部结构，不感知门面层
```

**关键约束**：门面层**只做装配与生命周期管理，不实现任何算法**。
`SearchIndex::add` 内部依次调用 `Chunker::chunk` → `Index::add` → `Embedder::embed_documents` → `VectorIndex::add`，
自己不碰 postings、不碰 BM25 公式、不碰 HNSW 图。边界与现状完全一致。

### 5.2 关键实现技巧：把 `Searcher<'a>` 的实现抽成自由函数

这是让本次重构**变成加法而非改写**的核心：

```rust
// crates/core/src/query/mod.rs
pub(crate) struct SearchParts<'a> {
    pub index: &'a Index,
    pub analyzer: &'a dyn Analyzer,
    pub embedder: Option<&'a dyn Embedder>,
    pub vector_index: Option<&'a dyn VectorIndex>,
    pub fusion: &'a dyn FusionStrategy,
    pub reranker: &'a dyn Reranker,
    pub bm25_params: Bm25Params,
    pub explain: bool,
}

/// 编排的唯一实现：两路召回 → 过滤 → 融合 → 回捞 → 精排。
pub(crate) fn search_parts(
    parts: &SearchParts<'_>,
    query: &str, mode: SearchMode, k: usize,
    filter: Option<&Filter>,
) -> Result<SearchResponse> { /* 现有 searcher.rs 的 160 行，原样搬过来 */ }
```

于是：

- `QueryExecutor<'a>`（旧 `Searcher`）**方法签名一个字不改**，内部 `search_parts(&self.parts(), ...)`，
  → **bench.rs / integration.rs / searcher.rs 的单测全部零改动**（仅类型名改一次）
- 新的 `Searcher` 持有 `Arc<dyn FusionStrategy>`，每查一次构造 `SearchParts { fusion: &*self.fusion, .. }` 再调同一个函数
  → **零逻辑重复，且避免了每次查询 2 次 `Box::new` 堆分配**

> 这一步不做，新 `Searcher` 就得复制 160 行编排逻辑，两份逻辑必然漂移。
> **I-01 必须是第一个任务，且必须先于一切功能。**

### 5.3 数据布局

```rust
pub struct SearchIndex {
    cfg:  Arc<Config>,   // analyzer / chunker / embedder / fusion / reranker / bm25 / batch_size
    inner: Arc<Inner>,   // 已提交状态；add() 期间 Arc::make_mut 原地改（refcount 恒为 1）
    pending: Vec<PendingChunk>,  // 待 embed 的 (chunk_id, text)
}

struct Inner {
    index: Index,                              // 倒排 + 正排 + 统计量
    vector_index: Option<Box<dyn VectorIndex>>, // HnswRsIndex（默认）
    raw_vectors: Vec<(ChunkId, Vec<f32>)>,      // 落盘用（快照策略 D1：存原始向量，不存 HNSW 图）
}

pub struct Searcher {
    cfg:   Arc<Config>,
    inner: Arc<Inner>,   // 从 SearchIndex 移动而来，零拷贝
}
```

`Config` 里 `Arc<dyn Analyzer>` / `Arc<dyn Embedder>` / `Arc<dyn FusionStrategy>` / `Arc<dyn Reranker>`
——四个 trait 现状均已声明 `Send + Sync`（`Analyzer`、`Embedder`、`FusionStrategy`、`VectorIndex` 已确认），
因此 `Inner + Config: Send + Sync` → **`Searcher: 'static + Clone + Send + Sync`**，
可以直接放进 axum 的 `AppState`、可以 `Arc<Searcher>` 分发给 handler、可以 `move` 进 rayon 任务。

---

## 6. 写入路径

### 6.1 `add(doc)` 内部做了什么

```
add(Document)
  ├─ 1. dedup_key 或 content_hash(text) 查重 → 命中则直接返回 deduped=true（FR-15）
  ├─ 2. Chunker::chunk(0, &text)      → Vec<Chunk>        （char 计数 / byte 偏移，约定不变）
  ├─ 3. Index::add(DocRecord, chunks, &*analyzer)          （倒排 / 正排 / 统计量，立即完成）
  └─ 4. 每个 chunk 文本 push 进 pending 缓冲
        └─ pending.len() >= batch_size 时自动 flush（不阻塞，见 6.2）
```

**L2 归一化由 flush 路径内部调用 `NormalizedVector::new` 保证**，用户代码里不再出现这个概念。
**BGE instruction 前缀仍由 `Embedder::embed_query` 自己加（R2），门面层不加任何前缀**，
只负责"入库调 `embed_documents`、查询调 `embed_query`"——这个调用契约现在由库保证，不再靠用户自觉。

### 6.2 写缓冲与批量 embed（NFR-03 的硬约束）

`Embedder::embed_documents(&[String])` 是批接口。若 `add` 逐条 embed，
12K 分片会把 NFR-03（构建 < 120s）直接打穿。因此：

| 机制 | 取值 | 说明 |
| --- | --- | --- |
| `batch_size` | 默认 **64**（**待实测校准**，见 I-04） | `pending` 满则自动 flush |
| 自动 flush | 是 | 让 `add` 的延迟有长尾（偶发一次 64 条 embed），需在文档写明 |
| `commit()` / `flush()` | 显式 | 强制冲刷 |
| `add_documents(iter)` | 批量路径 | 一次性 chunk 完再分批 embed，吞吐最优；`helix build` 走这条 |
| `save()` | 隐含 `commit()` | 不允许带着未刷的缓冲落盘 |

> **参数不盲信经验值**（项目既有约定）：`batch_size` 必须在 T2Ranking 12K 语料上
> 实测 32 / 64 / 128 / 256 四档后定稿，写入 `eval-report.md`。

### 6.3 `commit()` 语义：显式可见性（对齐 Lucene / tantivy）

**`add` 之后必须 `commit()` 才对检索可见。** 这是 Lucene / tantivy 的既有共识，
不是我们发明的，用户心智里已有模型。理由：

1. 批量 embed 需要一个明确的边界，否则永远不知道该什么时候算向量
2. 给"可见性"一个显式动作，比"写完立即可见"更容易推理（尤其在并发/多线程写入时）
3. 为 6.4 的 delta 分段留出语义空间——将来 `commit()` 就是"刷成一个新 segment"

代价是**多一步**。缓解：`SearchIndex::search(q, mode, n)` 便利法内部自动 `commit()`，
覆盖"写完马上查"的最常见路径；只在需要长期持有 `Searcher` 时才显式 `into_searcher()`。

### 6.4 读写关系：本轮用「所有权切换」，FR-17 完整版留到 P7

| 方案 | 形态 | 并发 | 成本 | 采纳 |
| --- | --- | --- | --- | --- |
| **A. 所有权切换（typestate）** | `let searcher = index.into_searcher();` … `let index = searcher.into_index();` | 同一时刻独占一端 | **零拷贝、零锁、编译期保证** | ✅ **本轮** |
| B. 内部 `RwLock` | `index.searcher()` 借用 | 读写可并发 | 锁竞争 → P99 抖动（NFR-02 风险）；`Inner` 需整体 `Send+Sync` | ❌ 本轮不做 |
| C. delta 分段（FR-17 正解） | `main: Arc<Segment>` + `delta` 合并查询 | 真·读不阻塞写 | 需跨两棵树合并 BM25（df / avgdl 可加，需实现） | ⬜ P7 |

**为什么 A 够用且正确**：`into_searcher()` 只移动 `Arc<Inner>`，不复制任何数据；
`SearchIndex` 被消耗后无人能改 `Inner`，因此 `Searcher` 的 `Arc` 语义上是不可变快照（refcount 可 >1 供 `Clone`）。
FR-17 的完整版（真·读不阻塞写）属于**并发能力**，不是**接口易用性**问题，
且现状本就不支持（B4）——**不应让它阻塞本轮重构**。
届时从 A 升级到 C，**`into_searcher()` 可以平滑换成 `searcher()`，API 形态不变，第一轮的投入不浪费。**

---

## 7. 检索路径

### 7.1 三个必需参数 + builder 承载可选参数

```rust
// 主形态（issue 要求）：三个必需参数
let resp = searcher.search("如何加快检索", SearchMode::Hybrid, 10)?;

// 可选参数走 builder（对齐 LanceDB / Meilisearch）
let resp = searcher
    .search_with("如何加快检索", SearchMode::Hybrid)
    .top_n(10)
    .filter(&Filter::eq("lang", "zh"))     // FR-14
    .explain(true)                          // FR-13
    .exec()?;
```

`search(q, mode, n)` 就是 `search_with(q, mode).top_n(n).exec()` 的语法糖，无第二份逻辑。

### 7.2 `mode` 参数：enum 为主，同时接受字符串

保留 `SearchMode` enum（类型安全、编译期拒绝拼写错误，符合 Rust 惯例），
但**额外支持字符串**——Agent 场景里 mode 常来自 LLM 输出或配置文件：

```rust
let resp = searcher.search(q, "hybrid", 10)?;   // impl TryInto<SearchMode>
let resp = searcher.search(q, SearchMode::Hybrid, 10)?;  // enum，零开销
```

签名：`fn search<M: TryInto<SearchMode>>(&self, query: &str, mode: M, top_n: usize) -> Result<SearchResponse>`
（`M::Error: Display`，解析失败 → `Error::InvalidInput`）。
**实现前需先验证这个泛型签名能过编译**（I-06 的冒烟项）；若不行，退化为
`search(q, mode, n)`（enum）+ `search_mode_str(q, "hybrid", n)` 两个方法。

### 7.3 逃生舱映射表（证明 G4：现有可调能力一个不丢）

这是本设计最需要被审查的一张表——**门面最容易犯的错就是把能力关死**。

| 现有能力 | 现状用法 | 新 API 注入点 |
| --- | --- | --- |
| 换分词器（charabia 对照） | `build_index(path, chunker, &CharabiaAnalyzer::new())` | `SearchIndex::builder().analyzer(Arc::new(CharabiaAnalyzer::new()))` |
| 强制单 chunk（评测口径） | `Chunker::new(200_000, 0)` | `.chunker(Chunker::new(200_000, 0))` |
| 换向量后端（brute 诊断） | `BruteForceIndex::from_entries(..)` | `.vector_backend(VectorBackend::Brute)` |
| 调 ef_search | `HnswRsIndex::with_ef_search(n)` | `.ef_search(n)` |
| BM25 网格搜索 | `Searcher::with_bm25_params(p)` | `.bm25_params(p)` 或 `search_with(..).bm25(p)` |
| RRF k / weights 网格 | `with_fusion(Box::new(RrfFusion::new(k, w)))` | `.fusion(Arc::new(RrfFusion::new(k, w)))` |
| 裸 embedder（bench 禁缓存） | 直接传 `&LocalEmbedder` | `.embedder(Arc::new(LocalEmbedder::new()?))` |
| 无向量（纯 BM25） | 不调 `with_vector` | `.embedder(None)` / `.vectors(false)` |
| 自研 rerank（P7） | `with_reranker(Box::new(..))` | `.reranker(Arc::new(..))` |
| `--runs N` 重建 HNSW 图 | `Setup.vectors` 反复 `build_backend` | `SearchIndex::raw_vectors()` + `rebuild_vector_index()` |
| 完全自定义 lane 组装 | `Searcher::new(&index, &analyzer)…` | **`QueryExecutor<'a>` 原样保留** |

最后一行是关键：**底层永远可达**。门面只是"默认装配"，不是"唯一路径"。

---

## 8. 持久化与配置指纹（修掉 B1 / B2）

### 8.1 问题

`helix search --index snap.idx` 现在**写死** `MixedAnalyzer::new()`（`main.rs:301`）。
只要快照是用别的分词器建的，查询侧就会静默换一套分词规则——**直接违反 R4**，
而且这种错误表现为"召回莫名其妙变差"，极难排查。

### 8.2 方案：快照写入配置指纹，load 时校验

```rust
/// 写入快照正文（FORMAT_VERSION 2 新增 section）
pub struct ConfigFingerprint {
    pub analyzer_id: String,     // Analyzer 自报身份，如 "mixed" / "charabia"
    pub embedder_id: String,     // 如 "bge-small-zh-v1.5"
    pub dim: u32,                // 向量维度
    pub chunker: (usize, usize), // (chunk_chars, overlap_chars)
}
```

配套：
- `Analyzer` 增加 `fn id(&self) -> &'static str`（**默认实现返回 `"custom"`**，
  不强制所有实现改，但 `MixedAnalyzer` / `CharabiaAnalyzer` 必须显式覆盖）
- `Embedder` 已隐含模型信息，增加 `fn id(&self) -> &str` 同例
- `SearchIndex::load(path)` 校验指纹与当前装配的组件是否一致，
  不一致 → **新增 `Error::ConfigMismatch` 明确报错**，绝不静默

### 8.3 格式版本

`FORMAT_VERSION` **1 → 2**（不换 magic `IDX1`）。
现状 `codec.rs` 的版本校验是严格相等（`found != FORMAT_VERSION` → `SnapshotVersionMismatch`），
所以旧快照会被干净地拒绝，不会读到垃圾数据。

⚠️ **与 `positions` feature 的交互**：`codec.rs:20` 约定"positions 开启时版本 +1 写入"。
base 升到 2 后，positions 应写 3——**这个约定在 I-09 必须同步更新并加注释**，否则两个 feature 会撞版本号。

**代价（需明确告知）**：现有 `.idx` 快照全部失效，需重建。`data/*.snapshot` 与评测脚本里的
快照路径要一起更新（I-10 的验收项之一）。

### 8.4 不做的：自动重建组件

LanceDB 有 `EmbeddingRegistry` 可以按配置自动重建 embedding 函数。
我们**本轮不做**——需要引入注册表机制，属于范围蔓延。
本轮只做到"**不一致就报错**"（防呆 > 自动化），自动重建留作后续。

---

## 9. 任务清单与执行顺序

| 编号 | 任务 | 依赖 | 风险 |
| --- | --- | --- | --- |
| **I-01** | 抽 `search_parts` 自由函数；`Searcher<'a>` → `QueryExecutor<'a>`（签名不变） | — | 低（纯搬家，有单测兜底） |
| **I-02** | `Document` 重定义为输入 DTO（含 `text`）；`DocRecord` 承接存储记录 | — | 中（breaking，实测 16 个文件引用 `Document`） |
| **I-03** | 新增 `Config` + `Inner` + `SearchIndexBuilder` | I-02 | 低 |
| **I-04** | `SearchIndex::add` / `add_documents` / 写缓冲 / `flush` / `commit` | I-03 | **中**（batch_size 待实测校准） |
| **I-05** | 新增 owned `Searcher` + `SearchRequest` builder + `into_searcher` / `into_index` | I-01, I-03 | 低 |
| **I-06** | `mode` 支持 `&str`（`TryInto` 泛型签名编译验证） | I-05 | 低（有退化方案） |
| **I-07** | `remove` / `save` / `load` 接入门面层 | I-04 | 低 |
| **I-08** | `ConfigFingerprint` + `Analyzer::id` / `Embedder::id` + `Error::ConfigMismatch` | I-07 | 中（升 FORMAT_VERSION） |
| **I-09** | `codec.rs` 版本约定更新（base 2 / positions 3）+ 快照兼容性测试 | I-08 | 低 |
| **I-10** | CLI 切到新 API（`build` / `search` / `compare`）——**参数与输出一字不改** | I-04~I-08 | 中 |
| **I-11** | `examples/search_basic.rs` 重写（79 → ≤15 行）+ `docs/user-guide.md` 库接入章节重写 | I-10 | 低 |
| **I-12** | **bench 迁移到新 API + 回归对账**（最高风险项） | I-10, I-05 | **高** |
| I-13 | `plan.md` 阶段表更新（P6 接口重构 / P7 顺延）+ 本文档状态回写 | I-12 | 低 |
| I-14 | 旧 `QueryExecutor` / `Index::add` 是否 `#[deprecated]`（见 D-I6） | I-13 | 低 |

**建议执行顺序**

1. **I-01 → I-02**：先把地基改好（抽函数 + 拆 Document），此时**行为零变化**，CI 应全绿
2. **I-03 → I-04 → I-05 → I-06**：新增门面，**纯加法**，旧路径完全不动
3. **I-07 → I-08 → I-09**：持久化与指纹（含升版本）
4. **I-10**：CLI 迁移——先迁 `compare`（最简单、无快照），再迁 `search`，最后 `build`
5. **I-11**：示例与文档（此时"新 API 更易用"这件事才第一次对外可见）
6. **I-12**：bench 迁移 + 回归对账（**放在最后**，因为它依赖前面全部稳定）
7. **I-13 → I-14**：收尾

> **为什么 bench 放最后**：bench 是评测可复现性的唯一保障（V1-04/05/06 的全部意义），
> 且它用到了最多的逃生舱（brute 后端、BM25 网格、RRF 网格、--runs 重建图、裸 embedder）。
> **在所有逃生舱都被验证过之前动 bench，等于拿评测数字冒险。**

---

## 10. 验收标准与决策点

### 10.1 验收标准

**接口易用性（G1 / G2）**
- [ ] `examples/search_basic.rs` ≤ 15 行，且代码里不出现 `content_hash`、`NormalizedVector`、`&analyzer`、`Chunker`
- [ ] `index.add("纯文本")` 可编译通过
- [ ] `Searcher: 'static + Clone + Send + Sync` —— 用编译期断言验证
  （`fn assert_send_sync<T: Send + Sync + 'static>() {}`）
- [ ] `docs/user-guide.md` 的六 trait 替换矩阵更新为新的注入点

**能力不倒退（G4）**
- [ ] 7.3 节映射表**逐项**有对应测试（尤其 brute 后端 / BM25 网格 / RRF 网格 / charabia）
- [ ] `cargo test --workspace` 全绿；`--features charabia` 与 `--no-default-features` 均通过（V1-10）

**行为一致（G5，最关键）**
- [ ] CLI 参数与输出格式一字不改：`helix build/search/compare --help` diff 为空
- [ ] **brute 后端下新旧 API 逐位一致**（确定性，无抖动）——用 `scripts/fixtures/` 存档 JSON 对账
- [ ] **hnsw 后端下差异 ≤ 已记录的抖动容差**（`eval-report.md` 3.4：NDCG 极差 0.0024 / 0.0023，R-P5-13）
- [ ] `make eval-quality` 结论不变：hybrid 的 MRR/Recall 最高、NDCG 略低于 vector
- [ ] `make eval-perf` 的 NFR 表**重新实测**（NFR-03 构建、NFR-05 内存会因本轮改动而变化，见第 11 节）
- [ ] CI 9 job 全绿（含 MSRV 1.90 与 feature 隔离）

**缺陷修复**
- [ ] B1：用 charabia 建库 → 用 MixedAnalyzer 加载 → **必须报 `ConfigMismatch`**，不得静默
- [ ] B2：`FORMAT_VERSION` = 2，旧快照触发 `SnapshotVersionMismatch`
- [ ] B3：`Document` 含 `text`，`content_hash` 由库内计算

### 10.2 决策点（待拍板）

| 编号 | 决策 | 建议 |
| --- | --- | --- |
| **D-I1** | 阶段编号：接口重构提为 **P6**，原「v2：Rerank / MMR / 自研索引」顺延 **P7**？ | **是**。接口是 breaking change，越晚做代价越大（P7 每加一个特性就多一处要改）；而 Rerank / MMR 是纯增量，晚做零损失 |
| **D-I2** | 旧 `Searcher<'a>` 改名为 `QueryExecutor<'a>`，把 `Searcher` 让给新的 owned reader？ | **是**。issue 明确要 `searcher.search(...)`；旧类型是"编排器"而非"检索器"，改名更准确。备选：新类型叫 `Reader`、旧名字不动 |
| **D-I3** | 可见性语义：显式 `commit()`（对齐 Lucene），还是写完立即可见？ | **显式 `commit()`** + `SearchIndex::search()` 便利法兜底。理由见 6.3 |
| **D-I4** | `Document` 改造为输入 DTO（breaking，实测 16 个文件引用）？ | **是**。不改则 `index.add(doc)` 无法实现——现状 `Document` 根本没有 `text` 字段 |
| **D-I5** | 快照升 `FORMAT_VERSION` 2 存配置指纹（旧 `.idx` 全部需重建）？ | **是**。B1 是静默错误，代价高；旧快照只有 2 个（`data/*.snapshot`），重建成本可控 |
| **D-I6** | 旧 API（`QueryExecutor` / `Index::add`）是否标 `#[deprecated]`？ | **本轮不加**。内部 bench / 测试仍在用，`#[deprecated]` 会因 `-D warnings` 打断 CI；等 P7 完成再评估 |
| **D-I7** | `batch_size` 默认值？ | **不预设，实测校准**（32 / 64 / 128 / 256 四档，T2Ranking 12K 语料） |

---

## 11. 风险与已知代价

| # | 风险 / 代价 | 影响 | 缓解 |
| --- | --- | --- | --- |
| R1 | **`raw_vectors` 内存翻倍**：向量索引 + 原始向量两份（12K×512 约 24.6MB → ~49MB） | NFR-05 | 快照策略 D1 要求存原始向量，无法回避。提供 `.keep_raw_vectors(false)` 逃生舱（省内存，代价是无法保存含向量快照）；**NFR-05 必须重新实测并记录** |
| R2 | **写缓冲让 `add` 延迟有长尾**（偶发一次 batch embed） | 写入端体验 | 文档明确写明；提供 `add_documents` 批量路径；`helix build` 走批量路径，不受影响 |
| R3 | **`commit()` 后首次 `add` 不会触发深拷贝**（所有权切换方案下 refcount 恒为 1） | —— | 已通过 6.4 方案 A 规避；若将来升级到方案 C 需重新评估 |
| R4 | **泛型 `TryInto<SearchMode>` 签名可能编译不过** | I-06 | 有退化方案（两个方法）；I-06 第一步就验证 |
| R5 | **bench 迁移改变向量插入顺序 → HNSW 图不同 → 评测数字变化** | 评测可复现性 | 验收只要求"结论一致 + 差异在抖动容差内"；**brute 后端要求逐位一致**（10.1） |
| R6 | **breaking change 与版本**：`Document` 语义变化、`FORMAT_VERSION` 变化 | 用户 | 项目当前 0.1.0，语义化版本允许 breaking；CHANGELOG 需显式记录迁移步骤 |
| R7 | **过度隐藏导致排查困难**：chunk/embed 全藏起来后，用户遇到"这段话为什么没召回"失去抓手 | 可观测性 | 保留 `--explain`（FR-13）全部能力；新增 `Searcher::config_report()` 打印 analyzer / chunker / embedder / chunk 参数实际取值 |
| R8 | **范围蔓延**：接口重构容易顺手改成"重写引擎" | 工期 | 第 3.2 节非目标清单是硬边界；任何超出清单的改动须先更新本文档 |

---

## 12. 参考

- issue #1 原文：https://github.com/Gong-Yubo/helix-index/issues/1
- 需求定义：`docs/devel/requirements-spec.md`（FR-12 / FR-13 / FR-14 / FR-15 / FR-17 / NFR-02 / NFR-03 / NFR-05）
- 架构边界：`docs/devel/architecture-design.md` 4.2（模块职责）、ADR-001~008
- 评测口径与抖动容差：`docs/devel/eval-report.md` 3.4（R-P5-13）、第 8 节（NFR 实测）
- 收尾现状：`docs/devel/v1-finish-design.md`
- 业界参照：tantivy 0.26 docs（IndexWriter / commit / TopDocs）、
  Chroma Embedding Functions（函数挂集合、add/query 自动向量化）、
  LanceDB Embedding Guide（SourceField / VectorField 声明式、`WithEmbeddingsScannable` 流式、
  `compute_source_embeddings` / `compute_query_embeddings` 两路分离）
