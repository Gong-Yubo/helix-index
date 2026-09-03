# P5 语料、评测与调参 — 设计说明

> ⚠️ **历史文档，正文保留原名未改写**：项目已于 2026-09-03 正式定名 **HelixIndex**
> （库 crate `index-core` → `helix-core`，CLI 命令 `idx` → `helix`）。
> 文中出现的旧名均指改名前的同一项目，未逐处改写以保留历史原貌（D-E4）。

| 项目    | 内容 |
| ----- | --- |
| 版本    | **v1.3（已确认）** |
| 日期    | 2026-09-03 |
| 状态    | **已完成**（2026-09-03：T5-01~T5-09 全部落地，评测结论见 `eval-report.md`；D8 已移除 instant-distance） |
| 上游    | `docs/devel/plan.md` 第 10 章（P5，T5-01 ~ T5-09）、`requirements-spec.md` 第 9 章（验收与评测）、`architecture-design.md` 第 7.2 / 9.4 节 |
| 关联    | FR-23（示例语料）、NFR-01~05（实测校准）、NFR-06（确定性）、ADR-004（RRF）、ADR-007（tantivy 基线）、风险 R6（中文参数）/ R7（RRF k）、项目风险 P1~P3 |
| 环境    | Rust 1.90.0 / P0~P4 已完成（hnsw_rs 生产路径、快照 IDX1、过滤已落地） |
| v1.2 变更 | **评测数据源从"手写语料 + 自标注"改为开源 T2Ranking 数据集**（Apache-2.0，SIGIR 2023）：① T5-01/T5-02 变为下载转换管线，标注者偏差（v1.1 最大方法论风险）**整体消除**；② 相关性从二元改为 **4 级分级**（专业标注）；③ 查询量 40 → **~320 条**（统计功效提升）；④ 合成压测语料废弃，NFR 实测改用真实段落；⑤ 新增数据集选型章与转换器设计。详见附录 C |
| v1.3 变更 | **外部评审修订**：① 承认并处理 **hnsw_rs 无固定 seed**（核源码属实）→ 新增 `--runs` 重复运行校验、风险 R-P5-13、D7 重开 hnswio 图持久化条件分支；② 新增 8.6 向量路诊断（`--vector-index brute` + ef_search 校准 + `with_ef_search` 注入口）；③ 8.2 补逐 query 配对符号检验；④ 网格表补 Recall/MRR 列且定稿参数不退化；⑤ 主表 thr=1/thr=2 双列；⑥ 段落→chunk 单 chunk 断言 + 抽样算法精确化；⑦ 勘误：延迟样本数 6,000→6,400、NFR-03 口径修正；⑧ `--json` 提为验收必需；⑨ 报告记录 git commit / HF revision / 模型 id。详见附录 C |

---

## 1. P5 要达成什么

**一句话**：用数据回答"**混合检索是否优于单路**"，把 NFR 从目标值变成实测值——这是整个项目立论的验证时刻。

**判定标准（Gate，对齐 plan.md 第 13 章）**

1. 三路对照实验有分桶数据（BM25 / Vector / Hybrid）
2. BM25 参数经 `k1 × b` 网格搜索定稿（NDCG@10 选优，风险 R6）
3. NFR-02~05 全部替换为实测值，或如实标注"未达标 + 原因"
4. 评测报告含**诚信声明**（T5-08：若混合未优于最优单路，如实记录并分析原因，**不得粉饰**）

> **v1.2 的本质改善**：v1.1 及之前的设计依赖"实现者自己写语料、自己标注"，最大的方法论风险是标注者偏差（容易无意识标出"检索器容易找到的"相关性）。改用 T2Ranking 后，query 与相关性标注均来自**真实搜索日志 + 专业标注团队（每对 ≥3 人多数票）**，该风险类别整体消除，且查询量从 40 条提升到 ~320 条。

---

## 2. 现状盘点（代码实况，2026-09-03）

| 项 | 状态 | 对 P5 的影响 |
| --- | --- | --- |
| `data/corpus.jsonl` | 30 篇手写 demo 语料（85~131 字，单 chunk，无 metadata） | **保留作 FR-23 / README 演示语料**，不再扩展；评测语料改由 T2Ranking 转换产出 |
| `Command::Bench` | CLI 占位：`bail!("bench 在 P5 实现")` | T5-03 落地处 |
| `Bm25Params` | 已定义（k1=1.2, b=0.75），`Bm25Retriever::with_params` 已存在 | **但 `Searcher` 内部构造 Bm25Retriever 用默认参数，需补 `Searcher::with_bm25_params` 注入口**（内核改动共两处，另一处见 7.2 的 `with_ef_search`） |
| `RrfFusion` | `RrfFusion::new(k, weights)` 可构造，`Searcher::with_fusion` 可注入 | RRF k 敏感性实验零改动 |
| 快照 | IDX1 格式，CLI `search --index` 已走通 | bench 复用同一加载路径 |
| 向量索引 | 生产路径 `HnswRsIndex`；instant-distance 降级为对照（P4 遗留：P5 评审时定去留）。**⚠️ hnsw_rs 0.3.4 `Hnsw::new` 内部 `StdRng::from_os_rng()`，无 seed API**（已核源码）→ 快照只存原始向量、图在 load 时重建，**图跨进程不一致**（R-P5-13）；`Searcher::with_vector` 收 `&dyn VectorIndex` → brute 诊断**零内核改动** | 见 D8 / D10 |
| tantivy 对照（T1-15） | 模式已验证：喂两侧相同 token 流（WhitespaceTokenizer），只比 BM25 数学 | T5-04 的 tantivy 基线直接复用该模式 |
| embedding | `LocalEmbedder` 单条约 1.58ms（debug）；`CachedEmbedder` 已实现未接入 CLI | bench 测延迟用**裸 embedder**（禁缓存），否则重复 query 命中缓存会系统性低估延迟 |
| `charabia` | 未引入，调研有版本矛盾（附录 B） | T5-09 先验证再定 |
| **T2Ranking 数据集** | **未下载**。已核实：Apache-2.0、HF 直下（`THUIR/T2Ranking`）、dev 划分 24,832 查询带 4 级 qrels | T5-01/T5-02 的数据源（第 3 章） |

---

## 3. 数据集选型：T2Ranking（v1.2 核心变更）

### 3.1 候选对比（全部经官方仓库核实，2026-09-03）

| 数据集 | 规模 | 标注 | 许可 | 获取 | 结论 |
| --- | --- | --- | --- | --- | --- |
| **T2Ranking**（THUIR，SIGIR 2023） | 30 万 query / 230 万段落；dev 24,832 query 带 qrels | **4 级分级**（0-3，TREC 标准），每对 ≥3 名专业标注者多数票；测试集对全部提取段落完整标注（缓解假负例） | **Apache-2.0**（数据本身，仓库明示） | HuggingFace 直下（git lfs 或 `resolve/main` 直链） | ✅ **选用** |
| DuReader-retrieval（百度） | 9 万 query / 809 万段落 | 远程监督 + 人工复核，二值为主 | 数据许可未明示（仓库许可是代码的） | **需在 aistudio.baidu.com 报名注册后下载** | ❌ 获取有注册墙 + 数据许可不明 + 语料过重 |
| mMARCO-zh | 大 | 机器翻译自 MS-MARCO | — | HF | ❌ 机器翻译产物（T2Ranking 论文明确批评其质量） |
| Multi-CPR | 多域 | 每 query 仅 1 个正例 | — | HF | ❌ 假负例严重（论文指出），不适合做评测 |

