# P3 融合、编排与可解释性 — 设计说明

> ⚠️ **历史文档，正文保留原名未改写**：项目已于 2026-09-03 正式定名 **HelixIndex**
> （库 crate `index-core` → `helix-core`，CLI 命令 `idx` → `helix`）。
> 文中出现的旧名均指改名前的同一项目，未逐处改写以保留历史原貌（D-E4）。

| 项目    | 内容                                                                         |
| ----- | -------------------------------------------------------------------------- |
| 版本    | **v1.1（已执行并回写）**                                                                          |
| 日期    | 2026-09-02                                                                              |
| 状态    | **已执行完成** —— RRF 手算值 + 确定性 + compare/explain 全部通过，见第 12 章执行记录                             |
| 上游    | `docs/devel/plan.md` 第 8 章（P3，T3-01 ~ T3-11）、`architecture-design.md` 第 5.8 / 7.4 节 |
| 关联    | ADR-004（RRF k=60）、FR-10 / FR-11（融合）、FR-12（出处引用）、FR-13（空结果原因）、FR-20（query 缓存）、NFR-06（确定性）、NFR-07（可观测性） |
| 环境    | Rust 1.90.0 / P0、P1、P2 已完成                                                       |

---

## 1. P3 要达成什么

**一句话**：把 P1 的 BM25 和 P2 的向量两路合成一路，并让 Agent 能理解"为什么召回这条 / 为什么没结果"——这是把"能检索"变成"Agent 能用"的关键一跃。

**判定标准（Gate）**

1. `idx compare` 同屏对比三路（BM25 / Vector / Hybrid）
2. `--explain` 输出完整（matched_terms + 两路 rank/score + fused_score）
3. **同一 query 连续 100 次结果顺序完全一致**（NFR-06）
4. 空结果时 `empty_reason` 非空（FR-13）
5. `make fmt && make lint && make test` 全绿

> P3 的正确性**数学可证的部分只有 RRF 融合排序**（手算值），其余是"接口设计对不对"——explain 字段语义是否清晰、空结果原因是否可据以决策、确定性是否守住。所以 P3 的单测重心在 RRF 手算值 + 确定性，评审重心在 explain 语义。

---

## 2. 现状

| 项         | 状态                       |
| --------- | ------------------------ |
| P1 产出     | BM25 路（`Bm25Retriever`）   |
| P2 产出     | 向量路（`VectorRetriever`）   |
| `fusion` / `query` / `rerank` | 空骨架（P0 占位）              |
| `moka`     | 已在依赖中（`sync` feature）    |
| HNSW 固定 seed | ✅ P2 已落实（D5 提前）          |

---

## 3. 关键规格（从架构文档逐条核对）

### 3.1 返回类型（架构 5.8，唯一定义源）

```rust
pub struct Hit {
    pub chunk_id: ChunkId,
    pub doc_id: DocId,
    pub score: Score,                 // 融合后分数（Hybrid）/ 单路分数（单路模式）
    pub text: String,
    pub source: String,
    pub metadata: serde_json::Value,
    pub explain: Explain,
}
impl Hit { pub fn to_context_block(&self) -> String; }   // 直接拼 prompt（FR-12）

pub struct Explain {
    pub matched_terms: Vec<String>,
    pub bm25_score:  Option<Score>,   // None = 该 lane 未召回此文档
    pub bm25_rank:   Option<u32>,
    pub vector_score: Option<Score>,
    pub vector_rank:  Option<u32>,
    pub fused_score: Score,
}

pub struct SearchResponse {
    pub hits: Vec<Hit>,
    pub total_candidates: usize,
    pub empty_reason: Option<EmptyReason>,
    pub took: std::time::Duration,
}

pub enum EmptyReason {
    NoDocuments,       // 索引为空
    AllTermsUnmatched, // 词全部未命中 —— 提示 query 可能含幻觉词
    FilteredOut,       // 有候选但被 filter 全部过滤（P4 才真正用到）
}
```

