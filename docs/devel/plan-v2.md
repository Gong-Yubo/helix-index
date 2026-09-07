# HelixIndex V2 开发计划（plan-v2）

> 面向 Agent 场景的通用检索引擎内核 —— **V2（下一个版本）的开发计划**。
> 本文件回答：V2 要做什么、为什么、按什么步骤做、每一步做到什么程度算完成。

| 项 | 内容 |
| --- | --- |
| 版本 | **v0.4（已定稿；2026-09-07 复审后重排，Step 3 起重新编号）** |
| 日期 | 2026-09-07 |
| 状态 | **决策已定案（D-J1~J7 + D-J8~J11）；Step 1 / Step 2 已完成并合并；Step 3+ 按 2026-09-07 复审重排** |
| 上游 | `requirements-spec.md`（需求定义源）、`architecture-design.md`（架构/ADR）、`eval-report.md`（P5 实测） |
| 前置 | V1（P0~P6）全部完成，见 `plan.md`（V1 计划，已冻结，不再更新） |
| 复审 | 2026-09-07 全量复审（7 处调整 A1~A7 + 4 个拍板问题 D1~D4）**结论已全部并入本文**，不另立复审文档；调整项索引与建议执行顺序见 **§附-3** |

> ⚠️ **编号变更（2026-09-07）**：V2.0 剩余步骤与 V2.1 全部步骤**从 Step 3 起重新编号**。
> 旧号→新号对照见 §附-2。历史设计文档（`v2-step1-design.md` / `v2-step2-design.md`）中的
> Step 编号**指重排前**，阅读时请对照映射表。

---

## 1. 本文档定位

| 文档 | 定位 |
| --- | --- |
| `docs/devel/plan.md` | **V1 开发计划**（P0~P6），已完成并冻结，不再追加任务 |
| **`docs/devel/plan-v2.md`（本文档）** | **V2 开发计划**：V2 的任务分解、开发步骤、验收门槛、进度追踪**都在本文件维护** |

本文档只做**规划**（做什么、按什么顺序、怎么验收），不展开 trait/算法/ADR 等**详细设计**——每个步骤开工时，如涉及架构决策，再单独出对应设计说明（沿用 `pX-design.md` 命名）或回写 `architecture-design.md`。§4.0 已把「开工前必须先澄清的设计决策」前置列出。

---

## 2. 版本范围与决策记录（已定案）

### 2.1 范围

| 批次 | 主题 | 归属 |
| --- | --- | --- |
| **V2.0** | 质量与性能夯实 + 精排（Reranker） | 本版发布 |
| **V2.1** | 读写并发 + 相关性深化 + 场景机制 | 下一迭代 |

**范围外（显式声明，防蔓延）**：
- ❌ **权限 / 多租户隔离**（需求 §7「权限/多租户隔离（v2）」）——内核只提供**命名空间过滤机制**（FR-32），鉴权、权限策略、租户边界由**场景层/接入层**负责。
- ❌ 不自研 HNSW（见 D-J7）；不引入嵌入式 KV / LMDB；不做 schema-first；不改 BM25/RRF 定稿参数。

### 2.2 决策记录（已定案）

| 编号 | 决策 | 结论 |
| --- | --- | --- |
| **D-J1** | V2.0 首版范围 | ✅ **工程夯实 + T7-01（Reranker）**；相关性深化与场景机制后置 V2.1 |
| **D-J2** | Reranker 模型 | ✅ **`bge-reranker-v2-m3`**（fastembed `RerankerModel::BGERerankerV2M3`） |
| **D-J3** | 崩溃恢复方案 | ✅ **原子 rename**（与图持久化的多文件原子性统一在 ADR-A 决策，见 §4.0 H1） |
| **D-J4** | delta 分段（高复杂） | ✅ **延后到 V2.1**；V2.0 先用「图持久化 + 增量构建」缓解写瓶颈 |
| **D-J5** | 时间衰减归属 | ✅ 内核只给 **打分钩子**（FR-33），衰减策略放场景层 |
| **D-J6** | 量化模型 | ✅ **不引入**；走「并行 + 增量构建」（fastembed 无 `bge-small-zh-v1.5` 量化变体） |
| **D-J7** | T7-05 改义 | ✅ 由 `plan.md` 原「自研 HNSW / 评估 arroy」改义为「hnsw_rs 能力接入（图持久化 + 并行建图）」——依据 §附 调研：hnsw_rs 0.3.4 已原生支持，自研不必要 |

### 2.3 复审决策（2026-09-07 全量复审，D1~D4 已拍板）

> 复审共提出 7 处调整（A1~A7），**全部采纳**并已落到本文对应章节，索引见 **§附-3**。

| 编号 | 决策 | 结论 | 对应复审问题 |
| --- | --- | --- | --- |
| **D-J8** | **NFR-03 构建指标口径** | ✅ **拆「首次全量 / 增量追加」双口径**：原单口径「1 万 chunk 含 embedding < 120s」在「不引入量化（D-J6）、不换模型」的前提下**无已识别路径可达**（实测 225.5s，差 1.6×）。新口径见 §4 Step 6 与需求文档 NFR-03 | **D1** |
| **D-J9** | **低选择度过滤延迟（R18）** | ✅ **从 V2.1 提前到 V2.0**（S 级，方案已明确）；同时**新增 NFR-13** 把「10 万级 + 低选择度」纳入口径，否则它永远在 NFR-02 之外 | **D2** |
| **D-J10** | **V2.0 剩余步骤顺序** | ✅ 重排为 **Step 3 可靠性 → Step 4 资源回收 → Step 5 查询性能与可观测 → Step 6 构建性能 → Step 7 精排**。即：原 Step 5（资源回收）**提前**，原 Step 4（构建性能）**顺延** | **D3** |
| **D-J11** | **`parallel_build` 默认值翻转** | ✅ **走独立 PR**，不并入任何 Step：改默认值的同时必须同步改 **S2-T22**（现断言「默认必须串行」） | **D4** |

---

## 3. 需求来源（遗留问题清单）

> 每项标注严重度与威胁的质量属性。取舍顺序（需求文档 6.3）：**正确性/确定性 > MRR > 延迟 P99 > 召回 > 资源**。
> **编号说明**：`Q-` 前缀为本计划自用，与架构 §14 风险表（R 系列）、`plan.md` 阶段号（P 系列）、P6 任务（I- 系列）**无关**。

