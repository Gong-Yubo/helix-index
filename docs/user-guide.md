# HelixIndex 使用指南

面向两类读者：**CLI 使用者**（运维 / 评测 / 不想写 Rust 的人）与
**库接入方**（把 HelixIndex 嵌进自己服务的 Rust 开发者）。

项目定位与评测结论见 [`README.md`](../README.md)；
设计决策与实现细节见 [`devel/`](./devel) 下各文档。

## 目录

- [Part 1 — CLI 使用](#part-1--cli-使用)
  - [1.1 命令总览](#11-命令总览)
  - [1.2 build 参数](#12-build-参数)
  - [1.3 search 参数](#13-search-参数)
  - [1.4 compare 参数](#14-compare-参数)
  - [1.5 compact 参数](#15-compact-参数)
  - [1.6 bench 参数](#16-bench-参数)
  - [1.7 典型工作流](#17-典型工作流)
  - [1.8 数据格式约定](#18-数据格式约定)
  - [1.9 已知坑与性能预期](#19-已知坑与性能预期)
- [Part 2 — 库接入](#part-2--库接入)
  - [2.1 最小闭环](#21-最小闭环)
  - [2.2 六 trait 替换矩阵](#22-六-trait-替换矩阵)
  - [2.3 feature flags 选用](#23-feature-flags-选用)
  - [2.4 性能预期](#24-性能预期)

---

# Part 1 — CLI 使用

## 1.1 命令总览

| 命令 | 用途 | 典型场景 |
| --- | --- | --- |
| `build` | 摄入语料并落盘快照 | 建库（一次性 / 定期重建） |
| `search` | 单次检索（`--input` 重建 或 `--index` 加载快照） | 日常使用、调试 |
| `compare` | 三路同屏对比 | 调试"为什么某条没召回" |
| `compact` | 墓碑物理回收（compaction 重新物化 + ID 重编号） | 删除累积后回收磁盘 / 图体积 |
| `bench` | 效果与性能评测 | 调参、回归验证 |

> **短参约定**：`-i` = `--input`（语料），`--index`（快照）**没有短参**。
> 这是刻意的——两者都给 `-i` 会让 debug 构建直接 panic。
> 若你写 `helix xxx -i foo.jsonl`，拿到的永远是"语料"语义。

## 1.2 build 参数

```
helix build --input <语料> [--output <快照>] [--vectors] [--single-chunk] [--no-graph-persist]
```

| 参数 | 默认 | 说明 |
| --- | --- | --- |
| `-i, --input` | （必填） | 语料 JSONL，每行 `{"source","text","metadata?"}` |
| `-o, --output` | 无 | 快照输出路径。不指定则只在内存中构建（进程结束即弃） |
| `--vectors` | 关 | 同时 embed 并保存向量，**vector/hybrid 检索必需**；首次运行会下载模型（约 91MB） |
| `--single-chunk` | 关 | 每段落强制单 chunk。评测口径专用——段落级标注时会防"同一 passage 的多个 chunk 各占位次"的双计 |
| `--no-graph-persist` | 关 | 只写快照、**不写图 sidecar**。**磁盘总占用 1.6× → 1.0×**（12K：≈84MB → 52.3MB，即省下约 0.6× 快照体积），代价是下次冷启动要重建图（≈10s）。磁盘紧张或排查图问题时用 |

输出示例：

```
索引构建完成:
  文档数   = 12000
  分片数   = 12000
  词项总数 = 3200042
  平均分片 = 266.67
  embed 12000 条 耗时 188.52s（NFR-03 口径 = embed，不含 HNSW / 落盘）
  快照已写入 /tmp/step2-12k.idx（含向量，52.3 MB）耗时 95.34ms
  总耗时 203.20s（含 embed / 落盘 / 图 dump）
  图 sidecar dump 耗时 43.52ms（纯落盘，不含 HNSW 建图）
  图 sidecar 落盘 12000 点 / 31.6 MB（graph 7.9 MB + data 23.7 MB；快照 52.3 MB，磁盘增量 1.6×）
```

**建库会多出三个文件**（图 sidecar，V2 Step 2 起）：

```
foo.idx                  ← 快照（真源）
foo.idx.hnsw.graph       ← HNSW 图拓扑
foo.idx.hnsw.data        ← 向量副本（hnsw_rs 的 dump 只支持全量模式，省不掉）
foo.idx.hnsw.manifest    ← 发布点（91B）：记父快照 CRC + 两个文件的 CRC/长度
```

图是**快照的派生缓存，可随时删除**——删掉后下次加载自动重建（慢但结果正确）。
⚠️ **拷贝/分发索引时四个文件要一起带**：少任何一个都会降级到重建路径。
⚠️ **图 sidecar 不可跨平台搬运**（裸 f32 + native endian），换平台请只带 `foo.idx` 让它重建。

## 1.3 search 参数

```
helix search [--input <语料> | --index <快照>] [-m bm25|vector|hybrid] [-k N] [--filter f=v]... [--explain] <查询>
```

| 参数 | 默认 | 说明 |
| --- | --- | --- |
| `-i, --input` | — | 从语料现场构建（每次重建，适合小语料/调试） |
| `--index` | — | 从快照加载（秒级，生产用法）。**与 `--input` 二选一** |
| `-m, --mode` | `bm25` | `bm25` 关键词 / `vector` 语义 / `hybrid` 两路融合（**推荐**） |
| `-k, --k` | 10 | 返回条数 |
| `--filter` | 无 | 元数据等值过滤，`field=value`，**可重复**（多个条件为 AND） |
| `--explain` | 关 | 打印命中词 + 两路 rank/score，用于排查召回质量 |
| `<QUERY>` | （必填） | 查询文本（位置参数，放最后） |

`--explain` 输出示例（Agent 自查检索质量用，FR-13）：

```
#1  score=0.0410  [hybrid-search.md]
    bm25 rank=1 score=3.15 | vector rank=2 score=0.87 | matched: 检索, 融合
```

## 1.4 compare 参数

```
helix compare --input <语料> [-k N] <查询>
```

三路（bm25 / vector / hybrid）同屏对比，调试主入口。只接受 `--input`（每次重建），
不支持快照。

## 1.5 compact 参数

```
helix compact --index <快照> [--output <新快照>] [--dry-run] [--json]
```

墓碑物理回收（V2 Step 4）：把删除累积留下的**墓碑**（快照 `None` 槽、倒排死词、
`hnsw_rs` 无 remove 留下的图墓碑点）按存活集**重新物化 + 重编号**清掉，产出更紧凑的
普通快照。

| 参数 | 默认 | 说明 |
| --- | --- | --- |
| `--index` | （必填） | 要 compact 的快照路径 |
| `--output` | 无 | 另存到新路径（`atomic_write` 崩溃安全）。不写则**原地**覆盖 `--index` |
| `--dry-run` | 关 | 只打印墓碑统计 + 预估回收量，**不写任何文件** |
| `--json` | 关 | 机器可读输出（A/B 脚本透传；含 before/after、三体积、`remapped`、`graph_status`） |

> ⚠️ **load 会做配置指纹校验**：`helix compact` 用**默认装配**加载（含 bge embedder），
> 因此只能 compact「用默认装配建的库」——`build` 加过 `--vectors` 的库可以；
> 用自定义 analyzer / embedder 建的库会报 `ConfigMismatch`，需走库 API
> `SearchIndexBuilder::load`（装配好同款组件）再 `compact_and_save`。
> 纯 BM25 快照（无向量）也可 compact。

> ⚠️ **compaction 会重编号 `doc_id` / `chunk_id`（D-S4-01）**：跨 compact 的持久引用
> 会失效。检索本身不受影响（`Hit` 带 `source`/`text`/`metadata`）；但**任何以
> `doc_id`/`chunk_id` 为键的外部状态**（如你自己的增量删除记录）在 compact 后需用
> `source` / `content_hash` 重建。`remapped` 字段（或人类输出的"已重编号"提示）可观测
> 是否发生了重编号。

> ⚠️ 已知代价：load 需装配同款 embedder ⇒ 首次会加载模型（几秒到 ~49s 下载），但
> **不会**调 embed（只取模型建装配），不产生向量推理开销。

## 1.6 bench 参数

```
helix bench [--input <语料> | --index <快照>] [--queries <judgments>] [选项...]
```

| 参数 | 默认 | 说明 |
| --- | --- | --- |
| `-q, --queries` | `data/t2-queries.jsonl` | judgments 文件（含 graded 标注） |
| `--modes` | `bm25,vector,hybrid` | 参评模式，逗号分隔 |
| `-k, --k` | 10 | 指标 @K |
| `--k1` / `--b` | 定稿 1.5 / 0.75 | 覆盖 BM25 参数（网格搜索用） |
| `--rrf-k` | 60 | 覆盖 RRF k（默认取 `RrfFusion::default()`） |
| `--rrf-weights` | `1,1.5` | RRF 路权重 `w_bm25,w_vector`（默认取 `RrfFusion::default()`） |
| `--rel-threshold` | 1 | Recall/MRR 的相关性阈值（1 或 2）；**主表两列都输出** |
| `--grid` | 关 | BM25 k1×b 16 格网格搜索 |
| `--reps` / `--warmup` | 20 / 3 | 延迟测量的重复与预热次数 |
| `--analyzer` | `mixed` | 分词器；`charabia` 需以 `--features charabia` 编译 |
| `--vector-index` | `hnsw` | `brute` 用于诊断：隔离 ANN 近似误差（p5-design 8.6） |
| `--ef-search` | 200 | 覆盖 HNSW ef_search |
| `--runs` | 1 | >1 时每轮重建 HNSW 图，输出 run-to-run 抖动 |
| `--json` | 无 | 机器可读输出路径（含 per-query 明细） |
| `--no-latency` | 关 | 跳过延迟测量（只跑效果） |

> **重要**：`--rrf-k` / `--rrf-weights` 的默认值**从内核 `RrfFusion::default()` 派生**，
> 不是 CLI 硬编码。因此 `helix bench` 与 `helix search --mode hybrid` 的"默认融合"必然一致。

**日常评测请用脚本**（固化定稿参数，一键复现）：

```bash
make eval-quality                                   # 三路对照
make eval-quality ARGS="--grid"                     # 网格搜索
make eval-quality ARGS="--modes bm25 --analyzer charabia"
make eval-perf                                      # NFR 实测
make report CHECK=--check                           # 与 eval-report 逐格对账
```

## 1.7 典型工作流

### 建库 → 秒级检索（推荐生产用法）

```bash
# 建库（含向量，首次会下载模型 ~91MB）
helix build --input data/corpus.jsonl --output /tmp/demo.snapshot --vectors

# 之后每次检索都是秒级加载，不再重建
helix search --index /tmp/demo.snapshot --mode hybrid "如何加快检索速度"
```

### 元数据过滤

```bash
# demo 语料的每篇文档都带 metadata.topic
helix search --input data/corpus.jsonl --mode bm25 --filter topic=vector "向量检索"

# 多个 --filter 为 AND
helix search --input data/corpus.jsonl --filter topic=vector --filter lang=zh "检索"
```

### 可解释性（Agent 自查检索质量）

```bash
helix search --index /tmp/demo.snapshot --mode hybrid --explain "向量检索与BM25融合"
```

`--explain` 给出两路各自的 rank/score 与命中词。对 Agent 场景特别有用：
看到 `bm25_rank=None` 说明词面完全没命中（可能是 query 含幻觉词），
看到两路排名差异大说明该条处于两路的判断分歧区。

### 三路对比调试

```bash
helix compare --input data/corpus.jsonl -k 5 "为什么这条没召回"
```

### 效果评测

```bash
make eval-quality
```

## 1.8 数据格式约定

**语料 JSONL**（每行一个文档）：

```json
{"source": "vector-search.md", "text": "向量检索把文本编码成高维向量……"}
{"source": "hnsw.md", "text": "HNSW 是一种近似最近邻搜索的图结构算法……", "metadata": {"topic": "vector"}}
```

- `source`（必需）：出处，用于溯源与结果展示（FR-12）
- `text`（必需）：正文
- `metadata`（可选）：任意 JSON 对象，`--filter field=value` 可对顶层字段做等值过滤

**judgments JSONL**（仅 `bench` 评测用）：

```json
{"qid":"10100","query":"农村地理水井是哪个部门安装的",
 "relevance":[{"grade":3,"source":"380887"},{"grade":1,"source":"330036"}],
 "type":"paraphrase"}
```

- `relevance[].grade`：0~3 的分级标注（0 = 已判定负例）
- `type`：分桶标签（mixed / natural / exact / paraphrase），用于分桶统计

## 1.9 已知坑与性能预期

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `helix bench -i x.jsonl` 报"快照版本不兼容" | 你把语料传给了 `--input` 之外的位置；注意 `-i` 恒等于 `--input` | 语料用 `-i`，快照用 `--index` |
| 建库极慢（12K 段落 ~237s） | ort CPU 推理吞吐约 50 条/秒；NFR-03 已拆双口径——**首次全量 <240s 达标**，增量追加口径待 Step 6 | 见 [2.4 性能预期](#24-性能预期)；并行 embed / 增量构建在 V2 Step 6 |
| 加载快照后首次检索仍要等 ~10s | 图 sidecar **未命中**（文件被删 / 没一起拷贝 / 快照被重写过 / 跨平台搬运） | 看 stderr 的 `[向量图加载：⚠️ 降级重建（原因：…）]`；四文件一起带、或干脆只带 `foo.idx` 让它重建 |
| `vector`/`hybrid` 模式首次运行很久 | 首次下载 bge-small-zh-v1.5（约 91MB） | 缓存在 `~/.cache/helix-index/models`，仅首次 |
| 同一 query 两次跑分数略有不同 | 只在**重建图**时发生：两次**建库** / 降级重建 / `bench --runs` 的重建轮——HNSW 拓扑跨进程本就不同（R-P5-13） | 图持久化后**同一快照两次加载已逐位一致**；要观察抖动用 `--runs`（它刻意跳过图），要稳定结果就带齐四个文件走快路径 |

---

# Part 2 — 库接入

## 2.1 最小闭环

完整可运行示例见 [`crates/core/examples/search_basic.rs`](../crates/core/examples/search_basic.rs)：

```bash
cargo run -p helix-core --example search_basic
```

核心链路（P6 门面层）：

```rust
use helix_core::prelude::*;
use helix_core::search::SearchIndex;

// 1. 零配置装配：MixedAnalyzer + bge-small-zh + HNSW + RRF(60, 1:1.5)
let mut index = SearchIndex::builder().build();

// 2. 摄入（分块 / 倒排 / 向量化 / 幂等去重全在 add 背后）
index.add(
    Document::new(text)
        .with_source(source)
        .with_metadata(metadata),
)?;

// 3. 检索（只有 query 必选；默认 Hybrid + top_n=10）
let searcher = index.into_searcher()?;         // 'static + Clone + Send + Sync
let resp = searcher.search("如何加快检索速度")?;
// 需要定制时走 builder：
//   searcher.search_with(q).mode(SearchMode::Hybrid).top_n(3)
//       .filter(&Filter::eq("topic", "vector")).exec()?

// 4. 拼进 prompt：带出处与命中词（FR-12）
for hit in &resp.hits {
    println!("{}", hit.to_context_block());
}
```

对比 P6 之前：同一闭环过去要 79 行、手接 8 个对象、7 步，其中三步是
**静默正确性陷阱**（漏算 content_hash → 幂等失效；漏归一化 → 余弦分全错；
analyzer 没活到最后 → 分词器不一致）。现在这些全部由库保证，用户代码里
不再出现 `content_hash` / `NormalizedVector` / `&analyzer` / `Chunker`。

`to_context_block()` 产出形如：

```
[来源: vector-search.md]
向量检索把文本编码成高维向量……
(命中词: 加快, 检索, 速度)
```

## 2.2 六 trait 替换矩阵

内核的全部能力都抽象为 trait，通过 `SearchIndex::builder()` 按需替换：

| trait | 默认实现 | 注入点（builder 方法） | 替换场景 / 注意事项 |
| --- | --- | --- | --- |
| `Analyzer` | `MixedAnalyzer`（jieba + 自研过滤链） | `.analyzer(Arc::new(..))` | ⚠️ **索引侧与查询侧必须同款**（R4）。换分词后需重建索引；快照会记录分词器指纹，load 时校验 |
| `Embedder` | `LocalEmbedder`（bge-small-zh-v1.5） | `.embedder(Some(Arc::new(..)))` / `.embedder(None)` | 换模型 / 纯 BM25。向量维度由库校验，不一致报 `DimensionMismatch` |
| `VectorIndex` | `HnswRsIndex`（原生增量） | `.vector_backend(VectorBackend::Brute)` | 诊断/小语料用 brute；`ef_search` 由 `HnswRsIndex` 默认（200） |
| `FusionStrategy` | `RrfFusion`（k=60 / weights 1:1.5） | `.fusion(Arc::new(..))` | 换融合算法 |
| `Reranker` | `NoOpReranker` | `.reranker(Arc::new(..))` | 接 rerank 模型（P7） |
| `BM25 参数` | `Bm25Params`（k1=1.5 / b=0.75） | `.bm25_params(..)` | 网格搜索 |

**底层永远可达（逃生舱）**：需要逐 lane 自定义组装时，借用型
`QueryExecutor<'a>`（原 `Searcher`）与六个 trait 原样可用——
门面只是"默认装配"，不是"唯一路径"。

**关于 `Analyzer` 的"双实现"说明**：内核自带 `MixedAnalyzer`；
`charabia` 实现以 feature 隔离提供（T5-09 验证性），但在真实中文语料上
**全面落后自研链**（NDCG −5.9%），故保留自研链为默认。
替换路径本身已定义且可跑，只是不建议换。

## 2.3 feature flags 选用

| feature | 默认 | 说明 |
| --- | --- | --- |
| `local-embed` | ✅ 开 | 本地 embedding（fastembed + ONNX）。**关闭可显著缩小依赖树** |
| `remote-embed` | 关 | 远程 embedding（P2 预留） |
| `positions` | 关 | posting 存储位置信息（短语查询/高亮预备） |
| `charabia` | 关 | 分词对照实现，**验证性依赖，不要在生产开** |

只用关键词检索（不需要向量）时：

```toml
helix-core = { version = "0.1.0", default-features = false }
```

可省掉 fastembed / ort / ONNX Runtime 一整条重依赖链（CI 有对应 job 守着这个能力）。

## 2.4 性能预期

全部为 **release + macOS aarch64 + 12K 段落**实测值
（详见 [`devel/eval-report.md`](./devel/eval-report.md) 第 8 节，请勿凭直觉外推）：

| 项 | 实测 | NFR 目标 | 状态 |
| --- | --- | --- | --- |
| BM25 延迟 | P50 1.13ms / P99 3.90ms | < 5ms | ✅ |
| 向量延迟 | P50 4.26ms / P99 8.37ms | < 10ms | ✅ |
| 混合延迟 | P50 4.41ms / P99 8.70ms | P99 < 20ms | ✅ |
| 快照加载（含位图/字段索引重建） | 57~77ms | < 2s | ✅ |
| HNSW 图加载（图持久化后） | 23~37ms | — | ✅ |
| **完整冷启动**（上两项之和） | **≈100ms**（实测 82~100ms） | < 2s | ✅（改造前 ≈11.6s，约 116×） |
| 图重建（**降级路径**，图 sidecar 缺失/损坏时） | ≈10~11.5s | — | ⚠️ 仅降级时触发，单次有 ±15% 波动 |
| **构建含 embedding** | **236.7s（12K 段落）** | < 240s（NFR-03 首次全量口径，2026-09-07 拍板双口径；原 <120s 不可达） | ✅ 达标（增量追加口径待 Step 6） |
| 峰值内存 | 383MB（整进程，含模型+ort） | — | 向量本身仅 24.6MB |
| save（原子写含 fsync） | +0.027~0.035s（54.5MB 快照，V2 Step 3 实测） | — | 崩溃一致性代价，一次性落盘可忽略 |

**构建慢的成因与对策**：ort CPU 推理批量吞吐约 50 条/秒（非超线性），
12K 段落即 ~234s。当前适用于**一次性冷建库**；若你的场景需要频繁重建，
V2 规划了并行 embed（Step 6，先 E1/E2/E3 spike）/ 增量构建（Step 6，
NFR-03 第二口径）两条路径。

## 2.5 检索模式怎么选

| 场景 | 建议 |
| --- | --- |
| 术语精确匹配（代码符号、专有名词、ID） | `bm25` |
| 同义改写、语义泛化（"怎么加速" ↔ "索引优化"） | `vector` |
| 通用 / 不确定 | **`hybrid`** |

依据（T2Ranking 320 query 实测）：hybrid 的 **MRR@10 = 0.6922** 与
**Recall@10 = 0.6829** 均为三路最高——对 Agent 场景（无翻页、首条即决定）
最关键的正是这两项。hybrid 的 NDCG 略低于 vector 单路（0.5183 vs 0.5249）
但差异不显著（p=0.385）。
