# HelixIndex V2 · Step 5 详细设计（查询性能与可观测：低选择度精确兜底 + `Metrics` 可观测化）

> 面向 Agent 场景的通用检索引擎内核——V2 Step 5 的详细设计（**v0.1：首版，待评审**）。
> 本文件回答：低选择度过滤为什么必然整图遍历、绕开 ANN 直接精确扫描在**本项目的向量存储上是否可行**、
> 阈值该怎么标定且怎么证明"没有把热路径搞坏"、以及那个"外部拿不到实例"的 `Metrics`
> 到底卡在哪一条链上。

| 项 | 内容 |
| --- | --- |
| 版本 | **v0.1（首版，待评审）** |
| 日期 | 2026-09-10 |
| 状态 | **待评审**。决策 D-S5-01 / 03 / 05 阻塞实现开工；D-S5-02 / 04 的**数值**由标定实验（S5-04）回填 |
| 修订记录 | v0.1 首版：现状源码级定位（§2，含 `Metrics` 四处断链与新发现的早退路径 `took` 恒 0）、**`hnsw_rs` 存储访问能力核实（§3.1 / 附录 C，本文关键技术前提）**、三路径设计（§4）、9 个决策 D-S5-01~09、S5-T1~T13 测试计划、S5-01~S5-08 任务拆分、风险 R31~R35 与未决 Q1~Q5 |
| 上游 | `plan-v2.md` §4 Step 5 / issue **#22**（V2-Step5）/ `requirements-spec.md` v1.9（**NFR-13** Should）/ `architecture-design.md` §8.3（可观测性）、§14.1（R11 / R13 / R17 / **R18**）/ `v2-step1-design.md` §5.7（A/B 双路径）/ `eval-report.md` §8.5~§8.6（S1-10 实测） |
| 范围 | **T7-22** 低选择度精确兜底（R18）+ **T7-23** `query::Metrics` 可观测化 + 阈值标定实验 + CLI/bench 观测入口 |
| 非范围 | **prefilter 结构**（索引侧预过滤位图，V2.1 议题，判据正是本 Step 暴露的 `vector_shortfall`）；**降级字段的 `doc_bits_scan` O(N) 全扫**（Step 1 已接受的残余，Q-I1 的边界，与向量路正交）；并发检索压测（**Step 6** / T7-17，本 Step 是它的前置）；读写并发 / 在线 compaction（**Step 8**）；横切任务 T7-21（`parallel_build` 默认翻转）、T7-24（工程卫生） |

---

## 决策速览：D-S5-01 ~ D-S5-09

> 评审者时间有限时先看这张表。每条在 §5 有完整取舍与证据。

| # | 决策 | 建议 | 状态 | 阻塞谁 |
| --- | --- | --- | --- | --- |
| **D-S5-01** | **精确兜底的落点**：放 `VectorIndex` 张（后端自己拥有策略）还是在编排层（把 `raw_vectors` 视图接进 `SearchParts`） | **放 `VectorIndex` 层**：新增必需方法 `search_exact_filtered` + 默认钩子 `prefers_exact(&dyn CandidateFilter) -> bool`。理由：**零 plumbing**——门面 `Searcher`、逃生舱 `QueryExecutor`、bench 三条入口全都自动受益；`BruteForceIndex` 的精确性由**类型**表达（它天然满足）。编排层方案被否决：要新增一条"精确向量源"通道，且 bench/逃生舱都得跟着接线，同一策略出现两个装配点 | ⏳ 待拍板 | 阻塞 S5-01~03 |
| **D-S5-02** | 兜底的**适用面与默认阈值** | **只作用 `FilterKind::Filtered`**，热路径（`None` / `Alive`）**一行不改**（保 R15 的 fast-return 不回归）；阈值以 `Option<usize>` 表达，`None` = 关闭（供 A/B 回归对照），默认值**待 §4.9 标定**，初值取 **1024**（理论分界 ≈ `ef`，见 §4.3） | ⏳ 数值待标定 | 阻塞 S5-03 |
| **D-S5-03** | 精确扫描的**候选来源** | **方案 A：遍历向量存储本身**（`HnswRsIndex` 迭代 layer 0；谓词逐点判定、只对通过的点算距离）。代价 `O(N)` 次谓词判定 + `O(allowed)` 次距离；**零新增状态**。方案 B（枚举 allowed + `origin→PointId` 映射，做到纯 `O(allowed)`）列为"标定不达标再上"的备选（§4.2.3） | ⏳ 待拍板 | 阻塞 S5-01 |
| **D-S5-04** | **NFR-13 的预算口径与数值** | 口径 = **端到端 P99（含过滤求值）**，10 万级、选择度 ≤1% 档位，**拟 ≤ 20ms**（与 NFR-02 同量级）。理由：只报"向量路"会掩盖降级字段档位上 `doc_bits_scan` 那 ~8ms；bench 必须**同时**打印 `filter_eval` / `vector_route` / `vector_shortfall`，才能区分"兜底没生效"与"兜底生效但过滤求值本身贵"。数值由 §4.9 标定后回填需求文档 | ⏳ 数值待标定 | 阻塞 S5-04 |
| **D-S5-05** | `Metrics` 的**承载方式** | **进 `SearchResponse` 新字段 `metrics`** + 在 `query/mod.rs` 再导出 `Metrics`；`tracing::info!` 通道**保留**（二者一个给宿主、一个给调用方）。理由：只有进响应，外部才能**单测**、bench 才能**采集**——这正是 issue #7 / #22 的原始诉求。代价是 `SearchResponse` 是公开结构体、字段全 `pub`，加字段会破坏外部**字面量构造**；库内只有 `searcher.rs:268` / `:413` 两处，记入 CHANGELOG `⚠️ 破坏性` | ⏳ 待拍板 | 阻塞 S5-05/06 |
| **D-S5-06** | 是否补 **per-lane 耗时** | **补**（`bm25_elapsed` / `vector_elapsed`）。Hybrid 走 `rayon::join`（`searcher.rs:164-167`），单一 `took` 无法归因"是哪一路慢"；架构 §8.3 的示例日志本就预期 `bm25=1.4ms(vector=6.1ms parallel)`。实现形状：lane 闭包返回 `(Result<Vec<Scored>>, Duration)` | ⏳ 评审无异议即可执行 | 阻塞 S5-05 |
| **D-S5-07** | **路径可见性**（T7-22 的 A/B 判据） | **`Metrics.vector_route: VectorRoute { None, Ann, Exact }`**。`None` = 未走向量路（Bm25 模式）；`Ann` = 走了 ANN（可能近似）；`Exact` = 走了精确扫描（保证 `min(k, allowed)` 条且无召回缺口）。**没有它，"兜底是否真的生效"只能靠延迟反推**——正是 Step 1 吃过的亏（重合率当召回读） | ⏳ 评审无异议即可执行 | 阻塞 S5-05 |
| **D-S5-08** | 早退路径 `metrics.took` 恒为 0 的修法 | **修**。`searcher.rs:125-127`（过滤排空的那条早退）设了 `filter_eval` 就直接 `log`，`took` 仍是 `Duration::ZERO` ⇒ 日志里 `took_ms=0` 是**假数据**。属观测正确性缺陷，与 T7-23 同主题（§2.5） | ⏳ 评审无异议即可执行 | 阻塞 S5-05 |
| **D-S5-09** | 是否顺带治理**降级字段** `doc_bits_scan` 的 `O(N)` 全扫 | **不做**。它与向量路正交（是**过滤求值**贵，不是召回贵），Step 1 已按"接受并测量"结案（S1-10 结论②）；本 Step 只把它**计入 NFR-13 预算**并在 bench 单列，避免"兜底做完却仍在预算外"被误判为兜底失败 | ⏳ 评审无异议即可执行 | 无 |

---

## 1. 目标与验收

### 1.1 要解决的问题

#### 问题一：R18 —— 低选择度过滤查询的延迟爆炸（T7-22）

S1-10 在 **10 万级**合成语料上测出的三元数据（`plan-v2.md` §4 Step 1 B 表，release）：

