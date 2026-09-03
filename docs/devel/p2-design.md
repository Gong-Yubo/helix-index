# P2 向量链路 — 设计说明

> ⚠️ **历史文档，正文保留原名未改写**：项目已于 2026-09-03 正式定名 **HelixIndex**
> （库 crate `index-core` → `helix-core`，CLI 命令 `idx` → `helix`）。
> 文中出现的旧名均指改名前的同一项目，未逐处改写以保留历史原貌（D-E4）。

| 项目    | 内容                                                                      |
| ----- | ----------------------------------------------------------------------- |
| 版本    | **v1.1（已执行并回写）**                                                                          |
| 日期    | 2026-09-02                                                                              |
| 状态    | **已执行完成** —— 全部单测 + 语义召回 + 万条重合率通过，见第 13 章执行记录                                 |
| 上游    | `docs/devel/plan.md` 第 7 章（P2，T2-01 ~ T2-10）、`architecture-design.md` 第 5.3 / 7.3 / 7.5 节 |
| 关联    | ADR-003（fastembed）、ADR-005（HNSW）、风险 R2（BGE 前缀）、风险 R3（L2 归一化）                |
| 环境    | Rust 1.90.0 / Apple M5 / P0、P1 已完成                                                    |

---

## 1. P2 要达成什么

**一句话**：打通 `embedding → 归一化 → HNSW → 向量召回`，让**字面完全不匹配但语义相关**的结果能被召回——这是 BM25 做不到的，也是混合检索存在的前提。

**判定标准（Gate）**

1. `idx search --mode vector` 对同义改写查询召回正确文档（P2 出口 Demo）
2. `cos = 1 − d²/2` 与点积换算误差 **< 1e-5**
3. **BGE 查询侧 instruction 前缀存在且只作用于查询**（R2 防护单测）
4. 暴力与 HNSW 在 1 万条内 **Top-10 重合率 ≥ 95%**
5. `make fmt && make lint && make test` 全绿

> P2 与 P1 的不同：P1 的正确性靠**手算值 + tantivy 对照**（数学可证）；P2 的正确性**没有唯一正确答案**——向量召回好不好，取决于 embedding 质量与语料匹配。所以 P2 的验证重心是**工程正确性**（前缀、归一化、距离换算、HNSW 与暴力的一致性），而非"效果好不好"（那是 P5 评测的事）。

---

## 2. 现状

| 项         | 状态                                        |
| --------- | ----------------------------------------- |
| P1 产出     | analyze / chunk / index / retriever(bm25) / CLI / 30 篇语料 |
| `embed` 模块 | 空骨架（P0 建的占位）                               |
| `vector` 模块 | 空骨架                                        |
| feature   | `local-embed` 已在 `Cargo.toml` 声明（`default`）   |
| `embed_smoke` 示例 | ✅ P0 已验证 fastembed 能下载模型并推理（dim=512, norm=1.0, 1.58ms/条） |

---

## 3. 关键事实：fastembed 与 instant-distance 源码核对

以下都是从**本机源码**（`~/.cargo/registry/...`）读出的结论，不是推断。

### 3.1 fastembed 6.0.2 —— 三个决定性事实

| 事实 | 出处 | 对设计的影响 |
| --- | --- | --- |
| **`BGESmallZHV15` 用 `Pooling::Cls`**，`model_code = "Xenova/bge-small-zh-v1.5"` | `models/text_embedding.rs:186,271` | 无需关心 pooling，交给 fastembed |
| **输出已 L2 归一化**（`text_embedding/output.rs:49` 的 `.map(normalize)`，`common.rs:226`） | P0 smoke 实测 `norm = 1.000000` 佐证 | 见 3.2 的 D2 防御性归一化 |
| **没有 `query_embed`，也不会加 BGE 查询 instruction 前缀** | 全库搜索 `为这个句子生成表示` 无结果；`TextEmbedding` 只有 `embed()` | **R2 是真的**：前缀必须我们自己加，见 D1 |

**关键 API 签名**（P0 已核实，此处复用）：

