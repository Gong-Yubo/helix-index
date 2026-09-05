# HelixIndex V2 · Step 1 详细设计（正确性修复：向量软删除 + 过滤下推）

> 面向 Agent 场景的通用检索引擎内核——**V2 Step 1 的开工前详细设计**。
> 本文件回答：改哪些模块、为什么这么改、每个接口长什么样、怎么验证。

| 项 | 内容 |
| --- | --- |
| 版本 | **v1.0**（S1-01 ~ S1-11 全部完成，含 10 万级实测） |
| 日期 | 2026-09-05 |
| 状态 | **已实施完成并合入 main**；R18（低选择度延迟）为已知限制，归 V2.1 |
| 上游 | `plan-v2.md`（Step 1 / H2 / Q-C1 / Q-I1 / Q-I2）、`requirements-spec.md` v1.4（FR-26 / FR-27）、`architecture-design.md` |
| 范围 | T7-07 向量软删除 + 存活过滤、T7-08 过滤下推 + 字段索引 |
| 非范围 | 图持久化（Step 2）、原子快照（Step 3）、墓碑物理回收与体积（Step 5）、精排（Step 6） |

**文件命名说明**：V1 的阶段设计用 `pX-design.md`（P 系列已被阶段号占用，`plan-v2` 评审 M2 已指出 P/R/I 前缀撞名问题）。V2 起改用 `v2-stepN-design.md`，避免与阶段号、风险表编号混淆。

---

## 0. 修订记录（v0.1 → v0.2）

> **最新修订见 §0.2（v0.3 → v1.0，S1-10 实测 + 评审收尾）。**

评审对 v0.1 逐条做了源码级核对，指出 **1 处事实性误读、5 处 P1 级设计漏洞、5 处 P2 级工程问题**。**全部采纳**，修订如下：

| 评审项 | 结论 | 本版修订位置 |
| --- | --- | --- |
| **P1-1** `search_filter` 停止条件误读 | ✅ **属实，且是本次评审最有价值的一条**。v0.1 说「凑够 `ef` 个通过点才返回」——错了。实码 `hnsw.rs:985-992` 只做 `retain` **并不 return**，带 filter 的 `search_layer` **从不 fast return**；更关键的是 `hnsw.rs:1019` 的 `\|\| return_points.len() < ef` 意味着**堆未满时距离剪枝全程关闭** ⇒ 只要 `allowed < ef`，必然整图遍历 | §3.2 机制表重写、§3.3 结论 2/3 重写、§5.7 `ef` 策略重写（去掉无效的 `allowed` 夹逼） |
| **P1-2** 字段索引类型覆盖漏洞 | ✅ 属实。`json_to_string`（`query/filter.rs:51-56`）对 bool/null/object/array 也返回字符串，`matches()` 能匹配，但字段索引只登记字符串+数值 ⇒ **静默假阴性** | §5.3 改为**全类型经 `json_to_string` 登记**；T6 补类型覆盖 |
| **P1-3** 下推等价性「严格相等」不成立 | ✅ 属实。下推结果是 `allowed` 内 top-C，现状是全局 top-C ∩ allowed，关系是**超集**而非相等 | 验收 3 拆为 **soundness + recall 超集**；T8 改写 |
| **P1-4** 无过滤热路径退化 | ✅ 属实且比评审指出的更严重：走 `search_filter` 会**丢掉 fast-return**，改变热路径成本结构；`ef=120 < 现状 200` 还会静默降召回 | §5.7 新增 **path A（无过滤走普通 `search` + 按存活比例过采样）**；BM25 无过滤传 `None` |
| **P1-5** `FilteredOut` 语义 + `NoMatch` 不存在 | ✅ 两项均属实（`EmptyReason` 仅 3 个变体 `response.rs:76-83`） | §5.8 引入 **`query_has_hits` 词典探针**，query 侧信号优先；T16 原因矩阵 |
| **P1-6** 逃生舱路径契约缺失 | ✅ 属实 | §5.5 写死 `None = 不过滤（含已软删除）` 契约 |
| **P2-1** HashSet 破 NFR-06 论据不实 | ✅ 属实。`allowed` 只用于 `retain`（保序），与迭代序无关。**撤回该论据** | §2.3、D-S1-02 改为诚实理由（更小/O(1) count/为未来迭代做准备） |
| **P2-2** 模块分层边界 | ✅ 采纳 | `CandidateFilter` 移入无依赖叶模块 `crate::predicate`；`vector`/`retriever` 只依赖 trait |
| **P2-3** 附录 B 行号漂移 | ✅ 属实（差 1~5 行） | 附录 B 全部重新精确定位 |
| **P2-4** Bitmap / FieldIndex 工程细节 | ✅ 采纳 | §5.1 `count` 维护与 `debug_assert`；§5.3 基数保护对 `terms+numbers` **合计**生效；S1-03 列全同步点 |
| **P2-5** 措辞与验收口径 | ✅ 采纳 | 验收 1 笔误修正；验收 5 给可证伪口径；§5.7 表补成本标注；新增 T15 |

**v0.2 额外改进（评审未提出，本次自查发现）**：v0.1 的 `expand_to_chunks` 仍是**每 query 一次 O(N) 全扫**——只把 O(N) 次 JSON 求值换成了 O(N) 次位运算，**O(N) 本身没消掉**，而 Q-I1 的病根正是 O(N)。v0.2 改为**惰性 doc 查找**（`contains(chunk_id) = alive(chunk_id) && doc_bits[doc_of(chunk_id)]`），每 query 成本降为 **O(匹配文档数)**，O(N) 彻底消除。见 **D-S1-09** 与 §5.5。

---

## 0.1 修订记录（v0.2 → v0.3，实施期）

实施 S1-01 ~ S1-09 期间发现 **2 处 v0.2 未覆盖的问题**，均已在代码与本文档中修正：

| # | 发现 | 影响 | 处理 |
| --- | --- | --- | --- |
| **F1** | **Q-C1 的危害形态被测试断言错了对象**。幽灵候选**进不了最终 `hits`**：`search_parts` 回捞时用 `Index::chunk(id)` 取正文，墓碑位返回 `None` 直接 `continue`（`query/searcher.rs:222-225`）。因此"删除后还能搜到已删内容"**不是**本 bug 的表现；真正危害是**名额被占**（要 5 条只给 2 条）。初版 T1/T2 断言"结果不含幽灵 chunk_id"，**在修复前后都为真，是无鉴别力的假测试** | 若无此修正，Step 1 的核心修复将没有任何测试能证明其有效性 | §2.1 加澄清框；T1/T2 改为断言 `hits.len() == k`；**反向验证**：摘掉向量路谓词后 T1/T2 由绿转红（5 → 2 条），确认鉴别力 |
| **F2** | **空集短路分支与 `fused.is_empty()` 分支口径不一致**。短路分支直接返回 `FilteredOut`，未跑 `query_has_hits` 探针 ⇒ 「query 无命中 + 过滤排空」误报 `FilteredOut`，违反 §5.8.1 定案的"query 侧信号优先" | Agent 会去调过滤条件，而真正的问题在 query 词 | 短路分支改为同口径跑一次探针（成本 O(query 词数)，相对省下的一次完整检索可忽略）；由 T16 抓出 |
| **F3** | **`hnsw_rs::search` 即使 `knbn == len`、`ef=200`，也可能返回不足 `len` 条**。20 点图上采样 200 次：191 次返回 20 条、8 次 19 条、1 次 18 条（≈4.5% 缺口）。这是 HNSW 的**固有近似误差**，与存活过滤无关，也**不是过采样逻辑的 bug** | 对近似后端断言 `hits.len() == k` 会 flaky。CI Linux 首次复现（K=5、存活 5 → 实际 4 条），本地 30 次挂 2 次 | T1/T2 配比改为 **存活数 = 2×K**（30 篇删 20 留 10 / K=5）：少枚举 1~2 条也凑得满 K。验证 50 次 0 失败；反向验证（摘掉谓词）仍 2 vs 5 保持鉴别力。见风险 **R17** |

---

## 0.2 修订记录（v0.3 → v1.0，S1-10 实测 + PR #6 评审收尾）

S1-10（10 万级 benchmark）是本版的主要来源。它产出了 **1 个新风险、1 处结论推翻、1 个新未决问题**：

| # | 发现 | 处理 |
| --- | --- | --- |
| **R18** | **低选择度过滤查询延迟爆炸**：10 万级上选择度 0.1% → vector P99 **158.45ms**（无过滤的 27.3×），1% → 41.33ms（7.1×）。这远超 R13 v0.2 的预估（"随规模线性上升"），实际是**规模 × 选择度的复合效应** | 新增 R18；R13 标注由 R18 接管；§10.1 T12 补 10 万级数据；CHANGELOG 记为**已知限制** |
| **结论推翻** | v0.3 写「hybrid 重合率未达 0.99，**缺口全部来自向量路**」。实测推翻：bm25 与 vector **分路重合率都是 1.0000**，只有融合后掉到 0.685~0.948 ⇒ 分歧**不可能**来自任何一路的下推，**只能**诞生于融合层 | §10.1 补分路重合率对照表与机制解释；v0.3 的错误归因已修正；新增未决问题 5 |
| **未决 5** | filter-then-fuse（当前）与 fuse-then-filter（oracle）在 10 万级上约 **1/3 的 Top-10 条目不同**。机制清楚（过滤重排路内名次，RRF 只吃名次），但**哪种相关性更好没有结论** —— 现有 fixture 的 relevance 针对全库标注，过滤后被机械稀释，无法判定 | 列为 V2.1 未决问题，需选择度专用相关性评测集 |

同时并入 PR #6（GLM-5.3 评审）的 9 条意见收尾，其中 **1 条 P2 由实测确认为真实短板**：

> **P2-2（高基数 Range 是真实主场景，Q-I1 对它整体失效）**：S1-10 在 10 万级上给出了直接证据 ——
> `ts-range-degraded` 档位的 bm25 **P99 8.26ms**，比 bm25 无过滤检索本身（2.43ms）还贵 **3.4 倍**，
> 即降级字段的 O(N) 全扫已经超过整个检索的代价；过滤求值加速比 0.9×（T13-B），
> **优化对该场景收益为 0**。

---

## 1. 目标与验收

### 1.1 要修的问题（plan-v2 §3 编号）