### 3.2 T2Ranking 关键事实（设计依据）

| 事实 | 对设计的意义 |
| --- | --- |
| 查询来自**搜狗真实搜索日志**，平均 11 字、最长 40 字，均为疑问式查询 | 真实 query 分布，远优于自造 query；长 query 即 Agent 典型输入的代理 |
| `qrels.dev.tsv` 为 **TREC qrels 格式**（qid, iter, pid, grade），40 万行 | 直接可解析；4 级分级支撑 graded NDCG |
| `collection.tsv`（pid, passage）230 万段落，模型切分保证语义完整 | 真实中文网页段落，词频自然分布（替代合成压测语料的理由） |
| 论文公布基线：**BM25 MRR@10 = 0.359**（全量 230 万段落）、DPR 0.486/0.517 | **外部锚点**：我们的 BM25 在子集上绝对值会更高，但量级可对照（防指标算错） |
| dev 划分平均每 query 标注 ~16 个段落（含 0 级） | 子集装配可做到"池内零假负例"（见 5.3） |
| Apache-2.0 + 要求引用论文 | License 在 deny.toml 白名单内 ✅；README/报告加引用即可 |

**引用义务**（Apache-2.0 + 学术惯例）：README 与 eval-report 均引用 Xie et al., *T2Ranking: A Large-scale Chinese Benchmark for Passage Ranking*, SIGIR 2023（arXiv:2304.03679）。

**已知局限**（写入诚信声明）：① 查询是**人类**搜索 query，作为 Agent（LLM 生成）query 的代理——长句问句桶是最接近的代理，如实声明；② 语料为 2022 年前后中文网页内容，域为通用 web（医药/教育/电商等），与 bge-small-zh（通用中文模型）匹配良好。

---

## 4. 总体设计：一个真实数据源，两种评测口径

v1.1 的"两条评测线"（人工语料 vs 合成语料）在 v1.2 中**统一为同一个真实数据源**：

```
T2Ranking（HF 下载，data/t2ranking/，gitignore）
  │
  └─► t2_prep.rs 转换器（Rust example，固定种子，可复现）
        │
        ├─► data/t2-corpus.jsonl   ~12K 段落（qrels 段落全集 + 均匀负例）
        └─► data/t2-queries.jsonl  ~320 查询（80/桶 × 四桶，带分级 relevance）
              │
              ├─【效果口径】三路对照 / 网格搜索 / tantivy 基线 / charabia / RRF 敏感性
              └─【性能口径】NFR-02 延迟 / NFR-03 构建 / NFR-04 冷启动 / NFR-05 内存
                            （~12K chunk 恰好对齐 NFR "1 万 chunk" 目标规模）
```

- **合成压测语料生成器（v1.1 的 gen_scale.rs）废弃**：真实段落自带自然词频长尾分布，优于模板合成（消除 v1.1 风险 R-P5-8）
- 两种口径共用语料但**测量协议独立**：效果口径每 query 检索 1 次；性能口径 warmup + 重复计时。报告中分节呈现，不混用
- 现有 `data/corpus.jsonl`（30 篇）保留为 demo 语料（FR-23 / README / 过滤演示）

---

## 5. 数据准备管线（T5-01 + T5-02，一个转换器两个产出）

### 5.1 下载（一次性，不入库）

```bash
# 方式一：git lfs（全量克隆，含不需要的 train 负例文件，体积大）
git lfs install && git clone https://huggingface.co/datasets/THUIR/T2Ranking data/t2ranking

# 方式二（推荐）：按需直链下载三个文件（国内可换 hf-mirror.com 域名，支持 curl -C - 断点续传）
curl -L -C - -o data/t2ranking/queries.dev.tsv  https://huggingface.co/datasets/THUIR/T2Ranking/resolve/main/data/queries.dev.tsv
curl -L -C - -o data/t2ranking/qrels.dev.tsv    https://huggingface.co/datasets/THUIR/T2Ranking/resolve/main/data/qrels.dev.tsv
curl -L -C - -o data/t2ranking/collection.tsv   https://huggingface.co/datasets/THUIR/T2Ranking/resolve/main/data/collection.tsv
```

`data/t2ranking/` 加入 `.gitignore`（体积大且可再下载）；转换产物是否入库见 D5。

### 5.2 转换器：`crates/core/examples/t2_prep.rs`

Rust example（与 embed_smoke 同形态），**零新依赖**（复用 `MixedAnalyzer` 算词汇重合度、`xxhash-rust` 做确定性抽样）。**全程固定种子，任何机器重跑产出逐字节一致**（NFR-06 的数据侧延伸）。

```
步骤：
1. 解析 queries.dev.tsv（qid \t query）与 qrels.dev.tsv（qid  iter  pid  grade，空白分隔 4 列）
2. 查询过滤：该 qid 在 qrels 中 ≥1 个 grade ≥ 2 的段落（保证有"有效正例"）；query 长度 4~40 字符
3. 候选池：过滤后的查询按 xxh64(qid, SEED) 排序取前 2000
4. 单遍流式扫描 collection.tsv：
   a. 收集 2000 个候选查询的全部 qrels 段落（pid 集合命中，含 0 级——已知负例，信息保留）
   b. 同时用哈希秩抽样（xxh64(pid, SEED2) 排序，等价于固定种子均匀抽样）抽取负例段落。
      **实现约束（精确性）**：单遍流式内维护 7K 容量最小堆（按哈希秩），精确取前 7,000——
      不得退化为 Bernoulli 抽样（计数随机会破坏"逐字节一致"承诺）；或两遍扫描（第一遍收集
      qrels pid、第二遍精确抽样），二选一并在 t2_prep.rs 注释写明所用算法
5. 分桶（对候选查询，规则顺序固定）：
   a. mixed   ：query 含 ASCII 字母/数字
   b. natural ：query 字符长度 ≥ 16（T2Ranking 均值 11，≥16 即"长问句"）
   c. exact / paraphrase：用 MixedAnalyzer 分词，计算重合度
      o* = max over grade≥1 段落的 |query 词项 ∩ 段落词项| / |query 词项|
      o* ≥ 0.5 → exact；< 0.5 → paraphrase
6. 每桶按 xxh64 排序取前 80 → 共 320 查询（不足 80 则全取，如实记录）
7. 语料装配：320 查询的全部 qrels 段落（去重）+ 负例段落补足至 ~12,000
8. 校验与统计输出：qrels pid 在 collection 缺失数（应为 0，非 0 则告警并跳过）、
   分级分布、分桶分布、语料长度分布（P50/P95/Max）、
   **段落→chunk 数断言**（默认 Chunker 下每段落应为 1 chunk；存在超限多 chunk 段落则告警数量，
   评测构建改走"每段落强制单 chunk"路径——防同一 passage 的多个 chunk 各占 Top-10 位次、
   同一相关文档被双计，见 7.4 关键点 5）、**o* 分布直方图**（分桶阈值 0.5 的定稿依据，见 5.4）

> **实施记录（2026-09-03）**：断言实测**不成立**——12,000 段落中 4,638 个（38.7%）在默认
> Chunker（512/64）下为多 chunk（语料 P95=2,269 字符、Max=76,895）。按预案落实"强制单 chunk"
> 路径：`idx bench --input` 用 `Chunker::new(200_000, 0)` 构建（200K > 最长段落，恒单 chunk），
> `--index` 快照路径加载时校验 chunk:doc=1:1，不等则告警提示改走 `--input`。
```

