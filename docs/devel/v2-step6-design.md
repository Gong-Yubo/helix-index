# HelixIndex V2 · Step 6 详细设计（构建性能与多线程：embed 并行 spike + 增量构建 + 并发检索压测）

| 项 | 内容 |
| --- | --- |
| 版本 | **v0.1（首版，待评审）** |
| 日期 | 2026-09-11 |
| 状态 | **设计中**。D-S6-01~09 待评审拍板；D-S6-01 的**数值**由 S6-06 的 spike 回答，本版只定形态与判据 |
| 上游 | `plan-v2.md` §4 Step 6 / issue **#2**（V2-Step6）/ `requirements-spec.md` v1.12（**NFR-03 双口径**、NFR-05、NFR-10、NFR-11）/ `eval-report.md` §8.2、§8.4、§8.6 / `search/index.rs`、`embed/local.rs`、`search/config.rs`、`cli/{main,bench}.rs` |
| 范围 | **T7-09** embed 并行（**先 spike、后定是否投 M 级**）+ **T7-11** 增量构建 + **T7-17** 并发检索压测进 bench |
| 非范围 | 量化模型（D-J6 不引入）；换 embedding 模型；prefilter 索引侧预过滤（V2.1）；读写并发 / delta 分段（**Step 8**：T7-06 / T7-18）；横切 **T7-21**（`parallel_build` 默认翻转，独立 PR，与本 Step 正交）；**Step 7** 精排；并发**写**（`add` 仍需 `&mut self`，V2.0 单写者语义不变） |
| 交付物 | ① `crates/core/examples/bench_embed_session.rs`（E1/E2/E3 spike 载体）② 若 E2/E3 胜出：`LocalEmbedder` 池化 / EP 配置（**独立决策门**）③ `helix build --index` 追加路径 ④ `helix bench --threads` ⑤ `eval-report.md` §8.10 ⑥ 四处定义面回写 |

---

## 决策速览：D-S6-01 ~ D-S6-09

| # | 决策点 | 建议 | 定值来源 |
| --- | --- | --- | --- |
| **D-S6-01** | T7-09 是否投 M 级（多 session 池化 / CoreML EP） | ⏳ **由 E1/E2/E3 spike 数据决定**；本 Step 只承诺 **S 级 spike**，不预先承诺落地 | S6-06 决策门 |
| **D-S6-02** | 并行的落点（若投） | ✅ 建议：`LocalEmbedder` 内部**会话池 + 保序回填**，`Embedder` trait **一字不改** | §4.3 |
| **D-S6-03** | E3 的依赖接入方式 | ✅ 建议：`ort` 提为**直接依赖**并开 `coreml` feature（fastembed 无 coreml 透传）；**不默认开启** | §4.4 / 附录 C |
| **D-S6-04** | EP / 线程配置如何进入快照语义 | ✅ 建议：**编码进 `embedder_id`**（`&'static str` 由 match 产生），**不扩 `ConfigFingerprint` 结构** | §3.1 / §4.4.2 |
| **D-S6-05** | `default_embedder()` 的静默降级 | ✅ 建议：本 Step 内**消除**（`LocalEmbedder::new().ok()` ⇒ 显式可见），与 NFR-07「降级不得静默」对齐 | §2.7 / §4.4.3 |
| **D-S6-06** | T7-11 的形态 | ✅ 建议：**形态 A（`build --index` 追加）必做**；形态 B（跨 run chunk 级 embed 缓存）**列为可选、本 Step 不做** | §4.5 |
| **D-S6-07** | 追加后是否自动 compact | ✅ 建议：**不自动**；文档给出「追加 → compact」的推荐序列（compact 是显式动作，Step 4 已定） | §4.5.5 |
| **D-S6-08** | NFR-10 的**数值目标** | ⏳ **缺失，需评审或用户拍板**：现口径只有「多线程并发检索吞吐与正确性」，**没有任何数字** ⇒ 无法判定达标 | §9.2 Q4 |
| **D-S6-09** | T7-17 的计时与对照口径 | ✅ 建议：**吞吐为主、延迟为辅**（并发下 per-query 延迟含竞争，不可与 NFR-02 横比）；**结果逐位对照单线程** | §4.6.2 |

---

## 1. 目标与验收

### 1.1 要解决的问题

| ID | 问题 | 证据 | 严重度 |
| --- | --- | --- | --- |
| **Q-P1** | NFR-03 构建 225.5s（embed 209.9s），原 `<120s` 口径无路径可达 | `eval-report.md` §8.2 | 中（资源/体验） |
| **Q-M1** | embed 串行：`LocalEmbedder` 用 `Mutex<TextEmbedding>` **全程持锁** | `embed/local.rs:35,67,75` | 中 |
| **Q-M2** | 无并发检索 QPS 压测（`Searcher` 跨线程能力**从未被量过**） | `cli/bench.rs`（无 `--threads`） | 中 |

⚠️ **本 Step 不承诺把 225.5s 压到 120s**——那已被 D-J8 判定为不可达（不量化、不换模型）。本 Step 对 **NFR-03 ①（首次全量 <240s）只做"别弄坏"**，真正的落点是 **NFR-03 ②（增量追加 < 首次 × 变更比 × 1.2）**，即 T7-11。

### 1.2 对应需求

| 编号 | 口径（原文见需求文档） | 本 Step 的落点 |
| --- | --- | --- |
| **NFR-03 ①** | 首次全量 1 万 chunk 含 embedding **< 240s** | 已达标（225.5s）；T7-09 **不得回退**（E2/E3 若改变数值须重测声明） |
| **NFR-03 ②** | **增量追加** 10% 文档 **< 首次全量 × 10% × 1.2** | **T7-11 的唯一硬指标** |
| **NFR-05** | 1 万 chunk 向量约 20MB；峰值 RSS 372MB | E2 多 session 会**抬高内存峰值** ⇒ 必须重测并给出增量 |
| **NFR-10** | 多线程并发检索吞吐与正确性（`Searcher` 跨线程） | **T7-17**；⚠️ 数值目标缺失（D-S6-08） |
| **NFR-11** | `commit()` 后立即可查；写延迟有界（= flush 一批耗时） | **T7-11** 的追加路径必须测量「load → add → commit → 可查」的写延迟 |

### 1.3 验收标准（Step 6 完成的定义，可证伪）

1. **T7-09 有结论**：`eval-report.md` §8.10 收录 E1/E2/E3 三组数据（吞吐 条/s、加速比、峰值 RSS），并给出**明确结论**「投 / 不投”；若投，额外给出落地后的 NFR-03 ① 复测值与 NFR-05 峰值增量。
2. **T7-11 达标**：`helix build --index <既有快照> --input <delta>` 追加 10% 文档，**实测耗时 < 首次全量 × 10% × 1.2**（NFR-03 ②），且脚本可一键复现。
3. **T7-11 正确性**：追加结果与「全量重建同语料」在**可比口径上一致**——比较对象是 **`(source, score)` 序列**，**不比较 `doc_id` / `chunk_id`**（追加语义下 id 必然不同；口径见 §4.5.3）。
4. **T7-11 幂等**：同一 delta 重复追加两次，索引的文档数 / 分片数 / 词项总数**不变**（`content_hashes` 双保险必须生效）。
5. **T7-17 达标**：`helix bench --threads {1,2,4,8}` 输出 QPS、各线程延迟分位；**多线程检索结果与单线程逐位一致**（`hits` 的 `chunk_id` 序列与 `score` 逐位相同）。
6. **若采纳 E3（CoreML）**：向量数值变更**已显式声明**，且 ① `embedder_id` 编码生效（CPU 建库 / CoreML 加载会 `ConfigMismatch` 而非静默错配）② `default_embedder()` 的静默降级已消除 ③ NFR-06（同快照两次加载逐位一致）与相关性评测**已重测或明确声明暂不重测**。
7. **守门全绿**：`make fmt && make lint && make test && make deny`；新代码无 `unsafe`；CI 全绿。

---

## 2. 现状与问题定位（源码级）