| ID | 问题 | 证据 | 严重度 |
| --- | --- | --- | --- |
| Q-C1 | **删除后向量残留（幽灵候选）**：`Index::remove` 不回滚向量索引；`is_live_chunk` 定义了未调用 → 已删 chunk 占 Top-K 名额、召回静默下降、图膨胀 | `index/mod.rs:173`、`vector/mod.rs`、`retriever/vector.rs` | 高（正确性） |
| Q-C2 | 墓碑 chunk 永久保留，不物理回收 | `index/forward.rs` | 中（资源） |
| Q-C3 | 快照落盘无原子性，崩溃损坏 | `storage/snapshot.rs` | **高（正确性）** —— ⚠️ V2 Step 2 重新定级（原「中」）：图进入 `save()` 后「快照」由单文件变多文件，多文件间无原子性 ⇒ **图新/快照旧的版本错配**属正确性风险，见 `v2-step2-design.md` §1（Q-C3 部分） |
| Q-P1 | NFR-03 构建 225.5s 未达标（ort ~51 条/s） | `eval-report.md` 8.2 | 中 |
| Q-P2 | NFR-04 冷启动图重建 11.57s（图不持久化，D7） | `eval-report.md` 8.3 | 中 → **✅ 已解决**（V2 Step 2 / ADR-A，实测完整冷启动 ≈100ms） |
| Q-P3 | 图重建抖动 R-P5-13（NDCG 极差 0.0024） | `eval-report.md` 3.4 | 低 → **✅ 已解决**（图落盘即冻结拓扑，同快照两次加载逐位一致） |
| Q-I1 | 过滤位图 `allowed_chunks` 全表扫描 O(N)，10 万级威胁 P99 | `query/filter.rs:39` | 高（延迟） |
| Q-I2 | 过滤是 post-filter，低选择度隔离召回暴跌 | `query/searcher.rs`、`query/filter.rs` | 高（召回） |
| Q-M1 | embed 串行（`LocalEmbedder` Mutex 全程持锁） | `embed/local.rs:67,75` | 中 |
| Q-M2 | 无并发检索 QPS 压测 | `cli/bench.rs` | 中 |
| Q-R1 | paraphrase 桶 hybrid 被弱路 BM25 稀释 | `eval-report.md` 3.3 | 高（MRR） |
| Q-R2 | Reranker 仍是 NoOp（FR-18） | `rerank/noop.rs` | 高（MRR） |
| Q-R3 | 假负例敏感度对照未做 | `eval-report.md` 10 | 低 |
| Q-R4 | 评测 query 来自人类日志，缺 Agent query | `eval-report.md` 9 | 中 |
| Q-U1 | 写端独占，`into_searcher()` 后无法写（FR-17 完整版未做） | `p6-design.md` 6.4 | 中 |
| Q-U2 | 旧 API 未标 deprecated（D-I6 延后） | `p6-design.md` 10.2 | 低 |
| Q-S1 | 无命名空间/会话隔离（依赖 Q-I2） | — | 中 |
| Q-S2 | 无时间衰减机制 | — | 低 |

> 另有跨快照永续的次生问题：`SearchIndex::remove` 后 `save()` 原样带走已删向量、`load_with` 全量重灌（`search/index.rs` 注释「会被丢弃」实际不成立）——Q-C1/Q-C2 不只在内存、还会**随快照持久化**，**Step 4**（资源回收）验收必须覆盖。

---

## 4. V2 开发步骤

> 步骤按依赖关系与质量属性优先级编排。每步列：任务、依赖、验收、风险、量级。
> **任务 ID**：T7-01~06 沿用 `plan.md` P7；**T7-07~20 为本计划新增**（T7-10 预留，原「量化评估」因 D-J6 不引入而空出，不启用）；
> **T7-21~24 为 2026-09-07 复审新增**（T7-21 `parallel_build` 默认值翻转、T7-22 低选择度暴力兜底、T7-23 `Metrics` 可观测化、T7-24 工程卫生）。
> 量级：S（天级）/ M（周级）/ L（双周+）。

### 4.0 开工前必须澄清的设计决策（不阻塞 Step 1 启动）

| # | 决策点 | 说明 | 归属步骤 |
| --- | --- | --- | --- |
| **H1** | 图持久化格式 + **多文件原子性** | hnsw_rs 图落盘是**双文件**（`hnsw.graph` + `hnsw.data`，`HnswIo`/`DumpInit`）。图持久化进入 `save()` 后，「快照」不再是单文件，「临时文件 + rename」无法保证跨文件一致（图新、快照旧版本错配）。**必须二选一**：① 图并入快照 blob（`FORMAT_VERSION` 升版）；② sidecar + manifest/generation 目录级原子替换。Step 2 与 Step 3 **不是无依赖**，需先合出这份 ADR（记为 **ADR-A**）。**→ 已由 `v2-step2-design.md` v0.3 回答（2026-09-06 拍板）**：**方案 C**——sidecar 三件套 + 独立 manifest 作唯一原子发布点（tmp+fsync+rename+fsync 目录），`FORMAT_VERSION` 保持 2；图 = 快照的派生缓存，可随时丢弃（删 manifest/删图/篡改任一字节 → 降级重建，功能不丢）。7 项决策（D-S2-01~07）已获外部评审全部同意；Step 3 的原子快照直接沿用该 manifest 机制 | Step 2 + Step 3 |
| **H2** | 向量删除的真实语义 | hnsw_rs 0.3.4 **无 remove/delete API**（架构 7.5 早记录「物理删除需 usearch，引入 C++」）。T7-07 实际语义只能是**向量墓碑 + `search_filter` 存活位图**；Q-C1 的「图膨胀」只能**缓解**，真正回收靠 T7-12 重建图。**Step 4**（原 Step 5）的「体积不无界增长」验收**必须显式覆盖向量图 + `raw_vectors` + 快照文件**。**→ 已由 `v2-step1-design.md` §4.3 / §6 D-S1-01 回答**：存活状态只住 `Index.forward`（单一真源），以位图谓词传给向量路，`VectorIndex` 不加 `tombstone`。 | Step 1 + Step 5 |
| **H3** | 精排候选窗口 | 当前编排 `search_parts` 融合后先 `take(k)` 再进 reranker（`query/searcher.rs:196`），精排只能重排 k 条。要拿像样的 MRR 提升必须放开窗口到 `candidate_k`（3k）或可配 R，这直接决定延迟预算。需**新增精排延迟 NFR**（NFR-12），并在 T7-01 内设计窗口参数。**⚠️ 至今未决**，是 Step 7 的开工前置。 | Step 7 |

### V2.0 —— 质量与性能夯实 + 精排（Step 1 ~ Step 7）

