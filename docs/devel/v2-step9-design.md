# HelixIndex V2 · Step 9 详细设计（相关性深化：自适应融合 + 评测资产纪律）

| 项 | 内容 |
| --- | --- |
| 版本 | **v0.1（2026-09-23，首版）** |
| 日期 | 2026-09-23 |
| 状态 | **待评审**。本文**纯设计、`.rs` 零改动**（体例同 `v2-step5-design.md` / `v2-step8-design.md` 的首版）。 |
| 上游 | `plan-v2.md` **v0.23** §4 Step 9 / §4.0 **H7** / §5（**FR-35** / **NFR-15** 草案）/ §6「V2.1 门槛」/ §附-4（P0-4、P0-5、P1-10）；`requirements-spec.md` **v1.23**（**FR-35** `:308` / **NFR-15** `:399`）；`architecture-design.md` **v1.22**（**§5.6** `FusionStrategy` / **§5.7** `Reranker` 的 provided 方法先例 / §14.6）；`eval-report.md` **§3.3**（分桶读数）/ **§3.4**（图抖动）/ **§8.15**；issue **#4**（本步入口） |
| 范围 | ① **T7-14** 自适应融合（**FR-35**；判据 **NFR-15**）；② **T7-15** 假负例敏感度对照（**只作对照**）；③ **T7-16** Agent query 评测集（**外部 LLM API** 生成 + 自查；**只作对照、不替换主基线** —— **H7** 已拍板）；④ 本步的**评测资产纪律**与**判据口径**定值 |
| 非范围 | **不做**权重网格搜索（**D-S9-08**）；**不改** `FusionStrategy::fuse` 的签名（**D-S9-01**）；**不把「桶标签」引入生产路径**（**D-S9-02**）；**不改** Step 7 的窗口机制（`candidate_window` / `R`，**R45** 原样）；**不做** Step 10 的后处理（MMR / token budget / 多 query 融合）；**不做** 精排器的改动（`Reranker` 一字不动）；**不替换**主基线（**H7**） |
| 交付物 | ① 本文（设计）；② 自适应融合实现（`FusionStrategy` 的 **provided 方法** + `Config::adaptive_fusion` 默认 **false**）；③ 评测载体（脚本侧**跨运行配对** + 早停/冻结图纪律）；④ 两个评测资产与生成脚本（T7-16 / T7-15）；⑤ 四处定义面回写 + `eval-report.md` **§8.16** 读数；⑥ 风险 **R55 ~ R58**（→ 架构 **§14.7**） |

---

## 决策速览（D-S9-01 ~ D-S9-10）

| 编号 | 决策点 | 建议 | 定值来源 |
| --- | --- | --- | --- |
| **D-S9-01** | 自适应融合的**落点形态** | **`FusionStrategy` 新增 trait provided 方法**（默认实现 = 今天的行为）⇒ **既有实现零改动、默认逐位一致**；**不改** `fuse` 签名 | **§2.4 N3** + `Reranker::candidate_window` 先例（`rerank/mod.rs:56-85`） |
| **D-S9-02** | 自适应的**信号**从哪来 | **只允许**「检索自身可观测的量」；**禁止**读 qrels / 桶标签 | **§2.4 N1**（桶标签含标注 ⇒ 泄漏 + 生产不可用） |
| **D-S9-03** | 启用面 | `Config::adaptive_fusion`（**默认 `false`**）+ `SearchIndexBuilder::adaptive_fusion(bool)` ⇒ **零行为变化** | 照 `embed_sessions` 先例（`S8-08`） |
| **D-S9-04** | 判据口径 | 主指标**分桶 `MRR@10(1)`**（只看名次 ⇒ 对 **R46** 的 `Hit.score` 语义免疫）+ `NDCG@10` 为辅；**基线 = 固定权重 `(1.0, 1.5)`**；**同一冻结图**；**`candidate_k` 两侧相同**；**精排默认关** | `plan-v2:634` + `eval-report.md` §3.3/§3.4 |
| **D-S9-05** | **统计口径** | **预先声明「按幅度读、不声称显著」**；报每桶 `n` + 同向幅度；**不写「显著」二字** | **已由用户 2026-09-23 拍板**（Step 7 教训：配对符号检验最小 p = 0.1553） |
| **D-S9-06** | **H7：评测资产能否替换主基线** | **只作对照、不替换**（主基线 = T2Ranking 12K / 320 query） | **已由用户 2026-09-23 拍板** |
| **D-S9-07** | T7-16 的 **LLM 通道** | **外部 LLM API**，形态 **provider-agnostic**：`HELIX_LLM_BASE_URL` / `HELIX_LLM_API_KEY` / `HELIX_LLM_MODEL` 三环境变量 + OpenAI 兼容 `POST /chat/completions`；**密钥不入库**；⚠️ **语料外发须用户确认**（见 **R58**） | **已由用户 2026-09-23 拍板** + §2.3（仓内零 LLM 先例） |
| **D-S9-08** | 防过拟合 | **不做权重网格搜索**；自适应规则取「**零参数**」或「**单阈值**」形态；标定只做**阈值灵敏度曲线**，不取最优点 | §2.4 **N5**（80 条/桶的样本量约束） |
| **D-S9-09** | 判据的**评测载体** | **脚本侧跨运行配对**（两次 `helix bench` + 按 `qid` 配对 `per_query` JSON）；**不改** `bench` 的 `--modes` | §2.3（`--modes` 表达不了「hybrid@A vs hybrid@B」） |
| **D-S9-10** | 「先 spike、再投」 | **T7-14 不预先承诺收益**：spike **S9-S1** 出数据再判「投 / 不投」；判「不投」则只交付**工具 + 结论**（同 Step 6 的 E1/E2/E3、Step 8 的 S8-S2 先例） | `plan-v2` 反模式纪律 + **D-S9-08** |

---

## 1. 目标与验收

### 1.1 要解决的问题