### 2.1 225.5s 的真实分解：embed 占 93%

| 段 | 12K（单 chunk 口径） | 占比 | 证据 |
| --- | --- | --- | --- |
| 纯索引构建（倒排 + 统计） | ~3s | 1.3% | `eval-report.md` §8.2 |
| **embed 推理** | **209.9s** | **93.1%** | `SearchIndex::embed_elapsed()` 累计值 |
| 建图（HNSW insert） | 11.7s | 5.2% | `eval-report.md` §8.6 |
| 落盘（快照 + 图 dump） | <0.2s | <0.1% | `eval-report.md` §8.7 |

⇒ **任何不触碰 embed 的优化，上界收益 < 7%**。这正是 D-J8 判「<120s 不可达」的算术依据，也是本 Step 把 T7-11（**跳过 embed**）放在 T7-09（**加速 embed**）之前的理由：**前者是结构性收益，后者是常数收益**。

### 2.2 embed 串行的真实形态

```rust
// crates/core/src/embed/local.rs:34-37
pub struct LocalEmbedder { inner: Mutex<TextEmbedding>, dim: usize }

// :67  embed_documents
let mut model = self.inner.lock().expect("embedder 锁已中毒");
// :75  embed_query
let mut model = self.inner.lock().expect("embedder 锁已中毒");
```

- `Mutex` 不是"顺手加的保护"，而是**必需**：`TextEmbedding::embed` 需要 `&mut self`（`fastembed-6.0.2/src/text_embedding/impl.rs:453`）。
- ⇒ 同一进程内**并发调用 `embed_documents` 只会排队**，不产生任何并行。这是 Q-M1 的准确描述。
- ⇒ 「并行」在这一层只有两条路：**多 session**（E2）或**换执行后端**（E3）。**没有第三条**（改 batch 已实测无效，见 2.3）。

### 2.3 A2 的技术前提：已被证伪一半（复审结论仍成立）

| 复审论断 | 核实结果 | 证据 |
| --- | --- | --- |
| `intra_threads` 默认 `None` = **用满所有核**，我们未覆盖 | ✅ 成立 | `fastembed-6.0.2/src/text_embedding/init.rs:33`（`pub intra_threads: Option<usize>`）、`embed/local.rs:42-46` 确实没调 `with_intra_threads` |
| 批次不是瓶颈 | ✅ 成立 | 32/64/128/256 → 62.6/59.1/54.4/51.6 条/s，差异 <20%，且**小 batch 略快**（`eval-report.md:147-148`） |
| 并行建图只占 4% | ✅ 成立 | 12K 里 11.7s/225.5s = 5.2%（`eval-report.md` §8.6） |
| fastembed 暴露 `with_execution_providers` | ✅ 成立 | `src/text_embedding/init.rs:51/83/122` |
| **走 CoreML 只要加个 feature** | ❌ **不成立，需修正** | fastembed 6.0.2 **没有 coreml feature**（只有 `directml = ["ort/directml"]`）⇒ 必须把 `ort` 提为直接依赖。详见 2.4 |

⚠️ 由此得到一个**反直觉但重要**的推论：`intra_threads` 已经用满所有核 ⇒ **E2（多 session × 线程分片）的本质不是"加并行"，而是"把同一批算力切开重新分配"**。若 ort 的 per-session arena 不能共享，E2 完全可能**零收益甚至负收益**。这正是它必须是 spike 而非承诺的原因。

### 2.4 【关键技术前提】fastembed 6.0.2 / ort 2.0.0-rc.13 的 session 与 EP 能力（源码核实）

> 本节是本 Step 唯一"新引入外部能力"的技术前提，逐条给出文件位置与行号。**未核实的不写。**

| 事实 | 位置 | 对设计的影响 |
| --- | --- | --- |
| `InitOptions.intra_threads: Option<usize>`，`None` = 用满所有核 | `fastembed-6.0.2/src/text_embedding/init.rs:33`；`with_intra_threads` 在 `:133` | E2 的**唯一可调旋钮** |
| `with_execution_providers(Vec<ExecutionProviderDispatch>)` 存在 | 同文件 `:51`（`TextInitOptions`）/ `:83`、`:122`（`InitOptions*`） | E3 的接入点 |
| `fastembed` 重导出 `ExecutionProviderDispatch` | `src/lib.rs:89` | 不需要直接依赖 `ort` 才能**表达** EP 类型 |
| **`fastembed` 无 `coreml` feature**（features 列表只有 `directml`/`cuda`/`mkl`/… 透传） | `fastembed-6.0.2/Cargo.toml` `[features]` 全文 | ⚠️ **E3 必须把 `ort` 提为直接依赖**并开 `coreml` |
| `ort` 有 `coreml = ["ort-sys/coreml"]` | `ort-2.0.0-rc.13/Cargo.toml` `[features]`（计划文档写的 "Cargo.toml:145" 位置对，名称对） | 同上 |
| EP 类型的真实路径是 **`ort::ep::CoreML`**，`#[cfg(feature = "coreml")]` | `ort/src/ep/mod.rs`（`pub mod coreml;` gate 段）、`ort/src/ep/coreml.rs:68 pub struct CoreML` | ⚠️ **不是** `CoreMLExecutionProvider`——计划 §4 Step 6 与 §附-1 的写法**已过时**，本文更正 |
| `Session` 构建接受 `Vec<ExecutionProviderDispatch>` | `fastembed/src/text_embedding/impl.rs:71` `init_session_builder(execution_providers, intra_threads)` | E1/E2/E3 共用同一构造路径 ⇒ 三组实验**只需换参数** |
| `TextEmbedding::embed(texts, batch_size: Option<usize>)` 内部按 `DEFAULT_BATCH_SIZE = 256` 分块 | `src/text_embedding/mod.rs:5`、`impl.rs:364`（`batch_size.unwrap_or(DEFAULT_BATCH_SIZE)`） | 我们传 `None` ⇒ 256 |
| ⚠️ **实际进入 ONNX 的 batch 不是 256，而是 64** | `search/index.rs:390`（`pending.len() >= batch_size` 触发 flush）、`:429`（`embed_documents(&texts)` 传整批）、`search/config.rs:26`（`DEFAULT_BATCH_SIZE = 64`） | 调用链在 **64** 处切片 ⇒ 每次 `embed` 最多收 64 条 ⇒ 内部 256 的切分**从不触发**。**排障时不要把"256"当实际 batch** |

### 2.5 增量构建的真实现状：骨架已在，缺两件事

**已经有的（比预期多）**：

1. `SearchIndex::add` **已有 doc 级短路查重**：`search/index.rs:356-365` —— `content_hash = xxh64(dedup_key.unwrap_or(text))`，命中即返回 `deduped: true`，**不分块、不进 pending、不 embed**。
2. `Index::add` **内部还有第二道**：`index/mod.rs:166-173` 同 hash 存活文档直接 `return (existing, Vec::new())`。⇒ **双保险**。
3. `content_hashes` **入快照**：`index/mod.rs:88`（`content_hashes: Vec<(u64, DocId)>`）、`:501-503`（导出按 hash 升序）、`:535`（导入重建 `HashMap`）。⇒ **`load` 之后 `doc_id_by_hash` 依然可用**，doc 级增量在**已加载的快照**上天然成立。

**缺的两件事**：

1. **CLI 没有"追加"入口**：`BuildArgs`（`cli/main.rs:49-66`）**只有 `--input`**，注释明写「每次重建索引」；`Command` 枚举（`:34-46`）里没有 `Add`。⇒ 今天的用户**无法**在已有快照上追加，只能全量重建。
2. **hash 粒度是"整篇文档"**：`content_hash(dedup_key.unwrap_or(text))` 取的是**整篇文本**。⇒ 文档改一个字 ⇒ hash 变 ⇒ **该文档的全部 chunk 重新 embed**。chunk 级复用**完全不存在**。

⚠️ 还有第三点，属于**必须写下来才不会被后人撞上**的：

