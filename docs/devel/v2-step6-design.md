# HelixIndex V2 · Step 6 详细设计（构建性能与多线程：embed 并行 spike + 增量构建 + 并发检索压测）

| 项 | 内容 |
| --- | --- |
| 版本 | **v0.4（S6-10 收尾回写，2026-09-13）** |
| 日期 | 2026-09-13 |
| 状态 | **✅ Step 6 已完整收尾（S6-10）**——所有实现任务**开工完毕**、**定义面四处已回写**（需求 **v1.15** / 架构 **v1.14**）。**S6-04 / S6-05 判「不投」**（T7-09 实测：E2 吞吐 +11.7%/+16.0%（门槛 ≥+30%）、峰值 RSS +96.4%/+289.2%（门槛 ≤+20%）；E3 调优后仅 +1.2%）⇒ `coreml` feature 作为默认关闭的探针基础设施保留（详见 `eval-report.md` §8.10）。**S6-10 的回写内容**：① §7 测试表末列按实现实况**逐条**回写（`✅` 落地 / `⛔` 随 S6-04 取消 / `⏸` 随 S6-05 挂起；**S6-T10 由本批补齐**），§7 的「可测性前提」注记已结账；② §4.6.1 的 `Arc<Searcher>` 措辞按实现更正（`&QueryExecutor` + `thread::scope`，评审 P3-4）；③ D-S6-01 行补**实现期结案标记**、§9.2 下方的漂移注记**作废**（保留原文留痕）、§11.3 的「仍待 S6-10」清单清账并新增 **§11.6**；④ **NFR-10 的处置**：② 的适用范围**收窄**为「不含查询侧编码的读路径」、**NFR-10 整体保持「打开」**、编码侧并发登记为架构 **R43**（复审触发条件 = 查询侧会话池落地时）；⑤ **hybrid 无锁对照实验已补跑**（`eval-report.md` §8.13）⇒ R43 的 hybrid 归因由**推断**升为**实测**。此前 v0.2：**设计中**。PR #38 评审的 **7 条意见（F1~F7）全部采纳**（见 §10）；**Q4 / Q5 / Q6 / Q7 已由评审表态拍板** ⇒ **D-S6-05** 与 **D-S6-08（NFR-10 口径 = 方案 A）** 定案，需求文档随之升 **v1.14**；**D-S6-01 的数值**仍由 S6-06 的 spike 回答，本版只定形态与判据 |
| 上游 | `plan-v2.md` §4 Step 6 / issue **#2**（V2-Step6）/ `requirements-spec.md` **v1.15**（**NFR-03 双口径**、NFR-05、NFR-10、NFR-11）/ `architecture-design.md` **v1.14**（§14.4 R36~R43）/ `eval-report.md` §8.2、§8.4、§8.6、§8.10~**§8.13** / `search/index.rs`、`embed/local.rs`、`search/config.rs`、`cli/{main,bench}.rs` |
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
| **D-S6-05** | `default_embedder()` 的静默降级 | ✅ **已拍板（评审 Q6）**：方案 A —— 本 Step 内**消除**（`LocalEmbedder::new().ok()` ⇒ 显式可见），与 NFR-07「降级不得静默」对齐；**不额外加兼容开关** | §2.7 / §4.4.3 |
| **D-S6-06** | T7-11 的形态 | ✅ 建议：**形态 A（`build --index` 追加）必做**；形态 B（跨 run chunk 级 embed 缓存）**列为可选、本 Step 不做** | §4.5 |
| **D-S6-07** | 追加后是否自动 compact | ✅ 建议：**不自动**；文档给出「追加 → compact」的推荐序列（compact 是显式动作，Step 4 已定） | §4.5.5 |
| **D-S6-08** | NFR-10 的**数值目标** | ✅ **已拍板（评审 Q4）**：方案 A（相对提升）—— `--threads 4` QPS ≥ `--threads 1` × **2.5**；**逐位一致为前置条件**；测量口径见 §4.6.2。**✅ S6-10 定稿（2026-09-13）**：`×2.5` 由「拟」定稿、**② 的适用范围收窄**为「不含查询侧编码的读路径」，编码侧并发登记为架构 **R43** 并**保持「打开」**（已落需求文档 **v1.15**，见 §11.6） | §4.6.3 / §9.2 Q4 / §11.6 |
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
5. **T7-17 达标（NFR-10，方案 A 已拍板）**：`helix bench --threads {1,2,4,8}` 输出 QPS、各线程延迟分位；
   **前置条件**：多线程检索结果与单线程**逐位一致**（`hits` 的 `chunk_id` 序列与 `score` 逐位相同）；
   **吞吐判据**：`--threads 4` 的 QPS ≥ `--threads 1` × **2.5**；测量口径见 §4.6.2 / 附录 B 第 3 步。
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
| `intra_threads` 默认 `None` = **用满所有核**，我们未覆盖 | ✅ 成立 | `fastembed-6.0.2/src/init.rs:20`（`InitOptionsWithLength.intra_threads`，即 `TextInitOptions`；⚠️ 路径已更正，原写 `src/text_embedding/init.rs:33` 是 `InitOptionsUserDefined`）、`embed/local.rs:42-46` 确实没调 `with_intra_threads` |
| 批次不是瓶颈 | ✅ 成立 | 32/64/128/256 → 62.6/59.1/54.4/51.6 条/s，差异 <20%，且**小 batch 略快**（`eval-report.md:147-148`） |
| 并行建图只占 4% | ✅ 成立 | 12K 里 11.7s/225.5s = 5.2%（`eval-report.md` §8.6） |
| fastembed 暴露 `with_execution_providers` | ✅ 成立 | `src/init.rs:83`（`InitOptionsWithLength`）/ `:122`（`InitOptions<M>`）。⚠️ 路径已更正（原写 `src/text_embedding/init.rs:51/83/122`） |
| **走 CoreML 只要加个 feature** | ❌ **不成立，需修正** | fastembed 6.0.2 **没有 coreml feature**（只有 `directml = ["ort/directml"]`）⇒ 必须把 `ort` 提为直接依赖。详见 2.4 |

⚠️ 由此得到一个**反直觉但重要**的推论：`intra_threads` 已经用满所有核 ⇒ **E2（多 session × 线程分片）的本质不是"加并行"，而是"把同一批算力切开重新分配"**。若 ort 的 per-session arena 不能共享，E2 完全可能**零收益甚至负收益**。这正是它必须是 spike 而非承诺的原因。

### 2.4 【关键技术前提】fastembed 6.0.2 / ort 2.0.0-rc.13 的 session 与 EP 能力（源码核实）

> 本节是本 Step 唯一"新引入外部能力"的技术前提，逐条给出文件位置与行号。**未核实的不写。**

| 事实 | 位置 | 对设计的影响 |
| --- | --- | --- |
| `InitOptions.intra_threads: Option<usize>`，`None` = 用满所有核 | `fastembed-6.0.2/src/init.rs:20`（`InitOptionsWithLength`）；`with_intra_threads` 在 `:94`。⚠️ **路径已更正**（2026-09-11 实现期复核，见附录 C） | E2 的**唯一可调旋钮** |
| `with_execution_providers(Vec<ExecutionProviderDispatch>)` 存在 | `src/init.rs:83`（`InitOptionsWithLength`，= 我们走的 `TextInitOptions`）/ `:122`（`InitOptions<M>`）。⚠️ **路径已更正** | E3 的接入点 |
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