> 关键语义（架构 4.5）：`bm25_rank` / `vector_rank` 用 `Option`，`None` 表示"该 lane 没召回这条"，是**诊断信号**——用 0 或 `usize::MAX` 会丢失这层语义。这条必须守住。

### 3.2 融合（架构 7.4 / ADR-004）

- **默认 RRF，`k = 60`，两路权重 1.0**。`score(d) = Σ w_i / (k + rank_i(d))`，`rank` 从 1 起，某 lane 未命中该文档则贡献 0。
- **为什么不是加权归一**：BM25 分数无上界、余弦集中在 [0.6, 0.95]，量纲不可比；RRF 只用排名，免疫量纲。
- **加权归一作备选（T3-02，Should）**：用「除以该 lane Top-1 分数」而非 min-max；单候选时退化为 1.0（FR-11）。
- **融合层不回捞正文**：融合只操作 `(chunk_id, score)`，正文回捞统一在融合与精排之后对 Top-K 做一次（架构 4.3 硬约束）。

### 3.3 并行与确定性（架构 7.6 / 7.7 / NFR-06）

- 两路**并行执行**，但融合前按 lane 名固定顺序收集——并行只影响耗时，不影响结果。
- 所有排序路径**先收集再按 `(score desc, chunk_id asc)` 排序**，不依赖 HashMap 迭代序。
- HNSW 固定 seed（P2 已落实）。「连续 100 次结果完全一致」是 P3 的硬验收。

---

## 4. 接口设计

```rust
// ---------- fusion ----------
/// 单路结果：已经按分数排好序的 (chunk_id, score)
pub type LaneResults = Vec<(ChunkId, Score)>;

pub trait FusionStrategy: Send + Sync {
    fn name(&self) -> &'static str;
    /// 融合多路结果，返回按 fused_score 降序的 (chunk_id, fused_score)
    fn fuse(&self, lanes: &[LaneResults], k: usize) -> Vec<(ChunkId, Score)>;
}

pub struct RrfFusion { k: f32, weights: Vec<f32> }
impl FusionStrategy for RrfFusion { /* score = Σ w/(k+rank) */ }

pub struct WeightedFusion;   // Should：除以 Top-1 分数，单候选→1.0

// ---------- rerank ----------
pub trait Reranker: Send + Sync {
    fn rerank(&self, _query: &str, hits: Vec<Hit>, _top_n: usize) -> Result<Vec<Hit>>;
}
pub struct NoOpReranker;    // 不改变顺序，仅把调用链留出来（FR-18）

// ---------- query ----------
pub enum SearchMode { Bm25, Vector, Hybrid }

pub struct Searcher<'a> {
    index: &'a Index,
    analyzer: &'a dyn Analyzer,
    embedder: Option<&'a dyn Embedder>,
    vector_index: Option<&'a dyn VectorIndex>,
    fusion: Box<dyn FusionStrategy>,
    reranker: Box<dyn Reranker>,
}
impl Searcher<'_> {
    pub fn search(&self, query: &str, mode: SearchMode, k: usize) -> Result<SearchResponse>;
}
```

**`Searcher` 的数据流**（Hybrid）：

```
query
 ├─ [并行] Bm25Retriever.search → LaneResults(bm25)
 └─ [并行] VectorRetriever.search → LaneResults(vector)
        │
        ▼
 fusion.fuse(&[bm25, vector], candidate_k)   ← 只操作 (chunk_id, score)
        │
        ▼
 对 Top-K 做一次正排回捞（chunk/doc/metadata）   ← 唯一一次正文回捞
        │
        ▼
 reranker.rerank(query, hits, k)              ← NoOp 留位
        │
        ▼
 SearchResponse { hits, empty_reason, took }
```

---

## 5. 任务设计

### T3-01 RRF 融合（`fusion/mod.rs` + `fusion/rrf.rs`）

- `RrfFusion::new(k=60, weights=[1,1])`
- `fuse`：对每路，`chunk_id → rank`（rank 从 1 起）；累加 `w/(k+rank)`；未命中贡献 0
- 输出按 fused_score 降序 + chunk_id 升序 tie-break
- **单测（手算值）**：构造两路已知 rank，验证融合顺序与 `w/(k+rank)` 累加一致