3. **`raw_vectors` 与图 sidecar 是"按 chunk_id 索引"的派生数据**。追加路径一旦产生 ID 冲突，就会让**旧向量挂到新 chunk 上**（幽灵向量，与 Q-C1 同类但成因相反）。本文 §4.5.2 给出"今天不冲突"的证明，并要求上锁。

### 2.6 并发检索的真实现状：是"缺采集点"，不是"缺能力"

**实测 grep（`crates/core/src/`）**：`query/`、`search/searcher.rs`、`retriever/`、`fusion/`、`index/` 中 **`Mutex` / `RwLock` / `RefCell` / `Cell<` / `unsafe` 命中数为 0**。

- 读路径是**纯不可变**的：`Searcher { cfg: Arc<Config>, inner: Arc<Inner>, .. }`，`search(&self, ..)`。
- 已有编译期断言：`search/searcher.rs:249` `fn assert_impl<T: Clone + Send + Sync + 'static>()` + `assert_impl::<Searcher>()`。
- ⇒ T7-17 **不需要改内核**，只需要**采集点**：`bench --threads N` + 正确性对照。若实测非线性，根因只可能在分配器争用 / 缓存行伪共享 / `Arc` 原子计数，**不在共享可变状态**（因为根本不存在）。

### 2.7 设计期新发现：`default_embedder()` 的**静默降级**

```rust
// crates/core/src/search/config.rs:312-318
fn default_embedder() -> Option<Arc<dyn Embedder>> {
    use crate::embed::LocalEmbedder;
    // 模型首次下载约 49s；失败时退化为纯 BM25 而非 panic（零配置可用）。
    LocalEmbedder::new().ok().map(|e| Arc::new(e) as Arc<dyn Embedder>)
}
```

- 现状语义：`LocalEmbedder::new()` 返回 `Err` ⇒ `.ok()` ⇒ `None` ⇒ `SearchIndex` **变成纯 BM25**，**没有任何错误或警告**。
- V2.0 之前这是"零配置可用"的合理取舍（模型下载失败不应 panic）。
- ⚠️ **E3 会把它从"罕见"放大成"常见"**：CoreML 初始化在容器/非 Apple 环境下必然失败；一旦 `--ep coreml` 写进构建脚本而机器不支持，得到的是**一个没有向量的索引**，而不是一个错误。
- ⇒ 与 **NFR-07「降级不得静默」** 同一原则，本 Step 必须处理（D-S6-05）。具体手段见 §4.4.3。
- ⚠️ 注意这条**不是** E3 的专属问题：今天用 `--vectors` 建库、模型缓存缺失时**已经是**这个行为。本文只是把它显式化。

---

## 3. 设计约束（既有事实，本文不重新论证）

### 3.1 `ConfigFingerprint` 是**快照格式的一部分**，扩字段 = 破坏性变更

```rust
// crates/core/src/storage/snapshot.rs:35-45
pub struct ConfigFingerprint {
    pub analyzer_id: String,
    pub embedder_id: String,
    pub dim: u32,
    pub chunker: (usize, usize),
}
```

- load 时与"当前装配"比对，不一致报 `Error::ConfigMismatch`（`:32-34` 注释：这是修 B1 的根本手段）。
- 快照用 bincode（非自描述）序列化 ⇒ **加字段无法优雅回退**（`#[serde(default)]` 在 bincode 下对缺失字段无效）。
- ⚠️ **同类先例：R35**（Step 5 已记录）——`SearchResponse` 加 `metrics`、`VectorIndex` 加必选方法**都被判定为破坏性变更**。本 Step **不得**重犯。
- ⇒ **D-S6-04**：EP / 线程配置若需要进入快照语义，**编码进 `embedder_id` 字符串**（`Embedder::id()` 返回 `&'static str`，用 `match` 从有限枚举产生，无需分配），结构一字不改。

### 3.2 `Embedder` trait 的**顺序契约**

```rust
// crates/core/src/embed/mod.rs（摘要）
fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
```

- `search/index.rs:449` 是 `self.pending.iter().zip(vecs)` —— **输出按位置与输入一一对应**。
- ⇒ 任何并行实现**必须保序回填**（§4.3.1），绝不能依赖 completes-out-of-order 的结果顺序。这是 trait 的隐含契约，本 Step 要把它**写进 rustdoc**（今天只隐含在调用点）。

### 3.3 `flush()` 是 embed 的唯一批量入口，其切片大小即实际 ONNX batch

见 §2.4 最后一行。⇒ 若要调"实际 batch"，改的是 `batch_size`（`Config`），不是 `embed` 的第二参数。E1 基线必须**同时记录这两个值**，否则未来的复现者会以为是 256。

### 3.4 不得动 NFR-02 热路径与 `Config: Send + Sync`

- `Config` 的 `Send + Sync` 是 `Searcher` 满足 `'static + Clone + Send + Sync`（G3）的前提（`search/config.rs:28-34` 注释）。
- ⇒ 池化实现必须用 `Vec<Mutex<..>>` 之类**保持 `Send + Sync`** 的容器，不得引入 `!Sync` 类型。

### 3.5 评测可比性：改 embed 数值 ⇒ 与 P5 基线**不可横比**

- Step 4 已有同类声明（「重建图会重新引入拓扑抖动，NFR-06 不破，但评测可比性需在文档说明」）。
- E3 若改变向量（EP 改变算子实现 ⇒ 浮点归约顺序变），则**所有既有效果指标（MRR/NDCG）与本次结果不可直接相减**。
- ⇒ 验收第 6 条要求**显式声明**，不允许沉默。

---

## 4. 详细设计

### 4.1 总览：三件事各自的落点

| 任务 | 落点 | 是否改内核 | 交付形态 |
| --- | --- | --- | --- |
| **T7-09** | `LocalEmbedder` 的**构造参数**（session 数 / `intra_threads` / EP） | 若投：改（trait 不变）；不投：不改 | **先出 `examples/bench_embed_session.rs` 的 E1/E2/E3 数据**，再决策 |
| **T7-11** | `cli/main.rs` 的 `BuildArgs` + 一张不变式测试 | 否（内核已有 `add` 短路 + `content_hashes` 持久化） | `helix build --index` 追加路径 |
| **T7-17** | `cli/bench.rs` 的采集点 | 否（读路径已 `Send + Sync`） | `helix bench --threads N` |

⚠️ **顺序说明**：`plan.md` §附-3 建议顺序把 T7-24 / T7-21 排在 Step 6 之前（都是独立 PR）。本设计**不依赖**它们，可并行推进；但 **T7-21 若先落地**，E1/E2/E3 的建图段会更快，**不影响 embed 相关结论**（embed 与建图正交）。

### 4.2 T7-09 · spike 设计（E1 / E2 / E3）

#### 4.2.1 共同口径（三组必须一致，否则数据不可横比）

| 项 | 取值 | 理由 |
| --- | --- | --- |
| 语料 | `data/t2-corpus.jsonl` 前 **4000** 段 | 与 `bench_batch_size.rs` 一致 ⇒ 与既有 51~62 条/s 数据**可对照** |
| 调用方式 | `embed_documents` 按 **64** 切片循环 | 复刻 `flush()` 的真实切片（§3.3），不人为放大 batch |
| 预热 | **1 轮完整语料丢弃计时** | 首次含 ONNX 图优化 / arena 分配，混入会让 E2/E3 全失真 |
| 计时 | 累计 `Instant::now()` 跨全部调用；重复 **3 轮取中位数** | 与 perf-ab-calibration 技能一致：**单轮数字不作决策** |
| 记账 | 每次运行记录：吞吐（条/s）、总耗时、**进程峰值 RSS**、`intra_threads`、session 数、EP | 缺任一项则该组数据作废 |
| 机器 | 本机 macOS aarch64 / release；**记录芯片型号与核数** | 结论的适用范围必须写死 |

#### 4.2.2 E1 · 单 session 基线（现状）

```
sessions = 1, intra_threads = None（= 满核）, EP = CPU
```

