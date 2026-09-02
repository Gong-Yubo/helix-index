# index-demo 开发计划

> 面向 Agent 场景的通用检索引擎内核（Rust，单机 MVP）

| 项目    | 内容                                                        |
| ----- | --------------------------------------------------------- |
| 版本    | v1.1                                                      |
| 创建日期  | 2026-09-02                                                |
| 状态    | **P0 已完成**（提交 `5850545`）；当前阶段 P1                          |
| v1.1 变更 | 依据 `docs/devel/thirdparty.md` 调研结论回写：新增 T1-15（tantivy 对照）、T4-06a（向量索引 A/B）、T5-09（分词对照）；T0-03/T0-06 增加 MSRV 1.90 与 `cargo-deny`；T1-04 引入 `unicode-segmentation`/`unicode-normalization`；T3-07 缓存改 `moka`；T4-01 改 `bincode 3.0.0`；附录 B 依赖清单同步至实测版本 |
| 上游文档  | `docs/devel/requirements-spec.md`（需求）、`docs/devel/architecture-design.md`（架构） |
| 阶段划分  | P0 → P5 为第一版；P6 为 v2                                      |

---

## 1. 本文档的定位

| 文档                       | 回答的问题                     |
| ------------------------ | ------------------------- |
| `docs/devel/requirements-spec.md`   | 做什么 / 为什么 / 怎么验收          |
| `docs/devel/architecture-design.md` | 怎么做 / 为什么这么做（trait、算法、ADR） |
| **`plan.md`（本文档）**       | **按什么顺序做 / 每个任务具体干什么 / 做到什么程度算完成** |

**与架构文档第 11 章的关系**：架构文档 11.1 只给了阶段纲要与验收标准（纲要级）；本文档把它展开成**可执行、可勾选的任务清单**（任务级），并补充了架构文档不写的工程细节：命令、目录、测试点、阶段门槛、并行工作流。

**更新规则**：任务完成勾选 → 更新本文档的进度追踪表；若任务拆分发生变化，**只改本文档**，不改架构文档的阶段划分（除非阶段本身变了）。

---

## 2. 现状与阻塞项

| 项          | 状态                                                | 阻塞      | 解决动作（P0 内） |
| ---------- | ------------------------------------------------- | ------- | ---------- |
| Rust 工具链   | ✅ 已安装（stable 1.98.0；项目 pin 1.90.0）。原"未安装"为误判，见 `p0-design.md` 12.1 | 无 | T0-01      |
| crates.io  | ⚠️ 国内访问可能慢                                        | P0      | T0-02 配镜像  |
| ONNX 模型下载  | ✅ **P0 已验证通过**：`Xenova/bge-small-zh-v1.5` 96MB，直连 49s | 无      | T0-05 已完成。⚠️ 该仓 HF 未声明 License，见需求文档 8.3 P5 |
| 示例中文语料     | ❌ 待准备                                             | P5      | T1-14 起并行准备 |
| 评测集标注      | ❌ 待准备                                             | P5      | T5-02      |
| 仓库         | ❌ 无 git                                           | —       | T0-03 顺带初始化 |

> **T0-05 是本计划里最重要的一条前置验证。** fastembed 依赖 ONNX Runtime，Apple Silicon 上编译链（C++、CoreML）较重，且模型下载可能失败。这两件事在 P0 用 30 分钟验证掉，能避免 P2 阶段才发现路线不可行。

---

## 3. 开发原则（贯穿所有阶段）

**1. 完成定义（DoD）—— 一个任务只有全部满足才算完成**

- [ ] 代码写完且 `cargo build` 通过
- [ ] `cargo clippy -- -D warnings` 无警告
- [ ] `cargo fmt` 已格式化
- [ ] 相关单元测试已写且通过
- [ ] 未违反模块边界（见下条）
- [ ] 在 `plan.md` 勾选

**2. 模块边界是硬性检查项**（架构文档 4.2/4.3）

| 模块          | 禁止                             |
| ----------- | ------------------------------ |
| `analyze`   | 不知道索引的存在                       |
| `index`     | 不碰融合与排序                        |
| `vector`    | 不认识文本，只认 `Vec<f32>`           |
| `embed`     | 不知道索引的存在                       |
| `retriever` | 不与其他 lane 交互（BM25 与向量两路互不引用）   |
| `fusion`    | 不回捞正文（只在最后对 Top-K 做一次正排回捞）      |
| `query`     | 不自己实现算法，只编排                    |

违反边界**不会导致编译失败**，所以只能靠评审。每个阶段结束前按此表自查一遍。

**3. 质量属性冲突时的取舍顺序**（需求文档 6.3）

```
正确性/确定性  >  首条精确率 MRR  >  延迟 P99  >  召回率  >  资源占用
```

**4. 三个"不会报错但会毁掉效果"的坑，每个阶段都要回头确认**

1. BGE **查询侧加** instruction 前缀，**入库侧不加**
2. 向量入库前 **L2 归一化**（`cos = 1 − d²/2`）
3. **索引侧与查询侧共用同一 `Analyzer`**

**5. 提交粒度**：一个任务一次提交，提交信息格式 `P1/T1-06: 实现倒排表结构`。

---