| ID | 问题 | 严重度 |
| --- | --- | --- |
| Q-C1 | 删除文档后向量仍留在 HNSW 图里 → 幽灵候选 | 高（正确性） |
| Q-I1 | `allowed_chunks` 每次检索全表扫描 O(N) | 高（延迟） |
| Q-I2 | 过滤是 post-filter，低选择度下召回暴跌 | 高（召回） |

### 1.2 对应需求

| 编号 | 需求 | 优先级 |
| --- | --- | --- |
| FR-26 | 向量软删除 + 存活过滤（删除语义扩展到向量侧） | Must |
| FR-27 | 过滤下推 + 字段索引（替换全表扫描） | Must |

### 1.3 验收标准（Step 1 完成的定义）

1. **正确性**：`remove(doc)` 后，Bm25 / Vector / Hybrid 三模式均**不再召回**该文档的任何 chunk；且 **save → load 后依然不再召回**（跨快照永续，见 §2.2）。
2. **等价性**：字段索引求值的过滤结果，与 `matches()` 全表扫描**逐 doc 一致**——性质测试的随机 metadata **必须覆盖全部 JSON 顶层类型**（string / number / bool / null / object / array / 混合，见 P1-2 与 §5.3）。
3. **下推正确性**（拆为两条可判定性质，P1-3）：
   - **soundness**：下推返回的结果**全部**通过 filter，且不含已软删除 chunk；
   - **recall**：`现状结果 ∩ allowed ⊆ 下推结果`（现状会返回的 allowed 一条不丢）。
   BM25 路与 Brute 向量路**严格成立**；Hnsw 路以现状 post-filter 为基线，**召回不回退**。
4. **召回不回退**：低选择度（0.1% / 1% / 10%）场景下，带过滤检索的返回条数不低于现状 post-filter。
5. **延迟**（NFR-02 口径：1 万 chunk，release 重测，可证伪）：
   - **无过滤路径**（最热路径）：P99 **不高于现状基线 × 1.10**，且 Top-10 召回重合率 ≥ 0.99（对比现状基线，记入 `eval-report`）；
   - **带过滤路径**：各选择度档位的 P99 实测入库；**高/中选择度（allowed ≥ 4·k_lane）P99 ≤ 无过滤 P99 × 3**；极低选择度（allowed < k_lane）允许超出，但必须给出实测值与 `vector_shortfall`。
6. **确定性**：NFR-06 不破（删除 + 过滤场景连续 100 次结果一致）。
7. `make fmt && make lint && make test && make deny` 全绿。

---

## 2. 现状与问题定位

### 2.1 Q-C1：删除后向量残留

| 环节 | 现状 | 证据 |
| --- | --- | --- |
| 倒排 | `Index::remove` 重新分析原文并移除 postings，统计量回滚 | `index/mod.rs:184-208` |
| 正排 | `tombstone_chunk` / `tombstone_doc` 置 `None` | `index/forward.rs:49-60` |
| 统计量 | `total_len` / `num_chunks` 递减 | `index/mod.rs:204-205` |
| **向量** | **没有任何处理**——`VectorIndex` trait 无 `remove`，`HnswRsIndex` 也不持有存活信息 | `vector/mod.rs:20-37` |
| 存活判定 | `Index::is_live_chunk` 已定义，**全仓零调用** | `index/mod.rs:247` |

后果链：已删 chunk 的向量仍在图中 → 被 ANN 召回 → 占 Top-K 名额 → 召回静默下降、图随删改膨胀。

> ⚠️ **危害的确切形态（v0.3 实测澄清，写测试前必读）**
>
> 幽灵候选**进不了最终 `hits`**——`search_parts` 在回捞阶段用 `Index::chunk(id)`
> 取正文，墓碑位返回 `None` 直接 `continue`（`query/searcher.rs:222-225`）。
> 所以「删除后还能搜到已删内容」**不是**本 bug 的表现形式。
>
> 真正的可观测危害是**名额被占**：Top-K 的 K 个位置里混进幽灵，直到回捞才被丢弃，
> 于是调用方要 5 条实际只拿到 1~2 条。而 Agent 无法区分「库里只有 2 条」和
> 「有 5 条但 3 个位置被幽灵占了」——这正是它比"召回脏数据"更隐蔽的地方。
>
> ⇒ **验收必须断言 `hits.len() == k`，而不是"结果不含幽灵 chunk_id"**。
> 后者在修复前后都为真，是**无鉴别力的假测试**（T1/T2 初版踩过这个坑：
> 把谓词摘掉后测试依然全绿，实测才暴露）。见 §9 T1/T2。

### 2.2 次生问题：幽灵候选跨快照永续

`SearchIndex::remove` 的注释（`search/index.rs:250-252`）声称"向量条目随 chunk_id 失效……save 时会被丢弃"——**该说法不成立**：

- `raw_vectors` 在 `flush()` 里 push 后**再无移除路径**（`search/index.rs:187-193`）；
- `save()` 原样导出 `raw_vectors`（`search/index.rs:264-270`）；
- `load_with()` 把全部 `raw_vectors` 重灌进向量索引（`search/index.rs:319-331`）。

⇒ 已删向量随快照持久化，加载后重新变成幽灵候选。**Step 1 必须覆盖这条路径**（plan-v2 §3 备注），体积回收则归 Step 5（T7-12）。

### 2.3 Q-I1 / Q-I2：过滤的现状

```rust
// query/filter.rs:39-49
pub fn allowed_chunks(filter: &Filter, index: &Index) -> HashSet<ChunkId> {
    for chunk in index.live_chunks() {              // ← 全表扫描 O(N)
        if matches(filter, &doc.metadata) {          // ← 每个 doc 一次 JSON 取值 + 字符串比较
            out.insert(chunk.chunk_id);
        }
    }
}
```

每次带过滤的检索都执行一遍；逐 doc 做 `serde_json::Value` 取值 + 字符串比较，1 万 doc 约数百 μs~ms，10 万级直接吃掉 NFR-02 的 P99 预算。

调用点在**融合之后**（`query/searcher.rs:142-151`）：先各路召回 `candidate_k`，再 `retain(allowed)`。这是**post-filter**——低选择度下候选被大量剔除，返回条数断崖下跌，而两路召回的算力已经花掉了。

> **v0.1 撤回**：v0.1 称 "`HashSet` 迭代序不确定 → 结果顺序依赖插入序（NFR-06 隐患）"。**该论据不成立**（P2-1）：`allowed` 只用于 `lane.retain(...)`（`searcher.rs:146`），`retain` 保持 `Vec` 原序，而 lane 顺序由 `sort`（score → chunk_id 全序 tie-break）决定，与 `HashSet` 迭代序无关。**位图方案的理由改为**：内存小 8~64 倍、`count_ones()` O(1) 供 `ef` 策略与空集短路决策、集合运算与迭代序无关（为将来直接迭代 allowed 做铺垫）。

---

## 3. 关键调研：`hnsw_rs::search_filter` 的真实语义

> 这是本设计的**决定性依据**。v0.1 对停止条件的描述是**误读**，v0.2 按实码重写（P1-1）。

### 3.1 接口（`hnsw_rs-0.3.4` 源码核实）

```rust
// hnsw.rs:50
pub type DataId = usize;

// filter.rs:6-8
pub trait FilterT { fn hnsw_filter(&self, id: &DataId) -> bool; }
// filter.rs:10-23：为 Vec<usize>（要求有序）与 F: Fn(&DataId) -> bool 提供 blanket impl

// hnsw.rs:1475-1481
pub fn search_filter(&self, data: &[T], knbn: usize, ef_arg: usize,
                     filter: Option<&dyn FilterT>) -> Vec<Neighbour>;
```

`DataId` 就是 `insert((vec, id as usize))` 传入的 `id`，即我们的 `ChunkId`——可直接对接。

### 3.2 内部行为（实码行号已逐条复核）

| 行为 | 源码 | 含义 |
| --- | --- | --- |
| 循环入口 | `while !candidate_points.is_empty()`（`hnsw.rs:960`） | **唯一的正常终止条件是候选队列耗尽** |
| 被过滤的节点**仍进入候选队列** | `candidate_points.push(...)` 在 filter 判断**之前**无条件执行（`hnsw.rs:1026-1027`；filter 判断在 `1032`） | ✅ 搜索能穿过被过滤区域，连通性不被破坏 |
| 被过滤的节点**不进结果堆** | `hnsw.rs:1028-1041` | 结果堆只累积通过过滤的点 |
| 结果堆上限 `ef` | `if return_points.len() > ef { pop }`（`hnsw.rs:1042-1044`） | 堆里最多 `ef` 个通过点 |
| **有 filter 时永不 fast return** | `hnsw.rs:983`：无 filter 时 `return return_points`（`984`）；有 filter 时**只做 `retain` 剔除未通过过滤的入口点**（`986-991`），**随后 fall-through 继续循环** | ⚠️ v0.1 误读为「凑够 ef 就返回」，实际**从不提前返回** |
| **堆未满时距离剪枝关闭** | `hnsw.rs:1019`：`if e_dist_to_p < f_dist_to_p \|\| return_points.len() < ef` | ⚠️ 堆内通过点数 < `ef` ⇒ **任何邻居都进候选队列** ⇒ 候选持续膨胀 |
| 守卫分支 | `hnsw.rs:1011-1014`：堆被清空（见 `1036` 的 `return_points.clear()`）时 `return` | 冷门路径，非正常终止 |
| `ef` 恒 ≥ `knbn` | `hnsw.rs:1519`：`let ef = ef_arg.max(knbn)` | 调小 `ef` 无效，会被 `knbn` 顶上去 |
| 外层截断 + 冗余过滤 | `last = knbn.min(ef).min(len)`（`hnsw.rs:1535`），取 `neighbours[0..last]` 再过滤一次（`1537-1553`） | 输出条数上限 = `min(knbn, ef)` = **`knbn`**（因 `ef ≥ knbn`） |

### 3.3 四条硬结论（写进设计约束）

**结论 1：layer 内部确实是真下推，且结果全部满足过滤条件。** 不需要在内核侧再做一遍 post-filter。

**结论 2（重写）：带 filter 时，`ef` 不是「目标收集数」，而是「堆满即开启距离剪枝」的阈值。**

真正的机制是 `hnsw.rs:1019` 的短路条件 `return_points.len() < ef`：