> **执行顺序（2026-09-07 重排后）**：Step 3 可靠性 → Step 4 资源回收 → Step 5 查询性能与可观测
> → Step 6 构建性能与多线程 → Step 7 精排。横切任务（T7-21 / T7-24）走独立 PR，不占步骤号。
> **含横切任务的完整执行顺序见 §附-3**。

#### Step 1 · 正确性修复（地基，最先做）

> **详细设计**：`docs/devel/v2-step1-design.md`（**v1.1**，2026-09-05，已吸收外部评审并**实施完成**）。
> v0.2 关键修订：`hnsw_rs::search_filter` 的停止条件**原判断有误**——带 filter 时**从不 fast return**，且堆未满时距离剪枝全程关闭 ⇒ `allowed < ef` 时必然整图遍历（结构性，tuning 治不了）。
> 由此新增：**无过滤热路径走普通 `search()` + 按存活比例过采样**（保住 fast-return 与 `EF_SEARCH=200`），**有过滤才走 `search_filter`**。另新增字段索引全类型覆盖、惰性谓词（消除 O(N) 展开）、`query_has_hits` 探针。
> 已回答 H2（向量软删除的真实语义）并核实 `hnsw_rs::search_filter` 的真实行为；
> 实施任务拆分为 S1-01~S1-11，建议顺序：先修幽灵候选（含跨快照），再做过滤下推。

**实施进度（2026-09-05 收尾）**：S1-01 ~ S1-11 **全部完成**。

| # | 任务 | 交付 |
| --- | --- | --- |
| S1-01~09 | 正确性（位图 / 存活位图 / 字段索引 / 谓词 / 快照重建 / 两路下推 / HNSW 双路径 / 编排层） | PR #6（`61e3dac`），8 个 CI job 全绿；GLM-5.3 评审 Approve |
| S1-10 | fixture + bench 选择度曲线 + NFR-02 重测 | PR #8（`0d87b39`）：`scripts/gen_synth_corpus.py`（确定性合成生成器）+ `scripts/eval_filter.sh`（一键扫描）+ bench 新增 `--filter` / `--filter-cost` / 自适应 oracle |
| S1-11 | 文档回写 | PR #8（`0d87b39`）：架构文档 **v1.5**（ADR-010、§5.4/§5.4.1、§5.5/§5.5.1、§7.5 重写、§7.6.1、§8.3/§8.4、R11~R18）+ 需求文档 v1.5 + `docs/README.md` 索引 + 设计文档 v1.0 |
| — | issue #9（bench `--oracle-depth ≥ 501` clamp panic） | PR #10（`e1e4b60`）：下界先被上限夹住再 clamp + `saturating_mul` + clap `1..=500` 范围校验 + 边界单测；设计文档升 v1.1（§0.3） |

**S1-10 实测结论 —— A. 1 万级**（release，K=10，60 query × 10 reps）：

- **T14① NFR-02 达标**：无过滤 P99 = bm25 0.42ms / vector 3.54ms / hybrid 3.46ms，均远低于「基线 × 1.10」
  （4.29 / 9.21 / 9.57ms）——**path A 保住 fast-return 的设计目标达成**（R15 已验证）
- **T13 两条结论**：① 未降级字段（`sel_01=hit`）求值从 **0.2221ms → 0.0005ms，加速比 471.8×**
  —— Q-I1 的收益真实存在；② **`ts_ms`（高基数 Range）档位加速比 0.9×**
  （新旧都退化为全扫，新路径还多一层「查索引发现降级 → 回落」的开销）——
  **Q-I1 对该场景收益为 0**，与评审 P2-2 的预判一致，且撞线比直觉更快（数值字段 terms + numbers
  合计计入限额，约 **512 篇**就降级）

**S1-10 实测结论 —— B. 10 万级**（release，K=10，200 query × 5 reps）：

| 档位 | 选择度 | bm25 P99 | vector P99 | hybrid P99 | vector ÷ 无过滤 |
| --- | --- | --- | --- | --- | --- |
| none | 1.0 | 2.43 | 5.81 | 3.46 | 1.0× |
| sel-10% | 0.1 | 0.50 | 7.91 | 7.75 | 1.4× |
| sel-1% | 0.01 | 0.32 | 41.33 | 41.30 | **7.1×** |
| sel-0.1% | 0.001 | 0.25 | 158.45 | 144.87 | **27.3×** |
| ts-range-degraded | 0.001 | 8.26 | 124.26 | 121.87 | **21.4×** |

- **R18（新增风险）：低选择度过滤查询延迟爆炸**。选择度 ≤1% 时直接违反 NFR-02（限额 10/20ms，
  实测超 **4~16×**）。这是 v0.2 对 R13 的预估（"随规模线性上升"）所不及的量级——
  实际是**规模 × 选择度的复合效应**。V2.0 **接受**：正确性优先于延迟，且优化前的 post-filter
  在该选择度下几乎返回不了任何结果（Q-I2 的原始病症）。V2.1 必须解决，
  数据已支持的廉价方案是 **allowed 很小时绕开 ANN 直接暴力扫描**（10 万级 0.1% 档位 allowed=100，
  代价约 0.05ms vs 158ms）
- **NFR-02 无过滤档位在 10 万级仍达标**（2.43 / 5.81 / 3.46ms），path A 未退化
- **P2-2 在 10 万级得到直接证据**：`ts-range-degraded` 的 bm25 P99 = **8.26ms**，
  比 bm25 无过滤检索本身（2.43ms）还贵 **3.4 倍**——降级字段的 O(N) 全扫已超过整个检索的代价
- **v0.3 的错误归因已推翻**：⚠️ 此前写「hybrid 重合率未达 0.99，缺口全部来自向量路」**是错的**。
  实测 bm25 与 vector **分路重合率都是 1.0000**，只有融合后掉到 0.685~0.948 ⇒ 分歧**不可能**
  来自任何一路的下推，**只能**诞生于融合层（filter-then-fuse vs fuse-then-filter 的排名语义差异）。
  因此 **hybrid 重合率不能当召回缺口读**；真正的召回判据是分路重合率 1.0000 + 返回条数 10.00（均满分）。
  两种融合顺序哪种相关性更好**未决**，需选择度专用相关性评测集 → 列 V2.1

| 任务 | 解决 |
| --- | --- |
| T7-07 **向量墓碑 + 存活过滤**（`VectorIndex` 软删除 + `search_filter` 存活位图，非物理 remove） | Q-C1 |
| T7-08 过滤下推 + 字段索引（替换 `allowed_chunks` 全表扫描） | Q-I1、Q-I2 |