预期：≈ 51~62 条/s（`eval-report.md:147-148` 同口径）。**若 E1 复现不出这个量级，先停下排查环境，不要往下做**——这既是基线也是**探针**（防"E2 有收益"其实是环境差异）。

#### 4.2.3 E2 · 多 session × `intra_threads` 分片

```
sessions ∈ {2, 4}，intra_threads = max(1, 核数 / sessions)，EP = CPU
```

- 实现载体（spike 内，不进内核）：`Vec<Mutex<TextEmbedding>>` + `rayon::ThreadPoolBuilder::new().num_threads(sessions)`，把 `texts.chunks(64)` **按块**分发给 session，**按块索引回填**（不依赖完成顺序）。
- ⚠️ **E2 的收益是"不确定"的，这是它必须是 spike 的原因**（§2.3）：`intra_threads` 本来就吃满所有核，E2 只是把总核数切开。**可能发生的三种结果都有意义**：
  - **正收益** ⇒ 说明单 session 内部并行度不足（图分段/算子串行），多 session 提高了利用率 ⇒ 值得投。
  - **零收益** ⇒ 说明单 session 已饱和 ⇒ 不投，**省下一个 M 级任务**。（这同样是好结论。）
  - **负收益** ⇒ 说明多 session 的 arena/线程开销超过了收益 ⇒ 不投，并记入风险。
- 额外必须记录：**峰值 RSS**。N×session 意味着 N 份 ort arena（每份含权重副本或引用 + 工作区）。**若 RSS 增量超过 NFR-05 可接受范围，即使有收益也不投**。

#### 4.2.4 E3 · CPU EP vs CoreML EP

```
sessions = 1（先固定），intra_threads 同 E1，EP = CoreML（按 E2 结论再考虑组合）
```

- **依赖接入（D-S6-03）**：`fastembed` 无 coreml 透传（§2.4）⇒ 需在 `crates/core/Cargo.toml` 增加
  `ort = { version = "=2.0.0-rc.13", default-features = false, features = ["coreml"] }`（**feature 参与加法式合并**，不会与 fastembed 拉进两份 ort；**必须精确锁定**，理由同 `Cargo.lock` 里 ort 的锁定注释）。
- ⚠️ **E3 的定位是"探路"而非"承诺"**：macOS aarch64 上的 CoreML EP 是否真的接入、是否真的加速，**没有任何先验证据**。三档可能：显著加速 / 无差异 / 初始化失败（此时正好验证 §2.7 的静默降级风险）。
- ⚠️ **E3 若有效，代价是三重的**：① 向量数值变（§3.5）② 快照语义变（§3.1，需 D-S6-04）③ 内存峰值变（CoreML 有额外转换缓冲）。三者**都必须在上线前处理**，因此 E3 落地**必须单独立决策**，不能"顺手打开"。

#### 4.2.5 载体：`crates/core/examples/bench_embed_session.rs`

照 `examples/bench_batch_size.rs` 的体例（自包含、`required-features = ["local-embed"]`、release 跑、打印表格）。设计要点：

- **一次进程内跑完 E1/E2/E3 的所有档位** ⇒ 消除跨进程的环境漂移（这是本项目已吃过亏的坑：`perf-ab-calibration` 里"非交错执行有系统性偏差 ×1.38~1.54"）。
- ⚠️ **档位内两轮、顺序交错**（A/B/A/B），并**打印控制组（E1）在两轮之间的漂移**，供事后归一。仅当漂移 <10% 才允许跨档位直接比较；否则按控制组归一后再比。
- 输出 JSON（可选 `--json <path>`）⇒ 供 `eval-report.md` §8.10 落表，避免手工抄录。

### 4.3 T7-09 若采纳 E2 的落地形态（**仅当 spike 判定投**）

#### 4.3.1 保序回填（硬要求）

```rust
// 示意：按块分发，按块索引回填；完成顺序无关
let mut out: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
pool.install(|| {
    out.par_chunks_mut(BATCH)
        .enumerate()
        .for_each(|(i, slot)| {
            let texts = &texts[i * BATCH..(i + 1) * BATCH];
            let v = sessions[i % sessions.len()].lock().unwrap().embed(texts, None).unwrap();
            for (s, x) in slot.iter_mut().zip(v) { *s = Some(x); }
        });
});
```

- 也**可以在内核外**（`examples/`）先验证，但落地必须进 `LocalEmbedder`，否则 `flush()` 拿不到收益。
- ⚠️ **不能**用 `.collect::<Vec<_>>()` 收集 `par_iter().map()` 的返回值就以为保序了 —— rayon 的 `collect` 对 `IndexedParallelIterator` 确实保序，但**这里的分块是 `chunks` 生成的非 `IndexedParallelIterator` 语义**，容易误判。**明确写成索引回填**，让正确性不依赖对 rayon 的信任。
- 必须有测试：S6-T4（**打乱完成顺序仍保序**）。

#### 4.3.2 `embed_query` 必须走**同一配置**

- `embed_query` 也持 `inner` 锁（`embed/local.rs:75`）。池化后必须从**同一个池**取 session（或至少保证 `intra_threads` / EP 与入库侧**完全一致**）。
- ⚠️ 若两侧配置不同，即使模型相同，浮点归约顺序不同 ⇒ **查询向量与入库向量口径分裂**，这是 R2（BGE 前缀）同类的静默陷阱，只是成因不同。**必须写测试**（S6-T5）。

#### 4.3.3 trait 边界不变

`Embedder` trait 签名**一字不改**（`&self`，内部池化）。理由：① 改签名会把 blow-up 传给所有实现（`remote.rs`、`cached.rs`、测试里的 `Dummy`）② `CachedEmbedder` 包装的是 `Box<dyn Embedder>`，本 Step 的收益**位置在 `LocalEmbedder` 之内**，不改 trait 即可被 `CachedEmbedder` 自动继承。

### 4.4 T7-09 若采纳 E3 的落地形态（**仅当 spike 判定投**）

#### 4.4.1 配置的表达

- `LocalEmbedder` 增加构造入口：`LocalEmbedder::with_options(EmbedOptions)`，其中
  `EmbedOptions { sessions: usize, intra_threads: Option<usize>, ep: Ep }`，`Ep ∈ {Cpu, CoreML}`。
- `LocalEmbedder::new()` 保持**行为不变**（等价于 `with_options(默认)`）——避免破坏既有调用点与测试。
- CLI：`helix build --vectors --ep {cpu|coreml}`（`SearchArgs` 无需该参数，因为检索侧**必须与建库侧一致**，一致性由指纹保证而不是由参数保证）。

#### 4.4.2 指纹：编码进 `embedder_id`（D-S6-04）

```rust
// 示意：由有限枚举 match 出 &'static str，零分配、零格式变更
fn id(&self) -> &'static str {
    match (self.opts.ep, self.opts.sessions) {
        (Ep::Cpu, _) => "bge-small-zh-v1.5",
        (Ep::CoreML, _) => "bge-small-zh-v1.5+coreml",
    }
}
```

- ⇒ **CPU 建库 + CoreML 加载** ⇒ `ConfigMismatch`，**从静默错配变成显式报错**（正是 B1 的修法）。
- ⇒ 老快照（`embedder_id == "bge-small-zh-v1.5"`）在**默认配置**下继续可加载，**零迁移成本**。
- ⚠️ 是否需要把 `sessions` / `intra_threads` 也编进去，**取决于 spike 是否观测到数值差异**（S6-T6 的探针结果）。**若观测到差异就必须编**；只靠"我认为它不影响"是不够的。

#### 4.4.3 消除静默降级（D-S6-05）

把 `default_embedder()` 的 `.ok()` 改为**可见的降级**，形态有两种（评审选一）：

| 方案 | 做法 | 优点 | 缺点 |
| --- | --- | --- | --- |
| **A（建议）** | `--vectors` 显式请求向量时**硬失败**（`Err`）；未指定 `--vectors` 时才允许退化为纯 BM25 | 与"用户意图"对齐：要了向量就必须给向量 | 需要 `default_embedder` 知道"是否被显式请求" ⇒ 传参 |
| **B** | 保留退化，但**打印 `tracing::warn!`** 并在 `SearchIndex` 上暴露 `embedder_status` | 改动最小 | 仍然"静默"（警告会被忽略），不满足 NFR-07 的严格读法 |