**产出格式**：

```jsonl
// data/t2-corpus.jsonl（source = pid 字符串，标注关联键；1 段落 ≈ 1 文档 ≈ 1 chunk）
{"source":"82391","text":"花卉名称:太阳花播种时间:春、夏、秋均可播种……","metadata":{"origin":"t2ranking"}}

// data/t2-queries.jsonl（relevance 含全部已标注对；grade 0 = 已判定负例）
{"qid":"30321","query":"太阳花怎么养","type":"natural","relevance":[{"source":"82391","grade":3},{"source":"10432","grade":1},{"source":"9811","grade":0}]}
```

### 5.3 关键设计决策：池内零假负例 + 负例披露

- 320 个查询的**全部 qrels 段落**（含 0 级判定）都进语料 → 对这些查询，池内已标注段落**无假负例**（T2Ranking dev 对提取段落做了多引擎汇总 + 完整标注）
- 随机负例段落理论上可能恰好是某查询的相关段落（未标注的假负例）——概率低（230 万段落中均匀抽 7K，对特定 query 相关而未被任何引擎召回的情形罕见），**在报告中如实披露**；必要时可跑一次"仅 qrels 语料"（去掉负例）的敏感度对照
- 语料 ~12K 段落对齐 NFR-01/02/03 的"1 万 chunk"目标规模——**同一语料服务效果与性能两种口径**（第 4 章）

### 5.4 分桶的诚实定位

分桶由**启发式规则**划分（非人工判类），规则与阈值（0.5 / 16 字符 / ASCII）全部披露，分布统计入报告。桶定义基于查询-语料的词汇特征（**不依赖任何一路检索器的输出**，非自我实现预言），用于检验"哪路占优"的假设仍然成立。

**分布先行**：0.5 阈值不预先拍死——转换器先输出 o* 分布直方图（网页长段落易让短 query 重合度虚高，exact/paraphrase 区分度可能低于直觉），若分布显示区分度不足（大量堆积在 0.4~0.6 区间），调整阈值并把决策记录在案，最终值入报告。**桶下限披露规则**：任一桶 < 30 条时，该桶结论标注"探索性"，不进入主结论。

> **实施记录（2026-09-03）**：实测 o* 直方图（1,536 条 exact/paraphrase 候选）主峰在
> 0.9~1.0（715 条），0.4~0.6 斜坡仅 139 条（9.1%），区分度足够——**0.5 阈值维持**。
> 分桶落位：mixed 353 / natural 111 / exact 1,456 / paraphrase 80（每桶取 80）。
> paraphrase 桶候选恰好 80 条取满，>30 条不触发"探索性"标注，但余量为零，报告披露。

---

## 6. 指标定义与手算基准（T5-03 的验收锚点）

> 延续 P1~P4 传统（BM25/RRF 手算值）：**指标实现也是算法，算错了整个调参会调向错误方向**。先有手算样例，单测逐位比对。

设某 query 的已标注段落集为 J（grade ∈ {0,1,2,3}），检索返回 Top-K（K=10）。

### 6.1 指标公式（v1.2：分级版）

```
相关集 R(thr)   = {p ∈ J : grade(p) ≥ thr}          （默认 thr = 1，可调 --rel-threshold）
Recall@10       = |Top10 ∩ R| / |R|                  （分母不截断到 10）
MRR@10          = 1/rank₁                             （首个 grade ≥ thr 的排名；10 名内无则 0）
gain(g)         = 2^g − 1                             （0/1/3/7，TREC 惯例）
DCG@10          = Σᵢ₌₁¹⁰ gain(gᵢ) / log₂(i+1)
IDCG@10         = J 中全部正例按 grade 降序的理想 DCG（分母取前 10 位）
NDCG@10         = DCG@10 / IDCG@10                    （|R| = 0 时定义为 0，非 NaN）
```

- **Recall / MRR 用阈值化二值**（默认 grade ≥ 1；**主表同时报告 thr=1 与 thr=2 两列**，而非仅敏感度——若 grade 1 语义偏"弱/可能相关"，单列 thr=1 会让弱相关灌水 MRR/Recall；grade 语义与 T2Ranking 官方定义在 T5-01/02b 对照复核并记录）
- **NDCG 用分级 gain**——T2Ranking 的 4 级专业标注支撑分级（v1.1 的 D4"二元"决策随之翻转：当时选二元是因为单人自标一致性差，该前提已不存在）

### 6.2 手算样例（单测锚点）

- **Recall**：R(1) = {A,B,C}，Top10 含 A、C → 2/3 ≈ **0.6667**
- **MRR**：首个 grade≥1 在第 4 位 → **0.25**；Top10 无 → **0.0**
- **NDCG（分级）**：J 中正例 X(g3)、Y(g1)、Z(g2)；返回第 1 位 = X、第 3 位 = Y、第 7 位 = Z，其余 grade 0
  - DCG = 7/log₂2 + 1/log₂4 + 3/log₂8 = 7 + 0.5 + 1 = **8.5**
  - IDCG = 7/1 + 3/log₂3 + 1/log₂4 = 7 + 1.89279 + 0.5 = **9.39279**
  - NDCG = 8.5 / 9.39279 ≈ **0.90495**
- **阈值切换**：同例 `--rel-threshold 2` → R = {X, Z}，Recall = 2/2 = 1.0（若 Top10 含 X、Z）
- **边界**：R = ∅ → NDCG = 0 非 NaN；|R| > 10 → IDCG 取前 10 位；Top10 全正例且按序 → 1.0

### 6.3 聚合与延迟口径

- 单 query 指标 → **宏平均**（全体 + 按 type 分桶各一份）
- 同分 tie：检索结果已由 `chunk_id` 升序 tie-break（NFR-06），按返回顺序计算
- **延迟 P50/P99（nearest-rank）**：N 样本升序，P_p 取第 `ceil(p/100 × N)` 个。手算：**6,400 样本**（320 query × 20 reps；预热 3×320 不计入样本）的 P99 = 第 `ceil(0.99 × 6400)` = **6,336** 个、P50 = 第 **3,200** 个（v1.2 误写 6,000/5,940/3,000，勘误——单测锚点随之修正）
- **外部锚点**（防指标算错）：T2Ranking 论文公布 BM25 在**全量** 230 万段落上 MRR@10 = 0.359；我们在 12K 子集上绝对值必然更高，但**量级**（0.3~0.8 区间）应可比，显著偏离即排查
  - **sanity 实测（2026-09-03，T5-01/02b）**：320 query 全量 BM25 MRR@10(thr=1) = **0.6394** / Recall@10 = 0.5754 / NDCG@10 = 0.4429，落于 0.3~0.8 量级区间 ✅；pid 映射（32562/32562 全命中）与 jieba 分词链路无误

---

## 7. T5-03 `idx bench` 设计

### 7.1 模块划分（遵守模块边界）

```
crates/core/src/bench/          ← 新增顶层模块（与 query 同级）
├── mod.rs        # 对外 API
├── metrics.rs    # 纯函数：ranked + graded relevance → Recall/MRR/NDCG（独立单测）
└── judgments.rs  # t2-queries.jsonl 解析 + source 存在性校验

crates/cli/src/main.rs          ← Bench 子命令落地（IO 与编排）
```

`bench` 只消费 `SearchResponse.hits` 的 chunk_id 顺序，不依赖 retriever 内部；`load_judgments` 放 core（tantivy 基线测试复用）。