- **依赖**：无。
- **验收**：删除后三路（含向量/hybrid）不再召回该 doc（回归测试）。10 万级语料过滤耗时：**10 万级为扩展验证**（需另行准备 fixture，`data/` 现仅 12K/24K）；NFR-02 的标定口径仍为 1 万 chunk（hybrid P99<20ms），10 万级按新口径另测另录。
- **风险**：**高**（`VectorIndex` trait 加软删除，Hnsw/Brute 两实现都改；且软删除语义需与 H2 澄清一致）。
- **量级**：M。

#### Step 2 · 冷启动与确定性（D7）

| 任务 | 解决 |
| --- | --- |
| T7-05 图持久化 + 并行建图（`HnswIo` 双文件 dump + `parallel_insert`） | Q-P2、Q-P3 |

- **依赖**：**先合出 ADR-A（H1，与 Step 3 合并）**，定稿图持久化格式与多文件原子性。**✅ ADR-A 已拍板**（`v2-step2-design.md` v0.3，2026-09-06）：方案 C（sidecar 三件套 + 独立 manifest 原子发布点，`FORMAT_VERSION` 保持 2）。
- **验收**：12K 冷启动（加载 + 图加载）< 2s；**同一快照两次加载**检索结果逐位一致（消 R-P5-13；注意「两次重建」在 `parallel_insert` 下不成立，验收必须指「同快照两次加载」）。
- **风险**：低（API 已源码核实；开工前复核另发现两项：Description 落盘 magic 实为 4、重载图 datamap_opt=true 拒绝覆盖旧文件——对策均已成文，见设计文档「开工前复核记录」）。
- **量级**：M。

> **状态（2026-09-07 更新）：✅ 已完成并合并进 `main`。**
> main 提交链：`68074c9`（#12 core 图持久化）→ `e9ac2b0`（#15，源自 #13：CLI/bench 接线 + 并行建图）
> → `15c1900`（#16，源自 #14：文档回写 + 12K 实测入库）→ `1e51156`（#17：S2-T6 oracle 断言改多 query 均值，修 flaky）。
> `main` 的 CI 全绿，issue #3 已关闭，8 项验收全绿。
> ⚠️ **踩过的坑**：原 #13/#14 的 base 是各自的堆叠分支，GitHub 显示 `MERGED` 但内容**从未进 main**，
> 最终靠 cherry-pick 两个 squash 提交、重建 base=main 的 PR 才真正落地 ⇒ **堆叠 PR 的 base 一律设 main**。
>
> S2-01~S2-12 全部落地，守门全绿
> （fmt / clippy `-D warnings` / **213 测试** / MSRV 1.90 / `RUSTDOCFLAGS="-D warnings" cargo doc` / `--no-default-features` /
> `--features charabia` / cargo-deny）。
>
> **12K 实测（`data/t2-corpus.jsonl`，release，macOS aarch64）**：
>
> | 项 | 实测 | 判据 |
> | --- | --- | --- |
> | 快照加载（含位图/字段索引重建） | 57~77ms | —— |
> | 图 sidecar 加载 | 23~37ms | —— |
> | **完整冷启动** | **≈100ms**（三次 82~100ms） | **< 2s ✅**（余量 20×） |
> | 降级路径（图 sidecar 缺失） | 56~78ms + 图重建 ≈10~11.5s ≈ **11s** | 对照：即 D7 时代的水平 |
> | 图 dump 耗时 | 43.5ms（31.6MB） | 远低于预估，非瓶颈 |
> | 磁盘增量 | 52.3MB → ≈84MB（**1.6×**，graph 7.9~8.1 + data 23.7） | 已接受（R22） |
>
> ⚠️ **达标依赖图 sidecar 命中**——降级路径仍是 ~10s。这正是 NFR-07「降级不得静默」
> 的由来：`GraphStatus::Rebuilt(reason)` 必须显式可见，否则 NFR-04 会静默失效而不自知。

#### Step 3 · 可靠性（原子快照）

| 任务 | 解决 |
| --- | --- |
| T7-13 原子快照（tmp + fsync + rename + fsync 父目录，D-J3） | Q-C3 |

**范围（2026-09-07 复审重写，依据 ADR-A）**：ADR-A 已把「多文件原子性」收敛为
**manifest 唯一发布点 + 父快照 CRC 版本锚点**，故架构 §7.6.2 明确：
> 「Step 3 只需让 `foo.idx` 自己 tmp+rename，图 sidecar 自动跟随」

⇒ 本步骤是**单文件原子写**，不是多文件事务；原验收里「覆盖图 + 快照双文件」的描述已过时。

**⚠️ 现状事实**：`storage/snapshot.rs:88` 仍是 `File::create(path)` 直写 + `flush()`，
**既无 tmp+rename、也无 fsync** ⇒ 崩溃中途产生半截文件，下次 `load` 得
`Error::SnapshotCorrupted`——**不是降级重建，是索引丢失**。（对比：图 manifest 已原子，
`storage/graph.rs:236 write_manifest_atomic()`。）

| 子任务 | 内容 |
| --- | --- |
| S3-a | 从 `write_manifest_atomic` 抽出通用 `atomic_write(path, bytes)`：tmp → flush → `sync_all` → rename → fsync 父目录；快照与 manifest **共用一份实现** |
| S3-b | `save_with_crc` 改走 `atomic_write`，补上此前完全缺失的 fsync |
| S3-c | tmp 孤儿回收：save/load 时清理 `*.idx.tmp` 与 `*.idx.hnsw.manifest.tmp`（并补 `.gitignore`） |
| S3-d | 故障注入测试：`catch_unwind` + 可注入失败点，在「写完 tmp、未 rename」处 panic，断言**原快照完好** |
| S3-e | 顺带收 **R19**（写路径 `panic_any` 的 TOCTOU 残余）——与 S3-a 属同一处代码边界，合并做最省 |

- **依赖**：无（ADR-A 已定案）。
- **验收**：中断/崩溃后 `load` 要么拿到**旧快照**、要么拿到**新快照**，**绝不出现 `SnapshotCorrupted`**；图 sidecar 因 CRC 锚点自动降级（该行为已被 Step 2 覆盖，不重复验收）。
- **风险**：低；⚠️ 造「写文件必失败」的测试场景**别用 chmod**（CI 以 root 运行时权限位无效，Step 2 已踩过），让目标位置被**同名目录**占据更稳。
- **量级**：S。**排在 V2.0 剩余工作最前**——Q-C3 定级为「高（正确性）」。