### T3-02 加权归一（`fusion/weighted.rs`，Should）

- 每路 score 除以该路 Top-1 分数；某路单候选（或空）时退化为 1.0
- 单测：单候选不除零；两路各归一化到 [0,1] 后相加

### T3-03 Searcher 编排（`query/searcher.rs`）

- `SearchMode::{Bm25, Vector, Hybrid}`
- 两路用 `rayon::join` 并行（vector 路内部有 Mutex，不影响正确性）
- **融合前按 lane 名固定顺序收集**（`["bm25", "vector"]`），不依赖执行完成顺序
- 单测：三种模式结果符合预期（bm25 模式不碰向量，vector 模式不碰 BM25）

### T3-04 结构化返回（`query/response.rs`）

- `Hit` / `Explain` / `SearchResponse` / `EmptyReason` 按 3.1 落地
- `to_context_block()`：`[source]` + 正文 + 可选的 explain 摘要（FR-12）
- 单测：context block 含出处与正文

### T3-05 可解释性（`query/explain.rs`）

- 组装 `Explain`：matched_terms（query 分词中命中的词）、两路 rank/score（Option 语义）
- 空结果时填 `empty_reason`：
  - 索引空 → `NoDocuments`
  - query 分词后空（全停用词）→ 归入 `AllTermsUnmatched`
  - 两路都无候选 → `AllTermsUnmatched`
  - `FilteredOut` 留 P4（过滤功能落地后启用）
- 单测：空结果时 `empty_reason` 非空

### T3-06 Reranker 留位（`rerank/mod.rs` + `rerank/noop.rs`）

- `trait Reranker` + `NoOpReranker`（原样返回）
- 单测：NoOp 不改变顺序

### T3-07 query embedding 缓存（`embed/cached.rs`）

- `CachedEmbedder` 包装 `Box<dyn Embedder>`，只缓存 `embed_query`（FR-20：Agent 多轮循环大量 query 重复）
- 用 **moka**（并发安全 + 有界容量），不用 `lru`（非并发安全，需套 Mutex，抵消并行收益）
- 单测：重复 query 命中缓存（计数）；并发命中无死锁

### T3-08 确定性（NFR-06）

- 全局自查：所有排序路径先收集再按 `(score desc, chunk_id asc)` 排序
- 单测：**同一 query 连续 100 次结果顺序完全一致**（Hybrid 模式）

### T3-09 可观测性（`query/metrics.rs`）

- tracing 输出：`took / bm25 / vector / candidates / fused` 五个字段
- 单测：检索日志样例可复现（字段齐全）

### T3-10 CLI compare / explain（`crates/cli`）

- `idx compare --input corpus.jsonl -k 5 "query"`：三路（bm25/vector/hybrid）同屏对比
- `idx search --mode hybrid --explain`：打印 explain 详情
- 输出可读

### T3-11 模块边界复查

- 重点：`fusion` 不回捞正文（只在 `query/searcher` 里对 Top-K 回捞一次）
- `query` 只编排、不实现算法

---

## 6. 单测设计

### 6.1 RRF 手算值（T3-01，可手算）

```
两路结果（rank 从 1 起）：
  bm25 路:  A(rank1) B(rank2) C(rank3)
  vector 路: B(rank1) D(rank2) A(rank3)
k=60, w=1：
  score(A) = 1/61 + 1/63 ≈ 0.016393 + 0.015873 = 0.032266
  score(B) = 1/62 + 1/61 ≈ 0.016129 + 0.016393 = 0.032522
  score(C) = 1/63 ≈ 0.015873
  score(D) = 1/62 ≈ 0.016129
期望融合顺序：B > A > D > C
```

### 6.2 确定性（T3-08）

同一 query、同一 Hybrid 模式，循环 100 次，逐次比较 `Vec<chunk_id>` 完全一致。

### 6.3 空结果原因（T3-05）