| 档位 | 选择度 | bm25 P99 | **vector P99** | hybrid P99 | vector ÷ 无过滤(5.81ms) |
| --- | --- | --- | --- | --- | --- |
| none | 1.0 | 2.43 | 5.81 | 3.46 | 1.0× |
| sel-10% | 0.1 | 0.50 | 7.91 | 7.75 | 1.4× |
| **sel-1%** | 0.01 | 0.32 | **41.33** | 41.30 | **7.1×** |
| **sel-0.1%** | 0.001 | 0.25 | **158.45** | 144.87 | **27.3×** |
| ts-range-degraded | 0.001 | 8.26 | 124.26 | 121.87 | 21.4× |

结论：选择度 ≤1% 时**直接违反 NFR-02**（1 万级标定限额 vector 10ms / hybrid 20ms，超 4~16×）。
该场景此前**不在任何 NFR 口径内** ⇒ 同步新增 **NFR-13**（D-J9）。10 万级是该场景的扩展验证档，
`NFR-13` 的口径就定在这里。

**注**：`ts-range-degraded` 档位的 bm25 P99 = 8.26ms 比 bm25 无过滤检索本身（2.43ms）还贵 3.4×
—— 那 8ms 是**过滤求值**（字段索引降级 → `doc_bits_scan` 逐文档 `matches()` 全扫），
不是向量路，本 Step 不治（D-S5-09），但必须计入预算。

#### 问题二：`Metrics` 外部拿不到（T7-23）

`query::Metrics`（`crates/core/src/query/metrics.rs`，8 个字段）目前**只在 `search_parts` 内部聚合、
经 `Metrics::log` 以 `tracing::info!` 输出**。它的模块文档自己就写了三条后果（`metrics.rs:7-15`）：
宿主不挂 subscriber 就静默丢弃、**无法单测**、**无法被 bench 聚合**——而 `vector_shortfall`
正是"V2.1 是否引入 prefilter"的判据，Step 6（NFR-10/11）的实测也要靠它。缺口已挂 issue #7（**已关闭**）
与 #22，代码里的引用还是失效的（`metrics.rs:14`「见 issue #7」）。

### 1.2 对应需求

| 需求 | 内容 | 本文动作 |
| --- | --- | --- |
| **NFR-13（Should，新增）** | 10 万级、选择度 ≤1% 档位的过滤检索 P99 有界（预算由 Step 5 标定后回填） | 全文；预算数值见 D-S5-04 / §4.9 |
| NFR-02（Must） | 1 万级 hybrid P99 < 20ms；**不得退化** | 热路径一行不改（D-S5-02）；S5-T7 钉住 R15 不回归 |
| NFR-07（Must） | 可观测：不得静默降级 | `Metrics` 进响应 + `vector_route` + 修早退 `took`（D-S5-05/07/08） |
| **NFR-10 / NFR-11（Should，Step 6）** | 并发读吞吐 / 增量可见性与写延迟 | 本 Step 是**前置**（D-S5-05 的可观测化） |
| R18（架构 §14.1） | 低选择度延迟爆炸 | T7-22 结案（含 recall 侧的意外收益，§4.4） |
| R11 / R13 / R17（架构 §14.1） | filtered-ANN 召回不足 / `search_filter` 结构性整图遍历 / hnsw 固有近似误差 | 精确路径在低选择度档位**同时消掉**这三条的可见影响（§4.4） |
| FR-27（Must） | 过滤下推 + 字段索引 | 不动；精确路径复用同一 `CandidateFilter` 契约 |

### 1.3 验收标准（Step 5 完成的定义，可证伪）

1. **R18 结案（issue #22 主验收）**：10 万级合成语料、`sel-1%` 与 `sel-0.1%` 两档，
   **端到端 P99 进入 NFR-13 预算**（拟 ≤ 20ms），且 `Metrics.vector_route == Exact`（§4.9）。
2. **热路径零回归**：`--brute-fallback off` 与默认档的对照下，**无过滤**档位
   vector / hybrid P99 与 Top-K 序列**逐位一致**（消 R15 复发；不是"看起来差不多"）。
3. **精确性可证**：同一 `(query, 谓词, k)` 下，`HnswRsIndex::search_exact_filtered` 与
   `BruteForceIndex` 的 Top-K **逐位一致**（`(chunk_id, distance)` 序列），
   且返回条数 `== min(k, allowed)`。
4. **`Metrics` 可测可采**（issue #22 第二条验收）：`Metrics` 至少 **1 条单测** +
   **1 处 bench 采集点**（`--json` 输出同时含 `vector_route` 与 `vector_shortfall`）；
   `query::Metrics` 从 `query` 模块可 `use`。
5. **口径自洽**：`SearchResponse.metrics.took == SearchResponse.took`、
   `metrics.candidates == total_candidates`；**空结果路径的 `took` 非 0**（D-S5-08 回归）。
6. **阈值可标定**：`--brute-fallback <N|off>` 可外部切换；标定脚本产出
   「allowed × 路径 × P99 × 召回」四元表入 `eval-report.md`（新增 §8.9）。
7. **守门全绿**：fmt / clippy `--workspace --all-targets -D warnings` / `cargo test --workspace` /
   MSRV 1.90 / rustdoc `-D warnings` / `--no-default-features` / `--features charabia` / cargo-deny。

---

## 2. 现状与问题定位（源码级）

### 2.1 R18 的机理：两条路径的代价结构

`HnswRsIndex::search_filtered`（`crates/core/src/vector/hnsw_rs_index.rs:200-221`）按谓词种类分派：

| 路径 | 触发 | 实现 | `hnsw_rs` 内部行为 | 代价 |
| --- | --- | --- | --- | --- |
| **A** | `filter` 为 `None` 或 `FilterKind::Alive` | 普通 `search()` + 按存活比例过采样 `knbn`，返回后 `retain`（`:238-265`） | **保 fast-return**（`hnsw.rs:983-984`：`filter.is_none()` 时直接 `return`） | 无过滤热路径的成本结构不变（R15 的设计目标） |
| **B** | `FilterKind::Filtered` | `search_filter(query, k, ef, Some(&adapt))`（`:271-286`） | ⚠️ **fast-return 被禁用**：带 filter 时 `983-992` 只 `retain` 不 `return`；且 **`return_points.len() < ef` 时距离剪枝全程关闭**（`hnsw.rs:1019`：`if e_dist_to_p < f_dist_to_p \|\| return_points.len() < ef`） | `allowed < ef` 时**堆填不满 ⇒ 剪枝不生效 ⇒ 整图遍历** |

`ef` 的取值：`ef = min(k × EF_FILTER_FACTOR(4), EF_FILTER_MAX(256))`（`:278`）。
编排层传进来的 `k` 是 `candidate_k = max(k × 3, 10)`（`searcher.rs:107`），
即默认 `k=10 → candidate_k=30 → ef=120`。

⇒ **`ef=120` 就是分水岭**：`allowed` 远小于它（sel ≤1% 时 allowed ≤1000，且每命中一个 allowed 才涨一格堆）
时，`return_points` 长期填不满，**整层图被走遍**。这是 `hnsw_rs` 的结构性行为（R13），
过采样与 `ef` 调参都治不了——把 `ef` 调小会让召回更差，调大让遍历更久。

### 2.2 为什么方案是"绕开 ANN"而不是"换个 ANN 参数"

R18 的对策在 D-J9 时已经定死：**`allowed` 小于阈值时绕开 ANN、直接精确扫描**。
本设计要做的是把它落到**本项目真实的向量存储上**——关键问题是
"精确扫描要访问哪些向量"，而这决定了方案的可行性与成本（§3.1、§4.2）。

`eval-report`/D-J9 给出的量级直觉是「10 万级 0.1% 档 `allowed≈100`，代价约 0.05ms vs 158ms」。
本文对该数字给出**更诚实的分解**（§4.2.4）：0.05ms 只覆盖了"对 100 个候选算距离"这一段，
**不含定位这 100 个候选的成本**；而定位方式正是 D-S5-03 的决策点。

### 2.3 `Metrics` 的四处断链

`Metrics` 拿不到不是"忘了导出"，而是**四条链各断一处**：