- 堆内通过点数 **< `ef`** ⇒ 距离剪枝**全程关闭** ⇒ 候选队列无条件扩张 ⇒ **整图遍历**；
- 堆**填满 `ef`** ⇒ 退化为「以 `ef` 为宽度的常规 ANN 搜索」，但因 fast-return 被禁用，尾部还要多耗一部分。

由此推出关键判据：

> **只要 `allowed < ef`，结果堆永远填不满 ⇒ 距离剪枝永不开启 ⇒ 必然整图遍历。**

这是**库层的结构性行为，不是参数没调好**。调参只能决定「是否进入整图遍历」，**无法降低整图遍历本身的成本**。

**结论 3（重写）：带 filter 的搜索从不提前返回**（fast-return 在 `hnsw.rs:983-992` 被绕过），唯一正常终止是候选队列耗尽。因此「低选择度延迟不可控」是结构性的，tuning 治不了——真正的解法是 prefilter 图结构（ACORN 一类），不在 V2.0 范围（§10 R11 / 未决问题 3）。

**结论 4（新增）：`knbn` 是输出条数的旋钮，`ef` 只是搜索宽度。** 因 `ef = max(ef_arg, knbn)`（`1519`），输出上限恒为 `min(knbn, ef) = knbn`。**想多返回就调大 `knbn`，不是调大 `ef`**（v0.1 的过采样思路在 `knbn = k` 下是无效的）。

---

## 4. 设计总览

### 4.1 数据流对比

```
【现状】
query → 两路召回(candidate_k, 无过滤) → 全表扫描构建 allowed_chunks (O(N) 次 JSON 求值)
      → 融合前 retain(allowed)  ← post-filter：剔除即丢失
      → 融合 → take(k) → 回捞 → 精排

【V2 Step 1（v0.2）】
query → filter 求值（字段索引，O(匹配文档数)）→ 得到 doc 位图
      → 惰性谓词：contains(chunk_id) = alive(chunk_id) && doc_bits[doc_of(chunk_id)]
      → 两路召回（BM25 遍历 postings 时跳过 / 向量路下推）        ← 无 O(N) 步骤
      → 融合 → take(k) → 回捞 → 精排
```

关键变化：**每 query 的 O(N) 全扫彻底消失**（v0.1 还保留了 O(N) 位运算展开，v0.2 由 D-S1-09 消除）。

### 4.2 三条主线的落点（按 P2-2 修正分层）

| 主线 | 新增结构 | 落点模块 | 依赖 |
| --- | --- | --- | --- |
| 稠密位图原语 | `Bitmap` / `ChunkBits` / `DocBits` | `bitmap.rs`（新，叶模块） | 仅 `std` |
| 候选谓词抽象 | `CandidateFilter` trait / `FilterKind` / `AliveOnly` / `ChunkFilter` | `predicate.rs`（新，叶模块） | 仅 `bitmap` + `types` |
| 存活状态单一真源 | `ForwardStore::alive: ChunkBits` | `index/forward.rs` | `bitmap` |
| 元数据字段索引 | `FieldIndex`（`field → value → doc 位图`） | `index/field_index.rs`（新） | `bitmap` + `index` |
| 过滤求值（含 oracle） | `doc_bits` / `doc_bits_scan`（依赖 `Index`） | `query/filter.rs`（现有文件扩展） | `index` |

**分层守则**：`vector/*` 与 `retriever/*` **只依赖 `crate::predicate`（不含 `Index`）**，不 import `query::filter`，也不 import `index`。这样不会把「vector → index」拖成文件级耦合，符合 `lib.rs` 的模块边界守则。

### 4.3 关键取舍：存活状态住在 `Index`，不进 `VectorIndex`

T7-07 的语义是「软删除 + 存活过滤」。有两个放置方案：

| 方案 | 存活状态存哪 | 评价 |
| --- | --- | --- |
| A | `VectorIndex` 自持墓碑集合，trait 加 `tombstone(id)` | ❌ 与正排墓碑**双写两处状态**，漂移风险；且与"快照加载时 `raw_vectors` 全量重灌"冲突（向量侧状态无法从快照恢复，必须靠外部重放） |
| **B（选定）** | 存活状态只住 `Index.forward`，检索时以**位图谓词**传给向量路 | ✅ 单一真源；快照加载后从 `chunks: Vec<Option<Chunk>>` 重建（O(N)，一次性）；向量侧零状态改动 |

方案 B 还顺带解决 §2.2：快照重灌已删向量后，位图来自正排墓碑（快照里 `None` 位保留），幽灵候选在检索期被过滤掉，与是否跨快照无关。

---

## 5. 详细设计

### 5.1 位图模块 `crates/core/src/bitmap.rs`（新）

自研最小稠密位图，**不引入 `roaring` 依赖**。理由（v0.2 修订，去掉不实论据）：`ChunkId` / `DocId` 都是 `u32` 连续自增（`types.rs:6-13`），ID 空间稠密，`Vec<u64>` 的 O(1) 位运算比 Roaring 的 container 跳转更快；内存小（10 万 chunk → 12.5 KB）；`count_ones()` O(1) 供 `ef` 策略与空集短路决策使用；集合求值与迭代序无关。

```rust
/// 稠密位图（ID 为 u32 连续分配场景）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bitmap {
    words: Vec<u64>,
    /// 置位计数（O(1) 判空，供短路决策与 ef 策略）
    count: usize,
}

impl Bitmap {
    pub fn new() -> Self;
    pub fn set(&mut self, id: u32);            // 自动扩容；已在位则 no-op
    pub fn clear(&mut self, id: u32);          // 未在位则 no-op
    pub fn contains(&self, id: u32) -> bool;
    pub fn union_with(&mut self, other: &Self);
    pub fn intersect_with(&mut self, other: &Self);
    pub fn count_ones(&self) -> usize;         // == count
    pub fn is_empty(&self) -> bool;            // count == 0，O(1)
}

/// 语义别名（仅文档价值，编译期同类型）
pub type ChunkBits = Bitmap;
pub type DocBits = Bitmap;
```

**`count` 的维护契约**（P2-4，必须写进实现注释）：

- `count` 是 `words` 中置位数的**缓存**，必须与之一致；
- `set` / `clear` 先判原位再决定是否 ±1（不可无条件自增）；
- `union_with`：`count = a.count + b.count - intersect_count(a, b)`——**需要一次逐字交集计数**，成本 O(words)，实现时不要误写成 `a + b`；
- `intersect_with`：`count = intersect_count(a, b)`；
- 所有修改路径末尾 `debug_assert_eq!(self.count, self.count_ones_slow())`，debug 构建下防漂移（release 零成本）。

### 5.2 存活位图：单一真源 + 快照重建

放在 `ForwardStore` 里，与墓碑动作**同一入口**维护，杜绝漂移：

```rust
// index/forward.rs
pub struct ForwardStore {
    chunks: Vec<Option<Chunk>>,
    docs: Vec<Option<DocRecord>>,
    /// 存活分片位图（与 chunks 的 Some/None 同源，唯一维护入口在本文件）
    alive: ChunkBits,
    /// 每个 doc 的 chunk 数（供 allowed_count O(匹配文档数) 精确求值，见 D-S1-09）
    doc_chunk_count: Vec<u32>,
}

impl ForwardStore {
    pub fn insert_chunk(&mut self, mut chunk: Chunk) -> ChunkId {
        let id = self.chunks.len() as ChunkId;
        chunk.chunk_id = id;
        self.chunks.push(Some(chunk));
        self.alive.set(id);          // ← 唯一置位入口
        id
    }
    pub fn tombstone_chunk(&mut self, id: ChunkId) {
        if let Some(slot) = self.chunks.get_mut(id as usize) {
            *slot = None;
            self.alive.clear(id);    // ← 唯一清位入口，与上一步同函数
        }
    }
    pub fn alive_chunks(&self) -> &ChunkBits;
    pub fn alive_count(&self) -> usize;          // O(1)
    pub fn doc_of(&self, chunk_id: ChunkId) -> Option<DocId>;   // O(1) 数组索引
    /// 从 chunks 重建（快照导入用；O(N) 一次）
    fn rebuild_alive(&mut self) { /* 遍历 chunks 的 Some/None */ }
}
```

`Index` 暴露 `pub fn alive_chunks(&self) -> &ChunkBits` 与 `pub fn doc_of(&self, chunk_id) -> Option<DocId>`；`Index::import` 调 `rebuild_alive()` 并重建 `doc_chunk_count`，**快照格式不变**（见 §5.10）。

> `Index::is_live_chunk`（`index/mod.rs:247`）保留，实现改为转发到位图（现在是 O(1) 位测试），并真正被调用起来。

### 5.3 字段索引 `crates/core/src/index/field_index.rs`（新）

metadata 挂在 doc 上、chunk 继承其判定结果，因此索引维护 **doc 级**位图：

```rust
/// 字段索引：field → value → 命中的 doc 位图。
#[derive(Debug, Default)]
pub struct FieldIndex {
    fields: HashMap<String, FieldValues>,
}

struct FieldValues {
    /// 等值匹配：值的 **json_to_string 形式** → doc 位图
    terms: HashMap<String, DocBits>,
    /// 范围匹配：数值 → doc 位图（仅当 metadata 值是数字时登记）
    numbers: BTreeMap<NumKey, DocBits>,
    /// 该字段的 terms + numbers 合计基数（基数保护用，见下）
    value_count: usize,
}

/// f64 的全序包装（BTreeMap 需要 Ord；用 `total_cmp`，MSRV 1.90 可用）
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct NumKey(u64);   // f64 的有序位模式（正数翻转符号位后可按 u64 比较）
```

**维护入口**（与正排同函数内，同样单一真源）：

- `Index::add`：`field_index.insert(doc_id, &doc.metadata)`
- `Index::remove`：`field_index.remove(doc_id, &doc.metadata)` —— **必须在 `tombstone_doc` 之前调用**（此时 metadata 仍可读）。这条时序约束由 T15 单独钉死。
- 位图为空的 value 键要删除（回收内存），并回退 `value_count`。

#### 5.3.1 类型覆盖：全类型登记（P1-2 修正）

`matches()` 的 `Eq` 语义是 `json_to_string(metadata[field]) == value`（`query/filter.rs:22-25`），而 `json_to_string`（`filter.rs:51-56`）对**所有** JSON 类型都返回字符串：