## 4. 里程碑总览

| 阶段            | 主题                    | 任务数 | 依赖    | 关键产出                     | 关键路径 |
| ------------- | --------------------- | --- | ----- | ------------------------ | ---- |
| **P0**        | 工程骨架 + 依赖验证           | 7   | —     | 空工程可编译，三个关键依赖可拉取        | ✅    |
| **P1**        | BM25 链路（analyze→index） | 15  | P0    | `idx search --mode bm25` 出结果 | ✅    |
| **P2**        | 向量链路（embed→HNSW）      | 10  | P1    | 向量路出结果，同义改写可召回          | ✅    |
| **P3**        | 融合 + 编排 + 可解释          | 11  | P2    | `idx compare` 三路对比       | ✅    |
| **P4**        | 持久化 + 增量 + 过滤          | 10  | P3    | save/load 结果一致，增量立即可查    |      |
| **P5**        | 语料 + 评测 + 调参           | 9   | P4    | 实测指标 + BM25 参数定稿         |      |
| **P6**（v2）    | Rerank / MMR / 自研索引    | 5   | P5    | —                        |      |

**关键路径：P0 → P1 → P2 → P3**。这三步完成即具备完整检索能力；P4/P5 是工程化与验证，可与后续场景层工作并行。

**并行工作流（不占关键路径，尽早启动）**

```
P1 ─────────────────────────────────────────►
     语料采集与整理（T1-14 起）──────────────► P5
     评测集标注（T5-02）──────────────────► P5
```

---

## 5. P0 工程骨架与依赖验证

> **详细设计见 `docs/devel/p0-design.md`**（含本机环境实测、完整配置文件内容、风险兜底、验收命令、待确认决策点）。本章只保留任务纲要。

**目标**：证明"这条技术路线在本机跑得通"，并完成工程骨架。此阶段不写任何业务逻辑。

**阶段门槛（Gate）**：全部 7 个任务完成，且 T0-05 的模型下载 + 单句 embedding 验证**必须成功**。若失败，先定方案（镜像 / 换模型 / 转远程 embedding）再进 P1。

| ID    | 任务                        | 产出 / 动作                                                                                                                    | 验收                                              |
| ----- | ------------------------- | -------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------- |
| T0-01 | 安装 Rust 工具链               | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh -s -- -y --default-toolchain stable`                      | `rustc --version` ≥ **1.90**（NFR-08）；`cargo --version` 可用     |
| T0-02 | 配置 cargo 镜像               | `~/.cargo/config.toml` 写入 `[source.crates-io] replace-with = 'rsproxy-sparse'` 及 `[source.rsproxy-sparse] registry = "sparse+https://rsproxy.cn/index/"` | `cargo search serde` 秒级返回                       |
| T0-03 | 建 workspace 骨架 + git + 许可文件 | 根 `Cargo.toml`（workspace members）、`crates/core`、`crates/cli`、`.gitignore`（**不得忽略 `Cargo.lock`**）、`rust-toolchain.toml`（**pin 1.90**，NFR-08）、`LICENSE-MIT` + `LICENSE-APACHE`、`git init` | `cargo build` 空工程通过；`git log` 有初始提交；`Cargo.lock` 已入库 |
| T0-04 | **依赖可获取性验证**              | `cargo add` 以下依赖并 `cargo build`：`instant-distance`(with-serde) / `jieba-rs` / `unicode-segmentation` / `unicode-normalization` / `fastembed` / `rayon` / `bincode:3.0` / `serde` / `thiserror` / `smol_str` / `clap` / `tracing` / `moka` / `crc32fast` / `serde_json` / `anyhow`；dev 依赖 `tantivy`（体积大，可放到 T1-15 前验证） | 全部编译通过。⚠️ `fastembed` 依赖 ort，编译最慢，单独先验证        |
| T0-05 | **ONNX 模型下载 + 推理验证**（最高风险） | `crates/core/examples/embed_smoke.rs`：`LocalEmbedder` 对一句中文 `embed_query`，打印前 4 维与 L2 范数                                    | 模型下载成功（必要时用 `HF_ENDPOINT=https://hf-mirror.com`）；单句耗时记录；向量范数 ≈ 1.0 |
| T0-06 | 固化工程命令 + 依赖合规            | 根目录 `Makefile`：`make fmt` / `make lint`（clippy -D warnings）/ `make test` / **`make deny`**（`cargo-deny` License 白名单，NFR-09 / ADR-008）/ `make all`；`deny.toml` 白名单：`MIT, Apache-2.0, BSD-3-Clause, BSL-1.0, CC0-1.0, Zlib, ISC, Unicode-DFS-2016` | `make all` 一键跑通；故意引入 GPL 依赖时 `make deny` 失败 |
| T0-07 | 建模块骨架                     | 按架构文档 4.1 建 `analyze/ index/ retriever/ vector/ embed/ fusion/ rerank/ query/ storage/ chunk/` 各 `mod.rs`，`lib.rs` 统一声明      | `cargo build` 通过，`cargo doc` 可生成                 |

**P0 完成后技术决策落定**：若 T0-04/T0-05 任一失败，回到架构文档 ADR-003（本地 fastembed 默认）重新决策，可能切换为 `remote-embed` 优先。