#### Step 4 · 资源回收（原 Step 5，D-J10 提前）

| 任务 | 解决 |
| --- | --- |
| T7-12 墓碑物理回收（compaction，重建向量图 + 回收 `raw_vectors` + 重写快照） | Q-C2 |

- **依赖**：Step 1（软删除语义就位）；**Step 2 的图 sidecar 机制**（Step 2 之后才成立，见下）。
- **⚠️ Step 2 引入的新复杂度**：compaction 重建图后**必须重发 manifest**（`nb_point` /
  `graph_crc` 全变），否则新图永远匹配不上 ⇒ 每次冷启动都走降级重建（≈10s），
  **白重建一次且 NFR-04 静默失效**。
- **验收**：长期零散写入后，**向量图 sidecar + `raw_vectors` + 快照正文**三者体积均不随删改
  无界增长（覆盖跨快照永续问题）；compaction 后 `GraphStatus` 回到 `Loaded`（不是 `Rebuilt`）。
- **新增**：可复现的「零散写入」workload 脚本（`scripts/` 目前没有；fixture 已齐：
  `data/synth-100000-corpus.jsonl`）。
- **风险**：中；**重建图会重新引入拓扑抖动**（NFR-06 不破，但评测可比性需在文档说明）。
- **量级**：M。

#### Step 5 · 查询性能与可观测（新增，D-J9）

| 任务 | 解决 |
| --- | --- |
| **T7-22** 低选择度暴力兜底（R18，从 V2.1 提前） | Q-I2 次生 / **NFR-13**（新增） |
| **T7-23** `query::Metrics` 可观测化 | issue #7 遗留 / NFR-10、NFR-11 前置 |

- **T7-22**：10 万级实测，选择度 ≤1% 时 vector P99 达 41~158ms（超 NFR-02 限额 4~16×）。
  方案已明确且廉价：**`allowed` 小于阈值时绕开 ANN、直接暴力扫描**
  （10 万级 0.1% 档 `allowed≈100`，代价约 **0.05ms vs 158ms**）。阈值在实现时标定。
  ⚠️ 该场景此前**不在任何 NFR 口径内**，故同步新增 **NFR-13**。
- **T7-23**：`query::Metrics` 目前只经 `tracing::info!` 输出——**外部拿不到实例 ⇒ 无法单测、
  无法被 bench 采集**；而 `vector_shortfall` 正是「V2.1 是否引入 prefilter」的判据，
  Step 6 的 NFR-10/11 实测也要靠它。需暴露进 `SearchResponse` 或 bench 采集链路。
  顺带修 `query/metrics.rs:14` 里「见 issue #7」的失效引用（#7 已关闭）。
- **依赖**：无；**T7-23 是 Step 6 的前置**。
- **验收**：10 万级 sel-0.1% 档 vector P99 进入 NFR-13 预算；`Metrics` 至少有一条单测 +
  一处 bench 采集点。
- **量级**：S。

#### Step 6 · 构建性能与多线程（原 Step 4，D-J10 顺延）

| 任务 | 解决 |
| --- | --- |
| T7-09 embed 并行（**先 spike 再定是否投 M 级**） | Q-P1、Q-M1 |
| T7-11 增量构建（content_hash → embed 缓存，跳过已 embed 文档） | Q-P1 |
| T7-17 并发检索压测进 bench（QPS + 正确性，`Searcher` 跨线程） | Q-M2 |

- **⚠️ NFR-03 已按 D-J8 拆双口径**，本步骤按新口径验收（不再是「1 万 chunk 全量 <120s」）：

  | 口径 | 目标 | 依据 |
  | --- | --- | --- |
  | **首次全量** | 1 万 chunk 含 embedding 构建 **< 240s** | 实测 225.5s（12K）；在「不量化（D-J6）、不换模型」前提下无已识别路径可降到 120s |
  | **增量追加** | 追加 10% 文档 < 首次全量 × 10% × 1.2 | Agent 场景的真实使用形态是反复追加，T7-11 才是这条口径的落点 |

- **T7-09 的技术前提已被证伪一半**（2026-09-07 复审，源码级核实）：
  - `fastembed-6.0.2` 的 `InitOptions.intra_threads` 默认 `None` = **用满所有核**
    （`src/init.rs:30-33`），我们未覆盖 ⇒ 单 session 的 ONNX 推理**已在满核跑**，
    多 session 大概率是**抢核**；
  - 批次不是瓶颈：32/64/128/256 → 62.6 / 59.1 / 54.4 / 51.6 条/s（`eval-report.md:147-148`）；
  - 建图并行只占 4%（225.5s 里 embed 209.9s）。
  ⇒ **T7-09 先做 S 级 spike，出数据再决定是否投 M 级**：

  | 实验 | 内容 |
  | --- | --- |
  | E1 | 单 session 基线（现状） |
  | E2 | 2~4 session × `intra_threads` 分片 |
  | E3 | **CPU EP vs CoreML EP**（`ort-2.0.0-rc.13` 有 `coreml` feature，fastembed 暴露 `with_execution_providers`）——macOS aarch64 上**从未评估过**的杠杆 |

  ⚠️ E3 若有效，**向量数值会变** ⇒ 必须同时重测 **NFR-06 确定性**与相关性基线，
  且 `ConfigFingerprint` 需纳入 execution provider（否则 CoreML 建库 / CPU 加载会静默错配，
  正是 B1/B2 那类坑）。

- **依赖**：T7-23（Step 5）是 T7-17 的前置；T7-11 依赖 Step 1。
- **风险**：中（embed 并行内存峰值上升，需重测 NFR-05；E3 改变数值）。
- **量级**：M（含 S 级 spike）。

#### Step 7 · 精排（相关性第一刀，V2.0 收尾，原 Step 6）

| 任务 | 解决 |
| --- | --- |
| T7-01 Reranker 接入（fastembed `TextRerank`，`bge-reranker-v2-m3`，D-J2）+ **精排候选窗口放开到 `candidate_k`/可配 R** | Q-R2 |

**开工前置三条（2026-09-07 复审新增）**：

1. **先复现基线**：验收写的是「MRR@10(1) 相对 P5 基线 0.6922 可测提升」，但**没人复现过基线**。
   第一步就用 `scripts/eval_quality.sh` + T2Ranking qrels 跑出当前值**与波动范围**，
   否则「提升」无法判定（Step 1 已吃过「重合率 0.685~0.948 波动」的亏）。