| metadata 值 | `json_to_string` | v0.1（只索引 string / number） | v0.2 |
| --- | --- | --- | --- |
| `"rust"` | `"rust"` | ✅ 索引 | ✅ 索引 |
| `2021.0` | `"2021.0"` | ✅ 索引 | ✅ 索引 |
| `true` | `"true"` | ❌ **静默假阴性** | ✅ 索引 |
| `null` | `"null"` | ❌ **静默假阴性** | ✅ 索引 |
| `[1,2]` | `"[1,2]"` | ❌ **静默假阴性** | ✅ 索引 |
| `{"a":1}` | `"{\"a\":1}"` | ❌ **静默假阴性** | ✅ 索引 |

⇒ **v0.2 规则：所有顶层字段值一律经 `json_to_string` 登记进 `terms`；数值额外登记进 `numbers`（供 `Range`）。** 这样与 `matches()` **逐类型等价**，且实现最简单（不必为"未索引类型"维护字段级降级标记）。

> 注意这个反直觉但必须保留的既有行为：`year: 2021.0` 用 `Eq("year", "2021")` **匹配不上**（字符串 `"2021"` ≠ `"2021.0"`）。字段索引必须**原样复刻**这一行为，不要"顺手修正"——那会破坏与 `matches()` 的等价性。

**同步点清单**（P2-4，S1-03 必须全部覆盖）：`Index::add`、`Index::remove`（时序！）、`Index::import`（从 `docs` metadata 全量重建）、`Index::export`（无需导出，import 重建）、`Default::default()`。

#### 5.3.2 基数保护（内存护栏）

metadata 里若出现高基数字段（uuid、毫秒时间戳），`terms` 会爆炸到「每 value 一张位图」。设 `MAX_VALUES_PER_FIELD`（**初值 1024，可配置**，见未决问题 2）：

- 保护对象为 **`terms` + `numbers` 的合计键数**（`value_count`），避免"terms 受保护但 numbers 爆炸"的缝（P2-4）；
- 超过后**不再为该字段新增 value 键**（已有键继续正常维护），该字段的过滤**退化为全表扫描**；
- 退化标记是**字段级**的（`FieldValues.degraded: bool`），求值器读到该标记即走 `doc_bits_scan`；
- 正确性不受影响，只是慢；实测记入 NFR-05。

### 5.4 过滤求值：字段索引驱动 + 全扫兜底

```rust
// query/filter.rs（依赖 Index，vector/retriever 不 import 本文件）

/// 求满足 filter 的 doc 位图（字段索引驱动；字段未索引/已降级时退化为全表扫描）。
pub fn doc_bits(filter: &Filter, index: &Index) -> DocBits;

/// 兜底/对照实现：逐 doc 用 `matches()` 判定（保留为 oracle，见 §8 T6）。
pub fn doc_bits_scan(filter: &Filter, index: &Index) -> DocBits;

/// 把 doc 位图换算为 chunk 数（O(匹配文档数)，供 Metrics 与 ef 策略）。
pub fn allowed_chunk_count(bits: &DocBits, index: &Index) -> usize;
```

求值规则：

| Filter | 求值 |
| --- | --- |
| `Eq { field, value }` | `fields[field].terms[value]`；字段/值缺失或字段已降级 → 走 `doc_bits_scan` |
| `Range { field, gte, lte }` | `numbers.range(gte..lte)` 求并；字段无数值索引或已降级 → 走 `doc_bits_scan` |
| `And(list)` | 逐项求交（**先按 `count_ones` 升序**，小集合先交，减少工作量） |
| `Or(list)` | 逐项求并 |

### 5.5 候选谓词 `crates/core/src/predicate.rs`（新叶模块）

```rust
/// 谓词类型：向量路据此选择检索路径（见 §5.7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    /// 无用户过滤，仅存活过滤（选择度≈1）
    Alive,
    /// 真实的用户过滤（已 AND 存活位图）
    Filtered,
}

/// 候选过滤谓词：编排层求值一次，传给各 lane 在遍历期判定（过滤下推）。
///
/// 必须 `Send + Sync`：Hybrid 模式下两路召回在 `rayon::join` 中共享同一谓词引用。
pub trait CandidateFilter: Send + Sync {
    /// chunk_id 是否允许进入结果
    fn contains(&self, chunk_id: ChunkId) -> bool;
    /// 允许通过的总数（O(1)）：`Alive` = 存活 chunk 数；`Filtered` = 通过过滤的 chunk 数
    fn allowed_count(&self) -> usize;
    /// 谓词种类（决定向量路走哪条路径）
    fn kind(&self) -> FilterKind;
}
```

两种实现：

```rust
/// 无用户过滤场景：只做存活过滤（Q-C1 的修复路径，热路径零构建成本）
pub struct AliveOnly<'a> { alive: &'a ChunkBits, count: usize }

/// 有用户过滤场景：doc 位图 + 正排引用，**惰性**判定（不展开 chunk 位图）
pub struct ChunkFilter<'a> {
    doc_bits: DocBits,
    index: &'a Index,        // 仅用于 chunk_id → doc_id 的 O(1) 映射
    count: usize,            // allowed_chunk_count()，构建时算好
}
```

**惰性判定（D-S1-09）**：

```rust
impl CandidateFilter for ChunkFilter<'_> {
    fn contains(&self, chunk_id: ChunkId) -> bool {
        // 1) 存活位图 O(1) —— 覆盖已软删除的 chunk（Q-C1）
        if !self.index.alive_chunks().contains(chunk_id) { return false; }
        // 2) doc 位图 O(1) —— chunk_id → doc_id 是数组索引
        match self.index.doc_of(chunk_id) {
            Some(doc_id) => self.doc_bits.contains(doc_id),
            None => false,
        }
    }
}
```

**为什么不用 v0.1 的 `expand_to_chunks`（O(N) 展开）**：展开把「O(N) 次 JSON 求值」换成「O(N) 次位运算」，**O(N) 本身没消掉**，而 Q-I1 的病根正是 O(N)。惰性判定把每 query 成本降为 **O(query 词数 + 匹配文档数)**，与语料规模解耦。代价是每次 `contains` 多一次数组索引（chunk → doc），可忽略。

**构建（每 query 一次，编排层）**：

```rust
/// 构建本 query 的候选谓词。
/// `Err(FilteredOut)` 表示过滤条件排空了所有文档 —— 编排层据此短路（不进两路召回）。
fn build_predicate<'a>(index: &'a Index, filter: Option<&Filter>)
    -> std::result::Result<Box<dyn CandidateFilter + 'a>, EmptyReason>
{
    Ok(match filter {
        None => Box::new(AliveOnly::new(index.alive_chunks())) as Box<dyn CandidateFilter>,
        Some(f) => {
            let doc_bits = doc_bits(f, index);
            if doc_bits.is_empty() {
                return Err(EmptyReason::FilteredOut);   // ← 空集短路（语义见 §5.8）
            }
            let count = allowed_chunk_count(&doc_bits, index);
            Box::new(ChunkFilter { doc_bits, index, count })
        }
    })
}
```

> **契约（P1-6，必须写进 trait 的 rustdoc）**：
> **`filter = None` 表示"不过滤任何东西"，包含已软删除的条目。**
> `VectorIndex::search()` / `Retriever::search()` 的默认转发即为此语义。因此**直连向量路 / BM25 路（逃生舱、bench、外部实现）不会自动获得存活过滤**——幽灵候选在这些路径上依然存在。
> **唯一正确的用法是经编排层（`query::searcher`）调用**；直连路径若要存活语义，必须显式构造 `AliveOnly` 传给 `search_filtered`。

### 5.6 检索层 trait 演进

**BM25 路**：`Retriever` trait 增加带谓词的入口，保留 `search` 为默认转发（对既有调用点零改动，符合逃生舱精神）：

```rust
pub trait Retriever {
    fn search_filtered(&self, query: &str, k: usize,
                       filter: Option<&dyn CandidateFilter>) -> Result<Vec<Scored>>;
    fn search(&self, query: &str, k: usize) -> Result<Vec<Scored>> {
        self.search_filtered(query, k, None)
    }
}
```

`Bm25Retriever` 的实现只在 TAAT 累加循环里加一道判定（`retriever/bm25.rs:94-100`）：

```rust
for posting in self.index.postings_by_id(term_id) {
    if let Some(f) = filter {
        if !f.contains(posting.chunk_id) { continue; }   // ← 下推：评分期跳过
    }
    ...
}
```

BM25 是**全量累加 + 末端 truncate**，跳过不改变其余候选的分数 ⇒ **真下推，召回不损失**，且累加顺序不变 ⇒ 确定性保持。

> **无用户过滤时编排层给 BM25 路传 `None`（P1-4）**：`Index::remove` 已在删除时物理摘除 postings（`inverted.rs:61`），活的 postings 里不可能有死 chunk。传 `None` 可省掉热路径上每 posting 一次 `dyn contains()` 虚调用——这条路径是所有无过滤查询的必经之路，对 P99 敏感。

**向量路**：`VectorIndex` trait 同样演进（`vector/mod.rs`）：

```rust
pub trait VectorIndex: Send + Sync {
    fn add(&mut self, id: ChunkId, vec: NormalizedVector) -> Result<()>;

    /// 检索最近的 k 条**且通过 filter 的**候选；`None` 表示不过滤（含已软删除，见 §5.5 契约）。
    /// 契约：结果按平方欧氏距离升序，且**全部满足 filter**（`None` 除外）。
    fn search_filtered(&self, query: &NormalizedVector, k: usize,
                       filter: Option<&dyn CandidateFilter>) -> Result<Vec<(ChunkId, f32)>>;

    fn search(&self, query: &NormalizedVector, k: usize) -> Result<Vec<(ChunkId, f32)>> {
        self.search_filtered(query, k, None)
    }

    /// 物理条目数（含已软删除的；与存活数无关，文档注明）
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool { self.len() == 0 }
}
```

- **`BruteForceIndex`**：线性扫描时判定，**精确、零召回损失**，作为 Hnsw 的 oracle（沿用 P4 的对照思路）。
- **`HnswRsIndex`**：见 §5.7 的两条路径。

### 5.7 向量路的路径选择与 `ef` 策略（本设计的核心难点，v0.2 重写）

v0.1 只设计了一条 `search_filter` 路径，并按「凑够 ef 即停」的误读调参。v0.2 按 §3.3 的真实机制拆成**两条路径**。