---

## 6. P1 BM25 链路

> **详细设计见 `docs/devel/p1-design.md`**（含 tantivy 源码核对、手算值、决策点、执行记录）。本章只保留任务纲要。

**目标**：从零手写出一条完整可用的关键词检索链路，并用 CLI 在示例语料上跑出结果。

**阶段门槛**：BM25 分值与**手算值**一致；**与 `tantivy` 的排序对照通过（T1-15）**；索引侧与查询侧分词一致；`idx search --mode bm25` 有合理排序。

| ID    | 任务          | 产出文件                                              | 要点                                                                            | 验收                                  |
| ----- | ----------- | ------------------------------------------------- | ----------------------------------------------------------------------------- | ----------------------------------- |
| T1-01 | 基础类型        | `types.rs`                                        | `DocId/ChunkId/TermId = u32`，`Score = f32`                                     | 编译通过                                |
| T1-02 | 文档与分块模型      | `document.rs`、`chunk/`                            | `Document{source, metadata, content_hash}`；`Chunk{ordinal, text, char_start, char_end}` —— 偏移是溯源与 token budget 的基础 | 单测：分块偏移能还原原文子串                      |
| T1-03 | Chunker     | `chunk/mod.rs`                                    | 固定长度 + 重叠（默认 512 字 / 重叠 64）；按段落边界优先切                                          | 单测：重叠部分内容一致；拼接后不丢字                   |
| T1-04 | 中英分段与分词      | `analyze/segment.rs`、`analyze/jieba.rs`           | 分段用 **`unicode-segmentation`**（不手写 Unicode 边界判断），文本先过 **`unicode-normalization`** 做 NFC；中文 jieba-rs，拉丁 `[A-Za-z0-9]+` | 单测：中英混排文本分词结果符合预期；全角符号、emoji 不产生异常 term |
| T1-05 | Token 过滤器链   | `analyze/filter.rs`                               | 小写化 → 停用词 → 长度过滤；**英文词干化第一版不做**（架构文档 7.1）                                     | 停用词可配置                              |
| T1-06 | 停用词表        | `analyze/stopwords.rs`                            | 中英合并，保守裁剪；⚠️ 只影响 BM25 路，向量路用原文                                                | 单测："的/了/the" 被过滤，"云/网/库" 保留        |
| T1-07 | Analyzer 抽象 | `analyze/mod.rs`                                  | `trait Analyzer`，`analyze_query` **默认转发** `analyze_doc`                        | 单测：**两侧输出完全一致**（架构文档 12.1 关键测试）      |
| T1-08 | 倒排表结构       | `index/inverted.rs`、`index/posting.rs`            | `HashMap<SmolStr, TermId>` + `HashMap<TermId, Vec<Posting>>`；`Posting{chunk_id, tf, positions?}`，位置信息走 `positions` feature | 单测：插入/查询正确                           |
| T1-09 | 正排存储与墓碑      | `index/forward.rs`                                | `Vec<Option<Chunk>>` + `Vec<Document>`；墓碑位（FR-15）                              | 单测：删除后按 chunk_id 查不到                |
| T1-10 | 统计量维护       | `index/stats.rs`                                  | `df: HashMap<TermId, u32>`、`total_len: u64`、`num_chunks`；增删时增量更新               | 单测：增量删除后与全量重建结果**完全一致**（关键测试，防 R5 漂移） |
| T1-11 | BM25 打分     | `retriever/bm25.rs`                               | `idf = ln(1 + (N − df + 0.5)/(df + 0.5))`；`k1=1.2, b=0.75`（起点，P5 调参）；**OR 语义** | 单测：构造 5 篇文档，逐条比对**手算值**（最关键测试）        |
| T1-12 | Top-K 与执行方式  | `retriever/bm25.rs`                               | TAAT（Term-At-A-Time）+ 二叉堆取 Top-K；**分相同的按 `chunk_id` 升序 tie-break**            | 单测：Top-K 顺序稳定                        |
| T1-13 | Retriever 抽象 | `retriever/mod.rs`                                | `trait Retriever { fn search(&self, req) -> Result<Vec<Scored>> }`             | BM25 路实现该 trait                      |
| T1-14 | CLI 最小版（并启动语料准备） | `crates/cli`、`data/corpus.jsonl`             | `idx build --input data/corpus.jsonl --output index.idx`、`idx search --mode bm25 -k 10 "查询"`；同期启动示例语料采集（50~200 篇短文档） | CLI 在语料上返回合理结果                       |
| T1-15 | **BM25 与 tantivy 对照测试** | `tests/bm25_vs_tantivy.rs`（dev-dep `tantivy 0.26`） | **ADR-007**：同语料下用工业实现当 oracle，逐条比对 Top-K **排序**（分值允许 `k1`/`b` 与平均长度口径差异）。必须覆盖边界：空 query、全停用词 query、超长 query、`df=0` 的 term | 排序一致率 ≥ 95%；不一致的 case 逐条分析并给出解释    |

**P1 出口 Demo**