```rust
TextEmbedding::try_new(InitOptions::new(EmbeddingModel::BGESmallZHV15)) -> Result<Self>
embed<S: AsRef<str> + Send + Sync>(&mut self, texts: impl AsRef<[S]>, batch_size: Option<usize>)
    -> Result<Vec<Vec<f32>>>
```

> ⚠️ `embed` 要 `&mut self`，且 `Embedder` trait 要求 `Send + Sync`。因此 `LocalEmbedder` 内部用 `Mutex<TextEmbedding>` 包裹（fastembed 的 `TextEmbedding` 本身是否 `Sync` 未在文档承诺，用 `Mutex` 最稳妥）。这也天然把 `embed_documents` / `embed_query` 的调用串行化——fastembed 内部是同步推理，串行没问题。

### 3.2 instant-distance 0.6.1 —— API 与两个坑

| 事实 | 出处 / 说明 |
| --- | --- |
| `Point: Clone + Sync { fn distance(&self, other: &Self) -> f32 }`，**distance 越小越近** | `lib.rs:780` |
| `Builder::default().ef_construction(n).ef_search(n).seed(u64).build(points, values) -> HnswMap<P, V>` | `lib.rs:33-99` |
| `HnswMap::search(&self, p: &P, s: &mut Search) -> impl Iterator<Item=MapItem>` | `lib.rs:154` |
| `MapItem { distance: f32, pid: PointId, point: &P, value: &V }` | `lib.rs:175` |
| `Search` 是复用 scratch；**search 内部自己设 `ef`**（取 Builder 的 `ef_search`），`Search::default()` 即可 | `lib.rs:352-383` |
| **无 `insert` 方法** → 主索引 + delta 暴力区（ADR-005） | 全库无 `pub fn insert` |
| `M = 32` 固定；`HnswMap` 内部会**重排 values 对齐点序**，`MapItem.value` 已正确对应 | `lib.rs:788,144-151` |

**两个坑**

1. **无增量 insert**：HNSW 必须一次性构建。→ 沿用架构 7.5 的"主索引 + delta 暴力区"。但见 D3：P2 的 CLI 场景是 build-then-search，全部向量在 build 时已知，**直接用一次性构建的 HNSW（或暴力）即可，暂不需要 delta**；delta 留给 P4 的增量场景。
2. **HNSW 构建带随机性**（`Builder::seed` 默认 `rand::random()`）：必须 `.seed(FIXED)` 才能满足 NFR-06（D5，把 T3-08 的确定性要求提前到 P2 落实）。

---

## 4. 数据流

```
原始文档
  │
  ├─ Chunker ─► chunks（与 P1 共用）
  │
  └─ LocalEmbedder.embed_documents(chunk.text) ─► Vec<NormalizedVector>   ← 入库侧**不加**前缀
                 │
                 ▼
        VectorIndex（HnswIndex / BruteForceIndex）
                 │
查询 ─► LocalEmbedder.embed_query(q) ─► NormalizedVector                    ← 查询侧**加**前缀
                 │
                 ▼
        VectorRetriever ─► distance → score(=cos) → Top-K ─► Vec<Scored>
```

---

## 5. 接口设计

```rust
// ---------- embed ----------
pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize;
    /// 入库侧：**不加** instruction 前缀
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// 查询侧：**加** instruction 前缀（BGE 要求，R2）
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
    /// 输出是否已 L2 归一化（LocalEmbedder 返回 true）
    fn is_normalized(&self) -> bool { false }
}

pub struct LocalEmbedder { inner: Mutex<TextEmbedding> }
const BGE_ZH_QUERY_PREFIX: &str = "为这个句子生成表示以用于检索相关文章：";

// ---------- vector ----------
pub struct NormalizedVector(Vec<f32>);
impl NormalizedVector {
    /// 构造函数强制 L2 归一化（幂等，即使输入已归一化也只多一次点积）
    pub fn new(raw: Vec<f32>) -> Self;
    pub fn cosine(&self, other: &Self) -> f32;   // = dot（两者均单位向量）
    pub fn dim(&self) -> usize;
}
impl instant_distance::Point for NormalizedVector {
    /// 返回**平方欧氏距离** d² = 2 - 2·cos（不开根号，单调性不变）
    fn distance(&self, other: &Self) -> f32 { self.distance_sq(other) }
}

pub trait VectorIndex: Send + Sync {
    fn add(&mut self, id: ChunkId, vec: NormalizedVector) -> Result<()>;  // 暴力/delta 支持，HNSW 不支持
    fn search(&self, query: &NormalizedVector, k: usize) -> Result<Vec<(ChunkId, f32)>>;
    fn len(&self) -> usize;
}

pub struct BruteForceIndex { entries: Vec<(ChunkId, NormalizedVector)> }   // 增量，精确
pub struct HnswIndex { map: HnswMap<NormalizedVector, ChunkId> }           // 一次性构建，近似

// ---------- retriever/vector ----------
pub struct VectorRetriever<'a> {
    embedder: &'a dyn Embedder,
    index: &'a dyn VectorIndex,
}
impl Retriever for VectorRetriever<'_> {
    fn search(&self, query: &str, k: usize) -> Result<Vec<Scored>>;  // score = 1 - d²/2 = cos
}
```