| 问题 | 证据（位置） | 严重度 |
| --- | --- | --- |
| **Q-R1** paraphrase 桶 hybrid 被**弱路 BM25 稀释** | `eval-report.md` §3.3（`:48`）：paraphrase 桶（n=80）NDCG@10 —— bm25 **0.2306** / vector **0.4294** / hybrid **0.3799**；hybrid vs vector 的逐 query 胜负 = **25 / 11 / 44（p = 1.0）** ⇒ hybrid 在该桶被拖到 vector 之下 | 高（MRR 口径，`plan-v2:87`） |
| **Q-R3** 假负例敏感度对照未做 | `eval-report.md` §10「残留 P6」第 2 条；`plan-v2:89` | 低 |
| **Q-R4** 评测 query 来自人类日志，缺 Agent query | `eval-report.md` §9「已知偏差」第 3 条；`plan-v2:90` | 中 |

### 1.2 对应需求

| 需求 | 本 Step 的落点 |
| --- | --- |
| **FR-35** 融合策略自适应（`requirements-spec.md:308`，**草案**） | §4.1 ~ §4.3：**按 query 可观测特征**动态决定两路权重；⚠️ 本文更正了该条的措辞「按桶（paraphrase / exact / 长尾）自适应」—— **见 §2.4 N1 与 §6.3**：生产侧**不得**「按桶」（桶含标注） |
| **NFR-15** 自适应融合的质量判据（`requirements-spec.md:399`，**草案**） | §4.5 + §1.3：**指标 / 阈值 / 统计口径**三条在本节定值（D-S9-04 / D-S9-05） |
| **H7**（`plan-v2:117`） | **D-S9-06**：只作对照、不替换 ⇒ P5 锚点 / Step 7 的 `R = 20` 标定 / NFR-12 保持可比 |
| **R46**（`Hit.score` 语义随开关而变） | 判据**只看名次**（MRR@10 / NDCG@10 都是 rank 的函数）⇒ 结构性免疫；⚠️ 但**必须钉死精排开关**（精排会改名次） |

### 1.3 验收标准（**可证伪**，编号化）

> 全部口径见 §4.5：**同一冻结图**（`--index` 读快照）+ **`--runs 1`** + **精排默认关** + **`candidate_k` 两侧相同** + **`k = 10`** + **主基线不替换**。判据只报**幅度与同向**，**不声称显著**（D-S9-05）。

| 编号 | 判据 | 通过条件 | 备注 |
| --- | --- | --- | --- |
| **S9-1a**（主） | paraphrase 桶：自适应 hybrid 的 `MRR@10(1)` **优于固定权重 hybrid** | `ΔMRR ≥ +0.01` | 对应 **NFR-15** 的字面口径（「在分桶上优于固定权重」） |
| **S9-1b**（辅） | paraphrase 桶：自适应 hybrid **追平 vector 单路** | `MRR_adaptive ≥ MRR_vector − 0.002`（噪声容差） | 来自 issue **#4** 的验收建议；⚠️ **上界结构性可达** —— 若自适应把 `w_bm25` 压到 `0`，该桶 hybrid **≡ vector 单路**（`rrf.rs:61-67`），故本条**不是碰运气** |
| **S9-1c**（防回归） | exact / natural / mixed 三桶：自适应 hybrid **不得回退** | `ΔMRR ≥ −0.0024`（= §3.4 实测的**图漂移极差上界**） | 防「为修一桶而伤三桶」；⚠️ **阈值取自实测**（`eval-report.md` §3.4：vector 0.0024 / hybrid 0.0023 / bm25 0.0000） |
| **S9-1d**（全局） | 全量 320 query：自适应 hybrid **不低于**固定权重 hybrid | `ΔMRR ≥ 0` | 「全局结论保持：hybrid MRR/Recall 仍全场最高」（issue #4） |
| **S9-2**（逐位一致） | **关闭**该开关时与今天**逐位一致** | 同快照、同 query、同参数的 `hits` **逐位相同**（`chunk_id` 序 + `score`） | 同 `S8-08` 的「默认 1 ⇒ 零行为变化」纪律 |
| **S9-3**（防泄漏，**源码级**） | 自适应**不读**相关性数据 | `FusionCtx` 的字段**逐个**可从「召回结果 / query / 索引统计」构造出来；**编译期**断言该类型不含 `relevance` / `qrels` / `bucket` 语义字段；依赖注入面（`searcher` 的入参）不含相关性数据 | **本 Step 最重要的一条**（§2.4 N1） |
| **S9-4**（可观测） | `Metrics` 暴露「本次用了哪组权重 / 走了哪个分支」 | 新增字段在 `SearchResponse.metrics` 可断言（照 `Metrics.rerank_window` 先例，`metrics.rs:119`） | 见 §6.2 的破坏性说明 |
| **S9-5**（T7-16） | Agent query 集**产出并在库内冻结** | 资产 + 生成脚本 + prompt/模型记录**同时入库**；**只作对照**读数落 `eval-report.md` §8.16 | **只作对照**（D-S9-06） |
| **S9-6**（T7-15） | 假负例敏感度对照**产出** | 「仅 qrels 语料」重建臂 vs 主基线臂的 Δ 读数落 §8.16；**只作对照** | 结论**不外推**（`eval-report.md` §9） |

⚠️ **判「不投」是合法结论**（D-S9-10）：若 **S9-S1** 的阈值灵敏度曲线表明「不存在一个阈值使 S9-1a 成立且 S9-1c 不破」，则 **T7-14 降级为「交付工具 + 如实记录未达标」**，`NFR-15` 保持「打开」并写明复审触发条件（照 `S8-09` 对 `NFR-14` 的处置口径 —— **三条并记**：口径定稿 + 另立未决 + 复审触发条件）。

---

## 2. 现状与问题定位（源码级）

### 2.1 融合的接口面