> **评审补充（已采纳）**：grep 词清单补入 **`rayon`** —— `search/searcher.rs:207` 有 **`rayon::join`**（双 lane 并行）。它是**并行但确定性合并**（结果按固定顺序拼装），**不构成** S6-T11「逐位一致」的反例；补进来是为了避免后人以为「没查并行」。

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
sessions ∈ {2, 4}，intra_threads = ceil(核数 / sessions)（⚠️ **实现期更正**：原写 `max(1, 核数 / sessions)` 是**截断**，10 核 / 4 session ⇒ 2；实现取 `div_ceil` ⇒ 3、共 12 个 intra-op 线程跑在 10 核上，**允许轻微超额以免留核空转**。§8.10 的「2×5 / 4×3」即后者），EP = CPU
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
  `ort = { version = "=2.0.0-rc.13", default-features = false, optional = true }`
  + `coreml = ["dep:ort", "ort/coreml", "local-embed"]`（**feature 参与加法式合并**，不会与 fastembed 拉进两份 ort；
  **必须精确锁定**，理由同 `Cargo.lock` 里 ort 的锁定注释）。
  ⚠️ **实现期更正（评审 F6）**：本行原写 `features = ["coreml"]` 的非可选形态 —— 实际落地的**可选依赖 + 默认关闭的 feature**
  更好（默认构建完全不拉 `ort`），此处已按代码更正。
- ⚠️ **E3 的定位是"探路"而非"承诺"**：macOS aarch64 上的 CoreML EP 是否真的接入、是否真的加速，**没有任何先验证据**。三档可能：显著加速 / 无差异 / 初始化失败（此时正好验证 §2.7 的静默降级风险）。
- ⚠️ **E3 若有效，代价是三重的**：① 向量数值变（§3.5）② 快照语义变（§3.1，需 D-S6-04）③ 内存峰值变（CoreML 有额外转换缓冲）。三者**都必须在上线前处理**，因此 E3 落地**必须单独立决策**，不能"顺手打开"。

#### 4.2.5 载体：`crates/core/examples/bench_embed_session.rs`

照 `examples/bench_batch_size.rs` 的体例（自包含、`required-features = ["local-embed"]`、release 跑、打印表格）。设计要点：

- ~~**一次进程内跑完 E1/E2/E3 的所有档位** ⇒ 消除跨进程的环境漂移~~（这是本项目已吃过亏的坑：`perf-ab-calibration` 里"非交错执行有系统性偏差 ×1.38~1.54"）。
  ⚠️ **实现期推翻（2026-09-11，S6-01）**：本机实测**单 session 在 batch 64 × 长文本下的峰值 RSS：1 个 batch 2.11 GB、8 个 batch 后饱和于 3.3 GB**（`eval-report.md` §8.10 表 3）
  （是**激活张量**不是权重；对照：同模型 batch-1 的 `search` 路径才 372MB），
  而同进程并存 7 个 session（`1+2+4`）会顶穿 32GB 内存 ⇒ 换页会**均匀拖慢所有档位**、
  使"档位间比较"失去意义。⇒ 改为**逐档位独立进程 + 按波交错**（`A/B/C` 各起一个进程算一波，跑 R 波），
  交错仍保留在"波"这一层，且顺带得到**可归因的分档位峰值 RSS**（peak RSS 是进程级单调量，
  同进程方案要么做不到、要么得用 `unsafe` 读 `getrusage(2)`）。实现与实测见
  `scripts/eval_embed_session.sh` 与 `eval-report.md` §8.10。
- ~~⚠️ **档位内两轮、顺序交错**（A/B/A/B）~~ ⇒ **实现期替换**：现协议是**每波一个独立进程、每档位每波只有一个计时样本**（`scripts/eval_embed_session.sh` 恒传 `--warmup 0`），
  **没有"档位内重复采样"**；交错只发生在「波」这一层，**每档位样本量 = 波数（实测用 3）**。控制组波间漂移仍打印，供事后归一（实测 −0.9%）。
- 输出 JSON（可选 `--json <path>`）⇒ 供 `eval-report.md` §8.10 落表，避免手工抄录。

### 4.3 T7-09 若采纳 E2 的落地形态（**仅当 spike 判定投**）

#### 4.3.1 保序回填（硬要求）

```rust
// 示意：按块分发，按块索引回填；完成顺序无关
let mut out: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
pool.install(|| {
    out.par_chunks_mut(BATCH).enumerate().for_each(|(i, slot)| {
        // ⚠️ 末块可能不足 BATCH：`texts[i * BATCH..(i + 1) * BATCH]` 会**越界 panic**（评审 F6）
        //    ⇒ 必须夹紧右界；等价写法是与 `texts.chunks(BATCH)` 配对。
        let lo = i * BATCH;
        let hi = (lo + BATCH).min(texts.len());
        let v = sessions[i % sessions.len()]
            .lock()
            .unwrap()
            .embed(&texts[lo..hi], None)
            .unwrap();
        for (s, x) in slot.iter_mut().zip(v) {
            *s = Some(x);
        }
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
- ⚠️ **评审 F3：若 S6-T6 判定「必须编」，`&'static str` 覆盖不了任意 `usize` / `Option<usize>` 组合**
  （`Embedder::id()` 现签名见 `embed/mod.rs:43`）。⇒ **两条升级路径先写在这里，别等探针出结果才临时找路**：

  | 路径 | 形态 | 对指纹 / 老快照的兼容性 |
  | --- | --- | --- |
  | **a（建议）有限档位枚举** | `sessions` / `intra_threads` 都**收敛成有限档**（如 `sessions ∈ {1,2,4,8}`、`intra_threads ∈ {None,1,2,4}`），`id()` 仍由 `match` 出 `&'static str` | **零结构变更**、零分配；老快照取值不变 ⇒ 照常加载。代价 = 配置面被收窄（超档组合需报错或吸附到最近档） |
  | **b 放宽为 `Cow<'static, str>`** | `id()` 返回 `Cow<'static, str>`，任意组合格式化出串 | `ConfigFingerprint` 的**字段类型变了** ⇒ 与 R35 同类的**破坏性变更**（老快照读不出该字段），需 `FORMAT_VERSION` 处理或迁移 |

  ⇒ 若走 b，D-S6-04 的"零迁移成本"结论**失效**，必须同步 §3.1 / §6 影响面与架构 R35 的判定。

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

- 实现：`std::thread::scope` × N 个 worker，**借用同一个 searcher 的 `&QueryExecutor`**
  （`QueryExecutor: Sync` 有**编译期断言**；`Searcher: Clone + Send + Sync`，**无需改内核**）。
  ⚠️ **更正（S6-10，评审 P3-4）**：原文写「共享 `Arc<Searcher>`」**与实现漂移**——实现走
  `&QueryExecutor` 共享借用 + `thread::scope`，**全程未使用 `Arc`**。两者功能等价（都是
  「零共享可变状态的多读」），但 `Arc` 会掩盖「只读借用」这一事实，并在读者脑中引入
  「需要克隆 / 共享所有权」的错误暗示 ⇒ 已按实现更正。
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

#### 4.6.3 NFR-10 的数值目标缺失（D-S6-08）——✅ **已拍板：方案 A**（评审 2026-09-11）

现状：NFR-10 全文只有「多线程并发检索吞吐与正确性（`Searcher` 跨线程）」+「V2.0 Step 6 实测」。**没有任何数字**。

这意味着**今天无法判定 NFR-10 是否达标**——因为不存在判据。三条出路：

| 方案 | 内容 | 代价 |
| --- | --- | --- |
| **A（建议）** | 先测出**基线数据**（`--threads 1` 的 QPS），把 NFR-10 的口径改为**相对提升**：「`--threads 4` 的 QPS ≥ `--threads 1` × 2.5（近线性，允许 40% 折损）」 | 需要一个"折损系数"，属拍脑袋；但**可证伪**且不伪装成绝对指标 |
| B | 定绝对 QPS 目标 | 与机器强耦合（本项目已明确「性能数字不具跨机可引用性」）⇒ **不建议** |
| C | 维持"只记录，不判定" | 诚实，但 NFR-10 永远无法关闭 |