---

## 6. 任务设计

### T2-01 Embedder 抽象（`embed/mod.rs`）

`trait Embedder` 如上。**两个方法强制分开**（验收：接口无法用单一方法同时服务两侧），从类型层面堵住 R2。

### T2-02 本地 Embedder（`embed/local.rs`）

- `LocalEmbedder::new()`：`TextEmbedding::try_new(InitOptions::new(EmbeddingModel::BGESmallZHV15).with_cache_dir(...))`
  - 缓存目录**显式指到仓库外**（P0 遗留的 TODO，见 p0-design.md 12.4）：`~/.cache/index-demo/models`
- `embed_documents`：直接 `embed(texts, None)`
- `embed_query`：`embed(&[format!("{BGE_ZH_QUERY_PREFIX}{text}")], None)`
- **单测**：同一文本，`embed_query` 与 `embed_documents` 的向量**不相等**（前缀生效的 R2 防护）；且 `is_normalized()` 为 true
- ⚠️ 这个单测依赖模型下载（首次约 49s），放 `#[cfg(feature="local-embed")]` 下的**忽略测试**（`#[ignore]`）或独立 integration test，避免 `make test` 默认拉模型。设计决策：**向量相关的模型依赖测试统一 `#[ignore]`，用 `cargo test -- --ignored` 单独跑**。

### T2-03 Feature flags（`Cargo.toml`）

已就位。补：`remote-embed` 空 feature（占位）；`LocalEmbedder` 仅在 `local-embed` 下编译；未启用任何实现时，`embed_documents`/`embed_query` 返回 `Error::NoEmbedder`。

### T2-04 向量类型与距离对齐（`vector/point.rs`）

`NormalizedVector` 如上。**单测**：随机向量对，验证 `cos = 1 - d²/2` 与直接点积误差 < 1e-5；非归一化输入被强制归一化。

### T2-05 VectorIndex 抽象（`vector/mod.rs` + `hnsw.rs` + `brute.rs`）

- `BruteForceIndex`：`Vec` 线性扫描，增量，精确。
- `HnswIndex`：`HnswMap::new` 一次性构建；`Builder::default().ef_construction(200).ef_search(100).seed(FIXED_SEED)`。
- **单测**：1 万条随机向量，暴力与 HNSW 的 Top-10 重合率 ≥ 95%（T2-05 验收）。

### T2-06 向量 Retriever（`retriever/vector.rs`）

- `search`：`embed_query` → `index.search` → 距离转相似度（`score = 1 - d²/2`）→ 按 score 降序 + `chunk_id` 升序 tie-break → 截断 k。
- **单测**：Top-K 按相似度降序；空查询/空索引返回空。

### T2-07 批量摄入并行（`index/mod.rs` / CLI）

- rayon `par_iter` 并行 `embed_documents`（FR-19）。
- 记录 1 万 chunk 构建耗时（NFR-03 校准数据）。P2 语料只有 30 篇，此数字**仅做机制验证**，真实规模数据留 P5。

### T2-08 远程 Embedder（`embed/remote.rs`，**推迟**）