| 事实 | 位置 |
| --- | --- |
| trait 只有两个方法，**签名里没有查询上下文**：`fn name()`、`fn fuse(&self, lanes: &[LaneResults], k: usize) -> Vec<(ChunkId, Score)>` | `fusion/mod.rs:21-28` |
| `LaneResults = Vec<(ChunkId, Score)>`（**不带 lane 名**） | `fusion/mod.rs:18` |
| `RrfFusion { k: f32, weights: Vec<f32> }`；默认 `k = 60`、`weights = [1.0, 1.5]`（P5 定稿） | `fusion/rrf.rs:19-22`、`:46-50` |
| **权重是构造期注入的**，`fuse` 内按 `lane_idx` 取：`self.weights.get(lane_idx)` | `fusion/rrf.rs:61-62` |
| 贡献式 `w / (k + rank + 1)`；排序 = `fused_score` 降序、**同分按 `chunk_id` 升序**（确定性 NFR-06）；末尾 `truncate(k)` | `fusion/rrf.rs:65`、`:72-77` |
| 融合**不回捞正文**（硬约束：一旦回捞，换策略就不能独立测） | `fusion/mod.rs:3-7` |
| 编排层的融合调用点：**两路召回之后**、回捞之前 —— `parts.fusion.fuse(&lanes, candidate_k)` | `query/searcher.rs:374` |
| `lanes` 的构造：**`bm25_lane` 与 `vector_lane` 各自 `Some` 就 push**（**空 lane 也会被 push** —— 见 §4.4 的 I9-2） | `query/searcher.rs:362-367` |
| `candidate_k = max(3k, window, 10)`；**窗口必须在召回之前算**（顺序是硬要求） | `query/searcher.rs:171-184` |
| 一个 `QueryExecutor` 持 **一个** `Box<dyn FusionStrategy>`（`bench` 每 mode × 每 run 装配一次） | `query/searcher.rs:725-741`、`:767-770` |
| `to_lane` **不过滤**分数（`Scored` → `(chunk_id, score)` 原样） | `query/searcher.rs:863-865` |

### 2.2 **分桶的定义**与判据载体

| 事实 | 位置 |
| --- | --- |
| 分桶常量：`OSTAR_THRESHOLD = 0.5` / `NATURAL_MIN_CHARS = 16` / `PER_BUCKET = 80` / `SEED_BUCKET` | `examples/t2_prep.rs:57` / `:58` / `:53` / `:48` |
| 分桶规则：含 ASCII 字母数字 ⇒ `mixed`；否则 `chars ≥ 16` ⇒ `natural`；**否则**算 `o*`：**遍历 `qrels[qid]` 里 `grade ≥ 1` 的段落**，取 `max(\|q_terms ∩ p_terms\| / \|q_terms\|)`，`o* ≥ 0.5` ⇒ `exact`，否则 `paraphrase` | `examples/t2_prep.rs:193-231` |
| 🔴 **`exact`/`paraphrase` 的划分用到了 qrels**（`grade ≥ 1` 的段落）⇒ **桶标签含标注信息** | 同上（`t2_prep.rs:206-230`） |
| 标签**物化在评测资产里**：`data/t2-queries.jsonl` 字段 = `qid` / `query` / `relevance` / **`type`（桶标签）**；共 **320 条**（四类各 80） | `data/t2-queries.jsonl` |
| 判据的执行载体 = `helix bench` 的「分桶（NDCG@10）」表（按 `j.qtype` 聚合 `agg_type`）+ JSON 的 `buckets` / `per_query`（逐条带 `qid` / `type` / 三个指标） | `cli/src/bench.rs:1218-1240`、`:2156-2200` |
| 「预期占优」是**声明式的假设表**（`exact → bm25`、`paraphrase → vector`），**已被 P5 实测推翻一多半** | `cli/src/bench.rs:61-67` + `eval-report.md` §3.3 |
| 逐桶符号检验（**hybrid vs 单路**、per-query NDCG delta、二项精确单侧） | `cli/src/bench.rs:525-532`、`:1246-1260` |

### 2.3 评测工具的能力与**缺口**

| 能 | 位置 |
| --- | --- |
| **固定权重**的对照臂**今天就能跑**：`--rrf-k` / `--rrf-weights`（后者约束「两个非负数」） | `cli/src/bench.rs:111`、`:116`、`:313-325` |
| **冻结图**：`--index` 读快照（与 `--input` 二选一） | `cli/src/bench.rs:73`、`:800` |
| 逐 query 结果落 JSON（可做**脚本侧配对**） | `cli/src/bench.rs:2166-2180` |

| 不能（**本步要绕开或补齐**） | 说明 |
| --- | --- |
| `--modes` 只认 mode 名（`bm25` / `vector` / `hybrid`）⇒ **表达不了「hybrid@权重 A vs hybrid@权重 B」的同轮对照** | ⇒ **D-S9-09**：两次运行 + 脚本按 `qid` 配对；⚠️ 两次运行**必须同一冻结图**（否则带上 HNSW 的 `OsRng` 图漂移） |
| 既有符号检验是 **hybrid vs 单路**，**不是**「两个 hybrid 配置之间」的逐桶配对 | ⇒ 配对与逐桶聚合在**脚本侧**做（本步**不改** `bench`） |
| 仓内**零 LLM 通道先例**：`grep -rnE "openai\|api_key\|OPENAI\|deepseek\|qwen\|LLM_\|llm_"` 在 `*.rs` / `*.sh` / `*.toml` / `*.py` 上 **0 命中** | ⇒ **D-S9-07**（外部 API + 环境变量 + provider-agnostic） |

### 2.4 🔴 设计期新发现

> 本节是本文价值的主体：**用源码推翻计划里的想当然**。

**N1 🔴 「桶」不是纯 query 特征 —— 它含标注信息（`t2_prep.rs:206-230`）**

`exact` / `paraphrase` 的划分 = 「query 词项在**标注为相关（`grade ≥ 1`）的段落**里的最大覆盖率是否 ≥ 0.5」。
⇒ 两个后果：

1. **生产不可用**：线上没有 qrels ⇒ 这个「桶」在生产环境**算不出来**；
2. 🔴 **标签泄漏**：若实现按桶标签调权，等价于**把答案喂进排序**（「这条 query 的答案段落不怎么含 query 词 ⇒ 所以压 BM25」）⇒ 在评测集上**必然**「优于固定权重」，但结论**既不可信也不可投产**。
   ⇒ 这正好命中 **NFR-15 的字面口径**（「在**分桶**上优于固定权重」）⇒ **必须在设计期就把两件事分开**：
   **桶 = 评测分组（可含标注）**；**自适应信号 = 检索自身可观测的量（禁含标注）**。⇒ **D-S9-02**，判据 **S9-3**。
   ⚠️ 顺带：**FR-35 的原文措辞「融合权重按桶（paraphrase / exact / 长尾）自适应」需更正**（见 §6.3 与 §5 的 D-S9-02）。

**N2 🔴 架构 §5.6 的 `FusionStrategy` 形态与实现漂移（`architecture-design.md:492-501`）**

架构写的是：