- ~~本文建议 **A**~~ ⇒ **评审已拍板：方案 A**。口径随之写入 `requirements-spec.md` **v1.14**，但**标「拟」、数值待 S6-08 实测后定稿**——
  与 Step 5 的 NFR-13 先例一致（v1.10 先落「拟 ≤20ms」，v1.12 标定后才定稿）。
  ✅ **S6-10 定稿（2026-09-13）**：S6-08 实测 `QPS(4)/QPS(1)` = bm25 **2.98×** ✅ / hybrid **1.63×** / vector **1.88×**
  ⇒ **×2.5 由「拟」定稿**（需求升 **v1.15**）；并**收窄 ② 的适用范围**为「不含查询侧编码的读路径」、
  **NFR-10 整体保持「打开」**（编码侧并发登记为架构 **R43**）。**先例如期走完**：v1.10 落「拟」→ v1.15 定稿。
- **评审补充的两点必须一并落进需求文本**（已写进 v1.14，后续修订**不得丢掉**）：
  1. 「各线程 `hits` 逐位一致」是**前置条件**（先正确、后吞吐），**不是**与吞吐并列的第二条判据；
  2. 判据必须**写明测量口径**：预热丢弃首轮 + 顺序交错（1/2/4/8/1/2/4/8）+ 报告运行范围（核数 / 是否插电 / 是否在 CI）
     —— 即 §4.6.2 与附录 B 第 3 步的口径，避免将来被不同口径的重测打脸。
- **评审对另两方案的反对**（记入决策）：**反对 B**（绝对 QPS 与机器强耦合，与本项目「性能数字不具跨机可引用性」的既定立场矛盾）；**反对 C**（只记录、不判定 ⇒ NFR-10 永远无法关闭）。
- ⚠️ 方案 A 的**鉴别力来源**：读路径**无共享可变状态**（§2.6）是个**强先验** ⇒ 4 线程低于 2.5× 反而需要解释。不是橡皮图章。
  🔴 **S6-10 更正（评审 P3-8）**：该先验**只对「索引读路径」成立**；**端到端**的 vector / hybrid 还含
  **查询侧编码**（`crates/core/src/embed/local.rs:35` 的 `Mutex<TextEmbedding>`）⇒ 实测 4 线程
  **1.88× / 1.63×** 的解释**不是**内存带宽或分配器争用，而是**这把锁**（临时把 `embed_query`
  移出被测路径的对照实验已把 vector 4 线程抬到 **3.15×**）。见架构 **R43**；`requirements-spec.md`
  的 NFR-10 备注已按同一口径限定。

---

## 5. 决策记录（D-S6-01 ~ D-S6-09）

### D-S6-01 T7-09 是否投 M 级 ✅ **已结案（2026-09-12，PR #40）：判「不投」**

> **实现期结案**：**E2**（多 session）增益 **+11.7% / +16.0%**（门槛 ≥ +30%），而峰值 RSS
> **+96.4% / +289.2%**（门槛 ≤ +20%）；**E3**（CoreML）调优后仅 **+1.2%** ⇒ **两侧门槛都不过**。
> ⇒ **S6-04（池化）/ S6-05（EP 接入）不开工**；`coreml` feature 作为**默认关闭的探针基础设施保留**
> （它是「不投」这个结论的物证，也是换机重测的唯一入口）。完整数据与协议更正见 `eval-report.md` **§8.10**。
> ⚠️ **本节其余行保留设计期原文**（已评审内容不重写），其语气**一律以本注记与 §11 为准**；
> **S6-10（2026-09-13）已把该结论回写到定义面四处**（需求 / 架构 / `docs/README.md` / `plan-v2.md`）。
> 🔑 **该结论不得外推到查询侧**：它测的是**建库**路径的多 session（batch 64 已饱和、RSS 代价巨大），
> 而**查询**路径每次只编 1 条 ⇒ 池化在查询侧的上界远大于建库侧（见架构 **R43**）。

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

### D-S6-05 消除静默降级 ✅ **已拍板（评审 Q6）：方案 A**（显式请求向量则硬失败）

- 见 §4.4.3 的方案对比。
- **评审意见（Q6）**：0.x 阶段 + CHANGELOG `Changed` + 迁移说明**足够**，**不需要**额外的兼容开关。
  理由：它把「静默拿错结果」换成「启动时显式报错」⇒ 对依赖旧行为的脚本而言是**把潜伏 bug 提前暴露**，收益大于成本。
- ⚠️ 这是**行为变更**（此前静默退化成功、此后报错）⇒ 必须在 CHANGELOG 标 `Changed` 并说明迁移方式。

### D-S6-06 T7-11 的形态 ✅ 建议：形态 A 必做，形态 B 不做

- `build --index` 追加 = **结构性收益**（跳过 93% 的耗时），且骨架已在（§2.5）。
- **形态 B（跨 run 的 chunk 级 embed 缓存）不做**：它要求把 `chunk_text → vector` 持久化（体积 ≈ `raw_vectors` 本身），并与 `FORMAT_VERSION` / 指纹 / compaction 的重编号全部耦合，**收益仅覆盖"文档内一处小改"**。⇒ 列 **V2.1 或不做**，本 Step 只记录理由。

### D-S6-07 追加后不自动 compact ✅ 建议：不自动

- `compact` 会**重编号 ID**（D-S4-01）⇒ 自动做会让"我刚追加的 doc_id"失效，破坏用户预期。
- 文档给出推荐序列（§4.5.5）。

### D-S6-08 NFR-10 的数值目标 ✅ **已拍板：方案 A**（评审 2026-09-11，§4.6.3）

- 口径 = **相对提升（近线性判据）**：`--threads 4` 的 QPS ≥ `--threads 1` × **2.5**（允许 40% 折损）；
  **逐位一致为前置条件**；测量口径见 §4.6.2 / 附录 B 第 3 步。
- 评审**反对 B**（绝对 QPS 与机器强耦合）与 **C**（只记录不判定 ⇒ 永不关闭）。
- 改 NFR 属**定义面变更** ⇒ 本 PR 已同步四处：`requirements-spec.md` **v1.13 → v1.14**（NFR-10 口径 + NFR-03 ② 的单位注记）/
  `plan-v2.md` **v0.14**（§6 门槛、§7 进度表、§8 回写计划）/ `architecture-design.md` **v1.13**（版本面同步，无架构变更）/ `docs/README.md` 索引行。
- ⚠️ **数值以「拟」写入，待 S6-08 实测后定稿**（同 Step 5 的 NFR-13 先例）——这样既让 NFR-10 从"无法判定"变为"可判定"，
  又不把未测的数字写成既成事实。**✅ S6-10（2026-09-13）：已按 S6-08 实测定稿**（需求 **v1.15**；
  ×2.5 保留，② 收窄为「不含查询侧编码的读路径」，编码侧并发登记为架构 **R43** 并保持「打开」）。

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