### 7.2 需要的内核 API 补充（共两处小改动，其余零改动）

| 改动 | 位置 | 理由 |
| --- | --- | --- |
| `Searcher::with_bm25_params(Bm25Params)` | `query/searcher.rs` | 网格搜索要从外部注入 k1/b；现状 Searcher 内部用默认参数 |
| `HnswRsIndex::with_ef_search(usize)` | `vector/hnsw_rs_index.rs` | ef_search 最小校准（8.6）需注入口；现状硬编码 200（P2 起从未校准，评审②指出） |

RRF k 已可经 `with_fusion(Box::new(RrfFusion::new(k, …)))` 注入，无需改动。`--vector-index brute` 诊断**零内核改动**——`with_vector` 收 `&dyn VectorIndex`，`BruteForceIndex` 已实现该 trait。

### 7.3 CLI 参数设计

```
idx bench --index <快照> [--queries <path>]      # 默认 data/t2-queries.jsonl
  [--input <语料>]              # 重建模式（与 --index 二选一）：charabia 实验用
  [--analyzer mixed|charabia]   # 默认 mixed；charabia 需编译 feature（第 9 章）
  [--modes bm25,vector,hybrid]  # 默认三路全跑（T5-04 数据源）
  [--k 10]                      # 检索深度 = 指标 @K
  [--k1 F] [--b F]              # 覆盖 BM25 参数（网格搜索用）
  [--rrf-k 60]                  # 覆盖 RRF k
  [--rel-threshold 1]           # Recall/MRR 的相关性阈值（1 或 2）
  [--grid]                      # BM25 k1×b 网格搜索（T5-05），16 格表 + 最优
  [--reps 20] [--warmup 3]      # 延迟测量：每 query 预热 + 重复次数
  [--vector-index hnsw|brute]   # 默认 hnsw；brute 为诊断开关（8.6）：区分"方法不行"vs"ANN 近似丢文档"
  [--ef-search N]               # 覆盖 HNSW ef_search（8.6 最小校准；依赖 with_ef_search 注入口）
  [--runs 1]                    # 重复运行次数 >1 时输出指标 run-to-run 抖动（R-P5-13）
  [--json <out>]                # 机器可读输出（per-query 明细 + 网格表）——**T5-03 验收必需**：报告表格由 JSON 生成，机械防"报告数字与命令输出不符"
```

（v1.1 的 `--pool` 标注底稿模式**删除**——不再有人工标注环节。）

### 7.4 执行流程与关键决策

```
加载快照（或 --input 重建 + --analyzer 选择）
  → 加载 judgments（source 校验，缺失即败；grade 合法性校验）
  → 阶段 A【效果】：每 query 每模式检索 1 次 → 逐 query 算指标 → 总表 + 分桶表
  → 阶段 B【延迟】：每 query 预热 warmup 次 + 实测 reps 次 → P50/P99（排除加载与模型初始化）
  → （--grid 时）阶段 C【网格】：16 组 (k1,b) × 320 query 的 BM25 NDCG@10
  → 输出（stdout 表格 [+ --json 文件]）
```

关键点：

1. **延迟测量用裸 `LocalEmbedder`**（禁 `CachedEmbedder`）——bench 对同一 query 重复 reps，缓存必然命中会系统性低估 vector/hybrid 延迟。NFR-02 口径为端到端（含 `embed_query`，不含模型冷启动），缓存收益属 FR-20 运行期行为，不进 NFR 测量
2. **模型加载耗时单独打印**（"模型加载 Xs（不计入延迟）"）；**HNSW 图重建耗时同样单独打印**（万级 ~15s，防误并入任何口径——NFR-03/04 的口径切分见 10.2）
3. 快照不含向量时，vector/hybrid 模式**报错退出**并提示 `build --vectors` 重建
4. **全部延迟数字要求 --release**；输出打印构建 profile，debug 构建给醒目警告
5. source → chunk 映射沿用现有机制（标注在文档级，自动展开为该文档全部 chunk；**t2_prep 已断言每段落单 chunk**（5.2 步骤 8），多 chunk 双计风险在数据侧消除；demo 语料的 source 展开机制不受影响）
   - **实施勘误（2026-09-03）**：实测 38.7% 段落为多 chunk（5.2 实施记录），"数据侧消除"不成立——改在**索引构建侧**消除：`bench --input` 强制单 chunk（`Chunker::new(200_000, 0)`），`--index` 路径加载时校验 chunk:doc=1:1 并告警
6. **重复运行校验（R-P5-13）**：hnsw_rs 图无固定 seed、跨进程重建 → 默认参数跑 `--runs 3`，输出 NDCG/延迟的 run-to-run 抖动区间；评测报告声明"同进程同图确定，跨进程复现 ±X"。**若抖动与结论相关差距同量级（不可忽略），触发 D7 的 hnswio 图持久化分支**

### 7.5 输出示意（eval-report 的数据源）

```
== 效果评测（320 queries，K=10，宏平均，thr=1 与 thr=2 双列 + graded NDCG）==
mode    Recall@10(thr1/thr2)  MRR@10(thr1/thr2)  NDCG@10(graded)
bm25      ...       ...      ...
vector    ...       ...      ...
hybrid    ...       ...      ...

== 分桶（NDCG@10）==
type         n | bm25 | vector | hybrid | 预期占优
mixed       80 | ...  |  ...   |  ...   | hybrid
natural     80 | ...  |  ...   |  ...   | hybrid
exact       80 | ...  |  ...   |  ...   | bm25
paraphrase  80 | ...  |  ...   |  ...   | vector

== 延迟（release，warmup 3 + 20 reps × 320 queries ≈ 6,400 样本/模式）==
mode    P50       P99
...
```

---

## 8. T5-04 / T5-05 对照实验与调参

### 8.1 执行顺序（沿 v1.1 修正）

```
T5-03 bench 就绪
  → T5-05 网格搜索（先用默认参数跑三路对照做 sanity check，对照外部锚点 6.3）
  → 参数定稿（改 Bm25Params::default 源码常量，提交信息注明实验依据）
  → T5-04 终版三路对照（定稿参数）+ tantivy 基线 + RRF k 敏感性
```

任务编号不变，仅 T5-04/T5-05 执行顺序对调（终版对照必须用定稿参数跑）。

### 8.2 三路对照（T5-04 核心）

- 配置：BM25 only / Vector only / Hybrid(RRF, k=60, 等权)，~320 query × 四桶
- 主指标：NDCG@10（graded）+ Recall@10 / MRR@10（成功标准 2.4 考核后两者；**主表 thr=1/thr=2 双列**，见 6.1）
- **显著性检验**：逐 query 配对**符号检验**（hybrid vs 最优单路，整体与逐桶各一份；精确二项 p 值，零新依赖自实现 ~30 行）——均值表之外给出"差异是否超出查询间波动"的证据；单侧检验方向预注册（hybrid ≥ 单路）
- **run-to-run 抖动**：默认参数 `--runs 3` 的指标区间（R-P5-13），与结论差距同屏呈现
- 逐桶对照"预期占优"假设（7.5 表）
- **验收不是"hybrid 必须赢"而是"数据必须真"**：未优于最优单路 → 8.5 诊断 + T5-08 如实记录

### 8.3 tantivy 基线（ADR-007 的 P5 义务）