```rust
fn fuse(&self, lanes: &[(&'static str, Vec<ScoredHit>)], weights: &[f32]) -> Vec<ScoredHit>;
```

实际是：`fn fuse(&self, lanes: &[LaneResults], k: usize) -> Vec<(ChunkId, Score)>`（`fusion/mod.rs:28`）。三处不一致：
① 形参里**没有** `weights`（权重在**构造期**注入，`rrf.rs:27-30`）；② 类型名 `ScoredHit` **不存在**（实际 `LaneResults` / `(ChunkId, Score)`）；③ 实际有 `k` 形参、架构里没有。
⇒ **回写架构时按实现更正**（本文 §6.4 已列）。

**N3 🔴 `fuse` 没有查询上下文 ⇒ FR-35 在「不动接口」的前提下**结构性做不到****

自适应 = 「同一个策略实例、不同 query 用不同权重」。而 `fuse(&self, lanes, k)` 的入参只有候选池，**拿不到任何 query 特征**；`searcher.rs:374` 也**只传这两个**。
⇒ 三条路：

| 路 | 形态 | 代价 |
| --- | --- | --- |
| A | 在 `searcher` 内**每次查询新建** `RrfFusion::new(k, 算出的 weights)` | 可行但**丢了可解释性**（策略退化成一次性参数）、且**换策略就得改编排层** |
| B | **改** `fuse` 签名（加 `ctx`） | **破坏性**：下游自定义 `FusionStrategy` 实现全部要改 |
| **C** ✅ | trait **provided 方法**（默认实现 = 今天的行为，返回 `None` ⇒ 用原权重） | **非破坏**：既有实现**一行不改**、默认**逐位一致** |

⇒ **取 C**（**D-S9-01**），依据是**同仓已有先例**：`Reranker::candidate_window`（`rerank/mod.rs:56-85`）—— 它同样是「给 trait 加一个默认零行为的方法」，并在 rustdoc 里明写「① 既有实现一行不改；② 未装精排器时与之前**逐位一致**」。

**N4 ⚠️ 判据载体表达不了「A/B 两个 hybrid 配置」（见 §2.3）** ⇒ **D-S9-09**（脚本侧配对），并把「必须同一冻结图」写成硬纪律。

**N5 ⚠️ 80 条/桶的样本量 ⇒「搜出来的最优权重」不可信（量化理由）**

paraphrase 桶只有 **80** 条，而该桶的**路间差距**（vector − bm25）= 0.4294 − 0.2306 = **0.1988**、**图漂移极差** = 0.0024（§3.4）⇒ 噪声/信号 ≈ **1.2%**，单看「能不能区分方向」是够的；但**在一个 80 点的集合上搜权重**会出现「最优值随样本抖动」：任何**多参数**或**多阈值**的搜索都能把噪声拟合进去，且**无法用同一样本自证**（Step 7 已在 320 条上吃到「配对符号检验不显著、p 最小 0.1553」的教训）。
⇒ **D-S9-08**：自适应规则**只允许**「零参数」或「单阈值」形态；标定只出**灵敏度曲线**（扫描阈值看判据是否在**一段区间**内成立），**不取最优点**。

**N6 ⚠️ `plan-v2` 的「T7-16 是 T7-14 的**前置**」（§附-4 P1-10）在源码事实上**不成立****

§附-4 的推理是「『在分桶上优于固定权重』需要**分桶依据**，而现 qrels 来自人类日志（Q-R4）」。**但**：
`data/t2-queries.jsonl` 的 **`type` 字段就是桶标签**（四类各 80、已物化）、`bench` 已按它出分桶表（`bench.rs:1223`）⇒ **T7-14 的判据今天就能算**，**不需要**等 T7-16。
⇒ 依赖关系应更正为：**T7-14 与 T7-15 / T7-16 独立并行**；T7-16 的定位是**外部效度增量**（「人类 query 作为 Agent query 的代理」这一已知偏差，`eval-report.md` §9），**不是** T7-14 的硬前置。
⚠️ 且 **T7-16 的产物若没有 qrels，就既算不出桶、也评不了分** ⇒ 它必须**自带标注**（弱标签，见 R57）⇒ 更不适合当 `NFR-15` 的主判据（**D-S9-04** 把主判据钉在**既有 320 条**上）。

---

## 3. 设计约束（既有事实，本文不重新论证）

1. **RRF 只用名次、免疫量纲**（`rrf.rs:1-4`）⇒ 自适应**只需要改权重**，不需要碰分数归一化。
2. **融合不回捞正文**（`fusion/mod.rs:3-7`）⇒ 自适应信号**不能依赖正文**（只能依赖 `lanes` + query + 索引统计）。
3. **确定性**（NFR-06）：结果排序必须确定（同分按 `chunk_id` 升序）；自适应**不得引入随机**。
4. **逐位一致前置**（NFR-10 ①）：关闭开关时与今天**逐位一致**。
5. **评测资产纪律**（`plan-v2:541`）：新造资产**只作对照、不替换主基线**（**H7** / **D-S9-06**）。
6. **结论不外推**（`eval-report.md` §9）：仅对「T2Ranking 中文网络语料 + bge-small-zh-v1.5 + 12K 段落」成立。
7. **A/B 必须同一冻结图**（`plan-v2` V2.1 开工纪律）：`--index` 读快照 + `--runs 1`。
8. **延迟类读数不具跨机可引用性**，报数必标运行范围（`architecture-design.md:1668`）。

---

## 4. 详细设计

### 4.1 形态（D-S9-01）

```rust
/// 融合上下文：**只含检索自身可观测的量**（I9-1）。
pub struct FusionCtx<'a> {
    /// 各路候选的**统计摘要**（不含正文、不含相关性数据）
    pub lanes: &'a [LaneStats],
    /// query 侧可观测特征（词项数 / 是否含 ASCII 词 / 字符数）
    pub query_terms: usize,
    // ⚠️ 新增字段前必须过 I9-1 的自检（S9-T3）
}

pub trait FusionStrategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn fuse(&self, lanes: &[LaneResults], k: usize) -> Vec<(ChunkId, Score)>;

    /// 【Step 9 / FR-35 新增，**provided**】按 query 侧信号**覆盖本次的权重**。
    ///
    /// 默认实现返回 `None` ⇒ 沿用策略自身的权重 ⇒ **既有实现一行不改**、**逐位一致**。
    /// ⚠️ 返回 `Some(w)` 时，`w` 必须与 `lanes` **一一对应**（含**空 lane** 的槽位，见 I9-2）。
    fn weights_override(&self, _ctx: &FusionCtx<'_>) -> Option<Vec<f32>> {
        None
    }
}
```