- 本文建议 **A**，理由是它与 Step 2 的 `GraphStatus` 先例同构（**"降级必须显式可见"**，且 Step 2 已经证明"仅打日志"在真机上会被忽略）。
- ⚠️ 无论选哪个，**都必须有测试**（S6-T7）：构造一次必失败的 embedder 初始化，断言"要么 Err、要么可观测标志为真"。

### 4.5 T7-11 · 增量构建

#### 4.5.1 形态 A：`helix build --index`（D-S6-06，必做）

```
helix build --index <既有快照> --input <delta.jsonl> [--vectors] [--output <新快照>]
```

- 语义：**加载既有快照 → 追加 delta → commit → 保存**。
- 与既有 `--output` 语义一致（Step 4 的 `compact` 已建立"默认原地覆盖 / `--output` 另存"的先例）：**默认原地**，`--output` 另存。
- ⚠️ 与 `--input` 的**互斥/共存**需要定：建议 **`--index` 与 `--input` 同时给出 = 追加语义**（`--index` 单独给出 = 仅加载后重存，用于验证往返，可留作诊断）。**不新增 `Add` 子命令**——子命令会把"装配 + 容器 + 落盘"这套逻辑复制一遍，正是 P6 重构想消除的重复。

#### 4.5.2 为什么 ID 不会冲突（今天的证明，以及为什么仍然要上锁）

- `ForwardStore::insert_doc` 用 `let id = self.docs.len()`（`index/forward.rs:40`），`insert_chunk` 用 `self.chunks.len()`（`:50`）—— **ID = 当前槽位长度**。
- `export()` 返回 `&self.docs` / `&self.chunks`（`:176-178`），`Index::export` 逐元素 `DocumentDto::from`（`index/mod.rs:505-510`）；导入时 `Vec<Option<..>>` **原样**（`:535`）。⇒ **向量长度 = 历史最大已分配 ID + 1，尾部空洞被保留**（`rebuild()` 用 `enumerate` + `Some` 判断，`:196-199`）。
- ⇒ 追加时新 ID 严格大于所有历史 ID，**`raw_vectors` / 图 sidecar 中按 `chunk_id` 存的旧向量不会被新 chunk 复用**。
- ✅ **今天安全**。但它是**三条隐含前提的合取**（① 导出不裁剪尾部空洞 ② 导入不做压缩 ③ compaction 会重编号且同步重写 `raw_vectors`）。⇒ 必须写成**不变式测试**（S6-T8），锁住它，而不是靠本文一段话。

#### 4.5.3 正确性对照口径（**必须先量清，再写断言**）

⚠️ 第 1.3 节验收第 3 条故意**不写"逐位一致"**，因为这在本语义下**是错的**：

| 可比较 | 不可比较 | 原因 |
| --- | --- | --- |
| BM25 检索的 **`(source, score)` 序列** | `doc_id` / `chunk_id` | 追加语义下 ID 分配顺序不同（`insert_*` 用 `len()`） |
| 文档数 / 分片数 / 词项总数 / `avgdl` | `hits` 的原始 `chunk_id` | 同上 |

- ⇒ 测试的比对函数必须**先映射到 `source` 再比**。这正是本项目已固化的一条教训：**「只在量出真实口径后才谈断言」**（Step 2 S2-T6 的 `score` 是余弦相似度、自匹配得 1 那次）。
- `score` 是否**逐位**相同？BM25 侧应相同（同一 `Index` 内容 + 同一统计量 ⇒ 同一浮点路径）；HNSW 侧**不会**（图拓扑因插入顺序不同而不同，这是既有的 R-P5-13 拓扑抖动，**不是本次回归**）。⇒ 向量路对照**只比"返回的 source 集合覆盖率"**，不比顺序。

#### 4.5.4 幂等性（验收第 4 条）

- 第一道：`SearchIndex::add` 命中 `doc_id_by_hash` ⇒ `deduped: true`，**不进 pending**（`search/index.rs:359-365`）。
- 第二道：`Index::add` 命中 `content_hashes` ⇒ `return (existing, Vec::new())`（`index/mod.rs:168-172`）。
- ⇒ 重复追加**不应改变** `num_docs` / `num_chunks` / `total_len`。
- ⚠️ 但 `num_chunks()` 与 `total_len()` 的语义是"**不含墓碑**"（`search/index.rs:472-474` 注释）。若 delta 中含 `remove`，该断言不成立 ⇒ **测试只覆盖"纯追加"**，墓碑场景另列。

#### 4.5.5 与 compaction 的关系（D-S6-07）

- 追加会产生**新 chunk**（不是墓碑），但**重复追加已修改的文档**会产生墓碑（旧 doc 未被 remove 时不会——hash 变了就是**新文档**，旧文档仍在）。
- ⚠️ **这是本 Step 必须写进文档的语义边界**：`build --index` 追加**不做 upsert-by-source**。同一 `source` 内容变化会**新增一篇文档**，旧文档仍在。若需要替换语义，用户必须显式 `remove`（V2.0 的 `helix` 没有 remove 子命令 ⇒ 走库 API）或用 **Step 4 的 `compact`** 配合。
- ⇒ 文档给出推荐序列：`build --index`（追加）→ 库侧 `remove`（如需要）→ `commit` → `compact_and_save`（回收）。**不自动 compact**（compact 是 O(N) 重物化 + ID 重编号，自动做会破坏用户对 ID 的预期）。

### 4.6 T7-17 · 并发检索压测

#### 4.6.1 采集点

```
helix bench --index <snapshot> --threads {1,2,4,8} --reps <n> [--modes bm25,vector,hybrid]
```

- 实现：`std::thread::scope`（或 rayon 池）× N 个 worker，共享 `Arc<Searcher>`（`Searcher: Clone + Send + Sync`，**无需改内核**）。
- 每 worker 跑 **同一批 query 的同一顺序**（保证可复现），各自累积 `LatencyResult`。
- 输出：**总 QPS**、**每线程 P50/P99**、**全局 P99**、加速比（相对 `--threads 1`）。

#### 4.6.2 计时与对照口径（D-S6-09）

| 项 | 做法 | 理由 |
| --- | --- | --- |
| **主指标** | **QPS（吞吐）** | 这是 NFR-10 要回答的问题；并发下 per-query 延迟含排队，不是"检索有多快" |
| **辅指标** | 每线程延迟分位 | 用于发现"吞吐上去了但尾延迟炸了" |
| ⚠️ **不得**与 NFR-02 横比 | NFR-02 是**单线程**口径 | 混比会得出"并发让检索变慢"的错误结论 |
| **正确性对照** | N 线程跑出的 `chunk_id` 序列与 `score` **逐位等于** 1 线程 | 读路径纯不可变（§2.6）⇒ 这条**应当**成立；不成立就是真 bug（如浮点环境变量、锁顺序、分配器导致的非确定性） |

⚠️ 与 `perf-ab-calibration` 的既有教训一致：**并发数字对机器状态极敏感** ⇒ ① 必须先跑一轮预热并丢弃 ② 顺序交错（1/2/4/8/1/2/4/8）③ 报告时**标运行范围**（核数、是否插电、是否在 CI）。

#### 4.6.3 NFR-10 的数值目标缺失（D-S6-08）——**需要评审拍板**

现状：NFR-10 全文只有「多线程并发检索吞吐与正确性（`Searcher` 跨线程）」+「V2.0 Step 6 实测」。**没有任何数字**。

这意味着**今天无法判定 NFR-10 是否达标**——因为不存在判据。三条出路：