见 D4：P2 不实现 HTTP 版本，只保留 feature flag 与 `Error::NoEmbedder` 分支。

### T2-09 CLI 向量模式（`crates/cli`）

- `idx search --mode vector -k 10 "查询"`：load → chunk → 建文本索引 → **rayon 并行 embed** → 建向量索引 → embed_query → 检索 → 展示。
- 展示复用 P1 的 `Hit` 逻辑，但 `--mode vector` 不打印"匹配词"（向量无词项），改打印相似度。

### T2-10 两路隔离自查

- 静态确认 `retriever/bm25.rs` 与 `retriever/vector.rs` **互不 import**（架构 4.3）。
- 计划用 grep 命令纳入验收清单。

---

## 7. 单测设计

### 7.1 距离换算（T2-04，确定性、可手算）

```rust
// 三个单位向量，两两 cos 已知
let a = NormalizedVector::new(vec![1.0, 0.0]);
let b = NormalizedVector::new(vec![0.0, 1.0]);   // cos(a,b) = 0
// 期望：d²(a,b) = 2，score = 1 - 2/2 = 0
assert!((a.cosine(&b) - 0.0).abs() < 1e-6);
// 归一化幂等
let c = NormalizedVector::new(vec![3.0, 4.0]);   // 自动归一化到 (0.6, 0.8)
assert!((c.cosine(&NormalizedVector::new(vec![0.6, 0.8])) - 1.0).abs() < 1e-6);
```

### 7.2 BGE 前缀（T2-02，`#[ignore]` 模型依赖）

```rust
// embed_query("检索") 与 embed_documents(["检索"]) 的向量不相等
assert_ne!(embed_query("检索"), embed_documents(&["检索".into()])[0]);
```

### 7.3 暴力 vs HNSW 重合（T2-05）

1 万条 512 维随机向量，固定 seed，Top-10 重合率 ≥ 95%。

### 7.4 语义召回冒烟（T2-09，`#[ignore]`）

对 30 篇语料，`--mode vector` 查询"怎样让检索支持中文分词"应召回 `chinese-tokenization.md`（字面不含"怎样/让/支持"）。

---

## 8. 风险与兜底

| #      | 风险                               | 影响         | 兜底                                             |
| ------ | -------------------------------- | ---------- | ---------------------------------------------- |
| R-P2-1 | fastembed 的 `TextEmbedding` 不 `Sync` | 编译失败       | 已用 `Mutex` 包裹（设计预置）                              |
| R-P2-2 | BGE 前缀漏加 / 加错                        | 向量路效果受损    | `#[ignore]` 单测断言两侧向量不相等 + 评审                     |
| R-P2-3 | instant-distance 无增量 insert            | 无法逐条加       | build-then-search 一次性构建；delta 区留 P4（架构 7.5）        |
| R-P2-4 | HNSW 随机性破坏确定性                       | NFR-06 不达标  | `Builder::seed(FIXED)`                              |
| R-P2-5 | 30 篇语料向量检索无区分度                       | Demo 不亮眼    | 语料已按主题覆盖；若仍不理想，如实记录，不粉饰（P5 前不做效果承诺）        |
| R-P2-6 | 模型下载 / 推理在 CI 里慢                      | 测试变慢       | 模型依赖测试统一 `#[ignore]`，默认 `make test` 不拉模型            |

---

## 9. 验收清单

```bash
cd /Users/gongyubo/Code/mine/index-demo

cargo test -p index-core                      # 默认：不拉模型
cargo test -p index-core -- --ignored         # 含模型依赖的向量单测

cargo run -p idx -- search --input data/corpus.jsonl --mode vector -k 5 "怎样让检索支持中文分词"
# 期望：召回 chinese-tokenization.md（字面不含"怎样/让/支持"）

make fmt && make lint && make test && make deny   # 全绿

# 两路隔离自查
grep -rn "mod bm25\|bm25::" crates/core/src/retriever/vector.rs || echo "✅ 无交叉引用"
```

---

## 10. 执行批次