- 形态：`crates/core/tests/eval_tantivy.rs`，`#[ignore]` + `--release` 手动跑（tantivy 是 dev 依赖）
- 方法：**复用 T1-15 模式**——MixedAnalyzer 分出的 token 流空格拼接喂 tantivy WhitespaceTokenizer，消除分词差异，只比排序数学；语料与评测集用 `data/t2-corpus.jsonl` / `data/t2-queries.jsonl`（路径可环境变量覆盖）
- 断言：自研 BM25 与 tantivy 的 NDCG@10 **相对差 ≤ 5%**（D6）；打印两者分值表入报告

### 8.4 T5-05 网格搜索

- `k1 ∈ {1.0, 1.2, 1.5, 2.0}` × `b ∈ {0.3, 0.5, 0.75, 0.9}` = 16 组合，NDCG@10（BM25 单路）选优；**16 格表同时输出 Recall@10 / MRR@10 列**（对齐成功标准 2.4 的考核口径，不只看 NDCG）
- **并列取更接近默认值（1.2/0.75）的组合**（防噪声过拟合）
- **定稿约束**：NDCG 最优（并列取近默认值）且 **Recall@10 / MRR@10 相对默认参数不退化**（否则改取次优组合并在报告说明取舍理由）；定稿写入 `Bm25Params::default()`，报告记录全部 16 格数据
- **不盲信英文经验值**（R6）：真实中文网页段落 + 真实查询，正是该风险的设计场景

### 8.5 RRF 敏感性（风险 R7）

- **默认做**：RRF k ∈ {20, 60, 100}（3 次全量评测，入报告附录）
- **预案做**（仅当 hybrid ≤ 最优单路）：追加 weights {(1,1),(1.5,1),(1,1.5)} 诊断，定位拖后腿的一路

### 8.6 向量路诊断（v1.3 新增：ANN 近似与方法好坏的隔离）

背景：`HnswRsIndex` 的 `ef_construction=300 / ef_search=200` 硬编码自 P2，从未在真实数据上校准；P4 的 0.970 重合率测于**随机向量**——不是 bge 真实向量、更不是 320 条评测 query 的相关文档（相关文档常处边界位置，恰是最易翻转的区间）。不隔离 ANN 近似误差，"hybrid 是否优于单路"的归因就不干净。

- **默认做**：`ef_search ∈ {100, 200, 400}` 最小校准（`--ef-search`，全量 320 query，入报告附录）
- **触发做**（hybrid ≤ 最优单路，或 paraphrase 桶不如预期）：`--vector-index brute` 暴力对照——320 query × 12K × 512 维 release 下约 1~2s，成本可忽略；**brute 与 hnsw 的指标差 = ANN 近似损失**，据此区分"稠密检索方法本身不行"vs"ANN 近似丢了文档"
- D8 的 `tests/vector_ab.rs` 改写在**真实语料 + 评测 query** 上跑（不再用随机向量）

---

## 9. T5-09 分词对照实验（charabia）

- **定位**（沿 v1.1）：charabia 中文分词底层即 jieba，对比实质是"规范化管道 vs 自研过滤链"；**在真实 T2Ranking 语料上做**，信号远优于手写小语料
- **三步验证先行**（调研有版本矛盾：thirdparty.md 核查 0.10.0 裁剪版"只多拉 jieba-rs/aho-corasick/csv/fst/whatlang"，v1.0 附录 B 核查 0.9.0"强依赖 lindera"）：

```bash
cargo add charabia@0.10 --no-default-features --features chinese -p index-core --dry-run  # 依赖树
cargo +1.90 check -p index-core --features charabia                                        # MSRV
make deny                                                                                   # License（csv 为 MIT OR Unlicense，应可过）
```

- **通过** → `analyze/charabia.rs` 实现 `CharabiaAnalyzer`（feature 隔离，以 docs.rs 实际 API 为准），用 `idx bench --input data/t2-corpus.jsonl --analyzer charabia` 重建对照（**索引/查询两侧同时切换**，R4）
- **失败**（拉入 lindera / MSRV 超限 / License 拒绝）→ 报告记录事实与跳过理由，T5-09 关闭，不影响主线
- 判优：charabia 总 NDCG@10 提升超出噪声且分桶无恶化 → 切默认；否则保留自研链

---

## 10. T5-06 NFR 实测校准（性能口径，同一真实语料）

### 10.1 测量语料

**即 `data/t2-corpus.jsonl`（~12K 段落 ≈ 1.2 万 chunk）**——恰好对齐 NFR-01/02/03 的"1 万 chunk"目标规模。真实 embedding 全管道（`build --vectors`）。废弃 v1.1 的合成语料生成器。

### 10.2 各 NFR 测量协议

| NFR | 目标 | 协议 | 预期（基于 P0/P4 实测外推） |
| --- | --- | --- | --- |
| NFR-02 延迟 | BM25 < 5ms；vector < 10ms；hybrid P99 < 20ms | `idx bench --index <12K快照> --reps 20`，release，~6,400 样本/模式 | 大概率达标（embed ~1.5ms + HNSW 亚毫秒） |
| NFR-03 构建 | 1 万 chunk 含 embedding < 120s | `build --vectors` 计时（release）。**口径 = embed + 快照落盘，不含 HNSW 图构建**（现状 build 不建图、图在 load 时重建——v1.2 预期表误把 HNSW 插入算入，勘误） | ✅ embed ~1.5ms/条 → 预计 20~30s |
| NFR-04 冷启动 | 快照加载 < 2s | `storage::load` 与 **HNSW 图重建分别计时分别报告**（勿混） | **P4 已知万级 ~15s 不达标（图重建主导）→ D7：默认如实标注；若 run-to-run 抖动不可忽略则走 hnswio 图持久化分支**（`Graph::dump` / `HnswIo::load_hnsw` 已核实存在，支持 mmap；图落盘后加载毫秒级，同时消解 R-P5-13） |
| NFR-05 内存 | 向量部分约 20MB（512 维 × 4B × 1 万） | `/usr/bin/time -l` 包住 build 与 search，peak RSS 对照理论值 | 向量 ~24MB + HNSW 图 + postings，如实报告 |
| NFR-01 规模 | 1 万 chunk 可跑 | 12K 语料构建+检索全程无错 | ✅（顺带验证） |

NFR-06（确定性）P3 已验证；NFR-07（took）已落地；NFR-08/09 走 `make deny` 阶段末复查。

### 10.3 回写（T5-06 交付）

更新 `requirements-spec.md` 第 6 章：NFR-02~05 的"待实测"替换为实测值（含测量环境：机器 / release / 语料规模 / 样本数），未达标项写"未达标 + 原因 + 缓解路径"。**按文档规则只改 requirements-spec.md**。

---

## 11. T5-07 README 与示例

- 根目录 `README.md`：是什么 / 快速上手（用 30 篇 demo 语料，秒级跑通）/ 架构速览（模块图 + 六 trait + feature 表）/ **评测结论摘要**（引用 eval-report 终版数字）/ **数据来源与引用**（T2Ranking，Apache-2.0，论文引用）/ 已知局限（含 D7 万级冷启动、人类查询代理声明）
- `examples/search_basic.rs`：库使用端到端示例（建索引 → hybrid 检索 → `to_context_block()`，约 60 行）
- **demo 语料补 metadata**：给 `data/corpus.jsonl` 部分文档加 `{"topic": "..."}` 字段，支撑 `--filter` 演示（T2Ranking 段落无主题元数据，过滤演示由 demo 语料承担）
- `docs/README.md` 索引同步

---