| # | 测试 | 覆盖 | 实现状态（**S6-10 回写，2026-09-13**） |
| --- | --- | --- | --- |
| **S6-T1** | `build --index` 追加后 `num_docs/num_chunks/total_len` == 全量重建 | 追加正确性 | ✅ `crates/core/tests/step6_incremental_build.rs` |
| **S6-T2** | 追加后 BM25 检索的 `(source, score)` 序列 == 全量重建 | 口径见 §4.5.3（**不比 id**） | ✅ 同上 |
| **S6-T3** | 重复追加同一 delta 两次，三个计数不变 | 幂等双保险 | ✅ 同上（CLI 侧另有 smoke 对应） |
| **S6-T4** | 池化 `embed_documents`：**人为打乱完成顺序**（注入 `sleep`/逆序回填）⇒ 输出仍保序 | 保序契约（§4.3.1） | ⛔ **不做** —— 随 **S6-04（池化）取消**（T7-09 判「不投」，见 §11.1） |
| **S6-T5** | `embed_query` 与 `embed_documents` 使用**同配置** session（探针：断言两者 session 的 `intra_threads`/EP 相同） | 口径分裂防护（§4.3.2） | ⛔ **不做** —— 同上；无池化 ⇒ 只有单 session，「两侧配置分裂」这一风险面不存在 |
| **S6-T6** | 探针：不同 `sessions` / `intra_threads` 下，同一批文本的向量是否逐位相同 | 决定 D-S6-04 是否需编入线程配置 | ⏸ **挂起** —— 随 **S6-05（E3）取消**；**Q3 同挂起**（见 §9.2 / §11.5）。若将来重开池化，**必须先答此题** |
| **S6-T7** | `default_embedder` 失败路径：要么 `Err`、要么可观测标志为真；**绝不静默** | D-S6-05 | ✅ `crates/core/src/search/config.rs` 单测（注入接缝，**不需真模型**） |
| **S6-T8** | 不变式：`export → import → insert_*` 分配的新 ID **严格大于**历史最大 ID（含尾部墓碑场景） | ID 复用防护（§4.5.2） | ✅ `step6_incremental_build.rs` |
| **S6-T9** | 追加路径下 `ConfigFingerprint` 不变（同一装配追加前后指纹相等） | 指纹稳定 | ✅ 同上 |
| **S6-T10** | 追加后 `save → load` 往返：检索结果与追加后一致（同快照两次加载逐位一致，NFR-06 不破） | 与 Step 2/3 的交互 | ✅ **由 S6-10 补齐**（`step6_incremental_build.rs`）—— ⚠️ 本行原设计承诺 ✅ 但**实现期漏做**，收尾时才发现（见 §11.6） |
| **S6-T11** | `--threads 4` 的 `hits`（`chunk_id` + `score`）**逐位等于** `--threads 1` | NFR-10 正确性 | ✅ `crates/cli/src/bench.rs` 单测 `并发检索结果逐位一致` + CI 阶段 B2 的**前置条件三条并检** |
| **S6-T12** | `bench --threads 1` 的默认行为与不带 `--threads` **完全一致**（回归防护） | 默认不变 | ✅ `bench.rs` 单测 `并发档位解析` + CI smoke（`--threads 1` 与不传**逐字一致**） |
| **S6-T13** | 追加后**图 sidecar** 的 manifest `nb_point` 与新增 chunk 数吻合；冷启动仍 `Loaded` 而非 `Rebuilt` | ⚠️ Step 4 的同一坑（漏发 manifest ⇒ 每次冷启动降级 ≈10s） | ✅ `step6_incremental_build.rs`（实测侧证见 `eval-report.md` §8.11） |
| **S6-T14** | S6-T6 的数值差异探针（若 E3 落地）：CPU 建库 + CoreML 加载 ⇒ `ConfigMismatch`（**不是**静默错配） | D-S6-04 | ⏸ **挂起** —— 随 **S6-05（E3）取消**。同文件另有一条**改向后的**用例（半向量判据的前提，见 §4.4.3），**与 E3 无关**，仍在 CI |

> **CI 口径**：S6-T6 / S6-T14 需模型与 EP ⇒ `#[ignore]`（沿用项目既有模式，共 6 个 `#[ignore]` 中的新增项）。
> **性能数字一律本地 release 跑**，不进 CI（CI 共享 runner 的数字不具可引用性——项目既有纪律）。
>
> **可测性前提（S6-T4 / S6-T5 / S6-T7 的共同前置；评审 F7）**：这三条要在 CI 里秒级跑，必须先做**逻辑与真模型解耦**，
> 否则与上表的 CI 口径自相矛盾：
> - **S6-T4 / S6-T5**：会话池 + 保序回填要抽成**对 session 泛型的辅助结构**（池化逻辑若内嵌在持有真模型的
>   `LocalEmbedder` 里，`Dummy` embedder 无法注入）⇒ 否则只能 `#[ignore]`。
> - **S6-T7**：`default_embedder()` 的**校验逻辑与构造必须分离**（现在是自由函数直接 `LocalEmbedder::new()`）
>   ⇒ 否则无法"注入必失败的 embedder"。
> ⇒ 这三条属**可测性驱动的设计约束**，在 **S6-04 / S6-05 / S6-09** 落地时一并满足；
> 若最终仍做不到，**如实降级为 `#[ignore]` 并在 PR 里说明**（不得把 `#[ignore]` 悄悄留在表里当成已覆盖）。
>
> ⚠️ **S6-10 结账（2026-09-13）**：这三条前提的归属**随 S6-04 / S6-05 的取消而消解** ——
> **S6-T4 / T5 不做**（无池化 ⇒ 无保序回填、无「两侧配置分裂」风险面）、**S6-T6 挂起**（E3 未投，
> 连带的 Q3 也挂起）；只有 **S6-T7 真正落地**，且它所需的解耦（`crates/core/src/search/config.rs:309`
> 的**内部测试接缝**，而非公开扩展点）已在 **S6-09 / PR #43** 完成，**未退化为 `#[ignore]`**。
> 上表末列已按实现实况**逐条**回写 —— **没有任何一条 `#[ignore]` 被留在表里充数**。

---

## 8. 实施任务拆分（S6-01 ~ S6-10）

| # | 任务 | 依赖 | 量级 | PR 切分建议 |
| --- | --- | --- | --- | --- |
| **S6-01** | `examples/bench_embed_session.rs`（E1/E2/E3 载体 + JSON 输出）+ `scripts/eval_embed_session.sh`（逐档位独立进程 + 按波交错 + 决策门合取表） | 无 | S | **PR 1**（spike 载体，可与本设计一并或紧随） |
| **S6-02** | 跑 E1/E2/E3 + 记账入 `eval-report.md` §8.10 | S6-01 | S | PR 1 或 PR 2 |
| **S6-03** | **决策门**：产出「T7-09 投 / 不投」结论 + 依据（D-S6-01 的 30%/20% 判据） | S6-02 | S | PR 2（文档） |
| **S6-04** | `LocalEmbedder` 池化（若投）+ S6-T4/T5 | S6-03 | M | **PR 3**（独立，可延后） |
| **S6-05** | E3 依赖接入 + 许可复核 + S6-T6/T14（若投） | S6-03 | M | **PR 4**（独立） |
| **S6-06** | `build --index` 追加路径 + S6-T1/T2/T3/T8/T9/T13 | 无（可与 S6-01 并行） | S | **PR 5** |
| **S6-07** | 增量口径实测（NFR-03 ②）+ 脚本 + `eval-report` §8.10 | S6-06 | S | PR 5 |
| **S6-08** | `bench --threads` + S6-T11/T12 + NFR-10 实测数据 | S6-06（需快照 fixture） | S | **PR 6** |
| **S6-09** | D-S6-05 静默降级消除 + S6-T7 | 无 | S | 并入 PR 5 或 6 |
| **S6-10** | 四处定义面回写（NFR-03 ②/05/10/11 实测、架构 **R36~R43**、plan-v2 §7 进度、docs/README 索引）+ CHANGELOG + 补集成测试 **S6-T10** | 全部 | S | 收尾 PR |