| 批次   | 任务                          | 说明            |
| ---- | --------------------------- | ------------- |
| 批次 1 | T2-01 Embedder trait + T2-03 feature | 纯接口 + 编译      |
| 批次 2 | T2-04 NormalizedVector（+单测）   | 距离换算，不依赖模型    |
| 批次 3 | T2-05 VectorIndex + HNSW/暴力（+重合单测） | 不依赖模型，随机向量     |
| 批次 4 | T2-06 VectorRetriever          | 不依赖模型（注入假 Embedder） |
| 批次 5 | T2-02 LocalEmbedder（+忽略单测）    | 首次触发模型下载       |
| 批次 6 | T2-09 CLI 向量模式 + T2-07 rayon | 端到端           |
| 批次 7 | T2-10 两路隔离自查                | grep + 评审       |

> 顺序设计：批次 1~4 **不依赖模型**（注入假 Embedder / 随机向量），先把类型与距离算对；模型只在批次 5 之后介入。这样即使模型下载出问题，前半段的工程正确性已经锁死。

---

## 11. 明确不做（P2 范围外）

- **远程 Embedder 的 HTTP 实现**（T2-08 推迟，见 D4）
- **delta 增量区**（build-then-search 用一次性构建；增量是 P4）
- 融合 / RRF（P3）、可解释性（P3）、query 缓存（P3）
- 向量维度 > 512 的模型切换（P5 视评测）
- HNSW 参数调优（P5 网格搜索）
- 效果评测与结论（P5，需要真实语料 + 标注）

---

## 12. 需要你确认的决策点

| #   | 决策点                          | 我的建议                                                     | 影响                                |
| --- | ---------------------------- | -------------------------------------------------------- | --------------------------------- |
| D1  | **BGE 前缀字符串与"只加查询侧"**        | 用官方 `为这个句子生成表示以用于检索相关文章：`；`embed_query` 加、`embed_documents` 不加 | R2 的核心；fastembed 实测**不自动加**，必须自己做      |
| D2  | **归一化策略**                    | 信任 fastembed 已归一化（源码实证），但 `NormalizedVector::new` **再归一化一次**（幂等，防上游变动） | 双保险；成本是一次点积，可忽略                  |
| D3  | **P2 的 CLI 向量检索用暴力还是 HNSW**  | **用 BruteForceIndex**（精确、确定性，30 篇规模足够）；HNSW 实现 + 单测但 P4 再接入 | 避免"为用而用"；HNSW 意义在规模               |
| D4  | **T2-08 远程 Embedder 推迟**      | 推迟。P2 只留 feature flag + `Error::NoEmbedder`，不做 HTTP     | 无实际远程服务需求时，做出来是死代码               |
| D5  | **HNSW 固定 seed（确定性提前）**      | `Builder::seed(FIXED_SEED)`，把 T3-08 的确定性要求提前到 P2       | NFR-06；HNSW 构建是随机的，不固定则结果不可复现       |
| D6  | **模型依赖测试统一 `#[ignore]`**    | 是。默认 `make test` 不拉模型；向量模型测试单独 `cargo test -- --ignored` | 保 CI/本地测试快；首次跑会下载 96MB             |
| D7  | **embed 缓存目录**               | `~/.cache/index-demo/models`（仓库外，落实 P0 遗留 TODO）          | 避免误提交 96MB 模型                     |

---

## 附录 A 源码对照位置（fastembed / instant-distance）

| 内容 | 位置 |
| --- | --- |
| `EmbeddingModel::BGESmallZHV15` = "Xenova/bge-small-zh-v1.5" | `fastembed/src/models/text_embedding.rs:46,271` |
| BGE 用 `Pooling::Cls` | `fastembed/src/text_embedding/impl.rs:186` |
| 输出 `.map(normalize)` | `fastembed/src/text_embedding/output.rs:49` |
| `normalize(v)` | `fastembed/src/common.rs:226` |
| `embed` 方法 | `fastembed/src/text_embedding/impl.rs:453` |
| `Point` trait / `distance` | `instant-distance/src/lib.rs:780` |
| `Builder` / `seed` / `build` | `instant-distance/src/lib.rs:33-99` |
| `HnswMap::search` / `MapItem` | `instant-distance/src/lib.rs:154,175` |
| search 内部自设 `ef` | `instant-distance/src/lib.rs:366-371` |
| `M = 32` / 无 insert | `instant-distance/src/lib.rs:788` |