## 12. T5-08 数据诚信（报告模板，执行期填充）

`docs/devel/eval-report.md` 结构：

```
1. 实验设置（git commit hash / **T2Ranking HF 仓库 revision 或下载日期**（防数据集日后变更）/ 模型 id（Xenova/bge-small-zh-v1.5）/ 转换器种子 / 测量环境 / 命令全文）
2. 数据来源与装配（采样规则、分桶启发式与分布、负例策略、缺失统计）
3. 三路对照总表 + 分桶（含"预期占优"逐桶验证）+ 逐 query 配对符号检验（整体与逐桶 p 值）+ run-to-run 抖动区间（--runs 3）
4. tantivy 基线对照（ADR-007 gate）
5. BM25 网格搜索与参数定稿（16 格表 + 过拟合风险声明）
6. RRF k 敏感性
7. 分词对照（T5-09；若跳过则记录三步验证事实）
8. NFR 实测（性能口径，与效果口径分节）
9. 数据诚信声明：
   - 已知偏差：随机负例的潜在假负例（附"仅 qrels 语料"敏感度对照）；
     分桶为启发式代理（非人工判类）；人类搜索 query 作为 Agent query 的代理；
     12K 子集任务难度低于 230 万全量（绝对指标不可与论文基线直接对比，仅量级对照）
   - 结论适用范围（不外推到其他域/规模/查询分布）
   - 未达预期项的如实记录与原因分析（若有）
10. 结论与残留问题（交 P6）
```

**红线**（需求文档 9.5）：若混合未优于最优单路，如实记录并分析原因，**不得粉饰**。结论只谈量级与方向性。

---

## 13. 任务清单与执行顺序

| 序 | 任务 | 内容 | 依赖 |
| --- | --- | --- | --- |
| 1 | T5-01/02a | 下载 T2Ranking 三文件 + `t2_prep.rs` 转换器（采样/装配/分桶/校验） | — |
| 2 | T5-03a | `core::bench`（graded metrics + judgments）+ 手算单测 | —（可与 1 并行） |
| 3 | T5-03b | `idx bench` 完整落地（--grid / 延迟 / --rel-threshold）+ `Searcher::with_bm25_params` | 2 |
| 4 | T5-01/02b | 跑转换器产出 `t2-corpus/t2-queries`，人工审阅分布统计（含 o* 直方图定桶阈值、grade 语义复核）+ **BM25 量级 sanity**（100 条随机 query 对照外部锚点 0.359，提前暴露 pid 映射/分词错误） | 1, 3 |
| 5 | T5-05 | 网格搜索（16 格 × Recall/MRR/NDCG 三列）+ ef_search 最小校准（8.6）→ 参数定稿 | 4 |
| 6 | T5-04 | 终版三路对照（含符号检验 + `--runs 3` 抖动）+ tantivy 基线 + RRF 敏感性 → eval-report 效果部分 | 5 |
| 7 | T5-09 | charabia 三步验证 →（通过则）真实语料对照 | 6 |
| 8 | T5-06 | NFR 实测（12K 真实语料）→ eval-report 性能部分 + 需求文档第 6 章回写 | 4 |
| 9 | T5-07 | README + examples + demo 语料补 metadata + docs 索引 | 6, 8 |
| 10 | T5-08 | 诚信声明终稿 + plan.md 进度勾选 + D8 清理（若确认） | 全部 |

每个任务一次提交（`P5/T5-xx: …`），阶段末 `make fmt && make lint && make test && make deny` 全绿。

---

## 14. 单测设计

| case | 断言 |
| --- | --- |
| Recall@10 手算 | 6.2 样例 = 0.6667（浮点逐位） |
| MRR@10 手算 | rank4 → 0.25；无相关 → 0 |
| NDCG@10 手算（分级） | 6.2 样例 ≈ 0.90495（误差 < 1e-4） |
| 阈值切换 | 同一数据 thr=1 vs thr=2 的 Recall/MRR 变化符合手算 |
| NDCG 边界 | R=∅ → 0 非 NaN；\|R\| > 10；全正例按序 → 1.0；grade 0 贡献 0 |
| 分位数 | nearest-rank P50/P99 取位正确（6.3 手算：6,400 样本 → P99 第 6,336 个、P50 第 3,200 个） |
| 符号检验手算 | 18 对中正 14 负 4 → 单侧 p ≈ 0.0154（二项精确） |
| judgments 校验 | 指向不存在 source → 报错；grade ∉ {0..3} → 报错；坏行带行号报错 |
| 网格 smoke | 微型语料 16 格全跑通、argmax 正确、并列取近默认 |
| charabia 一致性 | `analyze_doc` 与 `analyze_query` 输出一致（R4，feature-gated） |
| bench 端到端 | 小语料 + 3 条标注 → 三模式指标输出、hybrid 路径无 panic |

---

## 15. 风险与兜底

| # | 风险 | 影响 | 兜底 |
| --- | --- | --- | --- |
| R-P5-1 | **HF 下载失败/极慢**（collection.tsv 数百 MB） | T5-01 受阻 | hf-mirror.com 域名替换 + `curl -C -` 断点续传；转换器对直链下载的三个文件即可工作 |
| R-P5-2 | 随机负例引入未标注假负例 | Recall 被低估 | 概率低（230 万中抽 7K）；报告披露 + "仅 qrels 语料"敏感度对照（5.3） |
| R-P5-3 | 分桶启发式误分类 | 桶结论噪声 | 规则与阈值全披露 + 分布统计；桶为代理不作为硬结论 |
| R-P5-4 | 人类 query ≠ Agent query（代理偏差） | 结论外推受限 | 诚信声明明确"以长问句桶为最接近代理"；不外推 |
| R-P5-5 | 12K 子集难度低于全量，绝对指标偏高 | 与论文基线不可直接比 | 仅做量级对照（6.3 外部锚点），三路**相对比较**不受影响 |
| R-P5-6 | 网格搜索过拟合 | 参数"定稿"实为噪声 | 320 query + 并列取近默认值 tie-break；报告声明"在本评测集上" |
| R-P5-7 | hybrid 未优于最优单路 | 成功标准 2.4 不达 | 先查两单路效果与外部锚点；跑 RRF k/weights 诊断（8.5）；如实记录 |
| R-P5-8 | charabia 0.10 裁剪版仍拉 lindera / MSRV 超限 / License 拒绝 | T5-09 受阻 | 三步验证前置；失败即记录跳过，不影响主线 |
| R-P5-9 | 延迟误用 debug 构建 | NFR-02 数据无效 | bench 打印 profile + debug 醒目警告；验收全 --release |
| R-P5-10 | 万级 NFR-04 不达标（P4 已知 ~15s） | 成功标准部分不达 | D7 如实标注（图快照留 P6） |
| R-P5-11 | qrels pid 在 collection 缺失 / 数据坏行 | 装配中断或静默丢数据 | 转换器统计缺失数并告警；bench 加载强校验（缺失即败） |
| R-P5-12 | 模型下载失败（他人复现） | 复现受阻 | README 注明首次下载 + `HF_ENDPOINT` 镜像 |
| R-P5-13 | **hnsw_rs 图无固定 seed**（`Hnsw::new` 用 `StdRng::from_os_rng()`，快照只存向量、每进程重建图） | 跨进程评测数字自带噪声；若结论差距 < 噪声量级，"hybrid 优于单路"实为噪声上做文章 | `--runs 3` 重复运行输出抖动区间并披露（"同进程同图确定，跨进程 ±X"）；抖动不可忽略 → D7 的 hnswio 图持久化分支（图落盘 = 确定性 + 冷启动毫秒级，一并解掉 NFR-04） |