> **PR 切分原则**（沿用 Step 4/5 的教训）：**spike 载体与数据先落地**（S6-01~03），因为"投/不投"决定后面两个 M 级任务是否开工。**不要让 spike 和落地绑在一个 PR 里**——那会让"不投"这个结论无法合并。

---

## 9. 风险与未决问题

### 9.1 新增风险（**已写入架构 §14.4**，编号 R36 ~ R43；R36 / R37 为**条件性**）

> ⚠️ **S6-10 补充（2026-09-13）**：本表登记的是**设计期**识别的 R36~R40；**实现期**又发现三条 ⇒
> **R41**（建库路径峰值 RSS 2.32~3.33 GB 与 NFR-05 的 372MB 差一个数量级）/ **R42**（依赖安全公告
> 漂移：`bincode` + `paste`，均无升级路径）/ **R43**（**查询侧编码把并发读吞吐封顶**，`embed/local.rs:35`），
> 均已登记在 **架构 §14.4**（权威来源），**不在本表重复** —— 本设计文档的风险表**保持设计期快照**、
> 不追写实现期发现（避免两处各说各话）。

| # | 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- | --- |
| **R36** | **多 session 抬高内存峰值**：N 份 ort arena / 工作区 | NFR-05（峰值 RSS 372MB）被顶穿；小内存机器 OOM | spike 必录峰值 RSS；决策门含「增量 ≤ 20%」；不达标即不投 | 未投则风险消失 |
| **R37** | **E3（CoreML）改变向量数值** ⇒ 效果指标与 P5 基线**不可横比**，且快照语义变 | 相关性结论失真；老快照错配 | D-S6-04 指纹编码 + §3.5 显式声明 + E3 独立决策 | 声明是**缓解不是消除**：若投，P5 的效果基线需重跑 |
| **R38** | **`default_embedder()` 静默降级**（`.ok()`）：要向量却拿到纯 BM25 | 静默的功能缺失（最坏的一类：不知道坏了） | D-S6-05（方案 A 硬失败）+ S6-T7 | ✅ 可关闭（本 Step 内修） |
| **R39** | **追加后图 sidecar 未重发 manifest** ⇒ 每次冷启动降级重建（≈10s） | NFR-04 静默失效（与 Step 4 同一坑） | S6-T13 显式断言 `Loaded` + `nb_point` 吻合 | ✅ 可关闭（测试锁住） |
| **R40** | **ID 复用导致幽灵向量**（旧向量挂到新 chunk） | 正确性（与 Q-C1 同族但成因相反） | §4.5.2 的证明 + S6-T8 不变式测试 | ⚠️ 今天的证明依赖三条隐含前提（尾部空洞保留等）⇒ 用测试锁，不靠文档 |

> R38/R39/R40 都属"**今天不破、但没人锁**"的类型——本项目的既有教训是这类问题最终都会有人撞上（R34 的潜在死锁即先例）。

### 9.2 未决问题（**Q4~Q7 已由评审拍板**；Q1~Q3 待实测回答）

| # | 问题 | 由谁回答 | 阻塞谁 |
| --- | --- | --- | --- |
| **Q1** | E2 的收益方向与量级？ | S6-02 实测 | S6-03 决策门 |
| **Q2** | E3 在 macOS aarch64 上能否初始化？能否加速？ | S6-02 实测（可能"秒失败"） | S6-03 决策门 |
| **Q3** | 不同 `sessions` / `intra_threads` 是否改变向量数值？ | ⏸ **挂起**（S6-T6 随 S6-05 取消）—— **但 S6-10 的「复审触发条件」依赖它**：**查询侧会话池落地时须先答**（若改变会破坏 NFR-10 ① 的逐位一致前提，见 §11.6） | D-S6-04 是否需编入线程配置 |
| **Q4** | **NFR-10 的数值目标**（此前完全缺失） | ✅ **评审已拍板：方案 A**（§4.6.3 / D-S6-08）⇒ 需求落「拟」口径；**✅ S6-10 已定稿**：S6-08 实测 bm25 **2.98×** ✅ / hybrid **1.63×** / vector **1.88×** ⇒ `×2.5` 保留、需求升 **v1.15**（② **收窄**为「不含查询侧编码的读路径」、**保持「打开」**，编码侧并发 → 架构 **R43**） | **已解**；NFR-10 由「无法判定」变为「可判定」 |
| **Q5** | "增量追加 10% 文档"的 **10% 以什么为单位**（文档数？chunk 数？字节？） | ✅ **评审已拍板：文档数**（`NFR-03 ②` 原文即"追加 10% 文档"）；**chunk 数只作参考指标、不进判据**（已写进 v1.14） | **已解** |
| **Q6** | `default_embedder` 硬失败（方案 A）会不会破坏既有用户脚本？ | ✅ **评审已拍板：方案 A**（0.x + CHANGELOG `Changed` + 迁移说明足够，不需额外兼容开关） | **已解**（D-S6-05 定案） |
| **Q7** | 追加路径是否需要 `--no-graph-persist` 之类的逃生舱？ | ✅ **评审已拍板：沿用既有 `--no-graph-persist`，不新增** | **已解**（S6-06） |
| **Q8** | **build 路径的峰值 RSS 是否纳入 NFR-05 口径 / 单独立风险项（R41）？** —— 实测 build 路径（batch 64 × 长文本）2.32~3.33GB，而 NFR-05 的 372MB 是 search 路径（batch 1）口径 | ✅ **已裁定（2026-09-12）：方案 C —— 拆两步**（详见 §11.4）；三点实验已钉死「RSS 与语料规模解耦、1 batch 后饱和」（`eval-report.md` §8.10.4） | ✅ **两项均已落地（S6-10，2026-09-13）**：① NFR-05 补限定词「（检索路径，batch 1）」（需求 **v1.15**）；② 立 **R41**（架构 **§14.4**：预算按 `batch × 序列长度` 给、**只给形态不给数**）。**数值仍待 T7-11 实测后定** |
| — | ⚠️ **Q1 / Q2 / Q3 的实际状态**（评审 F 补充）：Q1/Q2 已由 S6-02 实测**结案**（结论 = 不投，见 §8.10）；**Q3（不同 sessions / intra_threads 是否改变向量数值）随 S6-05 取消而挂起** —— 它是 D-S6-04 的前置，若将来重开池化必须先答 | — | — |

> ⚠️ **决策状态的文档内漂移（评审 F，2026-09-12）**：`eval-report.md` §8.10 已写「D-S6-01 结案 / R36 命中」，
> 而本文档 `D-S6-01` 行与 §9.2 仍按设计期语气写「⏳ 由 spike 数据决定 / 待实测回答」，R36 的残余也未标注。
> **本版保留设计期语气**（避免半途改写已评审内容），但在此显式声明：**以 §8.10 为准，定义面的状态统一回写留待 S6-10**。
> ✅ **S6-10 已回写（2026-09-13）——本注记作废，保留原文仅为留痕**：
> ① 本句所指的漂移**已消解**——架构 §14.4 的 **R36 残余**已改为「**已实测命中 ⇒ 条件性风险随「不投」消失**」、**R37 同**（E3 未投）；
> ② `D-S6-01` 行与本节 Q1/Q2 的**设计期语气未逐字改写**（保留已评审原文），但**行内已补实现期结案标记**（见下）⇒ 不会误读；
> ③ 定义面四处（`requirements-spec.md` / `architecture-design.md` / `docs/README.md` / `plan-v2.md`）的 Step 6 状态
> **已同批回写**，本步骤的实现任务 **S6-01~S6-09 全部开工完毕**（S6-04/05 判「不投」、S6-10 即本次收尾）。