| # | 链 | 断点 | 位置 |
| --- | --- | --- | --- |
| 1 | **输出** | 只在 `search_parts` 内构造，生命周期结束即丢弃；唯一出口是 `tracing::info!` | `searcher.rs:93`、`metrics.rs:49-62` |
| 2 | **承载** | `SearchResponse` 只有 `hits` / `total_candidates` / `empty_reason` / `took` 四个字段，无 `metrics` | `response.rs:62-72` |
| 3 | **导出** | `query/mod.rs:19-20` 只再导出 `EmptyReason/Explain/Hit/SearchResponse` 与 `QueryExecutor/SearchMode/SearchParts`，**无 `Metrics`** | `query/mod.rs:19-20` |
| 4 | **采集** | bench 的 `LatencyResult`（`crates/cli/src/bench.rs:1003-1050`）自算 `mean_shortfall`，并在注释里明确写「**与 `Metrics::vector_shortfall` 口径不同**，后者按融合前的候选池 `min(candidate_k, allowed)` 算，bench 侧**拿不到内部 `candidate_k`**」（`bench.rs:186-193`） | `bench.rs:190-193` |

⇒ 断链 4 的根因就是断链 1~3：**因为拿不到实例，只能在 bench 侧另算一个"近似但口径不同"的替代指标**。
这正是 D-S5-05 要一次修掉的。

### 2.4 先修口径、再修可观测——已经做了一半

Step 1 已经把 `vector_shortfall` 的**口径**修对了（归一到 `min(candidate_k, allowed)` 而非裸 `candidate_k`，
`searcher.rs:174-179`，理由见 `metrics.rs:36-43`）。本 Step 只需补"能看见"这一半。

### 2.5 设计期新发现：早退路径的 `took` 恒为 0

`searcher.rs:110-129` 是"过滤把文档排空"的早退分支。它的尾部是：

```rust
metrics.filter_eval = t0.elapsed();
metrics.log(query);                       // ← 此时 metrics.took 仍是 Duration::ZERO
return Ok(empty_response(reason, started));  // ← 而响应里的 took 是真实耗时
```

于是**日志与响应各说一套**：`tracing` 输出 `took_ms=0`，响应里 `took` 是真实值。
同类早退的第二处（`searcher.rs:199-215` 融合后为空）反而**设了** `metrics.took = started.elapsed()`（`:212`）。
⇒ 两处不一致，属观测正确性缺陷，与 T7-23 同主题，**并入 S5-05 一起修**（D-S5-08）。

> 复核方式：`grep -n "metrics.log" crates/core/src/query/searcher.rs` 共 3 处（`:126` / `:213` / `:266`），
> 只有第 1 处漏设 `took`。

---

## 3. 设计约束（既有事实，本文不重新论证）

### 3.1 【关键技术前提】`hnsw_rs` 0.3.4 的存储访问能力（源码核实）

Step 1/2 已核实它的三条硬事实（无 remove API、图双文件、`'a: 'b` 只能 `Box::leak`）。
Step 5 需要第四条：**能不能拿到已入库的向量**。核实结论：**能，且零拷贝**（完整记录见附录 C）：

| API | 位置 | 语义 | 对本文的意义 |
| --- | --- | --- | --- |
| `Hnsw::get_point_indexation() -> &PointIndexation` | `hnsw.rs:1279` | 暴露点索引结构 | 取得遍历入口 |
| `Hnsw::get_layer_iterator(layer) -> IterPointLayer` | `hnsw.rs:614-616` | **只迭代指定层** | `layer=0` 即"全部点各一次" |
| `type Layer<'b,T> = Vec<Arc<Point<'b,T>>>` | `hnsw.rs:386` | 层 = 连续 `Arc` 数组 | 迭代是顺序读指针数组 |
| `Point::get_v(&self) -> &[T]` | `hnsw.rs:204-206` | **零拷贝切片**（mmap 时为 mmap 视图） | 距离计算无需 copy |
| `Point::get_origin_id(&self) -> usize` | `hnsw.rs:214-216` | **就是我们的 `ChunkId`** | 谓词与结果都只需它 |
| `Hnsw::get_point_data(&PointId) -> Option<Vec<T>>` | `hnsw.rs:582-593` | 按 `PointId` **克隆**返回 | 只在方案 B 里用到（需 `origin→PointId` 映射） |
| `IterPointLayer::next` | `hnsw.rs:715-723` | 只走 `pi_guard[layer]`，**不跨层** | ⚠️ 必须用 `get_layer_iterator(0)`，**不能用 `PointIndexation::into_iter()`**（后者跨层遍历，高层点会被**重复** yield） |

两条结构性推论（写实现时不能忘）：

1. **每个插入点必然在 layer 0 出现且仅出现一次**（`hnsw.rs:504-511`，`p_id.0 = 0` 起步、
   `p_id.1 = layer0.len()` 递增）⇒ `get_layer_iterator(0)` 的基数 == `get_nb_point()`。
   这条可以写成断言（S5-T2）。
2. **`PointId ≠ ChunkId`**：`PointId = (layer, slot)`，`ChunkId` 是 `origin_id`。
   **库没有公开 `origin_id → PointId` 的映射** ⇒ 方案 B 需要自建（§4.2.3）。

### 3.2 现有抽象边界（不得为了本 Step 破坏）

| 边界 | 内容 | 本文是否触碰 |
| --- | --- | --- |
| **模块依赖** | `retriever::bm25` 与 `retriever::vector` **禁止互相 import**（`retriever/mod.rs:5-6`）；`vector` 不认识文本 | 不触碰 |
| **`CandidateFilter` 的对象安全** | 以 `&dyn CandidateFilter` 传递（Hybrid 下跨 `rayon` 线程共享）。**不能用泛型方法**（`impl FnMut` 参数会让 trait 不再 object-safe） | 若要"枚举 allowed"必须是具体返回类型（方案 B 的约束） |
| **`VectorIndex` 是 `&dyn`** | 编排层只持有 `Option<&dyn VectorIndex>`（`searcher.rs:72`、`search/searcher.rs:62`），**拿不到 `raw_vectors`** | 这是 D-S5-01 选择"策略放后端"的直接原因 |
| **`None` = 不过滤**（含软删除） | `predicate.rs:13-21` 写死的契约 | 精确路径必须**同等对待**：`None` 时不过滤、**含幽灵**（S5-T5 钉住） |
| **确定性 NFR-06** | 排序 `(分数降序, chunk_id 升序)` 全序；距离升序 + `chunk_id` 升序 tie-break（`hnsw_rs_index.rs:218-220`、`brute.rs:52-57`） | 精确路径必须**沿用同一 tie-break**（S5-T6） |
| **热路径不得退化**（R15） | 路径 A 为 `None`/`Alive` 保住 fast-return 与 `EF_SEARCH=200` | D-S5-02：**只对 `Filtered` 生效** |

### 3.3 兼容性约束：`SearchResponse` 是公开结构体

`SearchResponse`（`query/response.rs:62-72`）字段全 `pub`，且经 `lib.rs:68` 再导出到 crate 根
（`helix_core::SearchResponse`）。**加字段会破坏一切以字面量构造它的下游代码**。
库内构造点只有两处（`searcher.rs:268` / `:413`），全部由内核自己产出，**库内零影响**；
对外属破坏性变更 ⇒ 记入 CHANGELOG 并在 rustdoc 注明（D-S5-05）。

> 替代方案（已否决）：`SearchResponse` 加 `#[non_exhaustive]`——那会**同时**禁掉下游的字面量构造
> 与穷尽匹配，破坏面更大；或把 `Metrics` 塞进 `Explain`——错位（`Explain` 是**单条命中**的解释，
> `Metrics` 是**整次检索**的；且空结果时没有 `Hit` 可挂）。

### 3.4 与 Step 6 / V2.1 的接口

- **Step 6（NFR-10/11）依赖本 Step 的 `Metrics` 可观测化**（`plan-v2.md` §4 Step 6「依赖」行）⇒
  S5-05/06/07 应在 Step 6 开工前合并。
- **V2.1 的 prefilter 决策依赖 `vector_shortfall`**（`metrics.rs:36-43`）⇒ 本 Step 之后，
  低选择度档位的该指标会**结构性归零**（走精确路径）。这不是"问题消失了"，而是
  **判据的适用范围被收窄到 `vector_route == Ann` 的档位**——必须在文档与 bench 输出里写清楚（§4.4）。

---

## 4. 详细设计

### 4.1 总览：从两条路径到三条路径

```
                        ┌─ filter == None ──────────────► 路径 A：普通 search() + 过采样     [不变]
search_filtered ────────┼─ FilterKind::Alive ───────────► 路径 A                          [不变]
                        └─ FilterKind::Filtered ─┬─ allowed > 阈值 ──► 路径 B：search_filter()  [不变]
                                                 └─ allowed ≤ 阈值 ──► 路径 C：精确扫描        [新增]
```

三条路径的**分工**：