- `LaneStats`（设计期定形为**最小集**）：`len` / `top_score` / `last_score`（都是 `lanes` 已有的量）。
- 编排层（`searcher.rs:374` 附近）：构造 `FusionCtx` → `weights_override(&ctx)` → `Some(w)` 时用 `w`、否则用策略原权重 ⇒ **不满足条件时行为与今天完全一致**。
- ⚠️ **不改 `fuse` 的签名**（N3）；⚠️ **不新增 `FusionStrategy` 实现**（`AdaptiveRrfFusion` 这类实现**走不通** —— 它的 `fuse` 拿不到 `ctx`）。

### 4.2 自适应规则（两级；**零参数 / 单阈值**，D-S9-08）

| 级 | 触发条件 | 动作 | 是否需要标定 |
| --- | --- | --- | --- |
| **tier 1**（**零参数、确定性**） | **某 lane 为空**（`len == 0`） | 把该 lane 的权重置 **0**（**不**从 `lanes` 里删 —— I9-2） | ❌ 不需要（`len == 0` 是确定事实） |
| **tier 2**（**单阈值**） | 信号 `s < θ`（信号 = 「BM25 路的弱路程度」，**候选**见 §4.3） | `w_bm25` 按**单参数**单调映射降到 `0`（或档位 `{1.0, 0.5, 0.0}`） | ✅ **spike S9-S1**（决策门见 §4.5） |

⚠️ 「某 lane 为空」在 Hybrid 下**可达**：`lanes` 是「两个 `Some` 就 push」（`searcher.rs:362-367`），而 lane 自身可以是**空 vec**（`to_lane` 不过滤、召回可以 0 命中）。
⚠️ **实现期须核实**（**Q9-2**）：BM25 是否会产生 **`score == 0` 的非空 lane**（`to_lane` 不过滤，但上游是否已过滤未核）—— 若会，则 tier 1 可扩为「`len == 0` **或** `top_score ≤ 0`」。**本文不拍这个前提**。

### 4.3 tier 2 的候选信号（**都是检索自身可观测的量**）

| 候选 | 构造 | 优点 | 风险 |
| --- | --- | --- | --- |
| **s1** 两路 top-k **重合率** | `\|BM25 top-k ∩ Vector top-k\| / k` | 纯检索侧、无需标注、与「词面 vs 语义」分歧直接相关 | 对 `k` 敏感（须与判据的 `k` 钉死） |
| **s2** BM25 路的**内部分布** | `top_score / (top_score − last_score)` 之类 | 只用一路 | BM25 无上界 ⇒ 绝对量不可比；形态须先实测 |
| **s3** query 词项的**索引侧覆盖** | 用 `Query` 的词项去 `Index` 查 `df(t)` / 命中词项数占比 | **最接近「词面重合」的代理**且**不依赖 qrels** | 需要一次索引侧查询（成本 O(词项数)，与 §2.1 的「不回捞正文」不冲突——只查词典） |
| **s4** query 侧字符特征（桶的**纯 query 部分**） | 含 ASCII / 字符数 ≥ 16 | **零成本**、与既有桶定义的 `mixed` / `natural` **同源**、**无标注** | 只覆盖两桶；对 exact/paraphrase 无区分力 |

⇒ **设计期不自选**：**spike S9-S1** 把 s1 / s3 / s4 各跑一遍**灵敏度曲线**，按 §4.5 的决策门选一个（选不出来就判「不投」）。

### 4.4 不变式清单

| 编号 | 不变式 | 判据 |
| --- | --- | --- |
| **I9-1** | 自适应信号**只由**「召回结果 / query / 索引统计」构造；**永不**读 qrels、桶标签、或任何相关性数据 | **S9-3**（源码级 + 类型级） |
| **I9-2** | **禁止**通过「从 `lanes` 里删掉一路」来剔除该路 —— 权重按 `lane_idx` 取（`rrf.rs:61-62`）⇒ 删 lane 会让权重**串到另一路** | `S9-T2`（空 lane 的槽位仍在、权重仍对齐） |
| **I9-3** | 关闭开关时**逐位一致** | **S9-2** |
| **I9-4** | 自适应**不改**候选池 / 截断顺序 / `candidate_k` ⇒ 只改融合权重 | `S9-T4`（`candidate_k` 两侧相同；`Metrics.candidates` 不变） |
| **I9-5** | 自适应**不引入**随机（NFR-06） | 代码里无 RNG（评审红线） |
| **I9-6** | 精排**一字不动**（`Reranker` / `candidate_window` / `R` 全部原样） | `S9-T5` |

### 4.5 标定设计（spike **S9-S1**）与决策门

**协议**（硬纪律）：

```text
① 先冻结图：helix build --index <snap>（一次），此后所有臂 --index <snap> --runs 1
② 固定 k = 10、candidate_k 两侧相同（都是 max(3k, 10) = 30 —— 精排关 ⇒ window = k）
③ 先跑 baseline：--rrf-weights 1.0,1.5   （= 今天的默认；这就是"固定权重"臂）
④ 再逐 θ 跑自适应臂（同一冻结图）⇒ 出「θ 灵敏度曲线」
⑤ 配对：脚本按 per_query 的 qid 配对，逐桶算 ΔMRR@10(1)，并出 4 桶 + 全局 4 条
```

**决策门**（**合取**，缺一即判「不投」）：

| 门 | 判据 | 阈值 |
| --- | --- | --- |
| G1 | paraphrase 桶 `ΔMRR`（自适应 − 固定权重） | **≥ +0.01**（S9-1a） |
| G2 | exact / natural / mixed 三桶**各自**的 `ΔMRR` | **≥ −0.0024**（S9-1c） |
| G3 | 全量 320 条 `ΔMRR` | **≥ 0**（S9-1d） |
| G4 | **G1 ∧ G2 ∧ G3 在 θ 的一段区间内同时成立**（不是单点） | 区间宽度 ≥ 一个可解释的档（如 0.1 的信号量纲） |
| G5 | 判据**只看名次**（对 R46 免疫）+ 精排关 + 同一冻结图 | 结构条件，逐条自证 |