> **评审同时背书（评审 §4）**：① **D-S6-01「只承诺 spike、不预先承诺落地」**（93% 分解的算术复核无误；
> "不把探路承诺成交付"正是 2026-09-07 复审 A2 刚纠正过的错误类型）；② **「先 T7-11（跳过 embed）后 T7-09（加速 embed）」的顺序**；
> ③ **R38 的定性**——`search/config.rs:312-318` 的 `.ok()` 属实，**今天既有缺陷、非本 PR 引入**，作为既有缺陷登记而非本 PR 范围；
> ④ **T13 flaky 不放宽阈值**（该 flaky 已由 PR #39 在 `main` 上按「同图可归因不变式」改造完毕，见 `CHANGELOG.md`）。

---

## 10. 评审收口记录（PR #38，2026-09-11）

**评审形态**：`pulls/38/reviews` = **1 条正式评审**（`COMMENTED`）／`pulls/38/comments` = 0 条行内／`issues/38/comments` = 0 条。
评审结论：**设计主体与证据链成立**（40+ 处 `file:line` 断言逐条独立复核全部命中，无一处行号/语义漂移），
但附录 B 有两处会让 S6-07 照抄执行时产出无效数据或直接报错 ⇒ **F1 / F2 为合并前必修**（合计 ~3 行），其余为 minor。

### 10.1 七条意见的处置（**全部采纳**）

| # | 意见 | 处置 | 落点 |
| --- | --- | --- | --- |
| **F1** | 附录 B 的 delta 与 base **完全重叠** ⇒ NFR-03 ② 的验收测量是空转（`data/t2-corpus.jsonl` **恰 12000 行**；base 吃全量、delta 取前 1200 ⇒ 1200 条全部命中 `add` 短路查重，不进 pending / 不 embed / 不建图，耗时 ≈ load+save，判据必然通过） | ✅ **采纳**（本次最重要的实质缺陷） | 附录 B 第 2 步：base/delta 改为**不相交**（`sed -n '1,10800p'` / `tail -n 1200`）+ 硬约束文字 + 预期耗时（≈23s vs 阈值 27.1s，**通过但不宽裕**）与"重叠版秒过=空转"的鉴别说明；S6-07 同步 |
| **F2** | `cargo run -p helix-cli` 的包不存在 | ✅ **采纳**（独立复算：`cargo pkgid -p helix-cli` 报 `did not match any packages`；`crates/cli/Cargo.toml:2` 是 `name = "helix"`） | 附录 B **3 处**改为 `-p helix` |
| **F3** | `Embedder::id()` 现签名 `-> &'static str` 覆盖不了任意 `sessions` / `intra_threads` 组合，而设计又说"观测到差异就必须编" ⇒ 别等探针出结果才临时找路 | ✅ **采纳** | §4.4.2 预写两条升级路径（**a** 有限档位枚举 = 零结构变更 / **b** `Cow<'static, str>` = 与 R35 同类的破坏性变更）及各自对指纹与老快照的影响 |
| **F4** | 设计文档"上游"行与附录 D 引 `requirements-spec.md` v1.12，而同批已升 v1.13 ⇒ 合并瞬间即成陈旧锚点（正是本 PR 在别处清理的"定义面漂移"） | ✅ **采纳**，锚点改为 **v1.14**——本 PR 因 D-S6-08 拍板而**再升一版**（见 §5） | 头表"上游"行 + 附录 D |
| **F5** | CHANGELOG `#### Fixed` 收录的是"发现"而非"修复"，会让读者误读为静默降级已修复 | ✅ **采纳** | `CHANGELOG.md`：该条目挪出 `Fixed`，首句改为「**发现并登记（未修复）**」 |
| **F6** | §4.3.1 示意代码 `texts[i*BATCH..(i+1)*BATCH]` 在 `texts.len()` 非 BATCH 整数倍时**末块越界 panic** | ✅ **采纳** | §4.3.1 改为夹紧右界 `(lo + BATCH).min(texts.len())`（等价写法：与 `texts.chunks(BATCH)` 配对） |
| **F7** | S6-T4 / T5 / T7 的**可测性前提**未写：`Dummy` / 必失败 embedder 无法注入 ⇒ 只能 `#[ignore]`，与 §7 的 CI 口径矛盾 | ✅ **采纳** | §7 新增「可测性前提」注记，把解耦要求写成 **S6-04 / S6-05 / S6-09** 的设计约束 |

### 10.2 评审建议的补充（已采纳）

- **§2.6 的 grep 词清单补 `rayon`**：`search/searcher.rs:207` 有 `rayon::join`（双 lane 并行、**确定性合并**），
  **不构成** S6-T11 逐位一致的反例，但写进清单可免后人以为"没查并行"。
- **工作区残留探针**：评审提醒 `crates/core/tests/zz_tmp_probe_dist.rs`——已核实**早已删除**（`git status --porcelain` 为空），无残留。

### 10.3 对 F1 / F2 的**独立复算**（不采信评审结论，自己跑一遍）

| 复核项 | 命令 | 结果 |
| --- | --- | --- |
| F1 的前提：`data/t2-corpus.jsonl` 到底几行 | `wc -l data/t2-corpus.jsonl` | **12000** ⇒ base 若吃全量、delta 取前 1200，确实 100% 重叠 ✅ 评审属实 |
| F2 的前提：包名是否存在 | `cargo pkgid -p helix-cli` / `-p helix`；`crates/cli/Cargo.toml` | `helix-cli` → `did not match any packages`；`helix` → `path+file:///…/crates/cli#helix@0.1.0`；`Cargo.toml:2` = `name = "helix"` ✅ 评审属实 |
| 残留探针 | `ls crates/core/tests/zz_tmp_*.rs` | `no matches found` ⇒ **早已删除**，工作区无残留 ✅ |

### 10.4 本文**未**按评审建议改的口径

（本版无。评审对本文档自述事实的复核**全部为属实**，未提出需撤回的断言；7 条意见 100% 采纳。）

---

## 11. 实现期更正与状态注记（2026-09-12，PR #40）

> 本节由 **S6-01 / S6-02 的实际实现与实测**回写，并吸收 PR #40 三轮评审的意见。
> **本节只更正「设计说错了什么」与「状态变成什么」，不重写决策与形态**（那是 S6-10 的事）。

### 11.1 实测定案：T7-09 判「不投」

- 判据（决策门，**合取**）：吞吐增益 ≥ +30% **且** 峰值 RSS 增量 ≤ +20%（§4.2.1 / D-S6-01）。
- 实测：**E2** +11.7%（2 session）/ +16.0%（4 session），峰值 RSS **+96.4% / +289.2%**；
  **E3** 调优后（`RequireStaticInputShapes` + `MLProgram`）仅 **+1.2%** ⇒ **两侧门槛都不过**。
- ⇒ **S6-04（池化）/ S6-05（EP 接入）不开工**；`coreml` feature 作为**默认关闭的探针基础设施**保留
  （评审裁定 2：它是「不投」这个结论的物证，也是换机重测的唯一入口）。
- 完整数据、局限与协议更正见 `eval-report.md` **§8.10**。

### 11.2 测量协议更正（§4.2.5 已就地标注）

1. **「一次进程内跑完所有档位」被实测推翻** ⇒ 改为**逐档位独立进程 + 按波交错**。
   理由：单 session 在 batch 64 × 长文本下的峰值 RSS 就 2~3.3 GB（**激活张量**，不是权重），
   7 个 session（`1+2+4`）并存会顶穿本机 32 GB ⇒ 换页会均匀拖慢所有档位。