```bash
cargo run -p idx -- build --input data/corpus.jsonl --output /tmp/index.idx
cargo run -p idx -- search --index /tmp/index.idx --mode bm25 -k 5 "BM25 参数怎么调"
# 期望：5 条结果，带 source 与匹配词，分数递减
```

---

## 7. P2 向量链路

> **详细设计见 `docs/devel/p2-design.md`**（含 fastembed / instant-distance 源码核对、BGE 前缀决策、距离换算、决策点）。本章只保留任务纲要。

**目标**：打通 embedding → 归一化 → HNSW → 向量召回，使同义改写可被召回。

**阶段门槛**：向量路能召回字面完全不匹配但语义相关的结果；`cos = 1 − d²/2` 换算误差 < 1e-5。

| ID    | 任务              | 产出文件                         | 要点                                                                                          | 验收                                     |
| ----- | --------------- | ---------------------------- | ------------------------------------------------------------------------------------------- | -------------------------------------- |
| T2-01 | Embedder 抽象      | `embed/mod.rs`               | `trait Embedder`：`dim()` / `embed_documents()` / `embed_query()` / `is_normalized()`；**两个方法强制分开** | 接口无法被误用为单一方法                            |
| T2-02 | 本地 Embedder     | `embed/local.rs`             | fastembed + `BAAI/bge-small-zh-v1.5`（512 维）；**`embed_query` 加 instruction 前缀，入库侧不加**；输出 L2 归一化 | 单测：**断言前缀存在**（对比同一文本两侧向量不相等，R2 防护）       |
| T2-03 | Feature flags   | `Cargo.toml`                 | `default = ["local-embed"]`；`remote-embed`、`positions`；未启用任何实现时返回 `Error::NoEmbedder`         | `cargo build --no-default-features` 行为正确 |
| T2-04 | 向量类型与距离对齐        | `vector/point.rs`            | `NormalizedVector(Vec<f32>)` 实现 `instant_distance::Point`；入库前强制归一化                            | 单测：`cos = 1 − d²/2` 与点积误差 < 1e-5       |
| T2-05 | VectorIndex 抽象  | `vector/mod.rs`、`hnsw.rs`、`brute.rs` | `trait VectorIndex`；`HnswIndex`（instant-distance，**固定随机种子**）+ `BruteForceIndex`（delta 区与对照用） | 单测：暴力与 HNSW 在 1 万条内 Top-10 重合率 ≥ 95%   |
| T2-06 | 向量 Retriever    | `retriever/vector.rs`        | query 向量化 → 检索 → 距离转相似度 → Top-K                                                             | 单测：Top-K 按相似度降序                         |
| T2-07 | 批量摄入并行          | `index/mod.rs`               | rayon 并行分词与 embedding（FR-19）                                                                | 1 万 chunk 构建耗时记录（NFR-03 校准数据）           |
| T2-08 | 远程 Embedder（Should） | `embed/remote.rs`        | HTTP API 实现，feature 隔离                                                                      | 无 Key 时报错清晰                             |
| T2-09 | CLI 向量模式        | `crates/cli`                 | `idx search --mode vector -k 10`                                                             | 同义改写查询能召回正确文档                           |
| T2-10 | 两路隔离自查          | —                            | 确认 `retriever/bm25.rs` 与 `retriever/vector.rs` **无相互引用**（架构文档 4.3 硬约束）                       | 评审通过                                    |

**P2 出口 Demo**

```bash
cargo run -p idx -- search --index /tmp/index.idx --mode vector -k 5 "怎样让检索支持中文分词"
# 期望：能召回标题里没有"中文分词"但讲同一件事的文档
```

---

## 8. P3 融合、编排与可解释性

**目标**：把两路结果合成一路，并让 Agent 能理解"为什么召回这条 / 为什么没结果"。

**阶段门槛**：`idx compare` 可同屏对比三路；`--explain` 输出完整；同一 query 连续 100 次结果顺序完全一致。

| ID    | 任务                 | 产出文件                          | 要点                                                        | 验收                        |
| ----- | ------------------ | ----------------------------- | --------------------------------------------------------- | ------------------------- |
| T3-01 | 融合抽象与 RRF          | `fusion/mod.rs`、`fusion/rrf.rs` | `score(d) = Σ w_i / (k + rank_i(d))`，`k=60`，rank 从 1 起，未命中不贡献 | 单测：已知两路 rank → 融合顺序与手算一致 |
| T3-02 | 加权归一（Should）       | `fusion/weighted.rs`          | **用「除以该 lane Top-1 分数」而非 min-max**；单候选时退化为 1.0（FR-11）      | 单测：单候选不除零                |
| T3-03 | Searcher 编排        | `query/searcher.rs`           | 两路 **rayon 并行**；融合前按 lane 名固定顺序收集；`SearchMode::{Bm25, Vector, Hybrid}` | 单测：三种模式结果符合预期            |
| T3-04 | 结构化返回类型            | `query/response.rs`           | `Hit{chunk_id, doc_id, score, source, char_start/end, metadata, explain}`；`to_context_block()` 供直接拼 prompt | 单测：context block 含出处与正文  |
| T3-05 | 可解释性               | `query/explain.rs`            | `Explain{matched_terms, bm25_rank, vector_rank, lane_scores}`；`EmptyReason`（无结果时给出原因） | 单测：空结果时 `empty_reason` 非空 |
| T3-06 | Reranker 留位（FR-18） | `rerank/mod.rs`、`rerank/noop.rs` | `trait Reranker` + `NoOpReranker`；**第一版不接模型**，但调用链完整       | 单测：NoOp 不改变顺序             |
| T3-07 | query embedding 缓存 | `embed/cached.rs`             | **用 `moka`**（并发安全 + TTL），**不用 `lru`**（非并发安全，需套 `Mutex`，会抵消两路并行收益）。Agent 多轮循环中大量 query 重复（FR-20） | 单测：重复 query 命中缓存；并发命中无死锁 |
| T3-08 | 确定性（NFR-06）        | 全局                            | HNSW 固定 seed；所有排序路径先收集再按 `chunk_id` 排序，不依赖 HashMap 迭代序     | 单测：**连续 100 次结果完全一致**     |
| T3-09 | 可观测性（NFR-07）       | `query/metrics.rs`            | tracing 输出 `took / bm25 / vector / candidates / fused`      | 检索日志样例可复现                 |
| T3-10 | CLI compare / explain | `crates/cli`                | `idx compare`（三路同屏对比，调试最常用）、`--explain`                    | 输出可读                      |
| T3-11 | 模块边界复查             | —                             | 按第 3 节边界表逐项检查；重点：`fusion` 不回捞正文                            | 评审通过                      |

