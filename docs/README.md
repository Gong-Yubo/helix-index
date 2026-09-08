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
| [`devel/plan-v2.md`](./devel/plan-v2.md)（V2 开发计划） | **V2 任务清单与进度**：V2.0 = Step 1~7，V2.1 = Step 8~10（`plan.md` 冻结为 V1 计划）。⚠️ **2026-09-07 重排**：Step 3 起编号已变（旧号→新号见文末 §附-2 映射表）；同日全量复审（7 处调整 + 4 个拍板）已并入，索引见 §附-3 | 实现者（V2 开工先看这个） |
| [`devel/v2-step1-design.md`](./devel/v2-step1-design.md)（V2 Step 1 设计） | **向量软删除 + 过滤下推**：`hnsw_rs::search_filter` 源码级行为核实、双路径设计、字段索引、风险表 R11~R18、S1-10 实测数据 | 实现者、评审者（Step 1 已完成） |
| [`devel/v2-step2-design.md`](./devel/v2-step2-design.md)（V2 Step 2 设计） | **图持久化 + 并行建图（ADR-A 方案 C）**：`hnsw_rs` 图 IO 源码级三条硬事实、manifest 唯一原子发布点、五道先验校验、降级可观测、S2-01~12 任务拆分、风险表 R19~R25、12K 实测（完整冷启动 ≈100ms） | 实现者、评审者（Step 2 已完成并合并，main `9db2c00`） |
| [`devel/v2-step3-design.md`](./devel/v2-step3-design.md)（V2 Step 3 设计） | **原子快照（崩溃一致性）**：`atomic_write` 通用原语（快照与 manifest 共用）、save 全序列崩溃窗口矩阵、tmp 孤儿回收、故障注入测试钩子（C′：`pub(crate)` 不进公开面）、R19 写路径残余收敛（`catch_unwind`）、D-S3-01~07 决策（**已全部拍板**）、S3-T1~T10 测试计划；**§10「实施结果」含评审回应与 S3-TI1~TI5 集成补强记录** | 实现者、评审者（**实现完成**：PR #27 含评审回应，CI 全绿，待合并） |
| [`devel/v2-step4-design.md`](./devel/v2-step4-design.md)（V2 Step 4 设计） | **墓碑物理回收（compaction，T7-12 / FR-30 / Q-C2）**：三条膨胀路径逐行盘点（图 sidecar ≈2.6KB/点 vs 快照正文 ≈6B/chunk，约 440:1）、`flush` 侧幽灵向量时序缺口、**按存活集重新物化 + ID 重编号**方案（含 BM25 逐位一致的证明骨架）、**`compact()` 必须先 `commit()`**（D-S4-10）、manifest 重发铁律如何被既有 `save` 链路自动满足、`TombstoneStats` / `CompactionReport` 可观测、churn workload 脚本设计、D-S4-01~10 决策、S4-T1~T13 测试计划 | 实现者、评审者（**v0.3 已拍板，核心可开工**） |
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
└─ 项目风险（P1~V2）              ──►  技术风险（R1~R25）在架构文档
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
