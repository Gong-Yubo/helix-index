# HelixIndex 文档索引

面向 Agent 场景的通用检索引擎内核（Rust，单机 MVP）。

> 使用文档见 [`user-guide.md`](./user-guide.md)；开发文档存放于 [`devel/`](./devel) 目录。

## 使用文档（面向使用者与接入方）

| 文档                              | 内容                                                             | 读者        |
| ------------------------------- | -------------------------------------------------------------- | --------- |
| [`user-guide.md`](./user-guide.md) | **CLI 全参数参考、典型工作流、六 trait 替换矩阵、feature 选用、性能预期与已知坑** | 使用者、场景层接入方 |
| [`../README.md`](../README.md)  | 项目定位、最短上手路径、架构速览、评测结论摘要                                     | 所有人       |

## 开发文档（面向实现者）

| 文档                                        | 内容                | 读者            |
| ----------------------------------------- | ----------------- | ------------- |
| [`devel/plan.md`](./devel/plan.md)（开发计划） | **P0~P7 任务级清单、阶段门槛、进度追踪** | 实现者（想直接开工先看这个） |
| [`devel/p0-design.md`](./devel/p0-design.md)（P0 设计说明） | **P0 详细设计**：本机环境实测、配置文件全文、风险兜底、执行记录 | 实现者（P0 已完成） |
| [`devel/p1-design.md`](./devel/p1-design.md)（P1 设计说明） | **P1 详细设计**：tantivy 源码核对、BM25 手算值、执行记录 | 实现者（P1 已完成） |
| [`devel/p2-design.md`](./devel/p2-design.md)（P2 设计说明） | **P2 详细设计**：fastembed / instant-distance 源码核对、BGE 前缀、距离换算、决策点 | 实现者（P2 已完成） |
| [`devel/p3-design.md`](./devel/p3-design.md)（P3 设计说明） | **P3 详细设计**：RRF 融合手算值、Explain 语义、确定性、决策点 | 实现者（P3 已完成） |
| [`devel/p4-design.md`](./devel/p4-design.md)（P4 设计说明） | **P4 详细设计**：hnsw_rs 调研、快照格式、向量 A/B 方案、决策点 | 实现者（P4 已完成） |
| [`devel/p5-design.md`](./devel/p5-design.md)（P5 设计说明） | **P5 详细设计**：T2Ranking 数据集选型与转换管线、评测指标（分级 NDCG）、bench 设计、NFR 实测协议、决策点 | 实现者（P5 已完成） |
| [`devel/p6-design.md`](./devel/p6-design.md)（P6 设计说明） | **接口重构**：issue #1 的业界做法调研、`SearchIndex` / `Searcher` 门面设计、写缓冲与 commit 语义、配置指纹、任务清单与决策点（**已拍板 v2.0**） | 实现者、评审者（P6 已定稿） |
| [`devel/plan-v2.md`](./devel/plan-v2.md)（V2 开发计划） | **V2 任务清单与进度**：V2.0 = Step 1~7，V2.1 = Step 8~10（`plan.md` 冻结为 V1 计划）。⚠️ **2026-09-07 重排**：Step 3 起编号已变（旧号→新号见文末 §附-2 映射表）；同日全量复审（7 处调整 + 4 个拍板）已并入，索引见 §附-3。⚠️ **2026-09-16 V2.1 计划修订**（审视结论 15 条并入）：Step 8 补 **T7-25（硬前置：修 R34）/ T7-26（会话池，解 R43）**、Step 9 **顺序修正（T7-16 → T7-14）**、Step 10 补 **T7-27（prefilter / 排序键索引）**、新增 **FR-34 / FR-35 / NFR-14 / NFR-15** 草案与 **§4.0 H4~H7**、新增「V2.1 横切任务」区，索引见 **§附-4**。✅ **2026-09-16 Step 8 详细设计已出、待评审**（`v2-step8-design.md` v0.1，纯文档）：**H4 / H5 / H6 三条开工前置结清**（H4 **不开 ADR-B** / H5 取 ①+② / H6 口径改直接计时 + 拟值）、**4 条需拍板**（D-S8-01 / D-S8-06 / D-S8-09 / D-S8-11）、风险 **R49 ~ R54** 已落架构 **§14.6** | 实现者（V2 开工先看这个） |
| [`devel/v2-step1-design.md`](./devel/v2-step1-design.md)（V2 Step 1 设计） | **向量软删除 + 过滤下推**：`hnsw_rs::search_filter` 源码级行为核实、双路径设计、字段索引、风险表 R11~R18、S1-10 实测数据 | 实现者、评审者（Step 1 已完成） |
| [`devel/v2-step2-design.md`](./devel/v2-step2-design.md)（V2 Step 2 设计） | **图持久化 + 并行建图（ADR-A 方案 C）**：`hnsw_rs` 图 IO 源码级三条硬事实、manifest 唯一原子发布点、五道先验校验、降级可观测、S2-01~12 任务拆分、风险表 R19~R25、12K 实测（完整冷启动 ≈100ms） | 实现者、评审者（Step 2 已完成并合并，main `9db2c00`） |
| [`devel/v2-step3-design.md`](./devel/v2-step3-design.md)（V2 Step 3 设计） | **原子快照（崩溃一致性）**：`atomic_write` 通用原语（快照与 manifest 共用）、save 全序列崩溃窗口矩阵、tmp 孤儿回收、故障注入测试钩子（C′：`pub(crate)` 不进公开面）、R19 写路径残余收敛（`catch_unwind`）、D-S3-01~07 决策（**已全部拍板**）、S3-T1~T10 测试计划；**§10「实施结果」含评审回应与 S3-TI1~TI5 集成补强记录** | 实现者、评审者（**已完成并合并**：PR #27 → `ed25d5c`，2026-09-08） |
| [`devel/v2-step4-design.md`](./devel/v2-step4-design.md)（V2 Step 4 设计） | **墓碑物理回收（compaction，T7-12 / FR-30 / Q-C2）**：三条膨胀路径逐行盘点（图 sidecar ≈2.6KB/点 vs 快照正文 ≈6B/chunk，约 440:1）、`flush` 侧幽灵向量时序缺口、**按存活集重新物化 + ID 重编号**方案（含 BM25 逐位一致的证明骨架）、**`compact()` 必须先 `commit()`**（D-S4-10）、manifest 重发铁律如何被既有 `save` 链路自动满足、`TombstoneStats` / `CompactionReport` 可观测、churn workload 脚本设计、D-S4-01~10 决策、S4-T1~T13 测试计划；**§10「实施结果」（v0.4）**含 10K churn 实测（graph+data 84.6→33.7MB）、Q1~Q7 结案、评审响应与集成补强记录 | 实现者、评审者（**v0.4 实现完成**：PR #29 / #30 / #31 均已合入 `main`） |
| [`devel/v2-step5-design.md`](./devel/v2-step5-design.md)（V2 Step 5 设计） | **查询性能与可观测（T7-22 / T7-23 / NFR-13 / R18）**：R18 的整图遍历机理（`hnsw.rs:983-992` 无 fast-return + `:1019` 堆未满关闭剪枝，分水岭 = `ef=120`）、**`hnsw_rs` 存储访问能力源码核实**（**全量遍历 = `&PointIndexation` 的 `IntoIterator`**，每点恰一次；⚠️ `get_layer_iterator(0)` 只走 layer 0，会漏掉 `level ≥ 1` 的点约 `1/M`（M=32 ⇒ ~3.1%）；`Point::get_v()` 零拷贝 + `get_origin_id()` = `ChunkId`）、**三路径设计**（A 热路径 / B filtered-ANN / **C 精确扫描**，策略归后端 `prefers_exact` + 编排层记账）、成本分解（路径 B 与 C 同为 `O(N)`，常数差 5~20×）、`Metrics` **四处断链**定位 + 进 `SearchResponse`、阈值标定实验设计、D-S5-01~09 决策、S5-T1~T13 测试计划、风险 R31~R35（**已写入架构 §14.3**） | 实现者、评审者（**v0.5 已完成并合并**：实现 PR **#36** 已并入 main `48c0ac7`、收尾 PR **#37** 已并入 main **`590315d`**；**S5-04 标定定稿** —— 阈值 **8192**（两端点实测、归一插值交叉点 ≈ 9700，原初值 1024 过保守）、**NFR-13 双口径**（常规 ≤20ms / 降级字段 ≤35ms）；**Q1~Q5 全部结案**；标定全表见 `eval-report.md` §8.9） |
| [`devel/v2-step6-design.md`](./devel/v2-step6-design.md)（V2 Step 6 设计） | **构建性能与多线程（T7-09 / T7-11 / T7-17）**：构建耗时的真实分解（embed 占 **93%** ⇒ 任何不碰 embed 的优化上界 <7%）、embed 串行的形态（`Mutex<TextEmbedding>` 全程持锁）、**A2 前提的三处更正**（fastembed **无 coreml 透传** / EP 类型名实为 **`ort::ep::CoreML`** / 实际 ONNX batch 是 **64** 而非 256）、**E1/E2/E3 spike 设计与决策门**（30% 吞吐 / 20% 内存）、增量构建的**骨架已在**（`content_hashes` 入快照 ⇒ load 后仍可短路跳过 embed）、`build --index` 追加的 **ID 不复用证明**、并发检索的**索引读路径零共享可变状态**实证【⚠️ **S6-10 更正**：该先验**只对索引读路径成立**，**端到端** vector / hybrid 还含**查询侧编码**（`Mutex<TextEmbedding>`）】、D-S6-01~09 决策、S6-T1~T14 测试计划（**末列已按实现实况逐条回写：`✅` 落地 / `⛔` 随 S6-04 取消 / `⏸` 随 S6-05 挂起**）、风险 **R36~R43**（R36/R37 **消解**、R38/R39 **关闭**、R40 **上锁**、**S6-10 新增 R41 / R42 / R43**）、未决 **Q1~Q8**（**Q4~Q8 已拍板**：NFR-10 口径 = **方案 A**（`--threads 4` QPS ≥ `--threads 1` × 2.5，逐位一致为前置条件）/ 10% 单位 = **文档数** / Q6 = 方案 A（硬失败，不加兼容开关）/ Q7 = 沿用既有 `--no-graph-persist` / **Q8 = 方案 C**（NFR-05 补口径限定词 + 立 R41）；**Q1 / Q2 已由 spike 结案（判「不投」）**、**Q3 随 S6-05 取消而挂起**） | 实现者、评审者（**v0.4：Step 6 实现全部完成、S6-10 收尾已回写**，2026-09-13）——设计 PR **#38**（F1~F7 全采纳、D-S6-05 / D-S6-08 拍板）→ spike S6-01~03（**#40，判「不投」**）→ 增量构建 S6-06/07/09（**#43**）→ 并发采集点 S6-08（**#44**）→ **收尾 S6-10**；需求随之 **v1.15**、架构 **v1.14**、`plan-v2.md` **v0.15** |
| [`devel/v2-step7-design.md`](./devel/v2-step7-design.md)（V2 Step 7 设计） | **精排（Reranker）接入（T7-01 / FR-18 / NFR-12 / issue #23）**：**开工前置三条的结账单**（① 基线复现**复现出「图漂移 ~0.6%」**——P5 锚点 `hybrid MRR@10(1)=0.6922` 本次 0.68815、而 **bm25 三项 Δ=0** ⇒ **A/B 必须钉在同一张冻结图上**；② 模型已**实下载并验 sha256**：**`rozgo/bge-reranker-v2-m3`（非 BAAI 官方，官方库无 ONNX）**、`model.onnx.data` ≈2.19GB；③ **H3 = 可配 `R`、默认 20（2026-09-16 定稿）**）；**五个设计期新发现 A~E**（候选池必须与 `candidate_k` 联动否则 R 被静默夹到 3k / 回捞组装成本随窗口线性放大 / `PaddingStrategy::BatchLongest` 让跨批次分数不可逐位复现 / `TextRerank::rerank` 返回全量排序需按 `index` 回填 / **512 token 截断 vs T2Ranking 最长段落 76,895 字符**）；**`Reranker::candidate_window`（provided，默认 `k` ⇒ 零回归）** + `candidate_k` 联动 + `explain` 组装推迟 + `LocalReranker`（gate `local-rerank`）+ `Metrics.rerank_window` / `rerank_elapsed` + `Explain.rerank_score`；**标定协议对齐「四条抗噪声规则」+ 可证伪决策门**；确定性命探针 P1/P2/P3；D-S7-01~10 决策（**其中 4 项待评审拍板**）、S7-T1~T12 测试计划、S7-01~05 任务拆分与 PR 切分、风险 **R44~R48**（**已写入架构 §14.5**）、未决 Q1~Q7 | 实现者、评审者（**v0.3：实现完毕 + S7-05 收尾**，2026-09-16）—— 设计 v0.1（**#51**）→ 实现 **#52**（内核 `candidate_window` + `LocalReranker`）/ **#53**（编排层窗口打通）/ **#55**（CLI 接线 + `eval_rerank.sh`）/ **#56**（**S7-04 标定**：默认 `R=20`、NFR-12 实测、R48 对照）→ **收尾 S7-05**（**NFR-12 定稿** = 双口径 + 仅开启时适用；**R44 内存数值回填** 2.64 GiB / 7.71×；**R48 收窄后关闭**；**R47 全部实测**；**T8/T9/T10 本地实跑**；锚点改**符号指代**）。需求随之 **v1.18**、架构 **v1.17**、`plan-v2.md` **v0.18** |
| [`devel/v2-step8-design.md`](./devel/v2-step8-design.md)（V2 Step 8 设计） | **读写并发（不可变视图 + 追加段 + 原子发布）（T7-25 / T7-06 / T7-26 / T7-18 / issue #58 / NFR-14 / NFR-11）**：**开工前置三条结账单**（**H4** = `save()` 前先 `merge_all()` ⇒ 落盘仍**单段** ⇒ `FORMAT_VERSION` 保持 **2**、`GraphManifest` 一字不改 ⇒ **不开 ADR-B**；**H5** = ① 读路径不取写锁 + ② 写不抬高读延迟，**③「读能看到未 commit 的写」明确不做**；**H6** = 写延迟口径改 **`commit()` 端到端直接计时** + 拟值 `P50 ≤ 1.0s / P99 ≤ 2.0s`）；**口径冲突择一**（取 NFR-11 ⇒ `requirements-spec.md` §5.3.5 的「写入后立即可查」改写为「`commit()` 后」，**只改措辞不改语义**）；**三个设计期新发现**（**A** BM25 **可加** ⇒ 全局精确整数统计量 + term 外层累加 ⇒ **跨段与单段逐位一致是结构性结论**；**B** fastembed **无 `sessions` 参数** ⇒ 会话池 = 每 worker 一个 `TextEmbedding`、挂起的 **Q3 改述为 Q3′**；**C** R36 的「不投」是**建库口径**、**不可外推**到查询侧）；四件事的落点（**T7-25** R34 逐层 `get_layer_iterator` 修法 / **T7-06** 不可变 `View` + delta 段 + 原子发布 / **T7-26** 查询侧会话池 / **T7-18** 旧 API `#[deprecated]`）；**D-S8-01 ~ 12**（**4 条需拍板**：持久化形态 / 旧 API 处置 / 向量合并取形 / 会话池投不投）、**S8-T1 ~ T16** 测试计划（**S8-T4「跨段 BM25 逐位一致」是最重要的一条**）、**S8-01 ~ 09** 任务与 **7 段 PR 切分**、风险 **R49 ~ R54**（**已写入架构 §14.6**）、未决 **Q1 ~ Q8** | 实现者、评审者（**v0.1：设计已出、待评审**，2026-09-16，**纯文档、`.rs` 零改动**） |
| [`devel/eval-report.md`](./devel/eval-report.md)（P5 评测报告） | **P5 评测结论**：三路对照、分桶与符号检验、tantivy 基线、网格/RRF 定稿、charabia 对照、NFR 实测、数据诚信声明 | 决策者、接入方 |
| [`devel/v1-finish-design.md`](./devel/v1-finish-design.md)（v1 收尾设计） | **使用文档 / 评测脚本 / CI / 改名与发布 / 工程收尾**的任务清单、执行顺序与决策点（含外部评审修正记录） | 实现者（v1 收尾已完成，V1 全量交付） |
| [`devel/requirements-spec.md`](./devel/requirements-spec.md)（需求分析说明书）     | 做什么 / 为什么 / 怎么验收  | 决策者、场景层接入方、实现者 |
| [`devel/architecture-design.md`](./devel/architecture-design.md)（架构设计说明书） | 怎么做 / 为什么这么做      | 实现者、代码评审者     |
| [`devel/thirdparty.md`](./devel/thirdparty.md)（第三方库调研） | 哪些能力用现成库、哪些自研，含 License 与 MSRV 核查 | 实现者、评审者     |
| [`devel/archive/requirements-and-design_v1.0.md`](./devel/archive/requirements-and-design_v1.0.md)（需求分析与设计 v1.0） | v1.0 合并版，仅供历史追溯 | —             |