| 路径 | 定位 | 召回 | 代价 |
| --- | --- | --- | --- |
| A | 热路径（无用户过滤 / 仅存活过滤） | 近似（R17） | `O(log N)` 级，保 fast-return |
| B | 高选择度用户过滤（堆能填满 ⇒ 剪枝生效） | 近似（R11/R17） | `O(min(N, ef/sel))` |
| **C** | **低选择度用户过滤** | **精确**（无缺口） | `O(N)` 谓词判定 + `O(allowed)` 距离 |

**"策略归后端、分派归编排层"**（D-S5-01 + D-S5-07 的组合）：

- 后端回答"这个谓词下我该走哪条"——`VectorIndex::prefers_exact(&dyn CandidateFilter) -> bool`（默认 `false`）；
- 编排层据此**分派**并**记账** `Metrics.vector_route`。

这样策略只有一份实现（在后端），而编排层既能观测又不必复制阈值规则。

### 4.2 精确扫描原语

#### 4.2.1 `VectorIndex` 新增两个方法

```rust
pub trait VectorIndex: Send + Sync {
    // ... 既有方法不变 ...

    /// 精确（无近似）的过滤检索：返回**全部**满足 `filter` 的最近 k 条，
    /// `(chunk_id, 平方欧氏距离)`，按距离升序、同距离按 chunk_id 升序。
    ///
    /// # 契约（与 `search_filtered` 对齐，仅"精确性"不同）
    ///
    /// - 每条结果的 `filter.contains(id)` 必须为 `true`；`None` 时不过滤（含幽灵）。
    /// - **返回条数 == `min(k, 命中的候选数)`**，不允许少返回（这是它与 `search_filtered`
    ///   的**唯一**语义差异，也是低选择度场景的价值所在）。
    /// - 代价不保证优于 `search_filtered`（它是 `O(N)`）⇒ **调用方须先问 `prefers_exact`**。
    fn search_exact_filtered(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<(ChunkId, f32)>>;

    /// 本后端在**该谓词**下是否应走精确路径。
    ///
    /// 策略归后端，编排层只据此分派与记账（`Metrics::vector_route`）。
    /// 默认 `false` ⇒ 不改变任何既有后端的行为（零回归）。
    fn prefers_exact(&self, _filter: &dyn CandidateFilter) -> bool {
        false
    }
}
```

**为什么 `search_exact_filtered` 是"必选方法"而不是提供默认实现**：
默认实现只能给出"某种"行为，而后端是否真精确是**实现事实**。设为必选 ⇒
新增后端必须**显式回答**"我的精确路径是什么"，与 Step 2「Brute 无图」用
`as_graph_persist()` 默认 `None` 把类型事实写进 trait 的同一手法。
（代价：外部 `VectorIndex` 实现要补一个方法；0.x 可破坏，记 CHANGELOG。）

#### 4.2.2 `HnswRsIndex` 的实现（方案 A，D-S5-03）

```rust
fn search_exact_filtered(&self, query, k, filter) -> Result<Vec<(ChunkId, f32)>> {
    if k == 0 { return Ok(Vec::new()); }
    let mut out: Vec<(ChunkId, f32)> = Vec::new();
    // 只走 layer 0（§3.1 推论 1：layer 0 恰好包含全部点一次）
    for point in self.hnsw.get_point_indexation().get_layer_iterator(0) {
        let id = point.get_origin_id() as ChunkId;
        if filter.is_none_or(|f| f.contains(id)) {
            out.push((id, distance_to_slice(query, point.get_v())));
        }
    }
    out.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    out.truncate(k.min(out.len()));
    Ok(out)
}

fn prefers_exact(&self, filter: &dyn CandidateFilter) -> bool {
    filter.kind() == FilterKind::Filtered
        && self.brute_fallback.is_some_and(|max| filter.allowed_count() <= max)
}
```

`BruteForceIndex`：

```rust
fn search_exact_filtered(&self, query, k, filter) -> Result<Vec<(ChunkId, f32)>> {
    self.search_filtered(query, k, filter)   // 它本来就是精确的（brute.rs:37-60）
}
fn prefers_exact(&self, _filter: &dyn CandidateFilter) -> bool { true }  // 恒精确 ⇒ 恒 Exact
```

> `BruteForceIndex::prefers_exact` 返回 `true` 的语义是"**我的结果本来就精确**"，
> 于是 brute 档位的 `vector_route` 恒为 `Exact`（也是真的）。转发调用多一次虚调用，可忽略。

#### 4.2.3 方案 A vs 方案 B（D-S5-03 的取舍）

| | **方案 A：遍历向量存储**（建议） | **方案 B：枚举 allowed + `origin→PointId` 映射** |
| --- | --- | --- |
| 成本 | `O(N)` 次谓词判定 + `O(allowed)` 次距离 | `O(allowed)` 次映射查找 + `O(allowed)` 次距离 |
| 新增状态 | **无** | 一份 `origin_id → PointId`（或 `→ slot`）映射：10 万级 ≈ 0.4~1.6MB |
| 维护点 | 无 | `add` / `add_batch` / `from_loaded` / compaction 重建图 **四处**必须同步；**并行建图**下顺序不确定 ⇒ 不能靠"插入序 == slot 序"省掉映射 |
| 依赖的库能力 | `get_layer_iterator(0)`（§3.1，已核实） | 需 `CandidateFilter` 能**枚举** allowed（`&dyn` 下发返回 `Vec<ChunkId>`）+ `get_point_data`（按 `PointId` 克隆） |
| 风险 | 100 万级时 `O(N)` 项重新成为瓶颈（R31） | 映射与图不同步会**静默错召回**（比慢更危险） |
| 何时改选 | —— | **标定（§4.9）显示方案 A 在 10 万级 P99 进不了 NFR-13 预算**时 |

**建议先做方案 A**：它零新增状态、零维护点、正确性只需一个遍历顺序无关的排序；
而方案 B 省下的那点 `O(N)` 常数，要拿"四处维护 + 并行建图下的顺序不可依赖"去换——
与项目"正确性 > 延迟"的取舍顺序（需求 §6.3）不符，除非数据说必须换。

> 另有**方案 C（已否决）**：编排层接一份 `raw_vectors` 的二分视图（`raw_vectors` 按 `chunk_id`
> 升序是**事实上成立但从未声明**的性质，`flush` 推入序 + `retain` 保序 + compaction remap 单调）。
> 否决理由：① 要把"升序"升格为跨模块不变式并加测试；② 需要新增 `SearchParts` 通道 ⇒
> 逃生舱 `QueryExecutor` 与 bench 都得接线（D-S5-01 已否决该形态）。

#### 4.2.4 成本分解（把 D-J9 的 0.05ms 拆开，避免误读）

D-J9 写的「0.05ms vs 158ms」只覆盖了**最后一段**。完整分解（10 万级、sel-0.1%、allowed≈100）：

| 段 | 工作量 | 量级预估 | 说明 |
| --- | --- | --- | --- |
| ① 遍历定位候选 | `N=10⁵` 次：`Arc::clone`（原子加）+ 追指针到 `Point` + `filter.contains`（存活位 + doc 位各一次查位） | **1~10ms** | **方案 A 的主要成本，必须实测**；方案 B 消掉这一段 |
| ② 距离计算 | `allowed≈100` × 512 维 | **≈0.02ms** | 即 D-J9 的「约 0.05ms」所指 |
| ③ 排序 | 100 条全序 | <0.01ms | —— |
| 对照：路径 B | 整图遍历 `N` 个点，每个点**都算 512 维距离** | **41~158ms**（实测） | 每个点的常数比 ① 大 1~2 个数量级 |

**关键洞察**：路径 B 与路径 C **都是 `O(N)` 遍历**，差别在**每个点做什么**——
B 每次算一次 512 维距离（≈100ns 量级），C 每次做一次位图判定（≈5~20ns 量级）
⇒ 理论上有 **5~20× 的常数差**，这正是"1~10ms vs 41~158ms"的来源。
第①段的实测值是 S5-04 要标定的头号数字。

#### 4.2.5 距离函数：新增一个不分配的切片版

`NormalizedVector::distance_sq(&self, other: &NormalizedVector)`（`point.rs:42-51`）要一个
`&NormalizedVector`——精确扫描里对每个候选都 `NormalizedVector::new(point.get_v().to_vec())`
会**逐点分配 + 重算范数**，把 ① 的成本放大到不可接受。