```rust
const EF_SEARCH: usize = 200;              // 现状无过滤检索宽度（hnsw_rs_index.rs:21），保持不变
const EF_FILTER_FACTOR: usize = 4;         // 带过滤时的过采样倍率（待实测校准）
const EF_FILTER_MAX: usize = 256;          // 硬上限，防止延迟失控
const PHYSICAL_OVERSAMPLE_CAP: usize = 1024;   // path A 的候选上限
```

#### 路径 A：无用户过滤（`None` 或 `FilterKind::Alive`）——**热路径，必须保住现状成本结构**

```rust
// 走【普通 search()】——保留 fast-return（hnsw.rs:983-984），成本结构与现状完全一致
let alive_ratio = match filter {
    None => 1.0,
    Some(f) => f.allowed_count() as f64 / self.len().max(1) as f64,
};
// 按存活比例过采样：期望需要 k/ratio 个候选才能凑出 k 个存活项，再加 k 的余量吸收方差
let knbn = ((k as f64 / alive_ratio.max(0.01)).ceil() as usize + k)
              .min(self.len()).min(PHYSICAL_OVERSAMPLE_CAP);
let mut out = self.hnsw.search(q.as_slice(), knbn, EF_SEARCH);   // ef 仍 200
// 后置存活过滤（≤ knbn 条，成本可忽略）→ 稳定排序 → 截断 k
out.retain(|(id, _)| filter.map_or(true, |f| f.contains(*id)));
out.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
out.truncate(k);
```

**为什么不用 `search_filter` 跑 `AliveOnly`（P1-4 的要点，且比评审指出的更严重）**：

1. `search_filter` 会**禁用 fast-return**（§3.2），彻底改变热路径的成本结构——这是所有无过滤查询都要付的代价；
2. v0.1 给的 `ef = min(4k, 256) = 120` **小于现状的 200**，会静默降低向量路召回；
3. 路径 A 用「按删除比例过采样」换「召回不回退」：删除比例低时 `knbn ≈ 2k`，成本与现状几乎相同；只有重度删除场景（>50% 已删）成本才明显上升——而那正是 Step 5 的 compaction（T7-12）要解决的。

#### 路径 B：有用户过滤（`FilterKind::Filtered`）

```rust
// 走 search_filter；knbn 是输出条数旋钮，ef 只是宽度（§3.3 结论 4）
let knbn = k;
let ef = (k * EF_FILTER_FACTOR).max(k).min(EF_FILTER_MAX);   // 纯过采样宽度
let out = self.hnsw.search_filter(q.as_slice(), knbn, ef, Some(&adapter));
```

**v0.2 已删除 v0.1 的 `.min(allowed.max(k))`**——评审 P1-1 的论证成立：当 `allowed < ef` 时结果堆恒不满，距离剪枝**本就关闭**，`ef` 取 `allowed` 还是 `4k` 都是整图遍历，夹逼无效；当 `allowed ≥ 4k` 时该 `min` 又不生效。**该项是无效项，删掉并如实说明。**

行为与成本表（**成本栏是 v0.2 新增的诚实标注**）：

| 选择度 | `allowed` vs `ef` | 返回 | **真实代价** |
| --- | --- | --- | --- |
| 高（`allowed ≥ 4k`） | 堆可填满 `4k` | top-k（ANN 近似） | 堆满后剪枝生效 ≈ 常规 ef 搜索；因无 fast-return，比无过滤多耗一段尾部开销 |
| 中（`k ≤ allowed < 4k`） | 堆**填不满** | top-k（ANN 近似，可能少于 k） | ⚠️ **整图遍历**（`allowed < ef` ⇒ 剪枝全程关闭） |
| 极低（`allowed < k`） | 堆**填不满** | 全部 `allowed` 条（< k） | ⚠️ **整图遍历**；保证不漏，但延迟随语料规模线性上升 |

> 「中档 ⇒ 近似精确」的**真实代价就是整图遍历**——它之所以"精确"，恰恰是因为它把全图都走了一遍。v0.1 未标注此成本，属于误导。

**诚实声明**：低选择度 + ANN 的延迟与召回无法两全，且这是 `hnsw_rs` 的结构性行为（§3.3 结论 3），不是 tuning 能解决的。V2.0 的选择是——**正确性优先（绝不返回已被删除或不匹配的候选），召回不足时降级返回少于 k 条，缺口暴露到 `Metrics::vector_shortfall`，并在 bench 里给出「选择度 × 延迟 × 召回」三元数据**，作为 V2.1 是否引入 prefilter 结构的依据。

### 5.8 编排层改造（`query/searcher.rs`）

```
1. filter 求值 → Result<Box<dyn CandidateFilter>, EmptyReason>   // 空集 → 短路
2. 组合谓词：有 filter 用 ChunkFilter；否则用 AliveOnly
   （BM25 路：无用户过滤传 None —— postings 已物理摘除死 chunk）
3. 两路召回传谓词（下推；Hybrid 仍 rayon::join 并行）
4. 融合（删除原 1.5 节的 retain 逻辑）
5. take(k) → 回捞 → explain → 精排
```

#### 5.8.1 `EmptyReason` 语义（P1-5 修正）

**v0.1 的两处错误**：① 写了不存在的 `NoMatch`（`EmptyReason` 只有 `NoDocuments` / `AllTermsUnmatched` / `FilteredOut` 三变体，`response.rs:76-83`）；② 「`doc_bits` 空 ⇒ 无条件 FilteredOut」会造成语义迁移——现状（`searcher.rs:171-177`）要求 `pre_filter_candidates > 0`，即"query 本来有候选、被 filter 过滤光"才报 `FilteredOut`；query 本身没命中时报 `AllTermsUnmatched`。下推后 lane 结果已经是过滤后的，这个信号丢失了。

**v0.2 解法：引入 `query_has_hits` 词典探针。** `Index::term_id`（`index/mod.rs:237`）+ `postings_by_id`（`:242`）可直接判断 query 词是否命中词典（postings 在删除时已物理摘除，因此"有 postings"即"有活候选"）：

```rust
fn query_has_hits(index: &Index, analyzer: &dyn Analyzer, query: &str) -> bool {
    analyzer.analyze_query(query).iter()
        .filter_map(|t| index.term_id(&t.term))
        .any(|id| !index.postings_by_id(id).is_empty())
}
```

成本 O(|query 词数|) 次哈希查找 + `is_empty`，可忽略。

**判定规则（query 侧信号优先——对 Agent 更可操作：「你的词是幻觉词」比「你的过滤太窄」更有指导性）**：

```
if fused.is_empty() {
    reason =
        if index.num_chunks() == 0            -> NoDocuments
        else if query_is_empty || !query_has_hits -> AllTermsUnmatched
        else if filter.is_some()              -> FilteredOut      // query 有命中，但过滤后排空
        else                                  -> AllTermsUnmatched
}
```

该规则下现有测试 `过滤后是子集且全过滤触发FilteredOut`（`searcher.rs:493`；query 有命中、过滤条件永不匹配）仍然返回 `FilteredOut` ✅，行为兼容。原因矩阵由 **T16** 钉死。

### 5.9 可观测性（`query/metrics.rs`，NFR-07）

```rust
pub struct Metrics {
    ... 现有字段 ...
    /// 过滤求值 + 谓词构建耗时（O(匹配文档数)，不再有 O(N) 展开）
    pub filter_eval: Duration,
    /// 通过过滤的候选总数（CandidateFilter::allowed_count）
    pub allowed: usize,
    /// 向量路"想取 k 条、实得 n 条"的缺口（暴露 filtered-ANN 降级）
    pub vector_shortfall: usize,
}
```

### 5.10 快照：格式不变（重要边界）

`FORMAT_VERSION` 保持 2，**不新增 section**：

- 存活位图：`Index::import` 从 `chunks: Vec<Option<Chunk>>` 重建（O(N)）。
- `doc_chunk_count`：同一趟扫描内重建。
- 字段索引：`Index::import` 从 `docs` 的 metadata 重建（O(N)）。

好处：与 Step 2 / Step 3 的 ADR-011（原 ADR-A：图持久化格式 + 多文件原子性）**完全解耦**，两边可独立推进。代价是加载多一次 O(N) 扫描——相对现有的向量重灌（NFR-04 的 11.57s 大头）可忽略。

**`save()` 不清理已删向量**（体积问题归 Step 5 T7-12），但要在 `search/index.rs:250-252` 那条**已经不成立的注释**改写为真实语义，避免继续误导。

---

## 6. 决策记录

| 编号 | 决策 | 结论与理由 |
| --- | --- | --- |
| **D-S1-01** | 存活状态归属 | ✅ 只住 `Index.forward`（单一真源），以位图谓词传给向量路；`VectorIndex` **不**加 `tombstone`。理由见 §4.3（避免双写漂移 + 快照重灌冲突） |
| **D-S1-02** | 位图实现（v0.2 修订） | ✅ 自研 `Bitmap`（`Vec<u64>`），不引 `roaring`。理由：ID 稠密连续、内存小、`count_ones()` O(1)、集合运算与迭代序无关。**已撤回 v0.1 的「现状 HashSet 威胁 NFR-06」论据——该论据不成立**（P2-1：`allowed` 只用于 `retain`，保序） |
| **D-S1-03** | 过滤下推粒度 | ✅ **doc 级字段索引 + chunk 级惰性判定**（v0.2 改，见 D-S1-09） |
| **D-S1-04** | 字段索引类型覆盖（v0.2 新增） | ✅ **全类型**经 `json_to_string` 登记进 `terms`，数值额外进 `numbers`。理由：与 `matches()`（`filter.rs:22-25`）逐类型等价，避免 bool/null/object/array 的静默假阴性（P1-2） |
| **D-S1-05** | 向量路两条路径（v0.2 重写） | ✅ 无用户过滤走**普通 `search()` + 按存活比例过采样 + 后置过滤**（保 fast-return，成本结构不变）；有用户过滤走 `search_filter`，`ef = min(max(4k, k), 256)` 纯宽度，**不夹逼 `allowed`**（P1-1 / P1-4） |
| **D-S1-06** | 低选择度取舍 | ✅ 正确性优先，允许返回少于 k 条并记 `vector_shortfall`；不引入 prefilter 结构（V2.1 按 bench 数据复议） |
| **D-S1-07** | 快照格式 | ✅ **不变**（FORMAT_VERSION 仍为 2），两个新索引在 `import` 时重建；与 Step 2/3 的 ADR-011 解耦 |
| **D-S1-08** | `allowed_chunks` / `matches` 去留 | ✅ **保留**，降级为 oracle（等价性测试与 bench 对照基准），移出查询路径 |
| **D-S1-09**（v0.2 新增） | 谓词求值方式 | ✅ **惰性 doc 判定**（`alive(chunk) && doc_bits[doc_of(chunk)]`），**不做 O(N) chunk 位图展开**。理由：展开只把 O(N) 次 JSON 求值换成 O(N) 次位运算，O(N) 未消除；惰性判定使每 query 成本降为 O(匹配文档数)，与语料规模解耦 |
| **D-S1-10**（v0.2 新增） | `None` 的语义契约 | ✅ `None = 不过滤任何东西（含已软删除）`，写进 trait rustdoc。直连向量/BM25 路（逃生舱、bench）**不自动获得存活过滤**，必须经编排层或显式构造 `AliveOnly`（P1-6） |
| **D-S1-11**（v0.2 新增） | 空结果原因判定 | ✅ 引入 `query_has_hits` 词典探针，**query 侧信号优先**于 `FilteredOut`，保持与现状行为兼容（P1-5） |
| **D-S1-12**（v0.2 新增） | 模块分层 | ✅ `bitmap.rs` / `predicate.rs` 为无依赖叶模块；`vector`、`retriever` **只依赖 `predicate`**，不 import `query::filter` 或 `index`（P2-2） |
| **ADR-010** | 向量软删除 + 过滤下推 | ✅ **定案拆分为 ADR-010（软删除 + 下推，本 Step 1）+ ADR-011（图持久化，原 ADR-A，Step 2）**。理由（采纳评审意见）：两者决策域、生效步骤、依赖（H1 多文件原子性）完全独立，一事一议符合本仓 ADR 风格。评审通过后回写 `architecture-design.md` §9.4 |