2. **「档位内两轮、顺序交错（A/B/A/B）」被替换** ⇒ 每波一个独立进程（恒传 `--warmup 0`），
   **每档位每波只有一个计时样本**，交错只发生在「波」这一层；**每档位样本量 = 波数（实测 3）**。
3. **预热口径**：脚本级预热跑在**一次性进程**里，只暖 OS 文件缓存；**每个计时波是该进程的首次推理**。
   （`run_pass` 的秒表起在 `SessionPool::build` 之后，模型加载与 ONNX 图优化本就不进计时。）

### 11.3 本次一并更正的源码锚点

| 位置 | 原写 | 更正为 |
| --- | --- | --- |
| §2.3 表（两行） | `src/text_embedding/init.rs:33` / `:51/83/122` | `src/init.rs:20` / `:83`、`:122`（`InitOptionsWithLength` 才是 `TextEmbedding::try_new` 收的类型） |
| §2.4 表（两行） | 同上 | 同上 |
| 附录 C（三处） | `impl.rs:381`、`ep/coreml.rs:147-153`、缺 E3 旋钮 | `impl.rs:373`、`ep/coreml.rs:159`、补 `:102` / `:121` / `:49-51` |
| §4.2.1 | `intra_threads = max(1, 核数 / sessions)`（截断） | `ceil(核数 / sessions)`（实现用 `div_ceil`，允许轻微超额以免留核空转） |
| §4.2.4 | `ort = { …, features = ["coreml"] }` | `optional = true` + `coreml = ["dep:ort", "ort/coreml", "local-embed"]` |

✅ **S6-10 已处理（2026-09-13）** —— 本清单**清账**：① `plan-v2.md` §附-1 的锚点**已按本附录更正**
（`src/init.rs` 而非 `src/text_embedding/init.rs`；内部切分锚点回到 `impl.rs:373`）；② **定义面四处已同批回写**
（需求 **v1.15** / 架构 **v1.14** / `docs/README.md` / `plan-v2.md`）；③ §9.2 下方的「决策状态漂移」注记**已作废**
（保留原文留痕）；④ 另有 **评审 P3-4**（§4.6.1 的 `Arc<Searcher>` 措辞）与 **§7 测试表末列**的状态回写，
以及 **S6-T10 的补齐** ⇒ 详见 **§11.6**。

### 11.4 Q8 已裁定：方案 C（拆两步）

build 路径峰值 RSS 与 NFR-05 的 372MB（search 口径）差一个数量级。

**✅ 已裁定（2026-09-12，评审建议 + 项目负责人拍板）：方案 C —— 拆两步。**

1. 给 `NFR-05` 补口径**限定词**「**（检索路径，batch 1）**」，消除「按 372MB 规划内存会在建库时被 OOM kill」的误读；
2. 单独立风险项 **R41**，**预算形态按 `batch × 序列长度` 给**（而不是按语料规模给）。

⚠️ **两项都归 S6-10**（登记会牵动定义面四处 + 版本行），且 **暂不给数值** ——
三点实验（`--texts 64 / 512 / 4096`，固定文本长度）钉死了「RSS 与**语料规模解耦**、1 batch 后饱和」
（2.11 / 3.31 / 3.33 GB），但「典型的 `batch × 序列长度` 分布」要等 **T7-11** 的实测才有。
⇒ **不把 build 路径并进 NFR-05 的既有读数**（会让「372MB」这条历史基线的含义变混、反而降低可判定性）。

### 11.5 挂起项：Q3 随 S6-05 取消

**Q3**（不同 `sessions` / `intra_threads` 是否改变向量数值）原本挂在 S6-05 的 **S6-T6** 上；
S6-05 不开工 ⇒ **Q3 挂起**。它不影响 D-S6-01，但它是 **D-S6-04（`embedder_id` 编码）的前置**
⇒ 若将来重开池化，**必须先答 Q3**。

### 11.6 S6-10 收尾结账单（2026-09-13）

S6-10 是 Step 6 的**收尾 PR**，做四件事；⚠️ **零生产代码改动**（纯文档 + 集成测试），
**不动任何 `src/` 逻辑**（NFR-10 的裁定选了「收窄 + 保持打开」，不是「修并发」）。

**(1) 定义面四处 + 版本行**（改结论必须同步四处，见 `MEMORY.md` 流程铁律）：

| 面 | 文件 | 版本 | 本次改了什么 |
| --- | --- | --- | --- |
| 需求 | `requirements-spec.md` | v1.13 → **v1.15** | NFR-10 **定稿 + ② 收窄 + ③ 新增（保持打开）**；NFR-05 补口径限定词「（检索路径，batch 1）」；NFR-03 ② 增量实测注记；NFR-11 写延迟旁证（**不标已达标**）；**P3-8 措辞更正**（「读路径零共享可变状态」只对**索引读路径**成立） |
| 架构 | `architecture-design.md` | v1.13 → **v1.14** | §14.4 由 **R36~R40** 扩为 **R36~R43**：R36/R37 **消解**、R38/R39 **关闭**、R40 **上锁（残余仍在）**、新增 **R41/R42/R43**；§14 导读与 §15.2 变更记录同步 |
| 进度 | `plan-v2.md` | v0.14 → **v0.15** | §6 验收门槛勾选、§7 进度表（T7-09 ⛔ / T7-11 ✅ / T7-17 ✅）、§8 回写计划、状态块与风险登记行；**§附-1 ③ 的源码锚点更正** |
| 索引 | `docs/README.md` | — | 风险范围 **R1~R35 → R1~R43**；`v2-step6-design.md` 索引行整行重写（含测试计划状态、Q1~Q8 状态） |

**(2) 集成测试补齐**：**S6-T10**（追加后 `save → load` 往返 —— 检索结果与追加后一致、同快照两次加载逐位一致，
NFR-06 不破）落在 `crates/core/tests/step6_incremental_build.rs`。
⚠️ **本行原设计就承诺 ✅，但实现期漏做**，两轮评审也没抓到 —— 由本次收尾的**自查**发现（§7 表已标注）。

**(3) NFR-10 裁定（三条并记）**：见 §4.6.3 / D-S6-08 ——
㈠ ② 的适用范围**收窄**为「不含查询侧编码的读路径」（bm25 2.98× 即该路径证据）；
㈡ **NFR-10 整体保持「打开」、不关闭**（编码侧并发是**如实记录的未达标项**，不是 D-S6-08 已否决的「只记录不判定」）；
㈢ **复审触发条件 = 查询侧会话池（每 worker 独立 session）落地时** —— 届时须**先答挂起的 Q3**（§11.5）再重测。

**(4) 新增实验 + 风险转正**：
- **hybrid 无锁对照实验已补跑**（补 PR #44 评审 **P2-2** 的遗留）⇒ R43 的 **hybrid 归因由「推断」升为「实测」**；
  协议与数据见 `eval-report.md` **§8.13**。
- **R42 / R43 由「拟登记」转正**：正式条目在架构 §14.4；`deny.toml` 与 `Makefile` 里「拟登记 R42」的措辞同步改为 **R42**。

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
#    逐档位独立进程 + 按波交错 + 控制组漂移打印；漂移 >10% 时按控制组归一后再比较

