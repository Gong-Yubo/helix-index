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
  构建 12K chunk 含 embedding **236.7s 未达标**（ort CPU 推理吞吐 ~51 条/s）；
  快照加载 53~107ms ✅，但 HNSW 图重建 11.6s（图不持久化）。

## 数据来源与引用

- 评测数据集 **T2Ranking**（THUIR，SIGIR 2023，Apache-2.0）：
  `Xiaohui Xie, Qian Dong, Bingning Wang, et al. T2Ranking: A Large-scale Chinese
  Benchmark for Passage Ranking. SIGIR 2023.`（HuggingFace `THUIR/T2Ranking`）
- 检索模型 `Xenova/bge-small-zh-v1.5`（BAAI bge 系列中文语义向量）。

## 已知局限

- **冷启动**：HNSW 图不持久化，加载快照后需重建（12K 段约 11.6s），万级冷启动不达秒级（D7，交 P6）。
- **构建吞吐**：本地 CPU embedding 构建 12K chunk 需 236.7s，未达 120s 目标（NFR-03）。
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
- 开发文档索引：[`docs/README.md`](docs/README.md)，开工前先读 `docs/devel/plan.md`

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