---

## 7. 影响面与兼容性

| 方面 | 影响 |
| --- | --- |
| 对外 API | `SearchIndex` / `Searcher` / `SearchRequest` 签名**零变化**；`Filter` 枚举零变化 |
| 底层 trait | `Retriever`、`VectorIndex` 各新增一个带谓词的方法（旧方法有默认转发，既有调用点不改） |
| 快照兼容 | 格式不变，旧快照可直接加载 |
| 确定性 NFR-06 | 位图集合运算与顺序无关；BM25 累加顺序不变；向量路返回前做稳定排序（距离 → chunk_id）。风险低 |
| 并发 | `CandidateFilter: Send + Sync`；位图只读共享，`rayon::join` 场景无新增锁 |
| 内存 | 存活位图 N/64 字节；字段索引受基数保护（`terms+numbers` 合计），上界 ≈ 字段数 × 1024 × N/8 字节（最坏），实测记入 NFR-05 |
| **热路径延迟**（v0.2 新增） | 无过滤路径保留 `search()` 的 fast-return 与 `EF_SEARCH=200`，成本结构不变；仅多出「按存活比例过采样」的 `knbn` 与一次 ≤ knbn 条的 `retain`（可忽略）。**这是 D-S1-05 path A 的设计目标，由 T14 强制验证** |
| CLI | `--filter` 已是 `field=value` 的 And 组合（`main.rs:118-129`），本次受益于下推，**无需改动**；建议在 `--verbose` 下输出 `allowed` / `vector_shortfall` |

---

## 8. 测试计划

| # | 测试 | 位置 | 断言要点 |
| --- | --- | --- | --- |
| T1 | 已删向量不霸占 Top-K 名额 | `tests/integration.rs` | 30 篇删 20 留 10 / K=5（**存活数 = 2×K**，规避 HNSW 近似误差 R17），Brute + Hnsw 两后端；**核心断言 `hits.len() == k`**（§2.1 澄清：幽灵进不了 `hits`，危害是占名额）。另附三模式 soundness。**反向验证**：摘掉向量路谓词后必须失败（实测 5 → 2 条） |
| T2 | 跨快照永续 | `tests/integration.rs` | 走 storage 层直接落盘（**绕过 `SearchIndex::remove` 对 `raw_vectors` 的摘除**），快照里带着幽灵向量 → `load` → 断言 `hits.len() == k`。覆盖 §2.2 的防线 ② |
| T3 | 删除后重新 upsert 可召回 | `index/mod.rs` 单测扩展 | 现有 `删除后可重新upsert` 扩到向量路 |
| T4 | 位图基础 | `bitmap.rs` 单测 | set/clear/union/intersect/count/边界（越界、扩容、幂等）；**`debug_assert` 覆盖 `count` 与 `count_ones_slow()` 一致性** |
| T5 | 存活位图与正排一致 | `index/forward.rs` 单测 | 随机增删后，`alive` 与 `chunks` 的 Some/None **逐位一致**（性质测试） |
| T6 | 字段索引等价性（**扩类型覆盖**） | `index/field_index.rs` 单测 | 随机 metadata × 随机 filter：`doc_bits()` == `doc_bits_scan()`。**metadata 生成器必须覆盖 string / number / bool / null / object / array / 混合类型**（P1-2）；含 `2021.0` vs `"2021"` 的反直觉案例 |
| T7 | 字段索引增量 = 全量重建 | `index/mod.rs` 单测 | 加 N 篇删 M 篇后的字段索引，与只含存活文档重建的索引一致 |
| T8 | 下推正确性（**拆两条**，P1-3） | `query/searcher.rs` 单测 | ① **soundness**：下推结果全部通过 filter 且不含已删 chunk；② **recall**：`无过滤结果 ∩ allowed ⊆ 下推结果`。BM25 / Brute **严格成立**；Hnsw 以现状 post-filter 为基线不回退 |
| T9 | 低选择度召回不回退 | `query/searcher.rs` 单测 | 0.1% / 1% / 10% 三档，返回条数 ≥ post-filter 基线 |
| T10 | 空集短路 | `query/searcher.rs` 单测 | 过滤条件无匹配 → `FilteredOut`，且 `allowed == 0`（不进两路召回） |
| T11 | 确定性 | `query/searcher.rs` 单测 | 删除 + 过滤场景连续 100 次结果一致（现有 `连续100次结果一致` 扩写） |
| T12 | 向量路 `ef` 策略 | `vector/hnsw_rs_index.rs` 单测（`#[ignore]`，release 跑） | 不同选择度下的返回条数与耗时，输出「选择度 × 延迟 × 召回」三元数据；**验证高/中选择度确实按预期进入/不进入整图遍历** |
| T13 | 性能对照 | bench | `doc_bits()` + 惰性谓词 vs `allowed_chunks()` 构建耗时（1 万 / 10 万） |
| T14 | **NFR-02 重测（含热路径保护）** | `make eval-perf` | ① 无过滤路径 P99 ≤ 现状 × 1.10 且 Top-10 召回重合率 ≥ 0.99；② 带过滤路径各选择度 P99 入库（P1-4） |
| T15 | 字段索引 remove 时序（P2-5 新增） | `index/mod.rs` 单测 | 断言 `field_index.remove()` 在 `tombstone_doc()` **之前**执行：删完后该 doc 的位图**已从索引中清除**（若时序颠倒则 metadata 已不可读 → 索引残留） |
| T16 | 空结果原因矩阵（P1-5 新增） | `query/searcher.rs` 单测 | 2×3 矩阵：{query 有命中 / query 无命中} × {无 filter / filter 有匹配 / filter 无匹配} → 断言 `NoDocuments` / `AllTermsUnmatched` / `FilteredOut` 各就各位。**⚠️ 实现注意**：空集短路分支与 `fused.is_empty()` 分支必须**同口径**跑一次 `query_has_hits` 探针，否则「query 无命中 + 过滤排空」会误报 `FilteredOut`（本测试正是这样抓到该不一致的） |
| T17 | 直连路径契约（P1-6 新增） | `vector/*` 单测 | 显式文档化并断言：`search_filtered(q, k, None)` 会**返回已软删除的** chunk；而经编排层或传 `AliveOnly` 则不会 |

> T12 / T13 的 10 万级 fixture 需另行准备（`data/` 现仅 12K 段落 + T2Ranking 原始集），列为本 Step 的前置准备项（见未决问题 4）。

---

## 9. 实施任务拆分

> 依赖顺序自左向右；S1-01~05 是数据层（可独立测），S1-06~08 是检索层，S1-09 收口。

| # | 任务 | 依赖 | 量级 |
| --- | --- | --- | --- |
| S1-01 ✅ | `bitmap.rs` 位图模块（含 `count` 维护契约 + `debug_assert`）+ 单测（T4，13 项） | — | S |
| S1-02 ✅ | 存活位图 + `doc_of` + `doc_chunk_count` 接入 `ForwardStore` / `Index` + `import` 重建 + 单测（T5） | S1-01 | M |
| S1-03 ✅ | 字段索引 `field_index.rs`：**全类型登记**（D-S1-04）+ 基数保护（合计）+ **五个同步点**（add/remove/import/export/Default）+ 单测（T6/T7/T15） | S1-01 | M |
| S1-04 ✅ | `predicate.rs` 叶模块（`CandidateFilter` / `FilterKind` / `AliveOnly` / `ChunkFilter` 惰性判定）+ `doc_bits()` 求值器 + `allowed_chunk_count` + 单测 | S1-02/03 | M |
| S1-05 ✅ | 快照重建与 `save/load` 语义修正（改掉失效注释）+ 集成测试（T2） | S1-02/03 | S |
| S1-06 ✅ | `Retriever` trait 演进 + BM25 下推（无过滤传 `None`）+ 单测（T8/T11） | S1-04 | S |
| S1-07 ✅ | `VectorIndex` trait 演进（含 `None` 契约 rustdoc）+ Brute 下推 + 单测（T17） | S1-04 | S |
| S1-08 ✅ | HnswRs 下推：`FilterT` 适配器 + **双路径**（path A 过采样 / path B `search_filter`）+ 单测（T12 `#[ignore]`） | S1-07 | **L** |
| S1-09 ✅ | 编排层 `search_parts` 改造（短路 / `query_has_hits` 探针 / `FilteredOut` 语义 / Metrics）+ 集成测试（T1/T2/T16，另修 F2） | S1-06/08 | M |
| S1-10 ✅ | fixture（10 万级，确定性合成生成器）+ bench 选择度曲线 + NFR-02 重测（T12/T13/T14） | S1-09 | M |
| S1-11 ✅ | 文档回写：ADR-010、架构 §5.4/§5.5/§7.5/§7.6/§8.3/§8.4、风险表 R11~R18、CHANGELOG、`plan-v2` 进度 | S1-10 | S |