## 附录 B 已知的 API 差异提醒（开发时注意）

- fastembed `embed` 需 `&mut self` → `LocalEmbedder` 用 `Mutex<TextEmbedding>` 包 `&mut`
- instant-distance `MapItem.value` 已按内部点序对齐，直接当 `ChunkId` 用，**不要**自己用 `pid` 反查
- instant-distance 的 `distance` 语义是"越小越近"，与"相似度越大越好"方向相反，`VectorRetriever` 里统一 `score = 1 - d²/2` 翻回来

---

## 13. 执行记录（v1.1 新增）

### 13.1 实测结果

| 验收项 | 结果 |
| --- | --- |
| 单元测试（默认 `make test`） | **37 个通过 + 3 个 `#[ignore]`** |
| 距离换算 | `cos = 1 - d²/2` 与点积误差 < 1e-5（100 组随机向量） |
| BGE 前缀 | `embed_query("检索") ≠ embed_documents(["检索"])` ✅（`#[ignore]`） |
| 输出归一化 | `norm = 1.0`（±1e-3）✅ |
| **万条 HNSW vs 暴力 Top-10 重合率** | **0.970**（release，`ef_search=200`） |
| 语义召回 Demo | `--mode vector` 查"怎样让检索支持中文分词" → **#1 = chinese-tokenization.md**（similarity 0.659，字面不含"怎样/让/支持"） |
| 两路隔离 | `vector.rs` 无 bm25 引用；`bm25.rs` 无 vector/embed 引用 ✅ |
| make fmt/lint/test/deny | 全绿 |

### 13.2 与设计的几处细化（均已落地，非方向性调整）

1. **T2-07 的"rayon 并行 embedding"实为不必要**。fastembed 单次 `embed()` 已通过 ONNX Runtime 的 **intra-op 并行**（默认用满全部 CPU 核）处理整批文本；再在外部套 rayon 分片并行，反而会因 `LocalEmbedder` 内部的 `Mutex` 串行化而失效。结论：**分词并行留 P4 规模化时做，embedding 直接单次批处理**。已在 `retriever` / CLI 落实。

2. **HNSW 万条测试在 debug 模式极慢**（2000 条就要 102s），拆成两个：默认的**快速冒烟**（n=100，几秒内验证自匹配 + 排序），和 `#[ignore]` 的**万条重合率**（`cargo test --release ... -- --ignored`）。

3. **`ef_search` / `ef_construction` 从 100/200 上调到 200/300**。初测 1 万条随机向量重合率仅 **0.93**，调高后 **0.97**。随机 512 维向量是比真实 embedding 更难的场景，真实数据召回会更高；参数仍属 P5 网格搜索范畴。

4. **`Index::add` 改为返回 `(DocId, Vec<ChunkId>)`**。CLI 向量模式需要"chunk_id ↔ 文本"对齐来建向量索引，原来 `add` 返回 `()` 拿不到分配后的 ID。

### 13.3 过程中自行解决的小问题

- `fastembed::TextEmbedding::get_model_info` 是**关联函数**（`TextEmbedding::get_model_info(&EmbeddingModel)`），不是实例方法——`model.get_model_info()` 编译不过
- `instant_distance` 的 `Search` 在 search 内部自己设 `ef`（取 Builder 的 `ef_search`），`Search::default()` 即可
- `.unzip()` 解构顺序与直觉相反（`(ChunkId, Vec)` → `(Vec<ChunkId>, Vec<Vec>)`）
- `HnswIndex::build` 的 `seed` 用 `0x5EED_2026_0902`（clippy 要求十六进制按 4 位分组）

### 13.4 遗留与交接

- `embed/remote.rs` 是占位（D4 推迟），`remote-embed` feature 空置
- delta 增量区（instant-distance 无 insert）留 P4，与 `hnsw_rs` 做 A/B（架构 7.5）
- HNSW 的 `ef_construction` / `ef_search` 参数 P5 网格搜索定稿
- 真实语料与效果评测（P5）——30 篇冒烟语料仅验证机制