| 方案 | 内容 | 代价 |
| --- | --- | --- |
| **A（建议）** | 先测出**基线数据**（`--threads 1` 的 QPS），把 NFR-10 的口径改为**相对提升**：「`--threads 4` 的 QPS ≥ `--threads 1` × 2.5（近线性，允许 40% 折损）」 | 需要一个"折损系数"，属拍脑袋；但**可证伪**且不伪装成绝对指标 |
| B | 定绝对 QPS 目标 | 与机器强耦合（本项目已明确「性能数字不具跨机可引用性」）⇒ **不建议** |
| C | 维持"只记录，不判定" | 诚实，但 NFR-10 永远无法关闭 |

- 本文建议 **A**，本 Step 先把数据测出来，**口径修订作为独立决策**在评审时定（因为改 NFR 需要需求文档版本行，属"定义面"变更）。
- ⚠️ 若选 A，需注意**读路径无共享可变状态**（§2.6）是个**强先验**：4 线程低于 2.5× 反而需要解释（内存带宽 / 分配器）。⇒ 该判据有鉴别力，不是橡皮图章。

---

## 5. 决策记录（D-S6-01 ~ D-S6-09）

### D-S6-01 T7-09 是否投 M 级 ⏳ **由 spike 数据决定**

- 本 Step **只承诺 S 级 spike**（`examples/bench_embed_session.rs` + 数据入 §8.10 + 一个明确结论）。
- 落地（E2 池化 / E3 EP）**只有在 spike 判定"投"之后**才排期，且**各自独立评审**。
- 理由：§2.3 已证明"多 session"在 `intra_threads` 满核的前提下**收益方向未定**。把"探路"承诺成"交付"，是本项目反复避免的错误（Step 6 的 A2 复审就是这么改过来的）。
- **决策门（写死在 spike 输出里）**：`E2 相对 E1 吞吐提升 ≥ 30% 且 峰值 RSS 增量 ≤ 20%` ⇒ 判「投」，否则「不投」。E3 同理，但**额外要求"向量数值差异已量化"**。

### D-S6-02 并行的落点 ✅ 建议：`LocalEmbedder` 内部池化 + 保序回填

- `Embedder` trait **不变**；`CachedEmbedder` / `remote` / 测试实现**零改动**。
- 保序用**显式索引回填**，不依赖 rayon 语义（§4.3.1）。

### D-S6-03 E3 的依赖接入 ✅ 建议：`ort` 提直接依赖 + `coreml` feature，默认关

- `fastembed` **无 coreml 透传**（§2.4 实测）⇒ 无替代方案。
- **必须 `=2.0.0-rc.13` 精确锁定**（与 `Cargo.lock` 的锁定理由同源）。
- ⚠️ **许可影响需复核**：`ort-sys` 的 `coreml` feature 只影响**链接的预编译二进制**，不改 Rust 依赖的 License 面；但 `--features coreml` 构建出的产物所链接的 ONNX Runtime 二进制**发行条款需单独确认**。⇒ 列入 S6-05 的开工前置（NFR-09 同类检查）。

### D-S6-04 EP / 线程配置如何进入快照语义 ✅ 建议：编码进 `embedder_id`

- **不扩 `ConfigFingerprint`**（bincode 非自描述 ⇒ 扩字段 = 老快照不可读 = 破坏性变更，同 R35）。
- 老快照在默认配置下**继续可加载**（`"bge-small-zh-v1.5"` 不变）。
- 是否把 `sessions` / `intra_threads` 也编进去，**由 S6-T6 探针的实测结果决定**（观测到数值差异就必须编）。

### D-S6-05 消除静默降级 ✅ 建议：方案 A（显式请求向量则硬失败）

- 见 §4.4.3 的方案对比。
- ⚠️ 这是**行为变更**（此前静默退化成功、此后报错）⇒ 必须在 CHANGELOG 标 `Changed` 并说明迁移方式。

### D-S6-06 T7-11 的形态 ✅ 建议：形态 A 必做，形态 B 不做

- `build --index` 追加 = **结构性收益**（跳过 93% 的耗时），且骨架已在（§2.5）。
- **形态 B（跨 run 的 chunk 级 embed 缓存）不做**：它要求把 `chunk_text → vector` 持久化（体积 ≈ `raw_vectors` 本身），并与 `FORMAT_VERSION` / 指纹 / compaction 的重编号全部耦合，**收益仅覆盖"文档内一处小改"**。⇒ 列 **V2.1 或不做**，本 Step 只记录理由。

### D-S6-07 追加后不自动 compact ✅ 建议：不自动

- `compact` 会**重编号 ID**（D-S4-01）⇒ 自动做会让"我刚追加的 doc_id"失效，破坏用户预期。
- 文档给出推荐序列（§4.5.5）。

### D-S6-08 NFR-10 的数值目标 ⏳ **需评审 / 用户拍板**（§4.6.3）

- 建议方案 A（相对提升，近线性判据）。
- 改 NFR 属**定义面变更** ⇒ 若采纳，需 `requirements-spec.md` 升版本行 + `plan-v2.md` §6 门槛同步。

### D-S6-09 T7-17 的计时与对照口径 ✅ 建议：吞吐为主、延迟为辅、结果逐位对照

- 见 §4.6.2。**明确禁止**把并发延迟与 NFR-02 横比。

---

## 6. 影响面与兼容性

| 面 | 变更 | 破坏性？ | 迁移 |
| --- | --- | --- | --- |
| `Embedder` trait | **无** | — | — |
| `LocalEmbedder::new()` | 行为不变（等价于默认 options） | 否 | — |
| `LocalEmbedder` 新增 `with_options` | 纯加法 | 否 | — |
| `ConfigFingerprint` | **结构不变**；`embedder_id` 的**取值集合扩大**（仅 `coreml` 档） | 否（默认档取值不变） | 老快照默认配置下照常加载 |
| `default_embedder()` 失败行为 | 静默退化 → **显式报错**（若选方案 A） | ⚠️ **是**（行为变更） | CHANGELOG `Changed` + 说明"要向量却拿不到向量时现在报错" |
| `helix build --index` | 新增参数组合 | 否（纯加法） | — |
| `helix bench --threads` | 新增参数（默认 `1`） | 否（默认行为不变） | — |
| `crates/core/Cargo.toml` | 新增 `ort` 直接依赖 + `coreml`（**仅当 E3 投**） | 否（feature 关时等价） | — |
| 效果指标（MRR/NDCG） | ⚠️ **若采纳 E3 则不可与 P5 横比** | — | 文档显式声明（§3.5） |

---

## 7. 测试计划（S6-T1 ~ S6-T14）

| # | 测试 | 覆盖 | CI |
| --- | --- | --- | --- |
| **S6-T1** | `build --index` 追加后 `num_docs/num_chunks/total_len` == 全量重建 | 追加正确性 | ✅ 秒级 |
| **S6-T2** | 追加后 BM25 检索的 `(source, score)` 序列 == 全量重建 | 口径见 §4.5.3（**不比 id**） | ✅ |
| **S6-T3** | 重复追加同一 delta 两次，三个计数不变 | 幂等双保险 | ✅ |
| **S6-T4** | 池化 `embed_documents`：**人为打乱完成顺序**（注入 `sleep`/逆序回填）⇒ 输出仍保序 | 保序契约（§4.3.1） | ✅（用 `Dummy` embedder，不依赖模型） |
| **S6-T5** | `embed_query` 与 `embed_documents` 使用**同配置** session（探针：断言两者 session 的 `intra_threads`/EP 相同） | 口径分裂防护（§4.3.2） | ✅ |
| **S6-T6** | 探针：不同 `sessions` / `intra_threads` 下，同一批文本的向量是否逐位相同 | 决定 D-S6-04 是否需编入线程配置 | ✅（需模型 ⇒ `#[ignore]`） |
| **S6-T7** | `default_embedder` 失败路径：要么 `Err`、要么可观测标志为真；**绝不静默** | D-S6-05 | ✅（注入必失败的 embedder） |
| **S6-T8** | 不变式：`export → import → insert_*` 分配的新 ID **严格大于**历史最大 ID（含尾部墓碑场景） | ID 复用防护（§4.5.2） | ✅ |
| **S6-T9** | 追加路径下 `ConfigFingerprint` 不变（同一装配追加前后指纹相等） | 指纹稳定 | ✅ |
| **S6-T10** | 追加后 `save → load` 往返：检索结果与追加后一致（同快照两次加载逐位一致，NFR-06 不破） | 与 Step 2/3 的交互 | ✅ |
| **S6-T11** | `--threads 4` 的 `hits`（`chunk_id` + `score`）**逐位等于** `--threads 1` | NFR-10 正确性 | ✅（小语料） |
| **S6-T12** | `bench --threads 1` 的默认行为与不带 `--threads` **完全一致**（回归防护） | 默认不变 | ✅ |
| **S6-T13** | 追加后**图 sidecar** 的 manifest `nb_point` 与新增 chunk 数吻合；冷启动仍 `Loaded` 而非 `Rebuilt` | ⚠️ Step 4 的同一坑（漏发 manifest ⇒ 每次冷启动降级 ≈10s） | ✅ |
| **S6-T14** | S6-T6 的数值差异探针（若 E3 落地）：CPU 建库 + CoreML 加载 ⇒ `ConfigMismatch`（**不是**静默错配） | D-S6-04 | `#[ignore]` |