# 2) T7-11 增量：先建基线，再追加 10%（⚠️ delta 必须与 base **不相交**）
sed -n '1,10800p' data/t2-corpus.jsonl > /tmp/base-corpus.jsonl   # base = 前 10800 篇（12K 的 90%）
tail  -n 1200     data/t2-corpus.jsonl > /tmp/delta.jsonl         # delta = 后 1200 篇（全新，与 base 不相交）
cargo run -p helix --release -- build --input /tmp/base-corpus.jsonl --vectors --single-chunk --output /tmp/base.idx
cargo run -p helix --release -- build --index /tmp/base.idx --input /tmp/delta.jsonl --vectors --output /tmp/inc.idx
#    ⚠️ **硬约束（评审 F1）**：delta 必须与 base 不相交。`data/t2-corpus.jsonl` **恰好 12000 行**，
#       若 base 吃全量而 delta 只是它的前 1200 行，则 1200 条**全部**命中 `add` 的短路查重
#       （`search/index.rs:356-365`）⇒ 不进 pending、不 embed、不建图，"本次耗时" ≈ load + save（~1-2s），
#       判据**必然通过却什么都没测到**；对照项「全量重建 12K + 1200」也退化成 12K。
#       测幂等是 **S6-T3** 的职责，不能充当 NFR-03 ② 的测量。
#    判据：本次耗时 < 首次全量 × 10% × 1.2（NFR-03 ②；首次全量 = 12K 的 225.5s ⇒ 阈值 27.1s）
#         预期 ≈ 1200/12000 × 209.9s + ~2s ≈ 23s ⇒ **通过但不宽裕**（这正是"重叠版秒过=空转"的证据）
#    对照：全量重建全 12K 与 /tmp/inc.idx 的 (source, score) 序列一致（S6-T2；**不比 id**）

# 3) T7-17 并发检索（结果正确性 + QPS）
for t in 1 2 4 8; do
  cargo run -p helix --release -- bench --index /tmp/base.idx --threads $t \
    --modes bm25,vector,hybrid --reps 20 --json /tmp/s6-threads-$t.json
done
#    判据（NFR-10 方案 A，已拍板）：--threads 4 的 QPS ≥ --threads 1 × 2.5
#          **前置条件**：$t 线程的 hits（chunk_id + score）序列**逐位等于** --threads 1
#    口径：预热丢弃首轮 + 顺序交错（1/2/4/8/1/2/4/8）+ 报告运行范围（核数 / 是否插电 / 是否在 CI）

# 4) 数据落表
#    把 1) 与 3) 的结果写入 docs/devel/eval-report.md §8.10（新增小节），
#    与 §8.2 / §8.6 的既有数字**同口径对照**（同为 12K / 4000 段 / release）。
```

---

## 附录 C：依赖源码核实记录（本文技术前提）

**`fastembed-6.0.2`**（`~/.cargo/registry/src/*/fastembed-6.0.2`）

| 位置 | 事实 |
| --- | --- |
| `src/init.rs:20` | `InitOptionsWithLength.intra_threads: Option<usize>`（默认 `None` = 用满所有核）。**这才是 `TextEmbedding::try_new` 收的类型**：`fastembed::InitOptions`（`src/lib.rs:105`）= `TextInitOptions`（`src/text_embedding/init.rs:20`）= `InitOptionsWithLength<EmbeddingModel>`（`src/init.rs:11`） |
| `src/init.rs:83` / `:122` | `with_execution_providers(Vec<ExecutionProviderDispatch>)`——`:83` 属 `InitOptionsWithLength`、`:122` 属 `InitOptions<M>` |
| `src/init.rs:94` / `:133` | `with_intra_threads`——`:94` 属 `InitOptionsWithLength`、`:133` 属 `InitOptions<M>` |
| ⚠️ **更正（2026-09-11，实现期复核）** | 本节原先把上述锚点写成 `src/text_embedding/init.rs:33/:51/:83/:122/:133`。实际 **`:33`/`:51`/`:67` 是 `InitOptionsUserDefined`**（`:27`，用于**用户自带模型**，不是我们走的路径），而 `:83`/`:122`/`:133` 的行号对、**文件错**（实为 `src/init.rs`）。已按本表更正；`embed/local.rs:42-46` 的推断（我们没调 `with_intra_threads`）**不受影响**，仍然成立 |
| `src/lib.rs:89` | `pub use ort::execution_providers::ExecutionProviderDispatch;`（**只重导出类型，不透传 feature**） |
| `Cargo.toml` `[features]` | **无 `coreml`**；EP 透传只有 `directml = ["ort/directml"]`（及 `cuda`/`mkl`/`metal`/`accelerate` 等 candle 侧） |
| `src/text_embedding/mod.rs:5` | `const DEFAULT_BATCH_SIZE: usize = 256;` |
| `src/text_embedding/impl.rs:364` | `batch_size.unwrap_or(DEFAULT_BATCH_SIZE)` |
| `src/text_embedding/impl.rs:71` | `init_session_builder(execution_providers, intra_threads)` —— E1/E2/E3 共用同一构造路径 |
| `src/text_embedding/impl.rs:373` | `texts.chunks(batch_size)` —— 内部切块（我们传 `None` ⇒ 256，但调用方已按 64 切，故不触发）。⚠️ 行号已更正（原写 `:381`） |

**`ort-2.0.0-rc.13`**（`~/.cargo/registry/src/*/ort-2.0.0-rc.13`）

| 位置 | 事实 |
| --- | --- |
| `Cargo.toml` `[features]` | `coreml = ["ort-sys/coreml"]` 存在 |
| `src/ep/mod.rs`（gate 段） | `#[cfg(feature = "coreml")] pub mod coreml; pub use self::coreml::CoreML;` |
| `src/ep/coreml.rs:68` | `pub struct CoreML`（**不是** `CoreMLExecutionProvider`——计划文档的写法已过时） |
| `src/ep/coreml.rs:159` | `with_compute_units(ComputeUnits::CPUAndNeuralEngine)`。⚠️ 行号已更正（原写 `:147-153`，那是 rustdoc 示例） |
| `src/ep/coreml.rs:102` | `with_static_input_shapes(bool)` —— **E3 结论所依赖的旋钮**：不开时 CoreML 会因每个 batch 形状不同反复重编译 |
| `src/ep/coreml.rs:121` / `:49-51` | `with_model_format(ModelFormat)` / `enum ModelFormat { MLProgram, NeuralNetwork }` —— 另一个被 E3 用到的旋钮 |
| `src/lib.rs:39` | `pub mod ep;` |

⚠️ **反面记录**：`plan-v2.md` §4 Step 6 与 §附-1 写「`fastembed` 暴露 `with_execution_providers`」（✅ 对）与「`RerankerModel::BGERerankerV2M3` 含 `model.onnx.data`」（属 Step 7，未在本步核实）——本文**只更正 EP 类型名**，其余不改。
⚠️ **更正（2026-09-12）**：本行原写「✅ 已同步：`plan-v2.md` §附-1 已补三条更正……与本附录一致」——**这句是假的**。
`plan-v2.md` §附-1 补的三条里，第 ③ 条把锚点写成了 `src/text_embedding/init.rs:33`（非 `src/init.rs`）与
`impl.rs:364`（非 `:373`），**恰好与本附录相反**。⇒ 现更正为：**`plan-v2.md` §附-1 的锚点仍待更正，随 S6-10 一并做**。

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
| NFR-03 双口径 / NFR-05 / NFR-10 / NFR-11 | `docs/devel/requirements-spec.md` §6.1（**v1.14**） |
| 225.5s（embed 209.9s）/ 批次表 / 建图 4% | `docs/devel/eval-report.md` §8.2（`:137-156`）、§8.6（`:232-233`）、§8.4（`:199`） |
| Step 4 的 manifest 重发铁律 | `docs/devel/plan-v2.md` §4 Step 4（`:288-290`） |
| R35（破坏性变更的判定先例） | `docs/devel/architecture-design.md` §14.3 |
| 标定方法学（交错 / 控制组归一 / 运行范围） | 技能 `perf-ab-calibration` v1.1.0 |