⇒ 新增：

```rust
/// 与**已归一化**的切片算平方欧氏距离（不分配、不重归一化）。
///
/// ⚠️ 前置条件：`other` 已 L2 归一化（图内数据天然满足）。
/// 与 `distance_sq` 用**同一公式**（Σ(a−b)²），保证与 `BruteForceIndex` 的结果**逐位一致**（S5-T3）。
pub fn distance_to_slice(&self, other: &[f32]) -> f32;
```

### 4.3 阈值与策略（D-S5-02）

**理论分界**：路径 B 的剪枝只在 `return_points.len() >= ef` 时生效（`hnsw.rs:1019`），
故 `allowed ≳ ef` 才谈得上"剪枝"。默认 `ef=120`。

**代价模型（用于定初值，参数由标定拟合）**：

```
C_B(allowed) ≈ min(N, ef / selectivity) × c_dist
C_C(allowed) ≈ N × c_visit + allowed × c_dist
```

令 `C_B = C_C` 解出临界选择度 `sel*`，则阈值 ≈ `sel* × N`。
用 §4.2.4 的量级代入（`c_dist≈100ns`、`c_visit≈20ns`、`ef=120`、`N=10⁵`）
得 `sel*` 在 **10⁻²~10⁻³** 区间 ⇒ 阈值落在 **10²~10³**。

**结论**：默认阈值初值取 **1024**（覆盖 `sel-1%` 档 `allowed≈1000`，且比 `EF_FILTER_MAX=256` 宽），
由 S5-04 标定后定稿。实现上它是一个常量 + 一个可注入开关：

```rust
pub const BRUTE_FALLBACK_MAX_ALLOWED: usize = 1024;   // S5-04 标定后定稿

/// `Some(n)` = 阈值为 n；`None` = **关闭**精确兜底（回归对照用，S5-T7 / bench A/B）。
pub fn with_brute_fallback(mut self, max_allowed: Option<usize>) -> Self;
```

⚠️ 注意 `with_brute_fallback` 引入的新字段**必须穿过 `from_loaded`**
（`hnsw_rs_index.rs:109-120`，图加载路径的唯一构造器）——`from_loaded` 目前三个参数，
新增字段后要么加参、要么保持默认并注释说明（附录 A 已列）。

### 4.4 语义收益：**召回从"近似"变"精确"**（需要一并声明的副作用）

路径 C 的返回值恒为 `min(k, allowed)` 条，于是低选择度档位：

| 指标 | 兜底前（路径 B） | 兜底后（路径 C） |
| --- | --- | --- |
| `vector_shortfall` | > 0（R11/R17 的可见信号） | **恒 0** |
| oracle 重合率 | 1.0000（S1-10 实测，分路） | **1.0000（精确，构造上成立）** |
| hybrid P99 | 41~158ms | ≤ NFR-13 预算 |

**两个必须写进文档的推论**（否则会误导 V2.1 决策）：

1. **`vector_shortfall == 0` 不再等于"没有 prefilter 需求"**——它现在只说明"这一档走了精确路径"。
   判读时必须**连看 `vector_route`**（D-S5-07 的第二个用途）。
2. **R11/R13/R17 在低选择度档位"可见影响消失"，但结构性成因仍在**（一旦 `allowed > 阈值` 又回到路径 B）。
   ⇒ 架构 §14.1 的 R11/R13 不能标"已解决"，只能标"**低选择度子集已绕过**"。

### 4.5 `Metrics` 扩展（D-S5-05/06/07/08）

```rust
pub struct Metrics {
    pub took: Duration,              // 既有
    pub bm25: usize,                 // 既有
    pub vector: usize,               // 既有
    pub candidates: usize,           // 既有
    pub fused: usize,                // 既有
    pub filter_eval: Duration,       // 既有
    pub allowed: usize,              // 既有
    pub vector_shortfall: usize,     // 既有
    // ↓ 本 Step 新增
    /// 向量路实际走的路径（D-S5-07）。`None` = 未走向量路（Bm25 模式）。
    pub vector_route: VectorRoute,
    /// BM25 路耗时（单路模式 = 该路耗时；Hybrid 下与 vector_elapsed 并行重叠）
    pub bm25_elapsed: Duration,
    /// 向量路耗时（含精确扫描的遍历成本）
    pub vector_elapsed: Duration,
}

/// 向量路路径（`vector/mod.rs` 定义，与 `VectorIndex::prefers_exact` 同源）。
pub enum VectorRoute { None, Ann, Exact }
```

**编排层改动**（`searcher.rs`，Hybrid 分支示意）：

```rust
let vec_route = vec.plan(vec_f);            // = if prefers_exact {Exact} else {Ann}；O(1)
let (r1, r2) = rayon::join(
    || { let t = Instant::now(); let r = bm25.search_filtered(query, candidate_k, bm25_f); (r, t.elapsed()) },
    || { let t = Instant::now();
         let r = match vec_route {
             VectorRoute::Exact => vec.search_exact_filtered(query, candidate_k, vec_f),
             VectorRoute::Ann   => vec.search_filtered(query, candidate_k, vec_f),
             // `plan()` 只可能返回 Ann / Exact（`None` 变体是"未走向量路"，
             // 由编排层在 Bm25 模式下直接写进 metrics，不经此路径）
             VectorRoute::None  => unreachable!("plan() 不返回 None"),
         };
         (r, t.elapsed()) },
);
```

> **`VectorRoute::None` 的归属**：它**不是** `plan()` 的返回值，而是编排层在 `SearchMode::Bm25`
> 下写进 `Metrics.vector_route` 的取值。这样"未走向量路"由**类型**表达，不必让
> `plan()` 去编码"这次检索有没有向量路"——那是编排层的信息。

**`plan()` 放哪**：`VectorRetriever` 上新增 inherent 方法（**不改 `Retriever` trait**——
trait 是两条 lane 的共同契约，精确/ANN 之分只属于向量路）：

```rust
impl VectorRetriever<'_> {
    /// 该谓词下向量路将走哪条（供编排层分派与记账）。
    pub fn plan(&self, filter: Option<&dyn CandidateFilter>) -> VectorRoute;
    /// 精确路径（与 `Retriever::search_filtered` 同转换、同排序，仅底层调用不同）。
    pub fn search_exact_filtered(&self, query, k, filter) -> Result<Vec<Scored>>;
}
```

内部把 `Vec<(ChunkId, f32)> → Vec<Scored>`（`score = 1 - d/2`）与排序抽成一个私有函数，
两条路径共用（现有实现见 `retriever/vector.rs:52-67`），**避免出现两份排序**。

**同时修 D-S5-08**：给 `searcher.rs:125` 那条早退补 `metrics.took = started.elapsed();`，
并把 `metrics` 装进 `empty_response`。

### 4.6 `SearchResponse` 承载（D-S5-05）

```rust
pub struct SearchResponse {
    pub hits: Vec<Hit>,
    pub total_candidates: usize,
    pub empty_reason: Option<EmptyReason>,
    pub took: Duration,
    /// 本次检索的内核指标（NFR-07）。`took == metrics.took`；`total_candidates == metrics.candidates`。
    pub metrics: Metrics,
}
```

- `Metrics` 加 `Copy` 已有（`metrics.rs:20`）⇒ 无克隆成本。
- `took` / `total_candidates` **保留**（下游已在用），只在 rustdoc 注明与 `metrics` 的对应关系；
  已否决"把 `took` 标 deprecated"（超出本 Step 范围）。
- `query/mod.rs` 增 `pub use metrics::{Metrics, VectorRoute}`（`VectorRoute` 定义在 `vector`，
  此处再导出以给调用方一个稳定的 import 路径）。

### 4.7 bench / CLI 采集点（D-S5-05 的"第二半"）