2. **模型下载可行性**：API 已核实可用（`TextRerank::try_new(RerankInitOptions)` +
   `RerankerModel::BGERerankerV2M3` 均存在于 fastembed 6.0.2），但该模型含外置
   `model.onnx.data`（**GB 级**）⇒ 需先确认下载体积与镜像可达性。
3. **H3 必须先定**：精排窗口 + NFR-12 至今未决；放开到 `candidate_k`（3k）会让 rerank
   成为延迟主导项。

- **依赖**：Step 2（性能基线稳定后测精排延迟）；H3 定案。
- **验收**：hybrid MRR@10(1) 相对**已复现的基线**可测提升；精排延迟有实测（**NFR-12**，独立口径，不并入 NFR-02）。
- **风险**：中；CI 需沿用 `#[ignore]` 模式 + 本地缓存策略。
- **量级**：M。

### 横切任务（独立 PR，不属于任何 Step）

| 任务 | 内容 | 决策 |
| --- | --- | --- |
| **T7-21** `parallel_build` 默认翻转为「开」 | 12K 真实语料 11.676s → 2.182s（**5.35×**），50K 合成 5.26×，真实语料 oracle 重合率无差异（0.995 / 0.995）。⚠️ 必须同步改 **S2-T22**（现断言「默认必须串行」） | **D-J11 / D4** |
| **T7-24** 工程卫生 | ① **CI 只在 `branches:[main]` 触发** ⇒ 堆叠 PR 至今跑不到 CI（Step 2 已吃过亏）；② `.gitignore` **未覆盖图 sidecar 与 manifest tmp**（31MB 级误提交风险）；③ 清理 11 条已合并远端分支 | 复审 A4-④ |

### V2.1 —— 读写并发 + 相关性深化 + 场景机制（后续迭代）

#### Step 8 · 读写并发（原 Step 7）

| 任务 | 解决 |
| --- | --- |
| T7-06 delta 分段（FR-17 完整版：读不阻塞写） | Q-U1 |
| T7-18 旧 API `#[deprecated]`（D-I6 复议） | Q-U2 |

- **依赖**：T7-18 依赖 T7-06。
- **风险**：高（跨两棵树合并 BM25 统计量）。
- **量级**：L。

#### Step 9 · 相关性深化（原 Step 8）

| 任务 | 解决 |
| --- | --- |
| T7-14 自适应融合（paraphrase 桶弱路稀释修复） | Q-R1 |
| T7-15 假负例敏感度对照（仅 qrels 语料重建） | Q-R3 |
| T7-16 Agent query 评测集（LLM 生成 + 多轮 + 自查） | Q-R4 |

- **依赖**：T7-14 依赖 **Step 7**（Reranker 就位）。
- **风险**：高（自适应融合需重测，防过拟合）。
- **量级**：M。

#### Step 10 · 场景机制（面向知识/记忆，原 Step 9）

| 任务 | 解决 |
| --- | --- |
| T7-19 命名空间/会话隔离（依赖过滤下推） | Q-S1 |
| T7-20 时间衰减打分钩子（可插拔，D-J5） | Q-S2 |
| T7-02 MMR 结果去重（FR-24） | — |
| T7-03 token budget 裁剪（FR-25） | — |
| T7-04 多 query 融合接口（FR-21） | — |

- **依赖**：T7-19 依赖 Step 1（过滤下推）。
- **风险**：中。
- **量级**：M。

---

## 5. 需求编号草案（评审通过后回写 `requirements-spec.md`）

> 回写纪律：需求文档 §1.3 要求编号唯一。**FR 无「升级」机制**——已存在编号一律**原条目内修订**，不新建同编号条目；新需求才新增编号。

> ⚠️ 「对应步骤」列已于 2026-09-07 按重排后的编号更新（旧号见 §附-2）。

| 编号 | 需求 | 优先级 | 对应步骤 | 与既有需求的关系 |
| --- | --- | --- | --- | --- |
| FR-18 | Reranker 接入真实模型（**原条目内修订**：保留单编号，将「第一版只留位」改为「V2.0 接入 bge-reranker-v2-m3」） | Must | Step 7 | 修订 FR-18 |
| FR-17 | 增量写入（**标注「部分交付」**：V1 已交付主索引增量；「读不阻塞写」完整版 V2.1 Step 8） | Must | Step 8 | 修订 FR-17 交付状态 |
| FR-15 | 幂等 upsert 与删除（**向量删除并入**，删除语义扩展到向量侧） | Must | Step 1 | 修订 FR-15 |
| FR-16 | 索引二进制快照（**原子性并入**：save 原子替换 + 图持久化格式） | Must | Step 2/3 | 修订 FR-16 |
| FR-26 | 向量软删除 + 存活过滤 | Must | Step 1 | 强化 FR-15 |
| FR-27 | 过滤下推 + 字段索引 | Must | Step 1 | 新增 |
| FR-28 | 增量构建（只 embed 新增） | Should | Step 6 | 新增 |
| FR-29 | 图持久化 | Must | Step 2 | 强化 FR-16 |
| FR-30 | 墓碑物理回收（compaction） | Should | Step 4 | 强化 FR-15/26 |
| **FR-31** | 原子快照 | **Must**（2026-09-07 由 Should 升：Q-C3 定级「高（正确性）」且为 V2.0 必做） | Step 3 | 强化 FR-16 |
| FR-32 | 命名空间隔离 | Should | Step 10 | 新增（≠权限/多租户，见 §2.1 范围外） |
| FR-33 | 时间衰减打分钩子 | Could | Step 10 | 新增 |
| FR-21 | 多 query 融合（**收编**进入 V2 范围，原即 Could=v2） | Could | Step 10 | 原有 |
| FR-24 | MMR 结果去重（收编） | Could | Step 10 | 原有 |
| FR-25 | token budget 裁剪（收编） | Could | Step 10 | 原有 |
| NFR-03 | 索引构建（**D-J8 拆双口径**：首次全量 < 240s / 增量追加 < 首次 × 变更比 × 1.2） | Must | Step 6 | 修订口径（原「1 万 chunk <120s」无路径可达） |
| NFR-10 | 并发读吞吐 | Should | Step 6 | 新增（前置：Step 5 的 T7-23） |
| NFR-11 | 增量可见性与写延迟（**口径修订**：`add` 后需 `commit()` 才可见 ⇒ 改为「commit 后立即可查；写延迟 = flush 一批耗时」） | Should | Step 6 | 新增 |
| **NFR-12** | **精排延迟**（rerank P99，独立口径） | Should | Step 7 | 新增（H3） |
| **NFR-13** | **低选择度过滤延迟**（10 万级 + 低选择度档位的 P99） | Should | Step 5 | 新增（D-J9；此前该场景不在任何 NFR 口径内） |