| 场景 | empty_reason |
| --- | --- |
| 空索引 + 任意 query | `NoDocuments` |
| 全停用词 query | `AllTermsUnmatched` |
| 词全未命中（BM25）+ 向量也无候选 | `AllTermsUnmatched` |

### 6.4 单路模式隔离（T3-03）

`SearchMode::Bm25` 不构造/不触碰 embedder 与向量索引；`SearchMode::Vector` 不触碰 BM25。

---

## 7. 风险与兜底

| #      | 风险                          | 影响        | 兜底                                        |
| ------ | --------------------------- | --------- | ----------------------------------------- |
| R-P3-1 | RRF 融合后大量同分导致排序不稳定         | NFR-06 不达标 | 所有排序路径显式 `chunk_id` tie-break               |
| R-P3-2 | explain 字段语义漂移（rank 用 0 而非 None） | 诊断信号丢失    | 架构 3.1 明示 Option 语义；单测断言"未召回 = None"       |
| R-P3-3 | 融合层越界回捞正文                  | 模块边界破坏    | 单测/评审：fusion 只吃 (chunk_id, score)           |
| R-P3-4 | query 缓存与模型升级冲突              | 旧向量残留     | 缓存无 TTL（模型固定）；模型切换时需清空（P5 处理，见 D3）      |
| R-P3-5 | 两路并行引入非确定性                | NFR-06 不达标 | 融合前按 lane 名固定顺序收集，与完成顺序无关               |
| R-P3-6 | `rayon::join` 与 `LocalEmbedder` 的 Mutex 死锁 | 挂起       | join 的两个闭包互不等待对方锁；vector 路单独持锁，无跨路锁嵌套 |

---

## 8. 验收清单

```bash
cd /Users/gongyubo/Code/mine/index-demo

cargo test -p index-core                       # 默认：RRF 手算值 + 确定性 + explain

cargo run -p idx -- compare --input data/corpus.jsonl -k 5 "向量检索和关键词检索的区别"
# 期望：三路（bm25 / vector / hybrid）同屏对比，hybrid 融合两路

cargo run -p idx -- search --input data/corpus.jsonl --mode hybrid --explain -k 3 "BM25 参数"
# 期望：每条带 matched_terms + 两路 rank/score + fused_score

make fmt && make lint && make test && make deny   # 全绿
```

---

## 9. 执行批次

| 批次   | 任务                                   | 说明                 |
| ---- | ------------------------------------ | ------------------ |
| 批次 1 | T3-01 RRF + T3-02 加权（+手算值单测）            | 融合层，纯数学             |
| 批次 2 | T3-04 response + T3-05 explain + T3-06 rerank | 返回类型与可解释性           |
| 批次 3 | T3-03 Searcher 编排（+三模式单测）               | 把两路接起来             |
| 批次 4 | T3-07 query 缓存（moka）+ T3-08 确定性 + T3-09 可观测 | 缓存 + 确定性 + tracing   |
| 批次 5 | T3-10 CLI compare/explain + T3-11 边界复查       | 端到端 + 评审            |

> 顺序设计：批次 1~2 **不依赖两路**（RRF 是纯数学，response/explain 是纯类型），先把融合与语义做对；批次 3 才把 BM25 与向量接进来。

---

## 10. 明确不做（P3 范围外）

- 过滤功能（`FilteredOut` 分支真正启用是 P4 / T4-07，P3 只定义枚举）
- Reranker 接真实模型（FR-18 明示第一版 NoOp）
- RRF 的 `k` 值调优（架构 R7：效果不佳时在 {20,60,100} 对比，P5 做）
- 快照落盘、增量、元数据过滤（P4）
- 真实语料评测（P5）

---

## 11. 需要你确认的决策点