| 位置 | 改动 |
| --- | --- |
| `bench.rs` `LatencyResult`（`:1003-1050`） | 新增 `mean_shortfall_kernel`（取 `resp.metrics.vector_shortfall`，与内核口径一致）、`exact_ratio`（`vector_route == Exact` 的占比）、`mean_filter_eval_us`、`mean_bm25_ms` / `mean_vector_ms`。**保留** bench 自算的 `mean_shortfall`（口径不同）**两列并列**——断链 4 的注释从「拿不到」改成「两者差异即 `candidate_k` 与 `k` 之别」（§2.3） |
| `bench.rs` `mode_to_json`（`:1266-1318`） | JSON 增加 `vector_route_exact_ratio` / `vector_shortfall_kernel` / `mean_filter_eval_us` |
| `bench.rs` `print_*`（`:900-980`） | 三元表新增 `route` 与 `shortfall(内核)` 两列 |
| `bench.rs` 参数 | 新增 `--brute-fallback <N\|off>`：`off` ⇒ `with_brute_fallback(None)`（回归对照），不传 ⇒ 内核默认。**A/B 的唯一开关** |
| `main.rs` `search` | 可选：`--metrics` 打印一行内核指标（与 NFR-07 的"用户可自查"对齐）。**默认关**，因为它是诊断输出 |

### 4.8 不变式清单（正确性红线，测试逐条对应）

| # | 不变式 | 对应测试 |
| --- | --- | --- |
| I1 | `prefers_exact == false` 时，**结果逐位回退到现状**（路径 A/B 一字不改） | S5-T7 |
| I2 | 精确路径每条结果都满足谓词（`None` 时= 不过滤、含幽灵） | S5-T2 / T5 |
| I3 | 精确路径条数 `== min(k, 命中候选数)` | S5-T2 |
| I4 | 同一 `(query, 谓词, k)`，`HnswRsIndex::search_exact_filtered` ≡ `BruteForceIndex`（`(chunk_id, distance)` 逐位） | S5-T3 |
| I5 | 排序恒为 `(距离升序, chunk_id 升序)`；连续 100 次结果一致 | S5-T6 |
| I6 | `Metrics.vector_route` **与实际走的路径一致**（不撒谎）；`Exact` ⇒ `vector_shortfall == 0` | S5-T8 / T12 |
| I7 | `metrics.took == took`、`metrics.candidates == total_candidates` | S5-T9 |
| I8 | 三条早退路径的 `took` 均为真实耗时（非 0） | S5-T8 |

### 4.9 标定实验设计（S5-04，D-S5-04 的数值来源）

**目标**：① 定阈值；② 定 NFR-13 预算；③ 给出"兜底是否值得"的直接证据。

**方法**：在既有 `scripts/eval_filter.sh`（8 档位 × 3 模式）之上加**路径维**——
同一档位跑 `--brute-fallback off` 与 `--brute-fallback <N>` 两次，产出四元表：

| 档位 | allowed | route | vector P99 | **filter_eval P99** | shortfall | recall(oracle) |
| --- | --- | --- | --- | --- | --- | --- |
| sel-10% | ≈10000 | B / C | | | | |
| sel-1% | ≈1000 | B / C | | | | |
| sel-0.1% | ≈100 | B / C | | | | |
| ts-range-degraded | ≈100 | B / C | | **≈8ms（不变）** | | |

**判定**：同一档位下 `route=C` 的 P99 **显著低于** `route=B` 才保留该阈值档；
扫出 P99 反转的第一个档位即阈值的上界。**NFR-13 预算**取「标定实测 P99 × 1.5~2 余量」并
向 5ms 取整（拟 20ms），回填需求文档 §6.1 与 plan-v2 §6。

**成本**：10 万级每档一次构建 + 两次扫描；沿用既有脚本的 `--skip-build` 复用快照。
**注意**：延迟数字**只在本地可引用**（CI 共享 runner 不可引用，`docs/README.md` 已声明）。

---

## 5. 决策记录（D-S5-01 ~ D-S5-09）

### D-S5-01 兜底的落点：`VectorIndex` 层 ✅ 建议放后端

| 方案 | 实现 | 优点 | 缺点 |
| --- | --- | --- | --- |
| **后端层（建议）** | `VectorIndex` 加 `search_exact_filtered` + `prefers_exact` | **零 plumbing**：三条入口（门面 / 逃生舱 / bench）自动一致；策略单一实现；Brute 的精确性由类型表达 | 需新增 trait 方法（外部实现要跟进） |
| 编排层 | `SearchParts` 加 `exact_vector: Option<&dyn VectorIndex>`（`raw_vectors` 视图） | 可消掉 `O(N)` 段（走方案 C） | 需新通道；**门面/bench/逃生舱三处都要接线**；策略出现两个装配点 |

**结论**：取后端层。`HnswRsIndex` 本来就拥有"怎么走图"的全部知识，把"什么时候不查图"也放这里最自然。

### D-S5-02 适用面与默认阈值 ✅ 建议：只对 `Filtered`，初值 1024

- 适用面：`filter.kind() == FilterKind::Filtered`。热路径（`None` / `Alive`）**一行不改**——
  R15 的 fast-return 是 Step 1 明确的设计不变量，本 Step 不碰。
- 默认值：**1024**（§4.3 的代价模型），标定后定稿；`None` = 关闭。

### D-S5-03 候选来源 ✅ 建议方案 A（遍历向量存储）

见 §4.2.3 对照表。**建议先做 A**，并把方案 B 写进 §9.2 作为"标定不达标再上"的备选。
关键前提是 §3.1 核实到的 `get_layer_iterator(0)` + `Point::get_v()`（零拷贝）。

### D-S5-04 NFR-13 预算口径 ⏳ 数值待标定

- 口径 = **端到端 P99**（含过滤求值），档位 = 10 万级 / 选择度 ≤1%，拟 **≤ 20ms**。
- 必须在文档里写死：**降级字段档位的过滤求值本身约 8ms**（`doc_bits_scan` O(N) 全扫，Step 1 已接受），
  故该档位的余量只有 ~1.5×；预算把这段算进去，才能避免"兜底没生效"与"过滤求值贵"混淆。

### D-S5-05 `Metrics` 进 `SearchResponse` ✅ 建议进响应

见 §3.3（兼容性）与 §4.6。**必须同时保留三样**：响应字段（给调用方）、`tracing`（给宿主）、
bench 采集点（给决策）。三者受众不同，不是重复。

### D-S5-06 per-lane 耗时 ⏳ 建议补

见 §4.5。Hybrid 并行下没有它，"哪一路慢"不可归因；架构 §8.3 的日志样例本就预期它。

### D-S5-07 路径可见性 ⏳ 建议做

`Metrics.vector_route`。它是 T7-22 的 A/B 判据（§4.9）、NFR-13 验收的断言点（§1.3 条 1），
也是 `vector_shortfall` 语义变更后的**必要配套**（§4.4 推论 1）。少了它，
"兜底生效了吗"只能从延迟反推——Step 1 已经吃过"用错了指标读错结论"的亏。

### D-S5-08 早退 `took` ⏳ 建议修

见 §2.5。一条赋值语句 + 一条回归测试（S5-T8）。

### D-S5-09 降级字段全扫 ⏳ 建议不做

`doc_bits_scan` 的 `O(N)` 与向量路正交：它让**过滤求值**贵，而不是让**召回**贵。
Step 1 已按"接受并测量"结案（S1-10 结论②：`ts_ms` 档位加速比 0.9×）。
本 Step 只把它**单列进 bench**并计入预算（D-S5-04）。

---

## 6. 影响面与兼容性

| 面 | 影响 | 处置 |
| --- | --- | --- |
| `VectorIndex` trait | **新增 1 个必选方法 + 1 个默认方法** | 0.x 破坏性；CHANGELOG 记；库内 2 个 impl（Hnsw / Brute） |
| `SearchResponse` | **新增 1 字段** | 外部字面量构造会编译失败；CHANGELOG `⚠️ 破坏性`；rustdoc 注明 |
| `query` 模块公开面 | 新增 `Metrics` / `VectorRoute` 再导出 | 纯增量 |
| `HnswRsIndex` | 新增 `brute_fallback` 字段 + `with_brute_fallback` + 穿过 `from_loaded` | 内部结构，非公开字段 |
| `NormalizedVector` | 新增 `distance_to_slice(&[f32])` | 纯增量 |
| 快照 / 图格式 | **零改动** | `FORMAT_VERSION` 保持 2；不新增持久化状态 |
| 检索结果 | 低选择度档位从**近似**变**精确**（可能改变 Top-K） | 见 §4.4；R32 |
| 性能 | 低选择度档位 P99 大幅下降；其余档位不变 | S5-T7 钉住 |
| 内存 | 零新增（方案 A） | —— |

---

## 7. 测试计划（S5-T1 ~ S5-T13）

> 单测放 `vector/hnsw_rs_index.rs` / `query/searcher.rs` / `query/metrics.rs`；
> 集成放 `crates/core/tests/step5_query_observability.rs`（新）。