**P3 出口 Demo**

```bash
cargo run -p idx -- compare --index /tmp/index.idx -k 5 "Rust 里怎么做中文 BM25"
# 期望：三列结果对比，hybrid 综合两路优点
cargo run -p idx -- search --index /tmp/index.idx --mode hybrid --explain "..."
# 期望：每条带 matched_terms 与两路 rank
```

---

## 9. P4 持久化、增量写入与过滤

**目标**：进程重启后索引秒级恢复；写入后立即可查；支持元数据过滤。

**阶段门槛**：**向量索引 A/B 已定案（T4-06a），并据此实现 T4-06b**；save → load 后检索结果**完全一致**；增量写入后新文档立即可被检索；删除后统计量与全量重建一致；快照加载实测是否满足 NFR-04（决定要不要上 `rkyv`）。

| ID    | 任务           | 产出文件                        | 要点                                                                                                | 验收                       |
| ----- | ------------ | --------------------------- | ------------------------------------------------------------------------------------------------- | ------------------------ |
| T4-01 | 快照编解码         | `storage/codec.rs`          | Header：`magic "IDX1"` + `format_version: u32` + `crc32: u32`；正文用 **`bincode 2.0.1`**（原"对齐 instant-distance 内部 bincode"的说法有误，其 `with-serde` 只含 serde；且 **3.0.0 是玩笑发布**，不可用） | 手写字节流解析单测               |
| T4-02 | Section 读写   | `storage/snapshot.rs`       | `term_dict / postings / forward / stats / vectors` 五个 section，vectors 可选                            | save/load 跑通             |
| T4-03 | 版本与校验         | `storage/snapshot.rs`       | 版本不匹配 → `SnapshotVersionMismatch`；CRC 失败 → `SnapshotCorrupted`；**绝不静默读错**                         | 单测：造损坏文件触发两个错误          |
| T4-04 | 幂等 upsert    | `index/mod.rs`              | 按 `content_hash` 去重（FR-15）；重复 upsert 不产生重复 chunk                                                   | 单测：upsert 两次 chunk 数不翻倍 |
| T4-05 | 墓碑删除与统计量回滚    | `index/stats.rs`            | 删除时**重新分析该 chunk 文本**再回滚 df/total_len（而非按存储值反推，避免漂移）                                               | 单测：与全量重建的统计量逐项相等        |
| T4-06a | **向量索引 A/B：`hnsw_rs` vs instant-distance+delta** | `vector/hnsw_rs.rs`（第二个 `VectorIndex` 实现） | **先做 A/B 再决定方案**。`hnsw_rs` 0.3.4 纯 Rust、原生增量 `insert`、内建 `search_filter`、`DistDot` 要求入库前归一化（与 7.3 方案天然契合）。对比项：增量写入延迟、召回一致性、内存占用 | 输出对比结论；**若达标则走 T4-06b 并删除 delta 区设计** |
| T4-06b | 增量方案落地（二选一） | `vector/store.rs` | 若 A/B 未达标 → `sealed: Option<HnswMap>` + `delta: Vec<...>`，两处都查后合并取 Top-K，触发条件 `delta ≥ 1000 或 ≥ 总量 10%` 时重建；若达标 → 直接用 `hnsw_rs` 原生增量，**无 delta 区、无重建** | 单测：新写入内容立即可被检索到 |
| T4-07 | 元数据过滤（FR-14） | `query/filter.rs`           | tag 等值 + 数值范围；**融合前用 bitmap 过滤**，查询路径零 IO（NFR-02）                                                 | 单测：过滤后结果集为子集            |
| T4-08 | 单元测试补齐        | `tests/`                    | 快照 round-trip、幂等 upsert、删除后统计量（架构文档 12.1 三条）                                                      | 全部通过                     |
| T4-09 | 集成测试          | `tests/integration.rs`      | 端到端：摄入 → 提交 → 快照 → 加载 → 检索结果一致；并发：多线程并发检索验证 `Send + Sync` 与 `Search` 对象复用安全                      | `cargo test` 全绿          |

