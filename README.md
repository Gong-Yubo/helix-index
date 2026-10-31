# HelixIndex

面向 **Agent 场景**的通用检索引擎内核（Rust，单机 MVP）。

Agent 作为检索调用方，与人类搜索有五个根本差异：query 由 LLM 生成（含噪声）、
结果直接进 prompt（无翻页）、处于多轮循环关键路径（被高频调用）、需要自查检索质量、
Agent 自己零散写入。本项目把这些差异翻译为**接口与质量属性约束**，而非领域逻辑——
内核不做企业知识库 / 记忆 / 代码检索的领域假设，场景层后接。

## 快速上手（秒级跑通，无需模型）

```bash
# 用 30 篇 demo 语料建索引并检索（BM25，纯 CPU 秒级）
cargo run -p helix -- build --input data/corpus.jsonl --output /tmp/demo.snapshot
cargo run -p helix -- search --index /tmp/demo.snapshot --mode bm25 "如何加快检索速度"

# 元数据过滤（demo 语料带 topic 字段）
cargo run -p helix -- search --input data/corpus.jsonl --mode bm25 --filter topic=vector "向量检索"

# 三路对比（hybrid 需先下载 bge-small-zh-v1.5，约 91MB）
cargo run -p helix -- search --input data/corpus.jsonl --mode hybrid "向量检索与BM25融合"

# 效果评测（P5，需 T2Ranking 评测语料，见下）
cargo run -p helix -- bench --input data/t2-corpus.jsonl --queries data/t2-queries.jsonl
```

库使用端到端示例：`cargo run -p helix-core --example search_basic`
（建索引 → hybrid 检索 → `to_context_block()` 拼 prompt 上下文块）。

> **完整文档见 [`docs/user-guide.md`](docs/user-guide.md)**：
> CLI 全参数参考、典型工作流（建库 / 过滤 / explain / 对比 / 评测）、
> 六 trait 替换矩阵、feature 选用建议、性能预期与已知坑。
> 本文件的"快速上手"只是最短路径。

## 架构速览

```
analyze ──► index（倒排/正排/统计量）──► retriever（单路召回：BM25 / 向量）
    │                                        │
embed ──► vector（向量存储与近邻检索）────────┘
                                              ▼
                                       fusion（多路合并，RRF）
                                              ▼
                                        rerank（重排，默认 NoOp）
                                              ▼
                              query（编排以上全部，不自己实现算法）
```

**六个核心 trait**（全部能力抽象为 trait，可替换实现）：

| trait | 职责 |
| --- | --- |
| `Analyzer` | 文本 → Token 序列（中英混合分词） |
| `Embedder` | 文本 → 向量（BGE 查询侧加 instruction 前缀） |
| `VectorIndex` | 向量存储与近邻检索（HNSW，P4 定稿） |
| `Retriever` | 单路召回（不与其他 lane 交互） |
| `FusionStrategy` | 多路合并（不回捞正文） |
| `Reranker` | 重排（默认 NoOp） |

**feature flags**：

| feature | 说明 |
| --- | --- |
| `local-embed`（默认） | 本地 embedding（fastembed + bge-small-zh-v1.5） |
| `remote-embed` | 远程 embedding（P2 预留） |
| `positions` | posting 存储位置信息 |
| `charabia` | charabia 分词对照（T5-09 验证性依赖，非默认） |

## 评测结论摘要（P5，详见 `docs/devel/eval-report.md`）

在 T2Ranking 中文检索数据集（12K 段落 / 320 query，4 级专业标注）上的实测：

- **三路定位**：BM25 作召回兜底与词面精确匹配主力；向量作语义检索主力；
  hybrid 在 **MRR@10=0.6922、Recall@10=0.6829** 上全场最高（Agent 最看重的两项），
  仅 NDCG@10 略低于向量单路（0.5183 vs 0.5249，p=0.385 不显著）。
- **参数定稿**：BM25 `k1=1.5 / b=0.75`（16 格网格）；RRF `k=60 / weights=[1, 1.5]`（weights 诊断）。
- **BM25 正确性**：对 tantivy 基线 NDCG@10 相对差 **0.31%**，Top-10 重叠率 97%。
- **分词对照**：charabia（jieba 底层 + kvariants 简繁归一化）全面落后自研链（NDCG −5.9%），保留自研。
- **性能**（release，macOS）：BM25 P99 3.9ms / 向量 8.4ms / 混合 8.7ms ✅；
  **完整冷启动 ≈100ms** ✅（V2 Step 2 图持久化后，此前 ≈11.6s；图 sidecar 缺失时
  降级重建仍 ～10s，显式可观测）；
  构建 12K chunk 含 embedding **225.5～236.7s ✅**（落在 NFR-03「首次全量 <240s」口径内；
  原单口径 <120s 经评审拍板不可达；**增量追加口径已交付** —— Step 6 实测 **23.705s < 27.973s**，余量 15.3%）。

## 当前进展（V2 路线图）

V1 内核（`P0~P6`）与 **V2 全部步骤均已交付** —— **V2.0**（`Step 1~7`：质量与性能夯实 + 精排）
与 **V2.1**（`Step 8~10`：读写并发 + 相关性深化 + 场景机制）。任务级清单与门槛判定见
[`docs/devel/plan-v2.md`](docs/devel/plan-v2.md)：

- **Step 1 · 向量软删除 + 过滤下推** ✅ 已交付
- **Step 2 · 图持久化 + 并行建图** ✅ 已交付——完整冷启动 ≈11.6s → **≈100ms**（NFR-04 达标），
  同快照两次加载结果逐位一致（NFR-06）
- **Step 3 · 原子快照（崩溃一致性）** ✅ 已交付——save 走 tmp + fsync + rename，崩溃后
  load 要么旧快照要么新快照、绝不半截文件；故障注入测试证明不变式；fsync 代价实测 +0.027～0.035s