| # | 测试 | 判据 |
| --- | --- | --- |
| **S5-T1** | 三条路径各自被选中（注入 `with_brute_fallback`） | `prefers_exact` 在 `allowed ≤ 阈值 / > 阈值 / kind=Alive` 三个输入下分别为 `true/false/false`；`plan()` 返回 `Exact/Ann/Ann` |
| **S5-T2** | 精确路径 soundness + 完整性 | 每条结果满足谓词；条数 `== min(k, allowed)`；`k=0` / 空图 / `index.is_empty()` 边界返回空 |
| **S5-T3** | **与 Brute oracle 逐位一致** | 同一批向量构造 `HnswRsIndex` 与 `BruteForceIndex`，多 query × 多谓词下 `(chunk_id, distance)` 序列**逐位相等** |
| **S5-T4** | 阈值边界 | `allowed == 阈值`（≤ 成立）与 `allowed == 阈值 + 1`（走 B）两侧行为正确；`allowed = 0` 不可达（编排层已短路） |
| **S5-T5** | `None` 谓词语义不变 | `search_exact_filtered(.., None, ..)` **含已软删除条目**（与 `brute.rs:92-131` 的 T17 同口径） |
| **S5-T6** | 确定性 | 连续 100 次结果一致；同距离按 `chunk_id` 升序 |
| **S5-T7** | **热路径零回归**（R15 护栏） | `None` / `Alive` 谓词下，`off` 与默认档的 Top-K **逐位一致**；`prefers_exact == false`（构造断言） |
| **S5-T8** | `Metrics` 单测 | `vector_route` 三态齐全；`vector_shortfall` 口径（`min(candidate_k, allowed) - len`）；**三条早退路径 `took != 0`** |
| **S5-T9** | 响应口径自洽 | `metrics.took == took`、`metrics.candidates == total_candidates`（含空结果与过滤排空两条路径） |
| **S5-T10** | bench 采集点存在 | `--json` 输出含 `vector_route_exact_ratio` 与 `vector_shortfall_kernel`；汇总表含 `route` 列 |
| **S5-T11** | 标定扫描（`#[ignore]` / 脚本） | 产出 §4.9 的四元表，无硬断言（延迟数字供人工判读） |
| **S5-T12** | **NFR-13 验收** | 10 万级 `sel-1%` / `sel-0.1%` 档：`route == Exact` **且** P99 ≤ 预算（本地 release；CI 不跑） |
| **S5-T13** | 召回不回归 | 兜底档位 `recall_vs_oracle ≥ 0.99`（构造上应为 1.0000）且 `vector_shortfall == 0` |

> ⚠️ **CI 边界**：S5-T11/T12 依赖 10 万级语料与延迟测量，**不在 CI 跑**（沿用 `eval_filter.sh`
> 的"本地实测"定位）；CI 只跑 S5-T1~T10、T13 的合成小规模版本。

---

## 8. 实施任务拆分（S5-01 ~ S5-08）

> PR 切分：**PR-A（T7-22 兜底）** = S5-01~04；**PR-B（T7-23 可观测）** = S5-05~07；
> **PR-C（文档回写）** = S5-08。base **一律 main**（记忆里 #13/#14 的堆叠 PR 教训）。
> PR-B 是 Step 6 的前置（§3.4），可在 PR-A 评审期间并行开工。

| # | 任务 | 交付 |
| --- | --- | --- |
| **S5-01** | `VectorIndex` 加 `search_exact_filtered`（必选）+ `prefers_exact`（默认 `false`）；`BruteForceIndex` 实现（转发 + `true`）；`NormalizedVector::distance_to_slice` | 前两个 impl + 单测（S5-T2/T3/T5/T6 的 brute 侧） |
| **S5-02** | `HnswRsIndex::search_exact_filtered`（layer 0 遍历，§4.2.2）；加 layer 0 基数 == `get_nb_point()` 的断言 | S5-T3（核心：与 Brute 逐位一致）、S5-T6 |
| **S5-03** | `HnswRsIndex::prefers_exact` + `brute_fallback` 字段 + `with_brute_fallback` + `from_loaded` 穿透；`VectorRetriever::plan` / `search_exact_filtered`；`search_parts` 分派改造（`vector_route` 占位） | S5-T1/T4/T7 |
| **S5-04** | bench `--brute-fallback` 旋钮 + 路径列；`scripts/eval_filter.sh` 加路径维；10 万级标定 → **定稿阈值与 NFR-13 预算** | S5-T11/T12 + `eval-report.md` §8.9（数值） |
| **S5-05** | `Metrics` 三字段 + `VectorRoute` 定义与再导出 + **修 D-S5-08 早退 `took`** | S5-T8（`metrics.rs` 首个单测） |
| **S5-06** | `SearchResponse.metrics` 字段 + 编排层回填（含两条早退路径）+ 集成测试 | S5-T9 + `tests/step5_query_observability.rs` |
| **S5-07** | bench `LatencyResult` / JSON / 表格采集点（两套 shortfall 口径并列）；修 `metrics.rs:14` 失效引用（#7 已关）；可选 `search --metrics` | S5-T10 |
| **S5-08** | 文档回写：架构 **§8.3**（`Metrics` 限制段销账 + 三张新表）、**§14.1 R18 结案** + 新增 **§14.3 R31~R35**、**§5.4.4**（`VectorIndex` 精确路径契约）；需求 **NFR-13 预算回填**；plan-v2 进度 + 验收勾选；`eval-report.md` §8.9；`user-guide.md`（`--brute-fallback`）；`docs/README.md`；CHANGELOG | 纯文档 |

---

## 9. 风险与未决问题

### 9.1 新增风险（拟写入架构 §14.3，编号 R31~R35）

| # | 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- | --- |
| **R31** | **精确扫描的 `O(N)` 谓词判定项**（方案 A 的固有成本，§4.2.4 第①段） | 100 万级时该项线性增长，可能重新超过路径 B | 阈值可配 + `vector_route` 可观测；标定给出"仍可接受"的规模上界；方案 B（候选枚举）留作逃生舱 | ⏳ 待标定（10 万级） |
| **R32** | **兜底改变向量路输出**（近似 → 精确） | 与历史 ANN 评测基线不再逐位可比（性质同 Step 4 的 R29） | 文档声明 + `vector_route` 可见 + `--brute-fallback off` 可复现旧行为（A/B 对照） | 已接受（**只在 `Filtered` 且低选择度档位**） |
| **R33** | **阈值是经验值**：选择度分布随字段基数/谓词组合剧烈变化，`allowed` 在阈值附近时性能台阶切换 | 阈值附近的档位收益不稳 | §4.9 标定曲线 + 可配 + 指标暴露；文档写明"阈值是性能决策点，不是正确性边界" | ⏳ 待标定 |
| **R34** | **精确扫描全程持 `points_by_layer` 读锁**（`IterPointLayer` 构造即 `read()`，迭代期间不释放） | 并发检索间**互相不阻塞**（读锁共享）；但与**写端**（`insert` 取写锁）互斥 | V2.0 单写者语义下无影响（Q-U1：`into_searcher()` 后无法写）；**Step 8（读写并发）必须复核**：精确扫描的持锁时长随 `N` 线性增长，会成为写端的长时间阻塞源 | 已记录，归 Step 8 |
| **R35** | `SearchResponse` / `VectorIndex` 的**对外破坏性变更** | 外部字面量构造 `SearchResponse`、外部 `VectorIndex` 实现会编译失败 | CHANGELOG `⚠️ 破坏性` 段 + rustdoc 迁移说明；0.x 阶段可接受 | 已接受 |

### 9.2 未决问题（需评审或实测回答）

| # | 问题 | 阻塞谁 | 谁回答 |
| --- | --- | --- | --- |
| **Q1** | 阈值默认值（初值 1024 是否可接受） | S5-03 的常量 | 标定实验（S5-04） |
| **Q2** | NFR-13 预算数值（拟 20ms） | §1.3 验收判定 | 标定实验（S5-04） |
| **Q3** | `search_exact_filtered` 设为**必选**方法是否接受（外部实现要跟进） | S5-01 | 评审 |
| **Q4** | `Metrics` 进 `SearchResponse`（D-S5-05）是否接受破坏性 | S5-05/06 | 评审 |
| **Q5** | 是否现在就把方案 B（候选枚举 + `origin→PointId` 映射）也做掉 | 无 | 标定实验：方案 A 达标则不做 |