## 文档间关系

```
devel/requirements-spec.md        devel/architecture-design.md
（需求分析说明书）                    （架构设计说明书）
├─ FR-xx / NFR-xx 唯一定义源   ──►  只引用编号，不重复定义
├─ 术语表唯一定义源               ──►  直接使用
├─ 成功标准 / 评测指标             ──►  第 12 章做「阶段 → 需求」覆盖映射
└─ 项目风险（P1~V2）              ──►  技术风险（R1~R54）在架构文档
```

**修改规则**：需求变更只改需求文档；实现变更只改架构文档。两边都不复制对方的内容，避免定义漂移。

### 评测与基准脚本

| 脚本 | 作用 | 备注 |
| --- | --- | --- |
| `scripts/eval_perf.sh` | **NFR-02/04/05 实测**（T2Ranking 真实语料）。NFR-04 自 V2 Step 2 起按**三口径**分别计时并自动求和判定：快照加载 + 图 sidecar 加载 = 完整冷启动（<2s），图重建单列为降级路径 | `make eval-perf` |
| `scripts/eval_filter.sh` | **过滤选择度扫描**：8 档位 × 3 模式 → 选择度 × 延迟 × 召回三元数据 + NFR-02 自动判定 | 需先跑 `gen_synth_corpus.py` |
| `scripts/gen_synth_corpus.py` | **确定性合成 fixture**（固定 seed）：主题化文本聚类 + 可精确控制的选择度档位，含档位自检 | T2Ranking 无法控制选择度，故选择度实验用它 |
| `data/download_t2ranking.sh` | **T2Ranking 原始数据下载**（约 3.5GB，带 sha256 校验、断点续传、支持 `HF_ENDPOINT` 镜像） | 仅在需**重新装配**评测子集时运行；装配用 `t2_prep` example |

> ⚠️ 延迟数字**在 CI 共享 runner 上不具可引用性**，两个 eval 脚本都只用于**本地**实测与决策。

## 阅读顺序

0. `devel/plan.md`（执行顺序与任务清单，想直接开工先看这个）
1. 需求文档第 2 章（背景与目标）→ 第 4 章（Agent 场景特殊性，立论基础）
2. 需求文档第 5、6 章（FR / NFR）→ 第 7 章（明确不做）
3. 架构文档第 2 章（关键决策摘要）→ 第 3 章（数据流）
4. 架构文档第 5 ~ 7 章（抽象 / 数据结构 / 算法）→ 第 9.4 节（ADR）
