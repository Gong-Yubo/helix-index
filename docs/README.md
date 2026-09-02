# index-demo 文档索引

面向 Agent 场景的通用检索引擎内核（Rust，单机 MVP）。

> 全部开发文档存放于 [`devel/`](./devel) 目录下，本文件仅作索引。

## 文档清单

| 文档                                        | 内容                | 读者            |
| ----------------------------------------- | ----------------- | ------------- |
| [`devel/plan.md`](./devel/plan.md)（开发计划） | **P0~P6 任务级清单、阶段门槛、进度追踪** | 实现者（想直接开工先看这个） |
| [`devel/p0-design.md`](./devel/p0-design.md)（P0 设计说明） | **P0 详细设计**：本机环境实测、配置文件全文、风险兜底、执行记录 | 实现者（P0 已完成） |
| [`devel/p1-design.md`](./devel/p1-design.md)（P1 设计说明） | **P1 详细设计**：tantivy 源码核对、BM25 手算值、执行记录 | 实现者（P1 已完成） |
| [`devel/p2-design.md`](./devel/p2-design.md)（P2 设计说明） | **P2 详细设计**：fastembed / instant-distance 源码核对、BGE 前缀、距离换算、决策点 | 实现者（执行 P2 前必读，当前**待确认**） |
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
├─ 成功标准 / 评测指标             ──►  第 11 章做「阶段 → 需求」覆盖映射
└─ 项目风险（P1~P4）              ──►  技术风险（R1~R10）在架构文档
```

**修改规则**：需求变更只改需求文档；实现变更只改架构文档。两边都不复制对方的内容，避免定义漂移。

## 阅读顺序

0. `devel/plan.md`（执行顺序与任务清单，想直接开工先看这个）
1. 需求文档第 2 章（背景与目标）→ 第 4 章（Agent 场景特殊性，立论基础）
2. 需求文档第 5、6 章（FR / NFR）→ 第 7 章（明确不做）
3. 架构文档第 2 章（关键决策摘要）→ 第 3 章（数据流）
4. 架构文档第 5 ~ 7 章（抽象 / 数据结构 / 算法）→ 第 9.4 节（ADR）