| #   | 决策点                        | 我的建议                                                        | 影响                            |
| --- | -------------------------- | ----------------------------------------------------------- | ----------------------------- |
| D1  | **默认融合策略**                 | Hybrid 默认 **RRF k=60 权重 1.0**；加权归一仅作备选实现（T3-02）             | ADR-004 已定，此处确认落地              |
| D2  | **两路并行的实现**                | 用 `rayon::join`（rayon 已在依赖中），不引入新线程库                         | 并行只影响耗时，不影响结果（融合前固定顺序收集）    |
| D3  | **query 缓存的 TTL**          | **无 TTL + 容量 4096**（query→向量确定，模型固定；moka 保证内存有界）             | 模型升级需清缓存（P5 处理）；若你担心，可设 TTL 30min |
| D4  | **`idx compare` 是 P3 的调试主入口** | 是。三路同屏对比是后续调参最常用的命令，值得在 P3 做好                        | 比 search 更复杂，但一次性投入           |
| D5  | **EmptyReason 的 `FilteredOut`** | 枚举先定义，但 P3 不触发（过滤是 P4），避免死分支                            | 保持枚举完整，P4 落地过滤后直接可用           |

---

## 附录 A 手算值复现脚本（RRF）

```python
k = 60
bm25  = {"A":1, "B":2, "C":3}
vec   = {"B":1, "D":2, "A":3}
def score(doc):
    s = 0.0
    if doc in bm25: s += 1/(k+bm25[doc])
    if doc in vec:  s += 1/(k+vec[doc])
    return s
for d in ["A","B","C","D"]:
    print(d, round(score(d), 6))
# B(0.032522) > A(0.032266) > D(0.016129) > C(0.015873)
```

---

## 12. 执行记录（v1.1 新增）

### 12.1 实测结果

| 验收项 | 结果 |
| --- | --- |
| 单元测试（默认 `make test`） | **56 个通过 + 3 个 `#[ignore]`** |
| RRF 手算值 | `B > A > D > C` 与 `w/(k+rank)` 累加逐条一致 |
| 确定性 | 同一 Hybrid query 连续 **100 次结果顺序完全一致** |
| `idx compare` | 三路同屏对比正常（BM25 命中字面 / Vector 命中语义 / Hybrid 融合） |
| `--explain` | 匹配词 + 两路 rank/score（`Option` 语义正确） |
| 空结果原因 | 空索引→`NoDocuments`；全停用词→`AllTermsUnmatched` |
| 两路隔离 / fusion 边界 | fusion 无正文/索引引用；query 只编排 ✅ |
| make fmt/lint/test/deny | 全绿 |

### 12.2 与设计的几处细化（均已落地，非方向性调整）

1. **`Scored` → `LaneResults` 需要显式转换**。`Retriever::search` 返回 `Vec<Scored>`，`FusionStrategy::fuse` 吃 `LaneResults = Vec<(ChunkId, Score)>`。加了 `to_lane` 辅助函数桥接。

2. **融合候选预算 `candidate_k = k*3`**。融合时多取几倍候选，给精排（rerank）留余地；最终回捞仍只对 Top-K 做一次（守"融合后只回捞一次"的边界）。

3. **单路模式不经过 fusion**。`SearchMode::Bm25 / Vector` 直接把该路结果作为输出（分数即单路分数），只有 `Hybrid` 才调用 `fuse`。这样单路模式的分值语义清晰，explain 的 `fused_score` 就是该路分数。

4. **空结果判定**：索引空 → `NoDocuments`；query 分词后空或两路都无候选 → `AllTermsUnmatched`。`FilteredOut` 枚举已定义但 P3 不触发（过滤是 P4）。

### 12.3 过程中自行解决的小问题

- clippy `unnecessary_lazy_evaluations`：`ok_or_else(|| Error::...)` 改为 `ok_or(Error::...)`
- `rayon::join` 的两个闭包捕获 `&Bm25Retriever` / `&VectorRetriever` 均为 `Send`，编译通过
- 中文测试名含英文缩写触发 `non_snake_case`，测试模块加 `#![allow(non_snake_case)]`

### 12.4 遗留与交接

- `EmptyReason::FilteredOut` 未触发（P4 过滤落地后启用）
- RRF 的 `k` 值（60）与权重（1.0）在 P5 按评测校准（架构 R7）
- `CachedEmbedder` 已实现但**未接入 CLI**（CLI 每次检索重建索引，缓存跨进程无效）；P4 快照落地后、查询服务常驻时再接入