---

## 10. P5 语料、评测与调参

**目标**：用数据证明"混合检索确实比单路好"，并把所有 NFR 从目标值变成实测值。

**阶段门槛**：三路对照实验有数据；BM25 参数经网格搜索定稿；NFR-02~05 全部替换为实测值。

| ID    | 任务                | 产出                                  | 要点                                                                                | 验收                          |
| ----- | ----------------- | ----------------------------------- | --------------------------------------------------------------------------------- | --------------------------- |
| T5-01 | 示例语料补全            | `data/corpus.jsonl`（50~200 篇短文档）    | 建议用技术博客/文档/Rust 主题文章，保证与 embedding 模型域大致匹配（防 P3 风险：模型与语料域不匹配）                     | ≥ 50 篇，可被 CLI 构建            |
| T5-02 | 评测集标注             | `data/queries.jsonl`（30~50 条）       | **必须覆盖四类**：精确术语 / 同义改写 / 中英混合 / 长句自然语言问句；标注 query → 相关 chunk_id 列表；标注后交叉复核（防 P2 风险） | 四类齐全，复核通过                   |
| T5-03 | `idx bench` 实现    | `crates/cli` + `core/bench`         | 输出 Recall@10 / MRR@10 / NDCG@10 / P50 / P99 延迟                                     | 三种模式各跑一遍有完整输出               |
| T5-04 | **强制对照实验**        | `docs/devel/eval-report.md`               | BM25 only / Vector only / Hybrid(RRF) 三路对比，按查询类型分桶统计                                | 有表格与结论                      |
| T5-05 | BM25 参数网格搜索       | `docs/devel/eval-report.md`               | `k1 ∈ {1.0, 1.2, 1.5, 2.0}` × `b ∈ {0.3, 0.5, 0.75, 0.9}`，以 NDCG@10 选优。**不盲信英文经验值**（R6） | 参数定稿并写入默认值                   |
| T5-06 | NFR 实测校准          | 更新 `docs/devel/requirements-spec.md` 第 6 章 | 把 NFR-02~05 的目标值替换为实测值，或标注"未达标 + 原因"                                                | 需求文档不再有"待实测"标注              |
| T5-07 | README 与示例         | `README.md`、`examples/`             | 快速上手、架构速览、评测结论摘要                                                                   | 新人不看文档能跑通                   |
| T5-08 | 数据诚信              | `docs/devel/eval-report.md`               | 若混合未优于最优单路，**如实记录并分析原因**（语料太小 / 标注质量 / 模型域不匹配），**不得粉饰**                            | 结论与实际数据一致                   |
| T5-09 | 分词方案对照实验          | `analyze/charabia.rs`（feature 隔离）   | 用裁剪版 `charabia`（`default-features=false, features=["chinese"]`，MIT）实现第二个 `Analyzer`，与 `jieba-rs + 自研过滤链` 对比 NDCG@10 | 输出对比结论；若 charabia 明显更优则切换（切换成本 = 一个实现文件） |

---

## 11. P6（v2，仅列方向，不排期）

| ID    | 任务                              | 说明                                     |
| ----- | ------------------------------- | -------------------------------------- |
| T6-01 | Reranker 接入                     | `bge-reranker-v2-m3`，对 Top-N 精排，提升首条命中率 |
| T6-02 | 结果去重（MMR，FR-24）                 | 降低进 prompt 的冗余                         |
| T6-03 | token budget 裁剪（FR-25）          | 按 token 上限裁剪上下文块                       |
| T6-04 | 多 query 融合接口（FR-21）             | 为查询改写 / HyDE 留口子                       |
| T6-05 | 自研 HNSW 或评估 `arroy`             | 解决 `instant-distance` 无增量插入；⚠️ arroy 耦合 LMDB，与"内存+快照"冲突，需重新评估持久化 |

---

## 12. 测试与验收总表

单元测试（架构文档 12.1）与阶段的对应，**加粗两项最关键**：

| 测试            | 阶段  | 防护的问题          |
| ------------- | --- | -------------- |
| **BM25 手算值比对** | P1  | 公式实现偏差（不报错，只变差） |
| **BM25 与 tantivy 排序对照**（ADR-007） | P1 | 同上，但覆盖整份语料与边界情况，覆盖率远高于手算 |
| **删除后统计量与全量重建一致** | P4  | 统计量漂移（R5）      |
| 索引侧/查询侧分词一致   | P1  | R4             |
| BGE 查询侧前缀断言   | P2  | R2（最难自查）       |
| 相似度对齐（cos 换算） | P2  | 排序等价性          |
| RRF 融合排序      | P3  | 融合实现错误         |
| 确定性（连续 100 次） | P3  | NFR-06         |
| 快照 round-trip | P4  | 持久化静默损坏        |
| 幂等 upsert     | P4  | 重复写入导致索引膨胀     |

**每个阶段的统一出口命令**

```bash
make fmt && make lint && make test
```

---

## 13. 阶段门槛（Gate）汇总