---

## 附录 A：新增 / 变更 API 一览

| 位置 | 签名 | 性质 |
| --- | --- | --- |
| `vector::VectorIndex` | `fn search_exact_filtered(&self, &NormalizedVector, usize, Option<&dyn CandidateFilter>) -> Result<Vec<(ChunkId, f32)>>` | **新增（必选）** |
| `vector::VectorIndex` | `fn prefers_exact(&self, &dyn CandidateFilter) -> bool` | 新增（默认 `false`） |
| `vector::VectorRoute` | `enum { None, Ann, Exact }`（`Copy`） | 新增 |
| `vector::HnswRsIndex` | `fn with_brute_fallback(self, Option<usize>) -> Self` | 新增 |
| `vector::HnswRsIndex` | 常量 `BRUTE_FALLBACK_MAX_ALLOWED: usize = 1024` | 新增（S5-04 定稿） |
| `vector::HnswRsIndex` | `from_loaded(hnsw, ef_search, parallel_build)` | **签名变更**：新增 `brute_fallback`（或保持默认，附录 §4.3 已注明） |
| `vector::NormalizedVector` | `fn distance_to_slice(&self, &[f32]) -> f32` | 新增 |
| `retriever::VectorRetriever` | `fn plan(&self, Option<&dyn CandidateFilter>) -> VectorRoute` | 新增 |
| `retriever::VectorRetriever` | `fn search_exact_filtered(&self, &str, usize, Option<&dyn CandidateFilter>) -> Result<Vec<Scored>>` | 新增 |
| `query::response::SearchResponse` | `pub metrics: Metrics` | **新增字段（破坏性）** |
| `query::metrics::Metrics` | `pub vector_route: VectorRoute` / `pub bm25_elapsed: Duration` / `pub vector_elapsed: Duration` | 新增字段 |
| `query`（模块根） | `pub use metrics::{Metrics, VectorRoute}` | 新增再导出 |
| `cli bench` | `--brute-fallback <N\|off>` | 新增参数 |

---

## 附录 B：标定与验收执行命令

```bash
# 1) 小规模正确性（CI 口径，秒级）
cargo test --workspace
cargo test -p helix-core --release -- --ignored T12_选择度与延迟与召回三元数据

# 2) 10 万级标定：同一档位跑「关兜底 / 开兜底」两次（S5-04 / D-S5-04）
./scripts/eval_filter.sh --n 100000 --skip-build --levels sel-10%,sel-1%,sel-0.1%
cargo run --release -p helix-cli -- bench \
  --input data/synth-100000-corpus.jsonl --queries data/synth-100000-queries.jsonl \
  --modes vector,hybrid --filter "<档位谓词>" --reps 20 \
  --brute-fallback off --json /tmp/sel01-off.json
cargo run --release -p helix-cli -- bench \
  --input data/synth-100000-corpus.jsonl --queries data/synth-100000-queries.jsonl \
  --modes vector,hybrid --filter "<档位谓词>" --reps 20 \
  --brute-fallback 1024 --json /tmp/sel01-on.json

# 3) NFR-13 验收（S5-T12；本地 release；CI 不跑）
#    读 /tmp/sel01-on.json：vector_route_exact_ratio == 1.0
#    且 vector P99 ≤ 预算（→ eval-report.md §8.9）
```

> ⚠️ 档位谓词与 `data/synth-100000-filters.json` 的对应关系见 `scripts/eval_filter.sh`；
> 延迟数字**只在本地可引用**（CI 共享 runner 不可引用）。

---

## 附录 C：`hnsw_rs` 0.3.4 源码核实记录（本文关键技术前提）

核实环境：`~/.cargo/registry/src/index.crates.io-*/hnsw_rs-0.3.4/src/hnsw.rs`。

| 结论 | 证据 |
| --- | --- |
| 每个插入点在 layer 0 出现且仅一次 | `Point::generate_new_point`：`p_id.1 = points_by_layer_ref[p_id.0].len()`，随后 `points_by_layer_ref[p_id.0].push(Arc::clone(&new_point))`（`:503-512`），`p_id.0` 自 0 起 |
| layer 0 可迭代，且**不跨层** | `get_layer_iterator(layer)`（`:614-616`）→ `IterPointLayer::next` 只索引 `pi_guard[self.layer]`（`:715-723`） |
| ⚠️ `PointIndexation::into_iter()` **会跨层遍历** | `IterPoint::next` 从 `layer=0` 逐层上升到 `entry_point_level`（`:647-677`）⇒ **同一高层点会被 yield 多次**，不可用于"遍历所有点" |
| 向量是**零拷贝**切片 | `Point::get_v(&self) -> &[T]`（`:204-206`），内部直接返回 `self.v` 的视图（mmap 加载时指向 mmap） |
| 原始 id 可读 | `Point::get_origin_id(&self) -> usize`（`:214-216`），即插入时传入的 `(data, origin_id)` 的第二项 ⇒ **就是 `ChunkId`** |
| 按 `PointId` 取向量存在，但**是克隆** | `Hnsw::get_point_data(&PointId) -> Option<Vec<T>>`（`:582-593`）：`Some(self.points_by_layer.read()[l][p].get_v().to_vec())` |
| **无 `origin_id → PointId` 公开映射** | 全文件仅 `Point` 内部持 `origin_id` 与 `p_id`；映射只存在于遍历中（`get_origin_id()` 与 `p_id` 同时可读）⇒ 方案 B 必须自建 |
| 带 filter 时 **无 fast-return** | `search_layer`：`if filter.is_none() { return return_points } else if return_points.len() >= ef { retain(..) }`（`:983-992`） |
| 堆未满时**距离剪枝关闭** | `if e_dist_to_p < f_dist_to_p \|\| return_points.len() < ef`（`:1019`） |
| 层类型 = `Vec<Arc<Point>>` | `type Layer<'b, T> = Vec<Arc<Point<'b, T>>>`（`:386`）⇒ 迭代含**每元素一次 `Arc::clone`**（§4.2.4 第①段的成本来源之一） |

---

## 附录 D：本文引用的项目内证据

| 结论 | 位置 |
| --- | --- |
| 路径分派（A/B）与常量 | `crates/core/src/vector/hnsw_rs_index.rs:19-34`、`:200-221`、`:238-286` |
| 图加载唯一构造器 | `crates/core/src/vector/hnsw_rs_index.rs:109-120` |
| `BruteForceIndex` 已是精确实现 | `crates/core/src/vector/brute.rs:37-60`；`None` 契约测试 `:92-131` |
| `VectorIndex` trait 与 `None` 契约 | `crates/core/src/vector/mod.rs:30-83` |
| 距离约定与 `distance_sq` | `crates/core/src/vector/point.rs:1-7`、`:41-51` |
| 谓词种类与对象安全约束 | `crates/core/src/predicate.rs:30-60` |
| `allowed_count` 口径（O(匹配文档数)） | `crates/core/src/query/filter.rs:118-126`、`:160-180` |
| 向量路转换与排序 | `crates/core/src/retriever/vector.rs:38-68` |
| 编排：过滤求值 / 两路并行 / shortfall 口径 | `crates/core/src/query/searcher.rs:107`、`:110-131`、`:145-179`、`:199-215`、`:262-273` |
| 早退路径 `took` 缺失 | `crates/core/src/query/searcher.rs:125-127`（对照 `:212-213`） |
| `Metrics` 现状与自述限制 | `crates/core/src/query/metrics.rs:1-15`、`:21-45` |
| `SearchResponse` 字段与再导出 | `crates/core/src/query/response.rs:62-72`、`crates/core/src/lib.rs:68` |
| bench 的 `mean_shortfall` 口径注释（断链 4） | `crates/cli/src/bench.rs:186-193`、`:1003-1050`、`:1266-1318` |
| R18 与 S1-10 十 万级三元数据 | `docs/devel/architecture-design.md:1456`；`docs/devel/plan-v2.md` §4 Step 1 B 表 |
| NFR-13 原文 | `docs/devel/requirements-spec.md` §6.1（NFR-13 行） |
| Step 5 任务定义与验收 | `docs/devel/plan-v2.md` §4 Step 5 / §6 / §7；issue **#22** |
| R11/R13/R15/R17 的原始表述 | `docs/devel/architecture-design.md:1449-1455` |