**守门状态（S1-01 ~ S1-11 全部完成，2026-09-05）**：`cargo fmt --check` ✅ /
`cargo clippy --workspace --all-targets -D warnings` ✅ / `cargo test --workspace` ✅ /
MSRV 1.90 构建 ✅ / `RUSTDOCFLAGS=-D warnings cargo doc` ✅ / `--no-default-features` ✅ /
`--features charabia` ✅ / `cargo-deny` ✅（依赖零改动）。

**建议开工顺序**（评审意见已采纳：S1-02/03 合并验收后再动 S1-06）：

```
S1-01 → S1-02 → S1-03 → (S1-02/03 合并验收) → S1-04 → S1-06 → S1-07 → S1-05 → S1-08 → S1-09 → S1-10
```

即：**先建数据层（位图 + 字段索引）并合并验收，再打通下推，最后做 Hnsw 双路径与编排层**。S1-08 的 `ef` 策略**必须在本设计定稿后按 §5.7 一次性实现，不得边写边改策略**（评审意见）。

---

## 10. 风险与未决问题

| 编号 | 风险 | 影响 | 应对 |
| --- | --- | --- | --- |
| **R11** | filtered-ANN 低选择度召回不足（`hnsw_rs` 结构性行为，§3.3 结论 3） | 低选择度过滤查询返回少于 k 条 | 过采样 + `ef` 策略（§5.7 path B）；`Metrics::vector_shortfall` 暴露；bench 出三元数据；V2.1 复议 prefilter 结构 |
| **R12** | 字段索引与正排 metadata 漂移 | 过滤结果静默错误 | 单一入口维护 + `import` 重建 + T6/T7 等价性性质测试 + T15 时序测试 |
| **R13**（v0.2 改写，v1.0 由 R18 接管） | `search_filter` **在中/低选择度下必然整图遍历**（堆填不满 ⇒ 剪枝全程关闭，`hnsw.rs:1019`）。这是库层机制，**不是参数问题** | 带过滤 P99 随语料规模线性上升 | path B 的 `EF_FILTER_MAX=256` 只限制高选择度的宽度；中/低选择度无法避免——**接受并测量**（T12 三元数据，10 万级实测见 **R18**）；V2.1 复议 prefilter |
| **R14** | 字段索引内存（高基数字段） | 内存膨胀 | 基数保护（对 `terms+numbers` **合计**生效）；做成可配置；实测记入 NFR-05 |
| **R15**（v0.2 新增） | 无过滤热路径退化：`search_filter` 会禁用 fast-return，且 v0.1 的 `ef=120 < 200` 会静默降召回 | 所有无过滤查询的 P99 与召回受影响 | D-S1-05 **path A** 走普通 `search()` 保住 fast-return 与 `EF_SEARCH=200`；T14 强制验证 P99 与召回重合率 |
| **R16**（v0.2 新增） | 直连向量路 / BM25 路绕过存活过滤（契约 `None = 不过滤`） | "Q-C1 已修复"在这些路径上被绕过而不自知 | D-S1-10 契约写进 rustdoc；T17 显式断言；`VectorBackend::Brute` 逃生舱文档标注 |
| **R17**（v0.3 新增，F3） | **`hnsw_rs::search` 有固有近似误差**：即使 `knbn == len`、`ef=200` 也可能少返回（20 点图 200 次采样：191 次满、8 次少 1、1 次少 2）。**这是库层行为，不是过采样参数问题**，path A / path B 均受影响 | ① 存活过滤后仍可能凑不满 k；② 任何对"返回条数"的严格断言都可能 flaky | ① 过采样已含 `+k` 方差余量；② 缺口由 `Metrics::vector_shortfall` 暴露（T12 三元曲线量化）；③ **验收测试配比须让存活数 ≥ 2×K**（T1/T2 已改，50 次 0 失败）；④ S1-10 已在 10 万级语料上测出该误差的真实量级：分路重合率 1.0000、平均条数 10.00，**在 K=10 下未观测到由 R17 造成的可见缺口** |
| **R18**（v1.0 新增，S1-10 实测） | **低选择度过滤查询的延迟爆炸**：10 万级语料上，选择度 1% → vector P99 **41.33ms**（无过滤 5.81ms 的 7.1×），选择度 0.1% → **158.45ms**（**27.3×**），降级字段 Range → **124.26ms**（21.4×）。机理是 R13 的整图遍历，但**量级是「规模 × 选择度」的复合，不是单纯的线性随规模** | 带过滤查询在选择度 ≤1% 时**违反 NFR-02**（限额 10/20ms，超 4~16×），10 万级上不可用于在线路径 | ① **V2.0 接受**：正确性优先于延迟（项目质量属性优先级），且优化前的 post-filter 在该选择度下几乎返回不了任何结果；② 已由 `Metrics::vector_shortfall` 与 bench 三元数据暴露；③ **V2.1 必须解决**，最廉价且已被数据支持的方案是 **allowed 很小时绕开 ANN 直接对 allowed 集合做暴力扫描**（10 万级 0.1% 档位 allowed=100，暴力扫描代价约 100 次点积 ≈ 0.05ms，相比 158ms 是 3000× 的改进）——该方案**本轮刻意不实现**，因为 S1-10 的验收目标是「让数据说话」，实现决策归 V2.1 |

**未决问题（评审已给倾向，待确认）**

1. **ADR 编号拆分**：✅ **评审支持拆分，本版已定案** —— ADR-010（软删除 + 下推，Step 1）+ ADR-011（图持久化 = 原 ADR-A，Step 2）。理由：决策域、生效步骤、依赖（H1 多文件原子性）完全独立。
2. **基数阈值**：✅ **已解决** —— `FieldIndex::with_max_values` 原有但**只有库内可达**（调用方无从设置）。
   S1-10 新增 `Index::with_max_values_per_field(n)` 把阈值下沉到 `Index`。
   ⚠️ 该阈值**不随快照持久化**（字段索引不入快照，`import` 时用 `FieldIndex::new()` 重建），
   因此加载旧快照后调用方需重新设置。
3. **低选择度逃生开关**：✅ **采纳评审建议，V2.0 不加显式开关** —— `VectorBackend::Brute` 已是逃生舱；真正缺的是**数据**，让 `vector_shortfall` + bench 三元曲线说话，V2.1 再按数据复议 prefilter 结构。
4. **10 万级 fixture**：✅ **已交付** —— `scripts/gen_synth_corpus.py`（确定性合成，固定 seed）+ `scripts/eval_filter.sh`（一键扫描 8 档位 × 3 模式）。详见 §10.1。
5. **filter-then-fuse 还是 fuse-then-filter**？（v1.0 新增，由 T12 实测催生）
   当前实现是 **filter-then-fuse**（两路各自先过滤，再对过滤后的名次做 RRF）；
   oracle 用的是 **fuse-then-filter**（先按无过滤名次融合取 Top(depth) 再过滤）。
   二者在 10 万级上的 Top-10 重合率只有 0.685~0.948，即**约 1/3 的结果条目不同**。
   机制清楚（过滤会重排路内名次，而 RRF 只吃名次），但**哪种相关性更好没有结论** ——
   现有 fixture 的 relevance 针对全库标注，过滤后可用正例被机械稀释（10 万级 sel-0.1% 档位
   的 MRR 只有 0.02），无法用于判定。**需要选择度专用的相关性评测集，列 V2.1。**

### 10.1 S1-10 实测结果（T12 / T13 / T14）

**工具链**

| 脚本 | 作用 |
| --- | --- |
| `scripts/gen_synth_corpus.py` | 确定性合成 fixture。固定 seed；主题化文本聚类（20 主题）；**选择度档位与内容正交**（见下）；产出 `corpus` / `queries` / `filters.json`（含自检） |
| `scripts/eval_filter.sh` | 一键扫描：构建快照 → 逐档位跑 bench → 汇总三元数据 → 判定 NFR-02 |
| `helix bench --filter <spec> --filter-cost` | 新增：过滤档位 + 求值耗时对照（T13）+ oracle 重合率 |

> ⚠️ **fixture 设计踩过的坑（值得记住）**：首版选择度档位用等距抽样（步长 100），而主题按 `i % 20`
> 分配 —— **100 与 20 不互质，allowed 文档全部落在同一主题**，其他主题的 query 返回 0 条，
> 平均条数只有 1.83。档位必须与内容正交，否则"过滤下推"看起来像"检索坏了"。
> 修法：档位步长取 `1009 / 101 / 11`（与 20 互质的素数），实测条数回到 7~10。

#### T14① NFR-02 重测 —— **达标**

无过滤档位（`--levels none`），基线取自 `eval-report.md` 8.1（P5 实测）：

| mode | 基线 P99 | 限（×1.10） | 1 万级 P99 | 10 万级 P99 | 判定 |
| --- | --- | --- | --- | --- | --- |
| bm25 | 3.90 ms | 4.29 ms | 0.42 ms | **2.43 ms** | ✅ |
| vector | 8.37 ms | 9.21 ms | 3.54 ms | **5.81 ms** | ✅ |
| hybrid | 8.70 ms | 9.57 ms | 3.46 ms | **3.46 ms** | ✅ |

**path A（无过滤走普通 `search()` 保 fast-return）的设计目标达成，R15 已验证。**
10 万级相对 1 万级 P99 上升 1.4~5.8×，仍在限额内 —— 说明 path A 的 per-query 成本随规模
增长平缓，无过滤热路径**没有**因 Step 1 的改动而退化。

#### T12 三元数据 —— 选择度 × 延迟 × 召回

**A. 1 万级**（release，K=10，60 query × 10 reps）