> v1.1 的 R-P5-1（标注者偏差）、R-P5-8（合成语料词频失真）随数据源切换**整体消除**；其余沿承。

---

## 16. 验收清单

```bash
cd /Users/gongyubo/Code/mine/index-demo

# 数据管线（T5-01/02）
ls data/t2ranking/                                # 三个已下载文件（gitignore）
cargo run --release -p index-core --example t2_prep   # 产出 + 分布统计
wc -l data/t2-corpus.jsonl data/t2-queries.jsonl  # ~12K 段落 / ~320 查询（四桶各~80）

# 单测（含分级指标手算锚点）
cargo test -p index-core

# 效果评测（定稿参数后；--runs 3 输出抖动，--json 为报告数据源）
cargo run --release -p idx -- build --input data/t2-corpus.jsonl --output /tmp/eval.idx --vectors
cargo run --release -p idx -- bench --index /tmp/eval.idx --runs 3 --json /tmp/eval.json
cargo run --release -p idx -- bench --index /tmp/eval.idx --rel-threshold 2   # 敏感度完整复核（主表已双列）

# 向量路诊断（8.6；触发条件：hybrid ≤ 最优单路或 paraphrase 桶异常）
cargo run --release -p idx -- bench --index /tmp/eval.idx --modes vector --vector-index brute

# 网格搜索（T5-05）
cargo run --release -p idx -- bench --index /tmp/eval.idx --grid

# tantivy 基线（ADR-007 gate）
cargo test -p index-core --release --test eval_tantivy -- --ignored --nocapture

# 分词对照（T5-09，三步验证通过后）
cargo run --release -p idx --features charabia -- bench --input data/t2-corpus.jsonl --analyzer charabia

# NFR 实测（同一 12K 语料，性能口径）
cargo run --release -p idx -- bench --index /tmp/eval.idx --reps 20

# 阶段出口
make fmt && make lint && make test && make deny
```

产物：`data/t2-corpus.jsonl`、`data/t2-queries.jsonl`、`docs/devel/eval-report.md`、更新后的 `requirements-spec.md` 第 6 章、`README.md`、`examples/search_basic.rs`。

---

## 17. 模块边界自查（阶段末）

| 模块 | 检查 |
| --- | --- |
| `bench`（新增） | 不依赖 retriever 内部，只消费 SearchResponse；指标为纯函数 |
| `analyze/charabia.rs`（新增） | 不知道索引的存在 |
| `t2_prep.rs`（example） | 只用公开 API（MixedAnalyzer）；不进 lib |
| `query` | 仍只编排；`with_bm25_params` 只是参数透传 |
| `fusion` | 不回捞正文（无改动） |

---

## 18. 需要你确认的决策点

| # | 决策点 | 我的建议 | 影响 |
| --- | --- | --- | --- |
| D1 | **数据集选型**：T2Ranking（vs DuReader-retrieval / mMARCO-zh / Multi-CPR） | **T2Ranking**：Apache-2.0 明示 + HF 直下无注册 + 4 级专业标注 + 公开基线锚点（3.1 对比表） | 整个 P5 数据基础 |
| D2 | **评测规模**：~320 查询（四桶各 80，启发式分桶）+ ~12K 段落语料（qrels 全集 + 均匀负例至 12K） | 照建议执行——12K 恰好同时满足 NFR"1 万 chunk"口径，一份数据两种用途 | T5-01/02 产出规模 |
| D3 | **相关性口径**：graded NDCG（gain = 2^g−1）+ Recall/MRR 阈值 grade≥1（`--rel-threshold` 可调） | 照建议执行——专业 4 级标注支撑分级（v1.1"二元"决策前提已消失，随之翻转） | 指标实现与单测锚点 |
| D4 | **分桶启发式**：mixed（含 ASCII）→ natural（≥16 字）→ exact/paraphrase（词汇重合度 0.5 切分），规则全披露 | 照建议执行；阈值在转换器统计后可微调，最终值入报告 | 四桶分析的有效性 |
| D5 | **转换产物是否入库**：commit `t2-corpus/t2-queries`（~10MB，Apache-2.0 + 署名）vs 仅提交转换器 | **commit**——评测可复现是 T5-08 硬要求；10MB 体积可接受；NOTICE 引用写入 README | 仓库自包含性 vs 体积 |
| D6 | **ADR-007"5% 差距"口径**：相对差（\|自研−tantivy\| / tantivy ≤ 5%） | 照建议执行 | eval_tantivy 断言 |
| D7 | **万级 NFR-04（~15s > 2s）**：如实标注未达标 vs hnswio 图快照 | **默认如实标注**（P5 任务已满；图快照动 IDX1 格式）；**条件分支（评审①后重开）**：若 `--runs 3` 抖动不可忽略（与结论相关差距同量级），P5 内改走 hnswio 图持久化——`Graph::dump`/`HnswIo::load_hnsw` 已核实存在，同时消解 R-P5-13 与 NFR-04 | 成功标准 2.4 第 3 条 + R-P5-13 |
| D8 | **instant-distance 去留**（P4 遗留，约定 P5 评审时定） | **移除**：依赖 + `vector/hnsw.rs` 一并删（**与 R-P5-13 同批决策**——删除后唯一带固定 seed 的 ANN 实现消失，确定性完全落在 hnsw_rs 的应对上）；`tests/vector_ab.rs` 改写为 `HnswRsIndex` vs `BruteForceIndex` 召回对照，**在真实语料 + 评测 query 上跑**（8.6，不再用随机向量） | 依赖树缩小 |
| D9 | **T5-09 执行方式**：三步验证通过则做、失败则记录跳过 | 照建议执行 | 不阻塞主线 |
| D10 | **向量路诊断与统计检验纳入 P5**（v1.3 新增，评审②③）：`--vector-index brute` 诊断 + ef_search {100,200,400} 校准 + 逐 query 符号检验 + `--runs 3` 抖动披露 | **纳入**——brute 零内核改动、符号检验 ~30 行零依赖、3 次全量评测分钟级；"hybrid 是否真优"是 P5 立论，必须把 ANN 近似噪声与统计噪声都排除后再下结论 | 结论可信度 |

---

## 附录 A 与已有设计 / 遗留项的衔接

- **T1-15 tantivy 对照模式** → 8.3 复用，从"内置微语料"升级为"真实 T2Ranking 语料 + 320 查询"
- **P4 遗留三项**：instant-distance 去留 → D8；positions feature → P5 不启用；CachedEmbedder 接入常驻服务 → 超范围，但 7.4 明确 bench 禁缓存测延迟
- **P0 遗留**（模型仓 License 复核）——P2 已按"沿用上游 MIT"接受并记录，P5 诚信声明引用，不再重开
- **风险 R6 / R7**：8.4 网格搜索与 8.5 敏感性闭环；**真实网页语料 + 真实查询正是 R6 的设计场景**
- **成功标准 2.4 第 4 条**（trait ≥2 实现或替换路径）：VectorIndex（D8 后仍有 HnswRs + Brute 双实现 ✅）+ FusionStrategy（RRF + Weighted）+ Reranker（NoOp + 路径）✅；**Analyzer 视 T5-09 而定**——若 charabia 三步验证失败跳过，Analyzer 为单实现，README 措辞须写"trait 隔离、替换路径已定义（接口与单测层面）"而非"双实现"（charabia 中文底层同为 jieba，其替代价值本身有限）
- **FR-23**（示例中文语料）：现有 30 篇 demo 语料继续承担；T5-07 补 metadata 支撑过滤演示