⚠️ **G4 是防过拟合的关键**（对应 N5）：**单点达标不算达标**（在 80 条上总能挑到一个点）。若不存在这样的区间 ⇒ 判「不投」。
⚠️ **报数纪律**（D-S9-05）：报 `Δ` 的**幅度与同向**，**不写「显著」**；同时报每桶 `n` 与 baseline 的绝对数，便于评审独立复算。

### 4.6 与 R45 / R46 的边界

- **R45**（窗口被 `candidate_k` 静默夹取）：本步**不碰窗口**（I9-6）；判据两侧 `candidate_k` 相同 ⇒ R45 不进本步的判据面，**只在 §9 登记为「Step 10 的输入」**。
- **R46**（`Hit.score` 语义随开关而变）：判据只看**名次** ⇒ 免疫；但**必须钉死精排开关**（精排打开会改名次 ⇒ 那是**另一个读数面**，只作对照）。

### 4.7 T7-16（Agent query 评测集）与 T7-15（假负例对照）

**T7-16**（**只作对照**，D-S9-06 / D-S9-07）：

- 通道：**外部 LLM API**，形态 **provider-agnostic** —— 三个环境变量（`HELIX_LLM_BASE_URL` / `HELIX_LLM_API_KEY` / `HELIX_LLM_MODEL`）+ OpenAI 兼容 `POST /chat/completions`；**密钥只走环境变量、不入库**。
- 生成协议：多轮 + 自查（prompt 与温度/种子等参数**必须记录**）⇒ **R57**（可复现性：模型版本漂移）。
- **产出形态**：query + **自带弱标注**（否则既算不出桶、也评不了分 —— **N6**）；**冻结入库**（不依赖"下次再跑一遍就能得到同样的集"）。
- 🔴 **边界**：**NFR-15 的主判据不建立在它上**（主判据 = 既有 320 条；见 D-S9-04）—— 否则标注噪声会混进判据。

**T7-15**（**只作对照**）：用「**仅 qrels 语料**」重建一份对照资产，量化随机负例带来的假负例对指标的**影响幅度**；protocol 与 T7-16 同级（冻结 + 只作对照），读数落 §8.16。

---

## 5. 决策记录（D-S9-01 ~ D-S9-10）

见文首「决策速览」。需**评审拍板**的三条：

1. **D-S9-02**（信号禁含标注）+ 由此对 **FR-35 原文措辞**的更正（§6.3）—— 这是本步**唯一**触碰需求措辞的地方。
2. **D-S9-08 / G4**（**不做网格搜索 + 单点达标不算达标**）—— 它把「判据怎么才算过」的严格度显著提高（后果：**更可能判「不投」**）。
3. **D-S9-04**（精排默认关为判据面）—— 若评审认为判据应在**精排开**的口径下定值，则 §4.5 的协议与 `k` / `candidate_k` 全部要重算（⚠️ **这会改变 `candidate_k`**：精排开 ⇒ `window = R = 20` ⇒ `candidate_k = max(30, 20)` 仍 30，但**交付条数**与名次会变 ⇒ 必须重测 baseline）。

---

## 6. 影响面与兼容性

### 6.1 兼容性

| 变更 | 是否破坏性 | 依据 |
| --- | --- | --- |
| `FusionStrategy` 加 **provided 方法** | **否** | 默认实现 ⇒ 既有实现一行不改（同 `Reranker::candidate_window` 先例） |
| 新增 `FusionCtx` / `LaneStats` 类型 | **否** | 纯新增 |
| `Config` 加 `adaptive_fusion`（默认 `false`） | **否** | `Config` **不是外部可达类型**（未从 `helix_core::search` 再导出 —— `S8-08` 已核）；且默认关 ⇒ 零行为变化 |
| `Metrics` 加字段（可观测性） | ⚠️ **按字面是**（`Metrics` 是 `pub`、字段全 `pub`、**无** `#[non_exhaustive]`，`metrics.rs:43-44`） | **先例**：Step 7 已加 `rerank_window` / `rerank_elapsed`（`metrics.rs:101` / `:119`）；且 `Metrics: Default` ⇒ 影响面小。**评审可要求改为不扩 `Metrics`**（改走 `Explain`） |
| CLI / `bench` | **否**（本步不改） | D-S9-09 |

### 6.2 `Metrics` 新增字段（提案，待评审）

- `fusion_weights: Option<Vec<f32>>`（**仅**在自适应生效时 `Some`）—— 「本次用了哪组权重」；
- `fusion_signal: Option<f32>`（本次算出的信号值）—— 「为什么这么选」。
  ⚠️ 二者都**只读**且默认 `None` ⇒ 关闭开关时与今天同形（除结构体多两个字段）。

### 6.3 对 **FR-35** 措辞的更正（**本文提出，需评审拍板**）

- 原文（`requirements-spec.md:308` / `plan-v2:603`）：「融合权重**按桶**（paraphrase / exact / 长尾）自适应」。
- 建议改为：「融合权重**按 query 侧可观测特征**自适应（**不得依赖相关性标注**）；『桶』仅用于**评测分组**」。
- 理由：**N1**（桶含标注 ⇒ 生产不可用 + 标签泄漏）。
- ⚠️ **保留原措辞引文**（本项目纪律：被推翻的结论不要静默删除）。

### 6.4 需回写的**架构漂移**（**N2**）

`architecture-design.md:492-501`（§5.6）的 trait 伪码与实现不一致（`weights` 形参 / `ScoredHit` 类型名 / 缺 `k` 形参）⇒ 本轮**按实现更正**，并把新增的 provided 方法一并写进该节（**原伪码全文引文见 §2.4 N2**）。

---

## 7. 测试计划（S9-T1 ~ S9-T12）