---

## 6. 验收门槛（Gate）

**V2.0 发布门槛**（Step 1~7 全部满足；✅ = 已达成）：

- [x] **Step 1**：Q-C1 修复有回归测试——删除后三路（含向量/hybrid）不再召回该 doc（PR #6）
- [x] **Step 1**：过滤下推后，1 万 chunk 口径单 query 过滤耗时在 NFR-02 预算内；10 万级扩展验证已另测另录（`eval-report.md`）
- [x] **Step 2**：图持久化后 12K 冷启动 ≈100ms（< 2s），且**同一快照两次加载**结果逐位一致（消 R-P5-13）
- [ ] **Step 3**：原子快照通过故障注入测试——崩溃后 `load` 要么旧快照、要么新快照，**绝不 `SnapshotCorrupted`**
- [ ] **Step 4**：compaction 后三个体积（向量图 sidecar / `raw_vectors` / 快照正文）不无界增长，且 `GraphStatus` 回到 `Loaded`
- [ ] **Step 5**：10 万级低选择度档位 P99 进入 **NFR-13** 预算；`Metrics` 有单测 + bench 采集点
- [ ] **Step 6**：**NFR-03 按双口径**重测达标（首次全量 < 240s / 增量追加达标）；NFR-05 内存重测记录；`Searcher` 跨线程并发读吞吐 + 正确性有数据
- [ ] **Step 7**：Reranker 接入后 MRR@10(1) 相对**已复现基线**可测提升；精排延迟满足 NFR-12
- [ ] `make fmt && make lint && make test && make deny` 全绿；CI 全绿

**V2.1 门槛**（Step 8~10）：delta 分段读不阻塞写、自适应融合在分桶上优于固定权重、命名空间低选择度过滤召回不回退、MMR/token 有单测与示例。

---

## 7. 进度追踪（V2）

> 每完成一个任务在此勾选。

| 步骤 | 任务 | 状态 |
| --- | --- | --- |
| Step 1 | T7-07 向量墓碑 + 存活过滤 | ✅ 已完成（2026-09-05，PR #6 → `61e3dac`） |
| Step 1 | T7-08 过滤下推 + 字段索引 | ✅ 已完成（2026-09-05，PR #6 → `61e3dac`） |
| Step 2 | T7-05 图持久化 + 并行建图 | ✅ 已完成（2026-09-07，PR #12/#15/#16/#17/#18 → `68074c9`/`e9ac2b0`/`15c1900`/`1e51156`/`9db2c00`） |
| **Step 3** | T7-13 原子快照（S3-a~e，含 R19） | ⬜ |
| **Step 4** | T7-12 墓碑物理回收（compaction，含 manifest 重发） | ⬜ |
| **Step 5** | T7-22 低选择度暴力兜底 | ⬜ |
| **Step 5** | T7-23 `query::Metrics` 可观测化 | ⬜ |
| **Step 6** | T7-09 embed 并行（**先 E1/E2/E3 spike**） | ⬜ |
| **Step 6** | T7-11 增量构建 | ⬜ |
| **Step 6** | T7-17 并发检索压测 | ⬜ |
| **Step 7** | T7-01 Reranker 接入（bge-reranker-v2-m3） | ⬜ |
| Step 8 | T7-06 delta 分段 | ⬜ |
| Step 8 | T7-18 旧 API deprecated | ⬜ |
| Step 9 | T7-14 自适应融合 | ⬜ |
| Step 9 | T7-15 假负例对照 | ⬜ |
| Step 9 | T7-16 Agent query 评测集 | ⬜ |
| Step 10 | T7-19 命名空间隔离 | ⬜ |
| Step 10 | T7-20 时间衰减钩子 | ⬜ |
| Step 10 | T7-02 MMR 去重 | ⬜ |
| Step 10 | T7-03 token budget 裁剪 | ⬜ |
| Step 10 | T7-04 多 query 融合 | ⬜ |
| **横切** | T7-21 `parallel_build` 默认翻转（独立 PR，含改 S2-T22） | ⬜（已出数据，D-J11） |
| **横切** | T7-24 工程卫生（CI 触发 / gitignore / 分支清理） | ⬜ |

> T7-10 预留（原「量化评估」，D-J6 决定不引入后空出，不启用）。

---

## 8. 文档回写计划（**执行中**：Step 1 / Step 2 已回写）

1. `requirements-spec.md`：
   - ✅ FR-18 原条目内修订（保留单编号）；FR-17 标注「部分交付 / 完整版 V2.1」；FR-15/16 扩展删除与原子性语义。
   - ✅ FR-26~33 / NFR-10~12 正式入表（含与 FR-15/16 的修订关系）。
   - ✅ **NFR-03 备注刷新为 225.5s**（P6 重测值，替换 P5 的 236.7s）；NFR-04 明确「快照 + 图加载」口径。
   - 🆕 **2026-09-07 复审回写（本轮）**：NFR-03 拆「首次全量 / 增量」双口径（D-J8）；**新增 NFR-13**（低选择度过滤延迟，D-J9）；NFR-11 口径改为「commit 后立即可查」；FR-31 优先级 Should → **Must**；NFR-10/11/12 的「对应步骤」按重排编号更新。
2. `architecture-design.md`：
   - 新增 **ADR-A**（图持久化格式 + 多文件原子性）、**ADR-010**（向量软删除 / 过滤下推 / 图持久化的架构决策）。
   - **第 14 章风险表 R1 补「已关闭」状态**（P5 已换 hnsw_rs，instant-distance 移除）；**p5-design 的 D7 结案**（图持久化落地）——**✅ 已由 V2 Step 2 完成**（2026-09-06）：ADR-A 方案 C，12K 完整冷启动 11.57s → ≈100ms，NFR-04 达标；R-P5-13 一并消解（图落盘即冻结拓扑，同快照两次加载逐位一致）。
   - ADR-002（instant-distance）在 ADR-010 落地时标废。
3. `CHANGELOG.md` / README「已知局限」随版本发布更新。
4. 详细设计（ADR-A、H2/H3 等）在对应步骤开工时单独出 `pX-design.md`，不在本文件展开。

---

## 附-1：证据清单