- **Step 4 · 墓碑物理回收（compaction）** ✅ 已交付——10K churn 实测三个体积不无界增长
  （graph+data **84.6 → 33.7MB**，-60%）
- **Step 5 · 查询性能与可观测** ✅ 已交付——低选择度暴力兜底 + `Metrics` 暴露；
  10 万级降档 P99 **7.06ms**（常规）/ **22.84ms**（降级字段），NFR-13 双口径达标
- **Step 6 · 构建性能与多线程** ✅ 已交付——增量追加 **23.705s**（NFR-03 ② 达标，余量 15.3%）；
  ⚠️ embed 并行 spike **判「不投」**（峰值 RSS 涨 96% / 289%，远大于收益）
- **Step 7 · 精排** ✅ 已交付——`bge-reranker-v2-m3` 接入 + 精排窗口（`R = 20`）；
  ⚠️ 评测集与目标场景不同构 ⇒ 结论不外推
- **Step 8 · 读写并发** ✅ 已交付——`searcher()` 与写入并存（delta 分段）；
  ⚠️ `NFR-14` 条目**整体保持「打开」**（两条保留：样本数不可比 / 尾部扰动不可复现）
- **Step 9 · 相关性深化** ✅ 已交付——自适应融合 **判「不投」**（适用范围收窄 + 另立风险 `R59` + 四条复审触发条件）
- **Step 10 · 场景机制** ✅ 已交付——命名空间 + 四个后处理器机制（时间衰减 / MMR / token budget / 多 query）；
  **后四个默认关 ⇒ 零行为变化**（命名空间是既有 `Filter` 路径、无开关）；prefilter spike **判「不投」**
  （`Q-1` 阈值缺陷另行跟踪）

⚠️ **已交付 ≠ 零遗留**：未关闭的风险（`R11` / `R13` / `R17` / `R18` / `R41` / `R43` /
`R59` / `R60` / `R64`）与未决项（`Q10-2` 等）**逐条有 issue 承载**，清单见
[`docs/devel/architecture-design.md`](docs/devel/architecture-design.md) **§14 导读**的
「V2 结项时的风险台账摘要」与 [`docs/devel/plan-v2.md`](docs/devel/plan-v2.md) §6 / §7。

## 数据来源与引用

- 评测数据集 **T2Ranking**（THUIR，SIGIR 2023，Apache-2.0）：
  `Xiaohui Xie, Qian Dong, Bingning Wang, et al. T2Ranking: A Large-scale Chinese
  Benchmark for Passage Ranking. SIGIR 2023.`（HuggingFace `THUIR/T2Ranking`）
- 检索模型 `Xenova/bge-small-zh-v1.5`（BAAI bge 系列中文语义向量）。

## 已知局限

- **降级路径冷启动**：图 sidecar 缺失 / 损坏 / 版本错配时，加载快照后需重建 HNSW 图
  （12K 段约 10s）——降级**显式可观测**（`GraphStatus` / NFR-07），绝不静默。
- **图持久化的磁盘代价**：sidecar 三件套使磁盘占用 1.6×（52.3MB → ≈84MB，R22）。
- **构建吞吐**：embedding 主导（ort CPU ～51 条/s）；首次全量 <240s 与**增量追加**
  （NFR-03 两条口径）**均已达标**（增量 **23.705s < 27.973s**，Step 6 交付）；
  ⚠️ **建库路径峰值 RSS**（实测 `2.32~3.33 GB`）与检索路径口径（NFR-05 的 372MB）**不是一回事**，
  按 NFR-05 选机器会使建库时 OOM —— 数值待定，见架构 `R41`。
- **V2.1 的四个后处理器机制默认关**：时间衰减 / MMR / token budget 与多 query（入口 `Searcher::search_multi`）
  默认不启用 ⇒ 既有调用路径**零行为变化**；启用方式见 `docs/user-guide.md` §2.6。
  ⚠️ **命名空间不在此列** —— 它不是「开关」，而是既有 `Filter` 路径（用就生效、不用就没有）。
- **评测代理**：T2Ranking 查询来自人类搜索日志，作为 Agent 生成 query 的代理存在分布差异；
  12K 子集任务难度低于 230 万全库，绝对指标不可与论文基线直接对比，仅量级对照。
- 详见 `docs/devel/eval-report.md` 第 9 节「数据诚信声明」。

## 开发

```bash
make fmt && make lint && make test && make deny   # 阶段门槛（CI 同样跑这些）
make eval-quality                                 # 效果评测
make eval-perf                                    # NFR 性能实测
make report CHECK=--check                         # 报告数字逐格对账
```

- 使用文档：[`docs/user-guide.md`](docs/user-guide.md)
- 开发文档索引：[`docs/README.md`](docs/README.md)；开工前先读
  [`docs/devel/plan-v2.md`](docs/devel/plan-v2.md)（**V2 已交付**，`plan.md` 已冻结为 V1 计划）

## 评测语料（可选）

`data/t2-corpus.jsonl` / `data/t2-queries.jsonl` 已随仓库分发，`bench` 开箱即用。
如需**重新装配**该子集（改抽样参数 / 上游数据集更新），先下载 T2Ranking 原始数据
（约 3.5GB，不入库）：

```bash
./data/download_t2ranking.sh                 # → data/t2ranking/（带 sha256 校验）
cargo run --release -p helix-core --example t2_prep -- \
    --t2ranking data/t2ranking --out data
```

## 许可

**MIT**（见 [`LICENSE-MIT`](LICENSE-MIT)）。第三方数据集、模型与运行时的许可及引用义务
见 [`NOTICE`](NOTICE)。