| 编号 | 名称 | 进 CI | 说明 |
| --- | --- | --- | --- |
| **S9-T1** | 关闭开关 ⇒ **逐位一致**（`hits` 的 `(chunk_id, score)` 序列） | ✅ | 与既有 `S8-08` 的零行为变化判据同形；**必须**含「今天的行为」作为期望值来源（用 `RrfFusion::default()` 现算，不硬编码） |
| **S9-T2** | **空 lane 的槽位仍在**、权重仍对齐（I9-2） | ✅ | 构造「BM25 空 / vector 非空」与反向两臂；断言权重**没有串到另一路**（用可区分两路的合成输入） |
| **S9-T3** | **I9-1 的类型级/源码级自检** | ✅ | 断言 `FusionCtx` 的字段集合 = 白名单（编译期常量 + 单测）；并在仓库内**反扫**「`fusion` 模块依赖 `qrels` / `relevance` / `bucket`」= 0 命中（⚠️ 对照样本先自证） |
| **S9-T4** | 自适应**不改** `candidate_k` / `Metrics.candidates`（I9-4） | ✅ | 开关两态对同一 query 比 `candidate_k` 与 `candidates` |
| **S9-T5** | 精排一字不动（I9-6） | ✅ | `Reranker::candidate_window` 的默认值与 `R` 联动回归（复用既有用例即可，**新增**一条「fusion 开关与窗口正交」） |
| **S9-T6** | `weights_override` 返回 `None` ⇒ 与 `fuse` 直调**同结果** | ✅ | provided 方法的**等价性**判据 |
| **S9-T7** | tier 1：`len == 0` ⇒ 权重 0、结果 ≡ 单路 | ✅ | 合成 lane；断言结果与「只有 vector 一路」**逐位相同** |
| **S9-T8** | 确定性（NFR-06）：同输入两次调用同结果 | ✅ | 含「同分按 `chunk_id` 升序」 |
| **S9-T9** | `Config::adaptive_fusion` 默认 `false` + 不进**配置指纹** | ✅ | 照 `embed_sessions` 先例（会话数不进指纹的判据可复制） |
| **S9-T10** | `Metrics` 新字段的语义（关时不写、开时写对） | ✅ | |
| **S9-T11** | **端到端判据臂**（S9-1a ~ S9-1d） | ⚠️ **`#[ignore]`**（需语料 + 冻结图） | 走 `scripts/eval_s9.sh`（本步新增） |
| **S9-T12** | T7-16 生成器的**离线桩**（不发网络请求） | ✅ | 用固定桩响应断言解析/落盘/配对逻辑（**CI 不发外部请求**） |

---

## 8. 实施任务拆分（S9-01 ~ S9-07）与 PR 切分

| 任务 | 内容 |
| --- | --- |
| **S9-01** | **本设计**（本文，`.rs` 零改动）+ 四处定义面回写 |
| **S9-02** | `FusionCtx` / `LaneStats` + `FusionStrategy::weights_override`（provided）+ 编排层接线 + `Config::adaptive_fusion`（默认 false）+ `Metrics` 两字段 |
| **S9-03** | 自适应规则实现（tier 1 + tier 2 的**信号与单阈值**，**由 S9-S1 选型**）+ 单测 S9-T6/T7 |
| **S9-04** | `scripts/eval_s9.sh`（**同一冻结图** + 两次运行 + 脚本侧配对 + 4 桶 × 4 条判据 + θ 灵敏度曲线） |
| **S9-05** | **spike S9-S1** 跑数与结论（**合取决策门 G1~G5**）⇒ 判「投 / 不投」 |
| **S9-06** | T7-16（外部 LLM API 通道 + 生成脚本 + 冻结资产 + 自查） / T7-15（假负例对照） |
| **S9-07** | 收尾：`eval-report.md` **§8.16** + 四处定义面回写 + `NFR-15` 定稿（**达标则去「拟」；不达标则收窄 + 另立 + 保持打开 + 复审触发条件** —— 三条并记） |

**PR 切分建议（4 段）**：

| PR | 内容 | 依赖 |
| --- | --- | --- |
| **PR9-0** | 本文（设计）+ 四处回写 | — |
| **PR9-1** | S9-02 + S9-03 + 判据 S9-T1~T9（**含生产代码**；默认关 ⇒ 零行为变化） | PR9-0 |
| **PR9-2** | S9-04 + S9-05（评测载体 + spike 结论） | PR9-1 |
| **PR9-3** | S9-06（T7-16 / T7-15；**只作对照**，可与 PR9-2 并行） | PR9-0 |
| **PR9-4** | S9-07 收尾 | PR9-1~3 |

---

## 9. 风险与未决问题

### 9.1 新增风险（**R55 ~ R58**，→ 架构 **§14.7**）

| 编号 | 风险 | 触发条件 | 后果 | 应对 |
| --- | --- | --- | --- | --- |
| **R55** | **桶标签含 qrels ⇒ 标签泄漏**：`exact` / `paraphrase` 的划分用到了 `grade ≥ 1` 段落（`t2_prep.rs:206-230`） | 实现若「按桶标签」调权 | 评测集上**必然**「优于固定权重」，但结论**不可信、不可投产**（且这正是 NFR-15 的字面口径） | **D-S9-02**（信号禁含标注）+ 判据 **S9-3**（类型级 + 反扫）+ §6.3 更正 FR-35 措辞 |
| **R56** | **80 条/桶的样本量 ⇒ 阈值可被拟合** | 用网格搜索挑最优点 | 「达标」是挑出来的；换样本即失效（同族：**R53** 的尾部不可复现） | **D-S9-08** + 决策门 **G4**（**区间**达标才算）+ 只报幅度不称显著（**D-S9-05**） |
| **R57** | **LLM 生成资产不可复现**：模型版本 / 端点 / 采样参数漂移 ⇒ 「同一份集」下次跑不出来 | 只存 query 不存来源参数 | 对照读数不可复核 | ① **冻结入库**（资产为准，不靠重跑）；② **记录 model + 参数 + prompt**；③ 生成脚本**只读环境变量**、**CI 不发外部请求**（S9-T12 用桩） |
| **R58** | **语料 / query 外发（数据出境与合规）** | 把 T2Ranking 语料或 query 发给外部 LLM 端点 | 合规风险（且 T2Ranking 的使用条款需核） | ⚠️ **开工前须用户确认**（**D-S9-07** 的附条件）；若不允许 ⇒ 降级为「规则化生成」并把**同构性声明降级**写进 §8.16 |

### 9.2 未决问题（Q9-x）