> **CI 口径**：S6-T6 / S6-T14 需模型与 EP ⇒ `#[ignore]`（沿用项目既有模式，共 6 个 `#[ignore]` 中的新增项）。
> **性能数字一律本地 release 跑**，不进 CI（CI 共享 runner 的数字不具可引用性——项目既有纪律）。

---

## 8. 实施任务拆分（S6-01 ~ S6-10）

| # | 任务 | 依赖 | 量级 | PR 切分建议 |
| --- | --- | --- | --- | --- |
| **S6-01** | `examples/bench_embed_session.rs`（E1/E2/E3 载体 + 交错 A/B/A/B + JSON 输出） | 无 | S | **PR 1**（spike 载体，可与本设计一并或紧随） |
| **S6-02** | 跑 E1/E2/E3 + 记账入 `eval-report.md` §8.10 | S6-01 | S | PR 1 或 PR 2 |
| **S6-03** | **决策门**：产出「T7-09 投 / 不投」结论 + 依据（D-S6-01 的 30%/20% 判据） | S6-02 | S | PR 2（文档） |
| **S6-04** | `LocalEmbedder` 池化（若投）+ S6-T4/T5 | S6-03 | M | **PR 3**（独立，可延后） |
| **S6-05** | E3 依赖接入 + 许可复核 + S6-T6/T14（若投） | S6-03 | M | **PR 4**（独立） |
| **S6-06** | `build --index` 追加路径 + S6-T1/T2/T3/T8/T9/T13 | 无（可与 S6-01 并行） | S | **PR 5** |
| **S6-07** | 增量口径实测（NFR-03 ②）+ 脚本 + `eval-report` §8.10 | S6-06 | S | PR 5 |
| **S6-08** | `bench --threads` + S6-T11/T12 + NFR-10 实测数据 | S6-06（需快照 fixture） | S | **PR 6** |
| **S6-09** | D-S6-05 静默降级消除 + S6-T7 | 无 | S | 并入 PR 5 或 6 |
| **S6-10** | 四处定义面回写（NFR-03 ②/05/10/11 实测、架构 R36~R40、plan-v2 §7 进度、docs/README 索引）+ CHANGELOG | 全部 | S | 收尾 PR |

> **PR 切分原则**（沿用 Step 4/5 的教训）：**spike 载体与数据先落地**（S6-01~03），因为"投/不投"决定后面两个 M 级任务是否开工。**不要让 spike 和落地绑在一个 PR 里**——那会让"不投"这个结论无法合并。

---

## 9. 风险与未决问题

### 9.1 新增风险（**已写入架构 §14.4**，编号 R36 ~ R40；R36 / R37 为**条件性**）

| # | 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- | --- |
| **R36** | **多 session 抬高内存峰值**：N 份 ort arena / 工作区 | NFR-05（峰值 RSS 372MB）被顶穿；小内存机器 OOM | spike 必录峰值 RSS；决策门含「增量 ≤ 20%」；不达标即不投 | 未投则风险消失 |
| **R37** | **E3（CoreML）改变向量数值** ⇒ 效果指标与 P5 基线**不可横比**，且快照语义变 | 相关性结论失真；老快照错配 | D-S6-04 指纹编码 + §3.5 显式声明 + E3 独立决策 | 声明是**缓解不是消除**：若投，P5 的效果基线需重跑 |
| **R38** | **`default_embedder()` 静默降级**（`.ok()`）：要向量却拿到纯 BM25 | 静默的功能缺失（最坏的一类：不知道坏了） | D-S6-05（方案 A 硬失败）+ S6-T7 | ✅ 可关闭（本 Step 内修） |
| **R39** | **追加后图 sidecar 未重发 manifest** ⇒ 每次冷启动降级重建（≈10s） | NFR-04 静默失效（与 Step 4 同一坑） | S6-T13 显式断言 `Loaded` + `nb_point` 吻合 | ✅ 可关闭（测试锁住） |
| **R40** | **ID 复用导致幽灵向量**（旧向量挂到新 chunk） | 正确性（与 Q-C1 同族但成因相反） | §4.5.2 的证明 + S6-T8 不变式测试 | ⚠️ 今天的证明依赖三条隐含前提（尾部空洞保留等）⇒ 用测试锁，不靠文档 |

> R38/R39/R40 都属"**今天不破、但没人锁**"的类型——本项目的既有教训是这类问题最终都会有人撞上（R34 的潜在死锁即先例）。

### 9.2 未决问题（需评审或实测回答）

| # | 问题 | 由谁回答 | 阻塞谁 |
| --- | --- | --- | --- |
| **Q1** | E2 的收益方向与量级？ | S6-02 实测 | S6-03 决策门 |
| **Q2** | E3 在 macOS aarch64 上能否初始化？能否加速？ | S6-02 实测（可能"秒失败"） | S6-03 决策门 |
| **Q3** | 不同 `sessions` / `intra_threads` 是否改变向量数值？ | S6-T6 探针 | D-S6-04 是否需编入线程配置 |
| **Q4** | **NFR-10 的数值目标**（今天完全缺失） | **评审 / 用户拍板**（§4.6.3 三方案） | NFR-10 能否关闭 |
| **Q5** | "增量追加 10% 文档"的 **10% 以什么为单位**（文档数？chunk 数？字节？） | 评审（建议：**文档数**，因为 `NFR-03 ②` 写的是"追加 10% 文档"） | NFR-03 ② 的判据 |
| **Q6** | `default_embedder` 硬失败（方案 A）会不会破坏既有用户脚本？ | 评审 | D-S6-05 |
| **Q7** | 追加路径是否需要 `--no-graph-persist` 之类的逃生舱？ | 评审（建议：沿用既有 `--no-graph-persist`，不新增） | S6-06 |

---

## 附录 A：新增 / 变更 API 一览

```rust
// crates/core/src/embed/local.rs（仅当 E2/E3 投）
pub struct EmbedOptions { pub sessions: usize, pub intra_threads: Option<usize>, pub ep: Ep }
pub enum Ep { Cpu, CoreML }

impl LocalEmbedder {
    pub fn new() -> Result<Self>;                       // 行为不变
    pub fn with_options(opts: EmbedOptions) -> Result<Self>;  // 新增（纯加法）
}

// crates/core/src/embed/mod.rs
// ⚠️ `Embedder` trait 签名**不变**；仅补 rustdoc 说明「输出顺序必须与输入一致」

// crates/cli/src/main.rs
struct BuildArgs {
    // 既有：input / output / vectors / single_chunk / no_graph_persist
    index: Option<PathBuf>,     // 新增：既有快照（与 --input 同给 = 追加）
    ep: Option<String>,         // 新增：cpu | coreml（仅当 E3 投）
}

// crates/cli/src/bench.rs
struct BenchArgs {
    // 既有：index / input / queries / modes / k / reps / warmup / ...
    threads: usize,             // 新增，默认 1（= 现状行为）
}
```