进入下一阶段前必须全部满足：

| 门槛  | 条件                                                                 |
| --- | ------------------------------------------------------------------ |
| P0→P1 | Rust 工具链可用；三个关键依赖可编译；**ONNX 模型下载 + 单句推理验证成功**；workspace 骨架就绪         |
| P1→P2 | BM25 分值与手算一致；两侧分词一致；`idx search --mode bm25` 出合理结果；语料采集已启动             |
| P2→P3 | 向量路能召回同义改写；相似度对齐误差 < 1e-5；两路无相互引用                                       |
| P3→P4 | `idx compare` 三路可对比；`--explain` 完整；**连续 100 次结果完全一致**；融合层不回捞正文       |
| P4→P5 | save/load 结果完全一致；增量写入立即可查；删除后统计量与全量重建一致；集成测试全绿                        |
| P5→完结 | 三路对照实验有数据；BM25 参数定稿；NFR-02~05 全部为实测值；评测报告含诚信声明                        |

---

## 14. 进度追踪

> 每完成一个任务在此勾选，并更新"当前阶段"。

| 阶段 | 任务                     | 状态 |
| -- | ---------------------- | -- |
| P0 | T0-01 安装 Rust 工具链      | ⬜  |
| P0 | T0-02 配置 cargo 镜像      | ⬜  |
| P0 | T0-03 workspace 骨架 + git | ⬜  |
| P0 | T0-04 依赖可获取性验证          | ⬜  |
| P0 | T0-05 **ONNX 模型下载 + 推理验证** | ⬜  |
| P0 | T0-06 固化工程命令（Makefile） | ⬜  |
| P0 | T0-07 模块骨架              | ⬜  |
| P1 | T1-01 基础类型              | ✅  |
| P1 | T1-02 文档与分块模型            | ✅  |
| P1 | T1-03 Chunker           | ✅  |
| P1 | T1-04 中英分段与分词            | ✅  |
| P1 | T1-05 Token 过滤器链         | ✅  |
| P1 | T1-06 停用词表              | ✅  |
| P1 | T1-07 Analyzer 抽象       | ✅  |
| P1 | T1-08 倒排表结构             | ✅  |
| P1 | T1-09 正排存储与墓碑            | ✅  |
| P1 | T1-10 统计量维护             | ✅  |
| P1 | T1-11 BM25 打分            | ✅  |
| P1 | T1-12 Top-K 与 TAAT       | ✅  |
| P1 | T1-13 Retriever 抽象      | ✅  |
| P1 | T1-14 CLI 最小版 + 语料启动     | ✅  |
| P1 | T1-15 **BM25 与 tantivy 对照测试** | ✅  |
| P2 | T2-01 Embedder 抽象        | ✅  |
| P2 | T2-02 本地 Embedder        | ✅  |
| P2 | T2-03 Feature flags     | ✅  |
| P2 | T2-04 向量类型与距离对齐          | ✅  |
| P2 | T2-05 VectorIndex 抽象    | ✅  |
| P2 | T2-06 向量 Retriever      | ✅  |
| P2 | T2-07 批量摄入并行            | ✅  |
| P2 | T2-08 远程 Embedder        | ✅  |
| P2 | T2-09 CLI 向量模式           | ✅  |
| P2 | T2-10 两路隔离自查            | ✅  |
| P3 | T3-01 融合抽象与 RRF          | ⬜  |
| P3 | T3-02 加权归一               | ⬜  |
| P3 | T3-03 Searcher 编排        | ⬜  |
| P3 | T3-04 结构化返回类型            | ⬜  |
| P3 | T3-05 可解释性               | ⬜  |
| P3 | T3-06 Reranker 留位        | ⬜  |
| P3 | T3-07 query embedding 缓存 | ⬜  |
| P3 | T3-08 确定性                | ⬜  |
| P3 | T3-09 可观测性               | ⬜  |
| P3 | T3-10 CLI compare / explain | ⬜  |
| P3 | T3-11 模块边界复查             | ⬜  |
| P4 | T4-01 快照编解码              | ⬜  |
| P4 | T4-02 Section 读写         | ⬜  |
| P4 | T4-03 版本与校验              | ⬜  |
| P4 | T4-04 幂等 upsert          | ⬜  |
| P4 | T4-05 墓碑删除与统计量回滚         | ⬜  |
| P4 | T4-06a **向量索引 A/B（hnsw_rs vs instant-distance+delta）** | ⬜  |
| P4 | T4-06b 增量方案落地（按 A/B 结论二选一） | ⬜  |
| P4 | T4-07 元数据过滤              | ⬜  |
| P4 | T4-08 单元测试补齐             | ⬜  |
| P4 | T4-09 集成测试               | ⬜  |
| P5 | T5-01 示例语料补全             | ⬜  |
| P5 | T5-02 评测集标注              | ⬜  |
| P5 | T5-03 `idx bench` 实现     | ⬜  |
| P5 | T5-04 强制对照实验             | ⬜  |
| P5 | T5-05 BM25 参数网格搜索        | ⬜  |
| P5 | T5-06 NFR 实测校准           | ⬜  |
| P5 | T5-07 README 与示例          | ⬜  |
| P5 | T5-08 数据诚信               | ⬜  |
| P5 | T5-09 分词方案对照（charabia）  | ⬜  |