- `crates/core/src/index/mod.rs:173` `remove` 只回滚倒排与墓碑，不含向量；`:247` `is_live_chunk` 定义了但未调用
- `crates/core/src/retriever/vector.rs` 无存活过滤；`vector/mod.rs` trait 无 `remove`
- `crates/core/src/query/filter.rs:39` `allowed_chunks` 全表扫描；`query/searcher.rs:196` 融合后 `take(k)` 再 rerank
- `crates/core/src/embed/local.rs:67,75` `Mutex` 串行 embed
- `crates/core/src/search/index.rs` `remove` 后 `save()` 原样带走已删向量、`load_with` 全量重灌（注释「会被丢弃」不成立）
- `docs/devel/eval-report.md` §8 NFR 实测、§9 数据诚信、§10 残留问题
- 依赖源码（已核实）：`hnsw_rs-0.3.4` —— `parallel_insert`/`search_filter`/`FilterT`/`load_hnsw` 属实；图落盘为**双文件**（`HnswIo` + `DumpInit`，`hnsw.graph` + `hnsw.data`）；**无 remove/delete API**。`fastembed-6.0.2` —— `TextRerank`/`RerankerModel::BGERerankerV2M3`/`with_intra_threads` 属实；`bge-reranker-v2-m3` 含 `model.onnx.data` 外置数据文件。

- **2026-09-07 复审补充核实**：`fastembed-6.0.2`
  `src/init.rs:30-33` —— `intra_threads` 默认 `None` = 用满所有核；`src/init.rs:133` —— `with_intra_threads` 存在；
  `src/text_embedding/impl.rs:373` —— `embed` 单 session 按 `DEFAULT_BATCH_SIZE=256` 串行分块；
  `src/models/text_embedding.rs` —— **无中文量化变体**（量化仅 `NomicEmbedTextV15Q` / `ParaphraseMLMiniLML12V2Q`）；
  `src/models/reranking.rs:6-11` —— `RerankerModel::BGERerankerV2M3` 存在且带外置 `model.onnx.data`。
  `ort-2.0.0-rc.13` `Cargo.toml:145` —— 有 `coreml = ["ort-sys/coreml"]` feature。

---

## 附-2：步骤编号映射（2026-09-07 重排）

> 重排依据：2026-09-07 全量复审（调整项索引见 §附-3）+ 用户拍板 D-J8~D-J11。**Step 1 / Step 2 编号不变**。

| 新编号 | 主题 | 旧编号 | 变动原因 |
| --- | --- | --- | --- |
| Step 3 | 可靠性（原子快照） | Step 3 | 不变；范围按 ADR-A 重写 |
| Step 4 | 资源回收（compaction） | **Step 5** | **提前**（D-J10 / D3） |
| Step 5 | 查询性能与可观测 | —— | **新增**（D-J9 / D2：低选择度兜底提前 + Metrics 可观测） |
| Step 6 | 构建性能与多线程 | **Step 4** | 顺延（D-J10 / D3）；embed 并行改 spike 制 |
| Step 7 | 精排 | Step 6 | 顺延 |
| Step 8 | 读写并发 | Step 7 | 顺延 |
| Step 9 | 相关性深化 | Step 8 | 顺延 |
| Step 10 | 场景机制 | Step 9 | 顺延 |

**历史文档的编号**：`v2-step1-design.md` / `v2-step2-design.md` 中的 Step 编号**指重排前**，
已在各自文档头部加注记；`plan.md`（V1）与 `p0~p6-design.md` 不受影响。

---

## 附-3：2026-09-07 复审 —— 调整项索引与建议执行顺序

> 复审当时的结论不另立文档，本表保留**可追溯性**：每条调整项对应本文的落地位置。

### A1~A7 索引

| # | 调整项 | 优先级 | 落地位置 |
| --- | --- | --- | --- |
| **A1** | Step 3 范围/验收按 ADR-A 重写（快照**本体**至今非原子，崩溃 = `SnapshotCorrupted` = 索引丢失，不是降级） | 高 | §4 Step 3（S3-a~e） |
| **A2** | Step 4（现 Step 6）的技术前提有一半被证伪（`intra_threads` 默认满核、批次已测、并行建图仅占 4%）⇒ embed 并行降级为 spike，并纳入从未评估的 **CoreML EP** | 高 | §4 Step 6（E1/E2/E3） |
| **A3** | NFR-03 `<120s` 无已识别路径可达 ⇒ 需求侧决策 | 高 | §2.3 D-J8、§4 Step 6 双口径、需求文档 NFR-03 |
| **A4** | 4 个 S 级任务插队：parallel 默认翻转 / 低选择度兜底 / `Metrics` 可观测 / 工程卫生 | 中 | §4「横切任务」T7-21、T7-24；Step 5 的 T7-22、T7-23 |
| **A5** | Step 5（现 Step 4）补「compaction 后必须重发 manifest」+ workload 脚本，并提前 | 中 | §4 Step 4 |
| **A6** | Step 6（现 Step 7）补三条前置：复现基线 / 模型下载可行性 / H3 未决 | 中 | §4 Step 7 |
| **A7** | 计划文档自身一致性（版本状态、进度表、NFR-04 数字、FR-31 优先级、NFR-11 口径） | 低 | §2 / §6 / §7 / 需求文档 |

### 建议执行顺序（含横切任务）

| 顺序 | 内容 | 量级 | 决策点 / 依赖 |
| --- | --- | --- | --- |
| 1 | **Step 3** 原子快照（S3-a~e，含 R19） | S | 无；排最前（Q-C3 定级「高（正确性）」） |
| 2 | **T7-24** 工程卫生（CI 触发 / gitignore / 分支清理） | S | 无 |
| 3 | **T7-21** `parallel_build` 默认翻转 | S | 独立 PR；须同步改 S2-T22 |
| 4 | **Step 5** T7-22 低选择度兜底 + T7-23 `Metrics` 可观测 | S | 新增 NFR-13；T7-23 是 Step 6 前置 |
| 5 | **Step 4** 墓碑回收（含 manifest 重发 + workload 脚本） | M | Step 1 |
| 6 | **Step 6** spike（E1/E2/E3）→ 视数据决定是否投 M 级 | S → M | NFR-03 按 D-J8 新口径 |
| 7 | **Step 7** 精排 | M | 先复现基线 + 定 H3 + 确认模型下载 |

> 排序原则：**正确性与确定性优先于性能**；收益不确定的（A2）排在确定的之后。

### 跟踪缺口（待用户决定，未在本轮处理）

- Step 3 ~ Step 7 目前**只有 Step 6（构建性能）有跟踪 issue**；标签只有
  `V2-Step2 / V2-Step4 / V2-Step6 / V2-Step6+`，**缺 Step 3 / Step 5**，
  且 `V2-Step4` 的语义已由「构建性能」变为「资源回收」⇒ 重命名会动到历史 issue。