**不变式（写入 rustdoc）**：
1. `Embedder::embed_documents` 的输出**按位置**与输入一一对应（顺序契约）。
2. `helix build --index` 追加**不做 upsert-by-source**；同一 `source` 内容变化会新增文档（§4.5.5）。
3. 新分配的 `DocId` / `ChunkId` **严格大于**同一 `ForwardStore` 历史最大已分配 ID。

---

## 附录 B：执行命令

```bash
# 0) 守门（CI 口径）
make fmt && make lint && make test && make deny

# 1) T7-09 spike（E1/E2/E3 一次跑完；release；本机 aarch64）
cargo run -p helix-core --release --example bench_embed_session -- --json /tmp/s6-embed.json
#    交错 A/B/A/B + 控制组漂移打印；漂移 >10% 时按控制组归一后再比较

# 2) T7-11 增量：先建全量基线，再追加 10%
cargo run -p helix-cli --release -- build --input data/t2-corpus.jsonl  --vectors --single-chunk --output /tmp/base.idx
head -n 1200 data/t2-corpus.jsonl > /tmp/delta.jsonl          # 12K 的 10%
cargo run -p helix-cli --release -- build --index /tmp/base.idx --input /tmp/delta.jsonl --vectors --output /tmp/inc.idx
#    判据：本次耗时 < 首次全量 × 10% × 1.2（NFR-03 ②）
#    对照：全量重建 12K + 1200 与 /tmp/inc.idx 的 (source, score) 序列一致（S6-T2）

# 3) T7-17 并发检索（结果正确性 + QPS）
for t in 1 2 4 8; do
  cargo run -p helix-cli --release -- bench --index /tmp/base.idx --threads $t \
    --modes bm25,vector,hybrid --reps 20 --json /tmp/s6-threads-$t.json
done
#    判据：--threads 4 的 QPS ≥ --threads 1 × 2.5（若 D-S6-08 采纳方案 A）；
#          且 $t 线程的 hits 序列逐位等于 --threads 1

# 4) 数据落表
#    把 1) 与 3) 的结果写入 docs/devel/eval-report.md §8.10（新增小节），
#    与 §8.2 / §8.6 的既有数字**同口径对照**（同为 12K / 4000 段 / release）。
```

---

## 附录 C：依赖源码核实记录（本文技术前提）

**`fastembed-6.0.2`**（`~/.cargo/registry/src/*/fastembed-6.0.2`）

| 位置 | 事实 |
| --- | --- |
| `src/text_embedding/init.rs:33` | `pub intra_threads: Option<usize>`（默认 `None` = 用满所有核） |
| `src/text_embedding/init.rs:51` / `:83` / `:122` | `with_execution_providers(Vec<ExecutionProviderDispatch>)` 存在 |
| `src/text_embedding/init.rs:133` | `with_intra_threads` 存在 |
| `src/lib.rs:89` | `pub use ort::execution_providers::ExecutionProviderDispatch;`（**只重导出类型，不透传 feature**） |
| `Cargo.toml` `[features]` | **无 `coreml`**；EP 透传只有 `directml = ["ort/directml"]`（及 `cuda`/`mkl`/`metal`/`accelerate` 等 candle 侧） |
| `src/text_embedding/mod.rs:5` | `const DEFAULT_BATCH_SIZE: usize = 256;` |
| `src/text_embedding/impl.rs:364` | `batch_size.unwrap_or(DEFAULT_BATCH_SIZE)` |
| `src/text_embedding/impl.rs:71` | `init_session_builder(execution_providers, intra_threads)` —— E1/E2/E3 共用同一构造路径 |
| `src/text_embedding/impl.rs:381` | `texts.chunks(batch_size)` —— 内部切块（我们传 `None` ⇒ 256，但调用方已按 64 切，故不触发） |

**`ort-2.0.0-rc.13`**（`~/.cargo/registry/src/*/ort-2.0.0-rc.13`）

| 位置 | 事实 |
| --- | --- |
| `Cargo.toml` `[features]` | `coreml = ["ort-sys/coreml"]` 存在 |
| `src/ep/mod.rs`（gate 段） | `#[cfg(feature = "coreml")] pub mod coreml; pub use self::coreml::CoreML;` |
| `src/ep/coreml.rs:68` | `pub struct CoreML`（**不是** `CoreMLExecutionProvider`——计划文档的写法已过时） |
| `src/ep/coreml.rs:147-153` | `with_compute_units(ComputeUnits::CPUAndNeuralEngine)` 等旋钮 |
| `src/lib.rs:39` | `pub mod ep;` |

⚠️ **反面记录**：`plan-v2.md` §4 Step 6 与 §附-1 写「`fastembed` 暴露 `with_execution_providers`」（✅ 对）与「`RerankerModel::BGERerankerV2M3` 含 `model.onnx.data`」（属 Step 7，未在本步核实）——本文**只更正 EP 类型名**，其余不改。
✅ **已同步**：`plan-v2.md` §附-1 已补「2026-09-11 Step 6 设计期复核」三条更正（EP 类型名 `ort::ep::CoreML`、`fastembed` 无 coreml 透传、两处源码路径 + 实际 batch 是 64），与本附录一致。

---

## 附录 D：本文引用的项目内证据

| 主张 | 位置 |
| --- | --- |
| `Mutex` 全程持锁（Q-M1） | `crates/core/src/embed/local.rs:35,67,75` |
| `TextEmbedding::embed` 需 `&mut self` | `fastembed-6.0.2/src/text_embedding/impl.rs:453` |
| `InitOptions` 未调 `with_intra_threads` | `crates/core/src/embed/local.rs:42-46` |
| `add` 短路查重（FR-15） | `crates/core/src/search/index.rs:356-365` |
| `Index::add` 内部第二道去重 | `crates/core/src/index/mod.rs:166-173` |
| `content_hashes` 入快照 / 出快照 | `crates/core/src/index/mod.rs:88`、`:501-503`、`:535` |
| `flush` 的 zip 顺序契约 | `crates/core/src/search/index.rs:427-459`（尤其 `:449`） |
| `flush` 的切片 = `batch_size`(64) | `crates/core/src/search/index.rs:390`、`crates/core/src/search/config.rs:26` |
| `embed_elapsed` 的累计口径 | `crates/core/src/search/index.rs:492-500` |
| CLI `build` 只有 `--input`（无追加入口） | `crates/cli/src/main.rs:49-66`、`:34-46`（`Command` 枚举） |
| `ForwardStore` ID = 槽位长度 | `crates/core/src/index/forward.rs:40,50` |
| `export/import` 保留尾部空洞 | `crates/core/src/index/forward.rs:176-190`、`:196-199`；`crates/core/src/index/mod.rs:505-510,535` |
| `Searcher` 的 `Send + Sync` + 编译期断言 | `crates/core/src/search/searcher.rs:20-30,249-252` |
| 读路径**零** `Mutex/RwLock/RefCell/unsafe` | 实测 grep `query/`、`search/searcher.rs`、`retriever/`、`fusion/`、`index/` 命中 0 |
| `ConfigFingerprint` 四字段 + `ConfigMismatch` | `crates/core/src/storage/snapshot.rs:30-45`、`:152` |
| `default_embedder` 静默降级 | `crates/core/src/search/config.rs:312-318` |
| `Config: Send + Sync` 的理由 | `crates/core/src/search/config.rs:28-34` |
| NFR-03 双口径 / NFR-05 / NFR-10 / NFR-11 | `docs/devel/requirements-spec.md` §6.1（v1.12） |
| 225.5s（embed 209.9s）/ 批次表 / 建图 4% | `docs/devel/eval-report.md` §8.2（`:137-156`）、§8.6（`:232-233`）、§8.4（`:199`） |
| Step 4 的 manifest 重发铁律 | `docs/devel/plan-v2.md` §4 Step 4（`:288-290`） |
| R35（破坏性变更的判定先例） | `docs/devel/architecture-design.md` §14.3 |
| 标定方法学（交错 / 控制组归一 / 运行范围） | 技能 `perf-ab-calibration` v1.1.0 |