| 编号 | 问题 | 谁答 / 何时 |
| --- | --- | --- |
| **Q9-1** | tier 2 的**信号选型**（s1 / s3 / s4）与单阈值形态 | **spike S9-S1**（PR9-2） |
| **Q9-2** | BM25 是否会产生 **`score == 0` 的非空 lane**（决定 tier 1 是否可扩为「`len == 0` 或 `top ≤ 0`」） | **实现期**（读 `retriever/bm25.rs` 的累加点 + `to_lane` 不过滤这一半已核）；⚠️ **本文不拍这个前提** |
| **Q9-3** | 判据面是否应改到**精排开**的口径（**D-S9-04** 的备选） | **评审拍板**（若改，§4.5 协议与 baseline 全部重测） |
| **Q9-4** | T7-16 的**弱标注**怎么产生（LLM 自评 / 多轮一致 / 人工抽检比例） | **PR9-3 设计期**；⚠️ 不得把弱标注用于 NFR-15 的主判据（**N6**） |
| **Q9-5** | 是否**扩展 `bench`** 以支持「同轮 A/B 两个 hybrid 配置」（替代脚本侧配对） | **评审拍板**；默认**不扩**（D-S9-09） |

---

## 附录 A：新增 / 变更 API 一览 + 不变式（写进 rustdoc 的三条以内）

| 项 | 形态 | 兼容性 |
| --- | --- | --- |
| `FusionCtx<'a>` / `LaneStats` | **新增** `pub struct`（`fusion` 模块） | 非破坏 |
| `FusionStrategy::weights_override` | **新增 provided 方法**（默认 `None`） | **非破坏**（既有实现零改动） |
| `Config::adaptive_fusion` | **新增** `pub` 字段，默认 `false` | 非破坏（`Config` 非外部可达；默认关 ⇒ 零行为变化） |
| `SearchIndexBuilder::adaptive_fusion(bool)` | **新增** builder 方法 | 非破坏 |
| `Metrics::fusion_weights` / `Metrics::fusion_signal` | **新增** 字段（`Option`，默认 `None`） | ⚠️ **按字面是破坏性**（见 §6.1；先例 = Step 7） |

**写进 rustdoc 的三条不变式**（I9-1 / I9-2 / I9-3）：信号只含可观测 ⇒ 不读相关性数据；剔路只能**置 0**、**不得删槽**（否则权重串路）；关闭开关 ⇒ **逐位一致**。

## 附录 B：执行命令（可复制的真实命令）

```bash
# ① 判据臂（同一冻结图；baseline = 固定权重）
cargo run --release -p helix -- bench --index <snap> --queries data/t2-queries.jsonl \
  --modes bm25,vector,hybrid --k 10 --runs 1 --rrf-k 60 --rrf-weights 1.0,1.5 \
  --json /tmp/s9_baseline.json
cargo run --release -p helix -- bench --index <snap> --queries data/t2-queries.jsonl \
  --modes hybrid --k 10 --runs 1 --rrf-k 60 --adaptive-fusion true \
  --json /tmp/s9_adaptive_theta-<θ>.json      # ⚠️ --adaptive-fusion 是 PR9-1 新增的门面开关

# ② 脚本侧配对（按 qid 配对 per_query，逐桶算 ΔMRR@10(1) + 4 桶 + 全局 + θ 曲线）
bash scripts/eval_s9.sh --baseline /tmp/s9_baseline.json --arms /tmp/s9_adaptive_theta-*.json
```

⚠️ **`--adaptive-fusion` 的 CLI 开关本身要先过 `#50` 的同族问题**（Step 7 的窗口参数也曾纠结「库内默认 vs CLI 默认」）⇒ PR9-1 设计期定：**库侧默认 `false`；CLI 只提供显式打开**（不给 CLI 默认值 ⇒ 与 `embed_sessions` 的处置口径一致）。

## 附录 C：依赖源码核实记录

| 位置 | 事实 |
| --- | --- |
| `crates/core/src/fusion/mod.rs:9-13` | 模块与再导出只有 `rrf` / `weighted` |
| `crates/core/src/fusion/mod.rs:21-28` | trait 签名**无查询上下文**（**N3**） |
| `crates/core/src/fusion/rrf.rs:19-22` | `weights` 是**字段**、非 `fuse` 形参（**N2**） |
| `crates/core/src/fusion/rrf.rs:61-62` | 权重按 `lane_idx` 取 ⇒ **删 lane 会串权重**（**I9-2**） |
| `crates/core/src/query/searcher.rs:362-367` | **空 lane 也会被 push** |
| `crates/core/src/query/searcher.rs:863-865` | `to_lane` **不过滤**分数（**Q9-2** 的一半已核） |
| `crates/core/src/rerank/mod.rs:56-85` | **provided 方法**先例（**D-S9-01** 的依据） |
| `crates/core/src/query/metrics.rs:43-44` | `Metrics` 无 `#[non_exhaustive]`、字段全 `pub`（§6.1） |
| `crates/cli/src/bench.rs:111-116` / `:313-325` | `--rrf-k` / `--rrf-weights` 已存在（固定权重臂**今天可跑**） |
| `crates/cli/src/bench.rs:2166-2180` | `per_query` JSON（脚本侧可配对） |
| `crates/core/examples/t2_prep.rs:206-230` | **桶划分用到 qrels**（**N1** / **R55**） |
| `data/t2-queries.jsonl` | 320 条、**`type` = 桶标签已物化**（**N6**） |

## 附录 D：本文引用的项目内证据

| 主张 | 位置 |
| --- | --- |
| paraphrase 桶被弱路稀释（0.2306 / 0.4294 / 0.3799；25/11/44，p=1.0） | `eval-report.md` §3.3（`:48`） |
| 图抖动极差 ≤ 0.0024（→ S9-1c 的阈值来源） | `eval-report.md` §3.4（`:63`） |
| 配对符号检验「不显著」的教训（最小 p = 0.1553） | `plan-v2` §4 Step 9 + `NFR-15` 原文 |
| H7（评测资产能否替换主基线） | `plan-v2:117` |
| Step 9 的验收门槛 | `plan-v2:634` |
| R45 / R46 的既有登记 | `architecture-design.md` §14.5（`:1625`） |
| Step 8 的风险占位到 R54（⇒ 本步从 **R55** 起） | `architecture-design.md` §14.6（`:1643-1657`） |