**当前阶段**：**P2 已完成**（2026-09-02），下一步 P3（融合、编排与可解释性）

**P0 实测基线**：工具链 1.90.0 / lockfile 414 包 / 轻依赖编译 7.3s / fastembed+ort 42.5s / 全量含 tantivy 24.2s / 模型下载 49.0s / **单条推理 1.58ms（debug）** —— 详见 `p0-design.md` 附录 A。

---

## 附录 A 命令速查

```bash
# 工具链
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable

# 工具链版本（NFR-08：1.90+，rust-toolchain.toml 已固定）
rustc --version

# 每日开发循环
make fmt        # cargo fmt
make lint       # cargo clippy --all-targets -- -D warnings
make test       # cargo test
make deny       # cargo-deny：依赖 License 白名单校验（NFR-09 / ADR-008）
make all        # 以上四条

# 分阶段验证
cargo run -p idx -- build   --input data/corpus.jsonl --output /tmp/index.idx
cargo run -p idx -- search  --index /tmp/index.idx --mode bm25   -k 5 "查询"
cargo run -p idx -- search  --index /tmp/index.idx --mode vector -k 5 "查询"
cargo run -p idx -- compare --index /tmp/index.idx -k 5 "查询"
cargo run -p idx -- search  --index /tmp/index.idx --mode hybrid --explain "查询"
cargo run -p idx -- bench   --index /tmp/index.idx --queries data/queries.jsonl

# 依赖与 feature 组合验证
cargo build --no-default-features
cargo build --features remote-embed
cargo build --features positions
```

## 附录 B 依赖清单（P0 验证后锁定版本）

> 全部版本于 2026-09-02 经 crates.io API 实测；License / MSRV / 候选对比详见 `docs/devel/thirdparty.md` 与架构文档 9.1。

| 依赖                          | 版本       | 用途                | 备注                                                   |
| --------------------------- | -------- | ----------------- | ---------------------------------------------------- |
| `instant-distance`          | 0.6.1    | HNSW 向量索引         | `with-serde`；**无增量插入 API**（R1）。**P4 与 `hnsw_rs` A/B** |
| `hnsw_rs`                   | 0.3.4    | 向量索引 A/B 候选（T4-06a） | 纯 Rust、原生增量 `insert`、`search_filter`、`DistDot` 要求入库前归一化 |
| `fastembed`                 | 6.0.2    | 本地 embedding      | Apache-2.0；⚠️ ① 传递依赖 `ort =2.0.0-rc.13`（预发布），`Cargo.lock` 必须入库；② 必须 `default-features = false` + 两个 TLS feature，否则引入 NCSA 许可的图像处理链；③ 默认把模型下到项目根 `.fastembed_cache/`（已 gitignore，P2 应改 `with_cache_dir`） |
| `jieba-rs`                  | 0.10.3   | 中文分词              | MIT                                                  |
| `unicode-segmentation`      | 1.13.3   | 中英分段              | 不手写 Unicode 边界判断                                     |
| `unicode-normalization`     | 0.1.25   | 文本 NFC 归一化        | 保证索引侧与查询侧一致                                          |
| `rayon`                     | 1.12     | 并行                | 两路召回并行、批量摄入并行                                        |
| `bincode`                   | **2.0.1** | 快照序列化             | ⚠️ **不可用 3.0.0**（玩笑发布，源码仅一行 `compile_error!`）；P4 末与 `rkyv` 实测对比 |
| `serde` / `serde_json`      | 1.0      | 序列化 / 元数据         |                                                      |
| `smol_str`                  | 0.3.6    | term 字符串          | MSRV 1.89                                            |
| `thiserror` / `anyhow`      | 2.0 / 1.0 | 错误类型（库 / CLI）     |                                                      |
| `clap`                      | 4.6      | CLI 解析            | MSRV 1.85                                            |
| `tracing` + `tracing-subscriber` | 0.1 / 0.3 | 可观测性（NFR-07）  |                                                      |
| **`moka`**                  | 0.12.16  | query embedding 缓存 | **替代 `lru`**：并发安全 + TTL；必须启用 `sync` feature，否则 `compile_error!` |
| `crc32fast`                 | 1.5.1    | 快照校验              |                                                      |
| `tantivy`（dev）              | 0.26.1   | **BM25 正确性对照基线**  | 仅 dev，不参与构建产物（ADR-007）                               |
| `criterion`（dev）            | 0.8.2    | 基准测试              |                                                      |
| `proptest`（dev）             | 1.11     | 属性测试              |                                                      |
| `tempfile`（dev）             | 3.27     | 测试临时文件            |                                                      |
| `cargo-deny`（工具）            | 0.20.2   | License 白名单校验     | NFR-09 / ADR-008                                     |

**明确不引入**：`arroy`（耦合 LMDB）、`redb` / `sled` / `heed`、`dashmap`（7.x 仍 rc）、`stop-words`、`lindera`、`axum` / `tokio`。理由见 `docs/devel/thirdparty.md` 第 4.12 节。
| `crc32fast`       | 快照校验            |                                |