| 档位 | 选择度 | bm25 P99 | vector P99 | hybrid P99 | 平均条数(vector) | 重合率(hybrid) |
| --- | --- | --- | --- | --- | --- | --- |
| none | 1.0 | 0.42 | 3.54 | 3.46 | 10.00 | — |
| sel-10% | 0.1 | 0.22 | 8.24 | 7.66 | 10.00 | 0.8850 |
| tenant-10% | 0.1 | 0.21 | 6.00 | 5.83 | 10.00 | 0.8833 |
| tier-5% | 0.05 | 0.18 | 6.94 | 7.95 | 10.00 | 0.9483 |
| score-range | 0.1 | 0.44 | 9.69 | 10.02 | 10.00 | 0.8617 |
| sel-1% | 0.01 | 0.18 | 10.73 | 10.47 | 10.00 | 0.9350 |
| sel-0.1% | 0.001 | 0.07 | 11.56 | 11.98 | 10.00 | 1.0000 |
| ts-range-degraded | 0.001 | 0.51 | 10.01 | 18.99 | 10.00 | 0.9883 |

**B. 10 万级**（release，K=10，200 query × 5 reps） —— **R13 的真实量级在此揭晓**

| 档位 | 选择度 | allowed | bm25 P99 | vector P99 | hybrid P99 | vector P99 ÷ 无过滤 |
| --- | --- | --- | --- | --- | --- | --- |
| none | 1.0 | 100 000 | 2.43 | 5.81 | 3.46 | 1.0× |
| sel-10% | 0.1 | 10 000 | 0.50 | 7.91 | 7.75 | **1.4×** |
| sel-1% | 0.01 | 1 000 | 0.32 | 41.33 | 41.30 | **7.1×** |
| sel-0.1% | 0.001 | 100 | 0.25 | 158.45 | 144.87 | **27.3×** |
| ts-range-degraded | 0.001 | 100 | 8.26 | 124.26 | 121.87 | **21.4×** |

**三条结论**

1. **R13 的量级远超 v0.2 的预估，且是本次收尾最重要的发现。** v0.2 写的是
   「带过滤 P99 随语料规模线性上升」，实测是**规模 × 选择度双重复合**：
   1 万级下带过滤最差 11.56ms（3.3×），10 万级下最差 **158.45ms（27.3×）**。
   `EF_FILTER_MAX=256` 对中/低选择度**完全无效** —— 因为它只约束搜索宽度，
   而瓶颈是「要在一整张图里把 100 个 allowed 节点找出来」，与 ef 无关。**升级为 R18。**
2. **⚠️ 带过滤的 vector / hybrid 在选择度 ≤1% 时直接违反 NFR-02**（限额 10 / 20 ms，
   实测 41 / 158 ms，超 **4~16×**）。这不是 bug —— 是**正确性换延迟的显式取舍**：
   优化前的 post-filter 在 0.1% 选择度下几乎返回不了东西（Q-I2 的原始病症），
   现在能稳定返回满 10 条。按项目的质量属性优先级
   （正确性 > 首条精确率 > 延迟 P99 > 召回率），这个方向是对的，
   **但必须记为已知限制，不得当作"过滤功能已可用"交付**。
3. **bm25 路反而是反向的**：带过滤后 P99 从 2.43ms 降到 0.25~0.50ms（候选变少），
   唯一例外是 `ts-range-degraded` 的 **8.26ms** —— 那是降级字段的 O(N) 全扫，
   比 bm25 无过滤检索本身（2.43ms）还贵 3.4 倍。**这是评审 P2-2 在 10 万级上的直接证据。**

**关于 hybrid 重合率 0.685~0.948（补充修正 v0.3 的解读）**

v0.3 写「缺口全部来自向量路」，这个归因**是错的**，实测推翻了它：

| 档位 | bm25 重合率 | vector 重合率 | hybrid 重合率 |
| --- | --- | --- | --- |
| sel-10% | 1.0000 | 1.0000 | **0.6850** |
| sel-1% | 1.0000 | 1.0000 | **0.8410** |
| sel-0.1% | 1.0000 | 1.0000 | **0.9280** |
| ts-range-degraded | 1.0000 | 1.0000 | **0.9430** |

**两路各自与自己的 oracle 重合率都是 1.0000，只有融合后掉下来** ⇒ 分歧**不可能**
来自任何一路的过滤下推，**只能**诞生于融合这一层。机制是清楚的：

- oracle 的做法是 **fuse-then-filter**：先按无过滤的 RRF 分数取 Top(depth)，再过滤
- 实际实现是 **filter-then-fuse**：两路各自先过滤，再对**过滤后的排名**做 RRF

过滤会**重排每一路内部的名次**（第 47 名变成第 3 名），而 RRF 只吃名次，
两种顺序的 RRF 分数自然不同 ⇒ Top-10 不同。这是**语义差异，不是召回损失**：
两路都返回满 10 条且全部满足过滤条件（T8 soundness），只是排序不同。

> 因此 **hybrid 的重合率不能当作召回缺口指标读**，它衡量的是
> 「filter-then-fuse 与 fuse-then-filter 的排序一致性」。
> 真正的召回判据是**分路重合率（1.0000）+ 返回条数（10.00）**，两者都满分。
> 至于两种融合顺序哪种相关性更好，需要带相关性标注的选择度专用评测集才能判定 ——
> 现有 fixture 的 relevance 是对全库标注的，过滤后可用正例被机械稀释，无法用于此判定。**列为 V2.1 未决问题。**

#### T13 过滤求值耗时对照 —— **未降级字段优化显著，降级字段收益为 0**

1 万级语料，release，60 query × 20 reps，仅计求值（不含检索）。

**A. 未降级字段（`sel_01=hit`，2 个取值）—— 优化生效**

| 路径 | 平均(ms) | P99(ms) |
| --- | --- | --- |
| `allowed_chunks`（旧·全扫 + HashSet） | 0.2221 | 0.4422 |
| **`doc_bits` + 惰性谓词（新）** | **0.0005** | **0.0014** |
| `doc_bits_scan`（降级字段才会走） | 0.2098 | 0.2305 |
| **加速比** | **471.8×** | |

**B. 降级字段（`ts_ms` Range，高基数）—— 收益为 0**

| 路径 | 平均(ms) | P99(ms) |
| --- | --- | --- |
| `allowed_chunks`（旧·全扫 + HashSet） | 0.3879 | 0.5875 |
| `doc_bits` + 惰性谓词（新，**内部回落全扫**） | 0.4106 | 0.6471 |
| `doc_bits_scan`（降级字段实际路径） | 0.3806 | 0.5250 |
| **加速比** | **0.9×** | |

> ⚠️ **这就是评审 P2-2 指出的问题，现已量化**：高基数 Range 是最常见的真实过滤场景
> （毫秒时间戳 / 雪花 ID），但字段索引对它**永久降级**，新旧两条路径都退化为全扫，
> 加速比 **0.9×**（新路径还多一层「查索引发现降级 → 回落」的开销，故略慢于 1.0×）。
> **Q-I1 对该场景的收益为 0，且不会比优化前更差** —— 正确性完好，性能原地踏步。
>
> 撞线比直觉更快：数值字段会**同时**登记 terms 键与 numbers 键，`value_count` 是两者合计，
> 因此毫秒时间戳约 **512 篇**就降级（不是 1024）。
>
> 这构成 V2.1 prefilter / 排序列方案的**第一用例**，而非泛泛的 tag 等值过滤。

---

## 附录 A：新增/变更 API 一览

```rust
// 新叶模块（无 crate 内依赖，除 types / bitmap）
crate::bitmap::{Bitmap, ChunkBits, DocBits}
crate::predicate::{CandidateFilter, FilterKind, AliveOnly, ChunkFilter}

// 现有文件扩展（依赖 Index）
crate::query::filter::{doc_bits, doc_bits_scan, allowed_chunk_count, matches, allowed_chunks}
crate::index::field_index::FieldIndex

// Index 新增
Index::alive_chunks(&self) -> &ChunkBits
Index::alive_count(&self) -> usize
Index::doc_of(&self, chunk_id: ChunkId) -> Option<DocId>
Index::field_index(&self) -> &FieldIndex          // 诊断/测试用

// ForwardStore 内部（不对外）
alive: ChunkBits / doc_chunk_count: Vec<u32> / rebuild_alive()

// trait 演进
Retriever::search_filtered(&self, query, k, Option<&dyn CandidateFilter>)
VectorIndex::search_filtered(&self, query, k, Option<&dyn CandidateFilter>)
Bm25Retriever::search_filtered / BruteForceIndex::search_filtered / HnswRsIndex::search_filtered

// Metrics 新增
filter_eval: Duration, allowed: usize, vector_shortfall: usize
```

## 附录 B：`hnsw_rs-0.3.4` 源码证据索引（v0.2 重新精确定位）

> 核对环境：`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/hnsw_rs-0.3.4`，版本由 `Cargo.lock` 锁定。

| 结论 | 源码位置 |
| --- | --- |
| `DataId = usize`，即 insert 时传入的 id | `src/hnsw.rs:50` |
| `FilterT::hnsw_filter(&self, id: &DataId) -> bool`；为 `Vec<usize>` 与 `Fn(&DataId)->bool` 提供 blanket impl | `src/filter.rs:6-23` |
| `search_filter(data, knbn, ef_arg, Option<&dyn FilterT>)` | `src/hnsw.rs:1475-1481` |
| `search_layer` 的循环入口（唯一正常终止 = 候选耗尽） | `src/hnsw.rs:960` |
| **无 filter 时 fast return；有 filter 时只 `retain` 不 return** | `src/hnsw.rs:983`（分支）、`984`（return）、`985-992`（retain，fall-through） |
| **堆未满时距离剪枝关闭**（整图遍历的根因） | `src/hnsw.rs:1019`（`e_dist_to_p < f_dist_to_p \|\| return_points.len() < ef`） |
| 守卫分支：堆被清空则 return | `src/hnsw.rs:1011-1014`（清空动作在 `1036`） |
| 被过滤节点仍进候选队列（连通性不破） | `src/hnsw.rs:1026-1027`（`candidate_points.push` 先于 `1032` 的 filter 判断） |
| 被过滤节点不进结果堆 | `src/hnsw.rs:1028-1041` |
| 结果堆上限 `ef`（超出则 pop 最远） | `src/hnsw.rs:1042-1044` |
| `ef` 恒 ≥ `knbn` | `src/hnsw.rs:1519`（`let ef = ef_arg.max(knbn)`） |
| 外层截断 `min(knbn, ef)` 后**冗余再过滤一次** | `src/hnsw.rs:1535`（`last`）、`1537-1553`（过滤） |
| `parallel_insert` 需配合 `set_searching_mode(true)` 才能搜索（**Step 2 用，提前记录**） | `src/hnsw.rs:825-832` |