## 附录 B 数据集候选核查记录（2026-09-03）

| 数据集 | 核查渠道 | 关键结论 |
| --- | --- | --- |
| T2Ranking | github.com/THUIR/T2Ranking README | Apache-2.0（数据明示）；HF `THUIR/T2Ranking` git lfs；collection.tsv 2,303,643 段落（pid, passage）；queries.dev.tsv 24,832（qid, query）；qrels.dev.tsv 400,536 行 TREC 格式；论文基线 BM25 MRR@10 = 0.359 / DPR 0.486 / CE 0.519（全量） |
| DuReader-retrieval | github.com/baidu/DuReader | 9 万 query / 809 万段落；**需 aistudio.baidu.com 报名注册后下载**；数据许可未单独明示；二值标注为主 |
| mMARCO-zh / Multi-CPR | T2Ranking 论文（SIGIR 2023）综述 | 前者机器翻译产物；后者每 query 仅 1 正例，假负例严重，不适合评测 |

（charabia 版本矛盾核查记录沿 v1.1 附录 B，见第 9 章三步验证。）

## 附录 C 版本变更

### v1.2 → v1.3（2026-09-03，外部评审修订）

| # | 变更 | 理由 |
| --- | --- | --- |
| 1 | 承认 **hnsw_rs 无固定 seed**（核源码：`Hnsw::new` 用 `StdRng::from_os_rng()`，无 seed API；快照存向量、图每进程重建）→ 新增 R-P5-13、`--runs` 重复运行校验、D7 重开 hnswio 条件分支 | v1.2 把"评测可复现"全押在 t2_prep 固定种子上，但跨进程图不一致使评测数字自带噪声——若结论差距 < 噪声即失效 |
| 2 | 新增 8.6 向量路诊断：`--vector-index brute`（零内核改动）+ ef_search {100,200,400} 校准 + `HnswRsIndex::with_ef_search` 注入口 | ef 参数硬编码自 P2 从未校准；P4 召回测于随机向量，ANN 近似与稠密检索方法好坏未隔离 |
| 3 | 8.2 补逐 query 配对符号检验（精确二项，零依赖 ~30 行；含单测锚点） | 320 条足以做配对检验；均值表目测不构成"hybrid 更优"的证据 |
| 4 | 8.4 网格表补 Recall/MRR 列 + 定稿参数"Recall/MRR 不退化"约束 | 与成功标准 2.4 的考核口径对齐（原仅 NDCG 选参，口径错位） |
| 5 | 6.1 主表 thr=1/thr=2 双列；grade 1 语义在 T5-01/02b 对照官方定义复核 | grade 1 若为弱相关会灌水 MRR/Recall 单列读数 |
| 6 | 5.2 步骤 8 补段落→chunk 单 chunk 断言（多 chunk 双计风险）；步骤 4b 抽样算法精确化（7K 最小堆或两遍扫描，禁 Bernoulli） | 指标正确性；"逐字节一致"承诺 |
| 7 | 勘误两处：延迟样本数 6,000→6,400（P99 第 6,336 个、P50 第 3,200 个，单测锚点同步）；NFR-03 口径 = embed+save（图构建归 NFR-04，v1.2 预期表混入 HNSW 插入） | 数字一致性；本文档自己强调口径独立，此处反而混了 |
| 8 | 5.4 o* 分布先行（直方图定阈值）+ 桶 <30 条标"探索性"；任务 4 加 BM25 量级 sanity（装配完即对照外部锚点，提前暴露 pid 映射/分词错误） | 分桶有效性；外部锚点对照从 T5-05 提前到数据校验 |
| 9 | `--json` 从 Should 提为 T5-03 验收必需（per-query 明细 + 网格表；报告表格由 JSON 生成）；报告模板补 git commit / HF revision / 模型 id | 数据诚信的工程化保障 |
| 10 | 附录 A：Analyzer"双实现"措辞修正（视 T5-09 而定，失败则如实写"替换路径已定义"） | charabia 失败时 2.4-4 的表述须诚实 |
| 11 | 新增决策点 D10（向量诊断与统计检验纳入范围）；D7/D8 按评审意见修正（hnswio 条件分支；D8 与 R-P5-13 同批决策、vector_ab 用真实语料） | 评审对 D1~D9 全部赞同，仅 D7 附条件、D8 附捆绑 |

### v1.1 → v1.2（2026-09-03，评测数据源切换）

| # | 变更 | 理由 |
| --- | --- | --- |
| 1 | **数据源：手写语料+自标注 → T2Ranking 开源数据集**（第 3~5 章重写） | 消除 v1.1 最大方法论风险（标注者偏差：实现者自标自评）；专业标注（3 人多数票、4 级分级）+ 真实搜索 query，质量与规模（40 → 320 条）双升 |
| 2 | **相关性二元 → 4 级分级**（D3 翻转） | v1.1 选二元的前提是"单人标注一致性差"；专业标注下该前提消失，graded NDCG（2^g−1）信息量更高 |
| 3 | **合成压测语料生成器废弃**，NFR 实测改用同一真实语料（12K ≈ 1 万 chunk 目标规模） | 真实段落自带自然词频长尾；一份数据两种口径，管线更简；消除 v1.1 R-P5-8 |
| 4 | **新增数据集选型章**（3.1 候选对比：T2Ranking vs DuReader/mMARCO/Multi-CPR） | 选型依据可追溯 |
| 5 | **新增转换器设计**（t2_prep.rs：过滤/采样/装配/分桶/校验，固定种子可复现） | 评测数据可复现是 T5-08 硬要求 |
| 6 | 指标手算锚点更新（分级 NDCG ≈ 0.90495）；bench 增 `--rel-threshold`、删 `--pool`；查询量 40→320 引发的样本数/功效更新 | 随 2/3 连带 |
| 7 | 风险表重写：删标注者偏差/合成失真，增下载失败/假负例/分桶误分类/人类查询代理/子集难度等 12 项 | 数据源切换的完整风险面 |
| 8 | 诚信声明模板重写（数据来源与装配、代理偏差披露、外部锚点仅量级对照） | 同上 |
| 9 | 沿承 v1.1 全部仍有效内容：`Searcher::with_bm25_params` 注入口、T5-04/05 顺序对调、tantivy 基线、RRF 敏感性、bench 禁缓存、D6~D9 决策 | — |

### v1.0 → v1.1（2026-09-03，P4 后代码实况修订）

| # | 变更 | 理由 |
| --- | --- | --- |
| 1 | 新增万级压测线与"两条评测线"设计 | v1.0 用 80 篇小语料测 NFR-02/03，但目标值是 @1 万 chunk |
| 2 | 补齐 tantivy 基线（ADR-007 P5 义务）与 RRF k 敏感性（R7） | v1.0 遗漏 |
| 3 | 补 `Searcher::with_bm25_params` 注入口 | `--k1/--b` 无落地路径 |
| 4 | 指标手算基准单测；T5-04/05 顺序对调；延迟测量细化（禁缓存/reps 20/debug 警告）；charabia 版本矛盾改三步验证裁决 | 方法论补强 |
| 5 | 语料 80 → 120 篇、queries 增 qid、bench 参数扩展、新增 D6~D9 | 落地细节 |
