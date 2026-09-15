# HelixIndex V2 · Step 7 详细设计（精排接入：`bge-reranker-v2-m3` + 候选窗口放开）

| 项 | 内容 |
| --- | --- |
| 版本 | **v0.2（2026-09-14，评审收口版）** |
| 日期 | 2026-09-14 |
| 状态 | **评审已收口（v0.2，2026-09-14）**——回答 **H3（精排窗口）** 并给出 **NFR-12** 的口径形态（数值标「拟」、待 **S7-04 标定（决策门 §4.6.4）、S7-05 回填**，同 **NFR-13 先例**：v1.10 落「拟」→ v1.12 定稿）。**四处拍板 D-S7-04 / D-S7-05 / D-S7-01 / D-S7-08 —— 评审全部同意本文建议**（2026-09-14 第 1 轮：1 条 `COMMENTED` + 6 条行内）。**F1~F6 六条意见全部采纳**：**F1**（P1）`plan-v2` §8 残留块与 **D-S7-02** 矛盾（含我自己同类漏改）／**F2**（P2）S7-04/S7-05 归属 **8 处**不一致／**F3**（P2）NFR-12 标定语料误写「10 万级」／**F4**（P2）「≤500ms（拟）」无出处且与 §4.2.2 粗估相抵／**F5**（P3）附录 C `impl.rs` 行号整段漂移（**语义属实**，只改行号）／**F6**（P3）两处笔误（§8 标题 + **§3.6「重复声明会报错」不成立**，作者已独立实测复现）。⚠️ **本轮不改任何决策、预算与风险条目**（R44~R48 一字未动）；评审另**代答**了附录 C 的「fastembed 是否重导出」未核实项，作者已复核属实 |
| 上游 | `plan-v2.md` §4 Step 7 / §4.0 **H3**；issue **#23**（V2-Step7；开工前置 ① ② **已完成**，结论见 `#issuecomment-5654947790` 与 `#issuecomment-5658037231`）；`requirements-spec.md` **v1.16**（**NFR-12** 落「拟」口径 / **NFR-05** 补口径限定词）/ `architecture-design.md` **v1.15**（§5.7 / §5.8 / **§14.5 R44~R48**）/ `eval-report.md` §3.1（P5 基线）、§8.12~§8.13（并发与对照） |
| 范围 | **T7-01** Reranker 接入（fastembed `TextRerank` + `RerankerModel::BGERerankerV2M3`，D-J2）+ **精排候选窗口放开到可配 `R`** |
| 非范围 | 自适应融合 / paraphrase 桶稀释（**Step 9**，T7-14）；跨编码器以外的小模型选型（如 `bge-reranker-base` / jina 系）；**多 session 精排池化**（**明确不做**，理由见 §4.4.2）；精排器的**训练 / 微调**；`bm25` 打分改造；V2.1 的 prefilter；**评测集换代**（仍是 T2Ranking 12K / 320 query，`#23` 的 Agent query 集在 Step 9） |
| 交付物 | ① `crates/core/src/rerank/local.rs`（`LocalReranker`）② `Reranker` trait 的 `candidate_window` ③ 编排层窗口打通 + `Metrics` 采集点 + `Explain.rerank_score` ④ CLI `--rerank-window`（`search` / `bench`）⑤ `scripts/eval_rerank.sh` ⑥ `eval-report.md` **§8.14** ⑦ 四处定义面回写 |

> ## 🔧 S7-01 实现期勘误（**E1~E7**，2026-09-14；E1~E6 = **PR #52 第 1 轮评审意见 3 的落地**，E7 = **第 2 轮「新 1 / 新 2」的落地**）
>
> **本节不触碰决策 / 预算 / 风险面**（D-S7-01~10、NFR-12/05/06/07、R44~R48 **一字未动**），
> 只把「**已实装的事实**」同步回本文 —— 依据 = PR #52 评审：三处偏差**已实装**、而本文仍是旧文本，
> 且 **PR 2~5 会照着本文写**（本文才是实现依据）。
>
> | # | 位置 | 原文 → **实装** |
> | --- | --- | --- |
> | **E1** | §4.4.1 / 附录 A | `with_max_length(self, …) -> Self`（**构造后** builder）→ **`with_params(window, max_length) -> Result<Self>`**（**构造期**）：`max_length` 在 `try_new` 时就烧进 tokenizer 的 `TruncationParams`（`fastembed/src/common.rs:181-184`）⇒ 构造后改字段**不生效**（「断言仍绿但没生效」那类假象） |
> | **E2** | §4.4.2 第 1 步 | 「零/**单条**早退」→ **只有空输入**早退（**单条不早退**）：否则「`score` 是否被替换」会**依赖候选条数**，与 **D-S7-05** 的 `explain.rerank_score.is_some()` 信号自相矛盾（R46 关心的正是 score 语义的可判定性） |
> | **E3** | §7 **S7-T9** 落点 | `tests/step7_rerank_local.rs` → **`rerank/local.rs` 模块内部**：公开面只暴露 `σ(logit)`，而 σ 是**多对一**的浮点映射 ⇒ `score.to_bits()` 相等**证明不了 logit 逐位相同** |
> | **E4** | §4.4.3 | `Hit.score ∈ (0, 1)` → **`∈ [0, 1]`**：f32 下 σ **精确饱和**（实测 `σ(16.7) == 1.0`（`1 + e^(−16.7)` 被舍回 1.0）/ `σ(−89.0) == 0.0`（`e^89` 上溢到 `inf`））；饱和处不再严格单调，但**序不增/不减**仍成立 ⇒ D-S7-05 依赖的是单调、不是单射 |
> | **E5** | §4.4.2 第 5 步 | 回填的**契约与两条防御路径**写实：`scored` 应**恰好覆盖** `hits`；`index` 越界 = 结构违反 ⇒ `debug_assert!`（dev）+ `warn!` + 忽略；**覆盖不足** = 保持**输入（融合）分** + `warn!`（不加 `debug_assert`，否则该语义**不可测**）。⚠️ **第 2 轮补记**：契约里「恰好覆盖」的**判定口径**也在实现期定死了（见 **E7**） |
> | **E6** | §6 影响面表 | **补列 `Error::Rerank`**（设计期**漏列**的新增公开面）+ `Reranker::rerank` 行补**写侧契约**（PR #52 评审意见 1） |
> | **E7** | §4.4.2 第 5 步（**第 2 轮评审「新 1 / 新 2」**） | ① 「恰好覆盖」的**判定口径** = **有效且去重后的 `index` 集合**（`rerank::scoring::uncovered_count`），**不是条数比较** —— 只比 `scored.len()` 漏掉「**重复 `index` + 漏一个**」（条数相等 ⇒ 判不出）；② **两条护栏各自有用例钉住**：越界 `debug_assert!` 用 `#[cfg(debug_assertions)] + #[should_panic]`（**本仓首个 `should_panic`**，全仓此前 0 处），覆盖判定用**纯函数单测**（不依赖 tracing subscriber） |
>
> ⚠️ **版本号刻意不升（仍 v0.2）**：本节**不含**决策/预算/风险变更，而升版会让 4 处定义面的
> 「依据 `v2-step7-design.md` v0.2」引用**连带漂移** —— 那属 **S7-05** 的回写范围。
> ⇒ **正式升版 + 定义面回写随 S7-05 一并做**（本文只做「实装事实」的就地勘误）。
>
> ### 记入 **S7-05** 回写清单（本轮评审发现，**不在本 PR 修**）
>
> 1. **`requirements-spec.md` 的 NFR-06 精排注记「机制句」反向**（`requirements-spec.md:388`）：
>    现写「精排**不改 `chunk_id` 序**（同分时稳定排序保持输入序，`fastembed` … 为 stable sort）」，
>    而 **D-S7-07 与本实现刻意不沿用**「保持输入序」、改用 **`chunk_id` 升序**
>    （`crates/core/src/rerank/scoring.rs` 的 `apply_scores`）⇒ 机制描述与设计/实现**相反**。
>    **NFR-06 的结论不受影响**（重排仍是逐位确定的）。**裁定：以 D-S7-07 为准**（评审 #52 意见 5）。
> 2. **`plan-v2.md:462` 的 fastembed 锚点仍是 F5 更正前的旧值**（`impl.rs:110` / `:186-198`，
>    应为 **`:126-132`** / **`:215-224`**）—— #51 的 F5 只改了本文 ⇒ 这是「一类缺陷」的
>    **第三个载体（跨定义面）**。
> 3. **`architecture-design.md:1628`（§14.5 **R48**）的 `fastembed/src/reranking/init.rs:16-18` 同为旧值**
>    （应为 **`17-19`**）—— 该行由 **`1859dbf`（#51）** 引入（`git log -S "init.rs:16-18"` ⇒ 仅此一个提交）
>    ⇒ **同样属「Step 7 引入的锚点」**。
>    ⚠️ **本条更正了我第 1 轮的结论**：当时写「Step 7 引入的锚点里**只有这一处**（`plan-v2.md:462`）漏网」
>    ⇒ **实际是两处**（第 2 轮评审「新 3」指出）。**根因（已记入教训）**：我的全仓扫描**确实命中了**该行，
>    但工具把它截断成 `[Omitted long matching line]` 而我**没有回读** ⇒ **凡「Omitted」的命中必须逐条回读**。
>    （顺带核过：同行 `:1627` 的 R47 锚点 `common.rs:174-180` **是正确的**，不动。）
> 4. 上述**跨定义面的同类漂移一次性修完**（#51 的 F5 只落在本文）—— 第 2、3 项同因同源。
> 5. **（S7-03 新增）`architecture-design.md:1628`（§14.5 R48）里的 `crates/cli/src/bench.rs:697`
>    因本 PR 改动 `bench.rs` 而漂移**（实测 **→ `:757`**）。⚠️ 属**定义面** ⇒ 本 PR **不改**，
>    与第 2、3 项并入同一次回写。
>    **本 PR 已做的部分**：把**本文**全部 `bench.rs` 行号引用按**实测**更新（10 处：
>    `697→757`、`380-390→436-446`、`655-668→711-724`、`661-663→717-719`、`160-162→178-180`、
>    `1530→1597`），并**全仓扫过**其余载体 —— `v2-step5-design.md` / `v2-step2-design.md` /
>    `CHANGELOG.md`（历史条目）里的 `bench.rs` 行号**在本 PR 之前就已漂移**（Step 2/5 之后
>    `bench.rs` 又改过多次）⇒ **不属本 PR 造成**，按「历史记录不改写」的纪律**不动**。
>    🔑 **给 S7-05 的建议**：顺手把**定义面**里的行号引用改成**符号指代**（如「`bench.rs` 的
>    `build_analyzer`」），从根上消掉「一改 CLI 就全体漂移」这一类问题。
>
> **核过仍准确、未改的**（免得后人分不清「没核」与「核了没事」）：`plan-v2.md:380` / `:659` 的
> `src/init.rs:30-33` 属 **2026-09-07 的旧锚点**（指 `InitOptions<M>`，我们走的是
> `InitOptionsWithLength` 的 `:20`），但 `plan-v2.md:671-680` **已有「以上一律以
> `v2-step6-design.md` 附录 C 为准」的取代指针** ⇒ 属**刻意保留的历史记录**
> （同「被推翻的旧值也要留引文」的纪律），**不建议改**。

---

## ⚠️ 「需评审拍板」清单 —— ✅ **已获评审同意（2026-09-14 第 1 轮）**

> 下方四行是**原始清单**（保留当时的「建议 / 不同意会怎样」，便于追溯）。**评审结论：四处全部同意本文建议。**

| # | 需拍板的事 | 本文建议 | 不同意会怎样 |
| --- | --- | --- | --- |
| **D-S7-04** | **精排默认「关」还是「开」** | ✅ **默认关**（零配置仍是 `NoOpReranker`；精排需显式装配 / CLI 传 `--rerank-window`） | 若改「默认开」⇒ 所有既有调用方的**端到端 P99 从 ~3ms 涨到百毫秒级**，NFR-02（20ms）名义失效；且首次运行要下 **2.19GB** 模型 |
| **D-S7-05** | **`Hit.score` 在精排生效时代表什么** | ✅ **`σ(logit)`**（保持「按 score 降序」契约），融合分保留在 `explain.fused_score`，并**新增 `Explain.rerank_score`（原始 logit）** | 若维持「score 恒为融合分」⇒ `hits` **不再按 score 降序**，与 `SearchResponse.hits` 的既有文档承诺直接矛盾 |
| **D-S7-01** | **默认窗口 `R`** | ✅ 形态定、**默认值 20（拟）**，由 **S7-04** 标定、S7-05 回填 | 若不允许「拟」值 ⇒ NFR-12 与 `--rerank-window` 的默认档都悬空，本 Step 无法合并 |
| **D-S7-08** | **`max_length`（512 token 截断）** | ✅ **保持库默认 512**，标定实验加 `1024` 对照档，是否调大**由数据定** | 若现在就调大 ⇒ 每对前向成本近似平方增长，且**无数据支撑**（R48） |

其余 6 条决策（D-S7-02 / 03 / 06 / 07 / 09 / 10）为工程取舍，本文按建议执行；评审若反对，改的是**一段代码 + 一段文档**，不动需求面。

> ✅ **2026-09-14 评审结论（第 1 轮）**：四处拍板**全部同意本文建议**；D-S7-02 / 03 / 06 / 07 / 09 / 10 六条工程取舍**无异议**。

---

## 决策速览：D-S7-01 ~ D-S7-10

| # | 决策点 | 建议 | 定值来源 |
| --- | --- | --- | --- |
| **D-S7-01** | **H3：精排窗口的形态与默认值** | ✅ 建议：**可配 `R`**（`--rerank-window`），语义 = 「希望拿到前 `max(k, R)` 条候选」；**默认 `R = 20`（拟）** | §4.2 / **S7-04** 标定、S7-05 回填 |
| **D-S7-02** | 窗口的接口形态 | ✅ 建议：`Reranker` trait 新增 **provided** 方法 `candidate_window(&self, k: usize) -> usize`（默认 `k`）——**非破坏性**，与 D-S5-01「策略归后端」同构 | §4.2 / 附录 A |
| **D-S7-03** | 候选池是否联动 | ✅ 建议：**必须联动** —— `candidate_k = max(3k, window, 10)`，且必须在两路召回**之前**算 | §4.3.1 |
| **D-S7-04** | **精排默认开 / 关** | 🔴 **需拍板**：✅ 建议**默认关**（`Config.reranker` 保持 `NoOpReranker`） | §5 / PR 正文 |
| **D-S7-05** | **`Hit.score` 的语义** | 🔴 **需拍板**：✅ 建议 `score = σ(logit)`；`explain.fused_score` 不变；**新增 `Explain.rerank_score: Option<Score>` = 原始 logit**（「分数被替换」不静默，NFR-07 精神） | §4.4.3 / §6 |
| **D-S7-06** | `explain` 组装的时机 | ✅ 建议：**推迟到精排截断之后**（只对最终 ≤ `k` 条算 `matched_terms`） | §4.3.3 |
| **D-S7-07** | 精排后的 tie-break | ✅ 建议：`σ(logit)` 相同时按 **`chunk_id` 升序**（与 NFR-06 的「tie-break by chunk_id」一致），**不沿用** fastembed 的「输入顺序」 | §4.4.2 |
| **D-S7-08** | `max_length`（token 截断） | 🔴 **需拍板**：✅ 建议保持 **512**，标定加 `1024` 对照档；`max_length` 进「精排器身份」可观测面 | §4.4.4 / R48 |
| **D-S7-09** | 精排是否对 `bm25` / `vector` 单路生效 | ✅ 建议：**生效**（它在融合之后、与 `mode` 无关）；文档写明「单路模式的 `score` 语义也随之变」 | §4.3.5 |
| **D-S7-10** | 精排器是否进 `ConfigFingerprint` | ✅ 建议：**不进**（精排不改索引内容；进指纹会让「换精排器」把老快照判成 `ConfigMismatch`）；但**要**在日志/`search` 输出里可见 | §4.4.4 |

---

## 0. 开工前置三条的结账单（2026-09-14）

| # | 前置 | 状态 | 结论落点 |
| --- | --- | --- | --- |
| ① | **先复现基线** | ✅ **已完成** | 复现协议 = `./scripts/eval_quality.sh --runs 3`（与 P5 同协议）。**关键结论**：`bm25` 三项**逐位复现（Δ = 0）**，而 `hybrid MRR@10(1)` = **0.68815 vs 记录 0.69220（−0.586%）**；跨图 NDCG 极差 hybrid **0.0024**（P5 fixture 自带 0.0023）⇒ **锚点自身带 ~0.6% 图漂移** ⇒ **任何 <~1% 的「提升」无法与噪声区分**。⇒ 本文 **§3.1** 把「A/B 必须在同一张冻结图上做」写成**第一条设计约束** |
| ② | **模型下载可行性** | ✅ **已完成** | fastembed 映射的仓库是 **`rozgo/bge-reranker-v2-m3`**（`fastembed-6.0.2/src/models/reranking.rs:28-33`，⚠️ **非** BAAI 官方库——官方库没有 ONNX）；合计 **≈2,187 MB**（`model.onnx.data` **2,271,088,656 B**），已**实下载并验 sha256** `84b66c78…8945`（与仓库声明的 LFS oid 一致）；本地**未缓存**。⇒ 本文 **§3.5 / R44** 据此写「不进 CI + 独立内存预算」 |
| ③ | **H3（窗口）+ NFR-12** | ⏳ **本文回答** | **形式上的「待拍板」已收敛为 4 个具体选项**（见上方清单）；**形态**（可配 `R` + 默认小值）**D-S7-01 建议采纳**，**数值**留 **S7-04** 标定、S7-05 回填 |

> ⚠️ **前置 ① 的另一条读数**（对本文有用）：`--input` 路径的 **HNSW 图重建 12,000 条约 38.7~39.3 s**（逐点 `add()`，不吃 T7-21 的并行默认）。⇒ **标定实验若走 `--input`，每跑一档要多付 ~39s 且换一张图** ⇒ 本文 §4.6 **一律走 `--index`（冻结图）**。

---

## 1. 目标与验收

### 1.1 要解决的问题

| ID | 问题 | 证据 | 严重度 |
| --- | --- | --- | --- |
| **Q-R2** | Reranker 仍是 `NoOpReranker`（FR-18 的 V1 占位）——精排环节存在但零副作用 | `crates/core/src/rerank/noop.rs:12-16`；`crates/core/src/search/config.rs:317-320` | 高（MRR） |
| **H3** | **精排窗口未定**：编排在融合后**先 `take(k)` 再进精排** ⇒ 精排只能重排 `k` 条 | `crates/core/src/query/searcher.rs:286`（take）vs `:316`（rerank） | 高（决定本 Step 能否达成验收） |
| **① 的推论** | P5 锚点 `0.6922` 自身带 ~0.6% 图漂移 ⇒ 「提升」不可判 | `#23` 前置 ① 评论 / `eval-report.md` §3.4 | 高（**方法学**） |

⚠️ **本 Step 的成败取决于 H3**：`take(k)` 之后再精排，**只可能改善排序、不可能改善召回**；而验收要的是 `hybrid MRR@10(1)` 提升 ⇒ 窗口必须放开。等价地说：**窗口不放开 ⇒ 本 Step 存在「做了但测不出来」的风险**，与 Step 6 的 F1（delta 与 base 重叠 ⇒ 测量空转）同族。

### 1.2 对应需求

| 编号 | 口径（原文见需求文档） | 本 Step 的落点 |
| --- | --- | --- |
| **FR-18** | Reranker 接入真实模型（`bge-reranker-v2-m3`，fastembed `TextRerank`），候选窗口从 `take(k)` 放开到 `candidate_k`/可配 `R` | **本文的主线**（§4.2~§4.5） |
| **NFR-12** | 精排延迟（rerank P99，**独立口径，不并入 NFR-02**） | §4.6 标定实验；**形态由本文落、数值标「拟」**（需求 v1.16） |
| **NFR-05** | 1 万 chunk 向量约 20MB；峰值 RSS 372MB（**检索路径，batch 1**） | ⚠️ **精排器常驻 ≈2.2GB 权重**，与 372MB **差一个数量级** ⇒ 本文建议给 NFR-05 补限定词「**不含精排器**」并登记 **R44** |
| **NFR-06** | 相同 query + 相同索引 → 完全相同结果（tie-break by chunk_id） | ⚠️ 精排引入 ONNX 推理 ⇒ **须先证逐位可复现**（**R47**，探针设计见 §4.7）；tie-break 见 D-S7-07 |
| **NFR-02** | 混合 P99 < 20ms | ⚠️ 精排**不在** NFR-02 口径内（故 D-S7-04 建议默认关；D-S7-05 让 score 语义显式可见） |
| **NFR-07** | 每次检索输出 `took` / 各 lane 耗时 / 候选数；**降级不得静默** | 精排耗时进 `Metrics`（§4.3.4）；「`score` 被精排替换」**不得静默** ⇒ `Explain.rerank_score`（D-S7-05） |
| **FR-13** | 可解释性：每条 `Hit` 携带各 lane rank/score 与融合分 | `Explain.fused_score` **保留**（D-S7-05）；`matched_terms` 组装推迟但**结果不变**（D-S7-06） |

### 1.3 验收标准（Step 7 完成的定义，**可证伪**）

1. **窗口真的放开了（空转防护）**：在**同一张冻结图**上，`k = 10`、`R = 100` 时，**交给精排的候选条数 ≥ 100**（由 `Metrics.rerank_window` 与间谍精排器的入参双证）。⚠️ 这条**单列**，因为「只改 `take` 不改 `candidate_k`」会让它静默退化为 30（§4.3.1）。
2. **零回归（NoOp 路径）**：`R ≤ k`（含默认 `R = k`）时，`hits`（`chunk_id` + `score` + `explain`）与改动前**逐位一致**；`Metrics.rerank_window == k`。
3. **`hybrid MRR@10(1)` 相对「已复现的基线」可测提升**：基线 = **同一张冻结图、`R ≤ k`、精排关**；提升判据 = **差值 > 同一冻结图的重复测量抖动带**（§4.6.3）。
4. **NFR-12 有实测**：`eval-report.md` §8.14 收录 `R ∈ {k, 20, 50, 100, 200}` 的端到端 P50/P99 与**精排段** P50/P99（`Metrics.rerank_elapsed`），并给出**默认 `R` 的决策门结论**（§4.6.4）。
5. **确定性（NFR-06 不破）**：同一 query 连续 N 次精排 ⇒ (`chunk_id`, `score`) 逐位一致；**同批次组成下**跨进程一致（S7-T8）。⚠️ 若探针发现**批次组成敏感性**（R47），必须在文档中**显式声明**「不同 `R` 之间的分数不可逐位横比」，并把 NFR-06 的限定词落进需求面。
6. **可观测（NFR-07）**：`Metrics` 有 `rerank_window` / `rerank_elapsed`；精排生效时 `hit.explain.rerank_score.is_some()`（该等价关系写成测试）。
7. **守门全绿**：`make fmt && make lint && make test && make deny && make shell`；新代码无 `unsafe`；CI 全绿（`local-rerank` 走既有 `#[ignore]` 模式，**CI 不下载 2.19GB 模型**）。

---

## 2. 现状与问题定位（源码级）

### 2.1 精排今天到底能拿到几条

```text
search_parts()（crates/core/src/query/searcher.rs:86）
  ├ :118  let candidate_k = k.saturating_mul(3).max(10);        // 候选池 = 3k
  ├ :180-223  两路召回（各取 candidate_k 条）
  ├ :244-248  融合 → fused: Vec<(ChunkId, Score)>
  ├ :286  for (chunk_id, fused_score) in fused.into_iter().take(k) {   // ⚠️ 先截到 k
  │        :287-312  正排回捞 + 组装 Explain + 构造 Hit
  ├ :316  let hits = parts.reranker.rerank(query, hits, k)?;    // ⚠️ 精排只看到 k 条
  └ :318  metrics.fused = hits.len();
```

⇒ **结论**：`NoOpReranker` 之外的精排器拿到的是**已经截断到 `k` 的 `Vec<Hit>`**，`top_n` 传的也是 `k`。**精排只能重排、不能增补**。

⚠️ **锚点更正（本文核实）**：`plan-v2.md` §4.0 **H3** 行与 issue **#23** 都写 `take(k)` 在 `query/searcher.rs:196` —— **实际是 `:286`**（`:196` 是 `SearchMode::Vector` 分支里的 `(None, Some(lane))`）。**行号漂移，语义无误**（"融合后先 take(k) 再 rerank" 属实）。

### 2.2 窗口放开**不只是 `take()`** —— 候选池必须联动（设计期新发现 A）

`candidate_k = max(3k, 10)`（`:118`）既是**每路召回的目标条数**，也是融合的输出上限（`:245` 的 `fuse(&lanes, candidate_k)`）。于是：

- `k = 10` ⇒ `candidate_k = 30` ⇒ `fused.len() ≤ 30`；
- **若只把 `take(k)` 改成 `take(R)`、`R = 100`** ⇒ `fused` 只有 ≤30 条 ⇒ **实际窗口 = 30，静默封顶 70%**。

⇒ **D-S7-03 要求两处一起改**，且 `candidate_k` 的赋值必须**在 `:180` 的召回之前**完成。⚠️ 这条与 Step 6 的 **F1** 是同一类缺陷（**测量/功能空转**：改对了参数、但被上游的上限吃掉），故 §7 的 **S7-T3** 专门钉它。

**附带影响（必须一并声明）**：`candidate_k` 变大 ⇒ ① 两路召回成本上升（BM25 与向量路都取 `candidate_k`）；② `Metrics.vector_shortfall` 的归一化分母是 `candidate_k.min(allowed)`（`searcher.rs:230-232`）⇒ **窗口变化会让该指标的口径随之变**，跨窗口**不可横比**（写进 `Metrics::vector_shortfall` 的 rustdoc 与 §4.3.4）。

### 2.3 窗口需要一个「我需要多少候选」的通道

`Reranker` trait 是：

```rust
// crates/core/src/rerank/mod.rs:16-19
pub trait Reranker: Send + Sync {
    fn rerank(&self, query: &str, hits: Vec<Hit>, top_n: usize) -> Result<Vec<Hit>>;
}
```

**`rerank` 的签名不需要改**（它本来就能收任意长度的 `Vec<Hit>`）。缺的是「**我需要多少条**」这条**单向通道**：编排层在召回**之前**就得知道窗口。

三条候选路径：

| 路径 | 形态 | 破坏性 | 评价 |
| --- | --- | --- | --- |
| **A（建议）** | trait 加 **provided** 方法 `fn candidate_window(&self, k: usize) -> usize { k }` | ✅ **无**（有默认实现；`NoOpReranker` 一字不改） | 与 **D-S5-01「策略归后端、编排层只做分派」** 同构；窗口是精排器的**成本/精度画像**（cross-encoder 想要 R≈20~100，NoOp 只要 k） |
| B | `SearchParts` / `Config` 加 `rerank_window: usize` 字段 | ⚠️ **是**（两者都是字段全 `pub` 的公开结构体 ⇒ 下游字面量构造编译失败，同 R35 一族） | 多一个「装了精排器却忘了设窗口」的坑；且窗口与精排器**分裂**在两处 |
| C | 不加接口，用 `Reranker` 的**类型标识**（`std::any::type_name`）判断 | ✅ 无 | ❌ 用类型名当策略开关，脆且不可配 |

⇒ **D-S7-02 走 A**。

### 2.4 【设计期新发现 B】回捞组装成本随窗口**线性放大**，且与精排无关

`:286-313` 的循环里，每条候选都要做：

| 步骤 | 成本 |
| --- | --- |
| `parts.index.chunk(chunk_id)` + `doc(chunk.doc_id)` | 哈希查找，廉价 |
| `chunk.text.clone()` / `doc.source.clone()` / `metadata.clone()` | **整段文本拷贝**（T2Ranking 段落最长 **76,895 字符**，见 `crates/cli/src/bench.rs:757` 的注释） |
| **`matched_terms(analyzer, query, &chunk.text)`**（`:296`） | 内部 `analyzer.analyze_doc(chunk_text)` —— **对整段文本跑一遍分词**（`crates/core/src/query/explain.rs:13-17`） |

⇒ 窗口从 `k=10` 放到 `R=100`，**这一段就 ×10**。而它服务的东西（`Explain`）**只有最终 ≤k 条会被下游读到** ⇒ **`R−k` 条的 `matched_terms` 是纯浪费**。

⚠️ **成本未实测**（本文不猜数）：`analyze_doc` 对 76K 字符段落跑一次的量级、以及它在 `took` 中的占比，**S7-04 必须分别计时**（这正是要把「组装」与「精排」分开的原因）。⇒ **D-S7-06：把 `matched_terms` + lane rank 的组装推迟到精排截断之后。**

### 2.5 【设计期新发现 C】fastembed 的 `rerank` 用 `PaddingStrategy::BatchLongest`

`fastembed-6.0.2` 的 tokenizer 在 `common.rs:174-180` 配了：

```rust
.with_padding(Some(PaddingParams { strategy: PaddingStrategy::BatchLongest, pad_token, pad_id, .. }))
```

而 `reranking/impl.rs:143-212` 是**按 `documents.chunks(batch_size)` 分批**编码（`batch_size` 默认 **256**，`reranking/mod.rs:2`），批内**padding 到该批最长序列**，再 `Array::from_shape_vec((batch_size, encoding_length), …)`。

⇒ **同一个 `(query, doc)` 对，在不同「批次组成」下的 padding 长度不同** ⇒ 虽然 attention mask 会遮住 pad，但**矩阵形状变了** ⇒ 浮点结果**不保证逐位相同**。

这件事的**三个后果**（全部要写进文档与测试）：

1. **同一 `R` 下是可复现的**（批次组成由 `R` 与融合顺序唯一决定）⇒ NFR-06 在「同一 `R`、同一图」下**仍然成立**；
2. **不同 `R` 之间的 `score` 不可逐位横比** ⇒ 标定实验只能比 **MRR / NDCG 这类序数指标**，不能比分数（§4.6.3）；
3. **`R > 256` 时窗口内部被切成多批** ⇒ 批边界之外的对**不受**批内最长序列影响。⇒ 「窗口内是否同一批」本身也是一条要观测的量（**S7-T9** 探针直接测它）。

### 2.6 【设计期新发现 D】`TextRerank::rerank` 返回**全量排序**，但 tie-break 是「输入顺序」

`reranking/impl.rs:215-224`：

```rust
top_n_result.sort_by(|a, b| a.score.total_cmp(&b.score).reverse());   // 全部文档，不是 top_n
```

- **返回的是全部打分文档的排序**（函数名叫 `top_n_result` 但只排序、不截断）⇒ 截断由我们做；
- `sort_by` 是**稳定排序** ⇒ **分数并列时保持「输入顺序」**，而输入顺序 = **融合顺序**。
- ⚠️ 这与全项目的 **`tie-break by chunk_id`**（NFR-06 明文；`retriever/bm25.rs` 的 `(score 降序, chunk_id 升序)` 全序）**不一致** ⇒ **D-S7-07**：精排后自行补 `chunk_id` 升序的次级比较。

### 2.7 【设计期新发现 E】512 token 截断 + T2Ranking 的长段落

`RerankInitOptions = InitOptionsWithLength<RerankerModel>`，而 `impl HasMaxLength for RerankerModel { const MAX_LENGTH: usize = 512; }`（`reranking/init.rs:17-19`）。⇒ **`query + passage` 合计只保留前 512 token**。

而评测语料是 **T2Ranking 段落级**（最长 **76,895 字符**）。⇒ **精排只看得到长段落的前 ~512 token** ⇒ 若「答案句」落在后面，精排会**给无关分数**。后果不是「精排没用」，而是**「精排可能在评测集上被系统性低估」** ⇒ 这直接威胁验收标准 3 的**可解释性**（一个「没提升」的结论可能是截断造成的，不是方法不行）。⇒ **R48**，并在 §4.6 的标定里加 `max_length = 1024` 的对照档。

### 2.8 精排器与快照语义的关系

`Config::fingerprint()`（`crates/core/src/search/config.rs:59-70`）只有四项：`analyzer_id` / `embedder_id` / `dim` / `chunker`。

- ✅ **精排器不进指纹是对的**：它**不改索引内容**（不改倒排、不改向量、不改图），只是查询侧的排序策略 ⇒ 进指纹会让「换个精排器」把老快照判成 `ConfigMismatch`（**把可用快照锁死**）。
- ⚠️ **但 `max_length` / 模型 repo / 窗口**这些**影响数值**的配置必须有落点（D-S7-08）：它们**不进快照**，但**要进日志与 `search` 输出**，否则「同一 query 两次结果不同」会变成无法排查的谜（NFR-07）。

---

## 3. 设计约束（既有事实，本文不重新论证）

### 3.1 【第一条约束】A/B 必须在同一张冻结图上做

前置 ① 的实测：`bm25` 三项 `Δ = 0`（对照），`hybrid MRR@10(1)` 复现偏差 **−0.586%**，跨图 NDCG 极差 **0.0024** ⇒ **锚点自身带 ~0.6% 漂移**。

理由链：`hnsw_rs` 建图用 `StdRng::from_os_rng()`（**无 seed**）⇒ 两张图拓扑不同 ⇒ 图相关指标不同。

⇒ **本 Step 的所有效果结论必须满足**：

1. 走 `--index <快照>`：图从 sidecar 加载、**落盘即冻结**（NFR-06 的口径：「同一快照两次加载逐位一致」）；
2. **`--runs 1`**：⚠️ `bench` 的 `--runs > 1` **会刻意跳过 sidecar 重建图**（`crates/cli/src/bench.rs:436-446`，注释明写「本处就是要制造跨进程差异」）⇒ **`--runs > 1` 与冻结图互斥**；
3. 「冻结」本身要**自证**：同一命令跑 2~3 遍，`bm25` 三项 `Δ = 0` 且 `hybrid` 逐位一致（**S7-T11**）。⚠️ 与 Step 6 的 F1 同族——**先证明 A/B 的两臂真的在测同一个东西**，再谈结论。

### 3.2 新增公开字段/方法 = 对外破坏性变更（R35 一族，本项目已有先例与流程）

| 类型 | 字面量构造点 | 新增字段/方法的影响 |
| --- | --- | --- |
| `SearchResponse` | 库内 2 处（`searcher.rs:323` / `:507`），Step 5 已加过 `metrics` | 新增字段 ⇒ 下游字面量构造编译失败（**已接受**，见 CHANGELOG 的「⚠️ 破坏性」段） |
| `Metrics`（`#[derive(Default, Copy)]`） | 无（全 `Default`） | **纯加法**，加字段零影响（保持 `Copy`：只加 `usize` / `Duration`） |
| `Explain` | 库内 1 处（`searcher.rs:295`）+ 测试用 `..Default::default()` | 新增字段 ⇒ 该字面量要改；下游未用 `..` 的构造会断 |
| `Reranker` trait | 库内 1 实现（`NoOpReranker`） | **provided 方法** ⇒ **非破坏性** |

### 3.3 精排器必须是 `Send + Sync` 且内部可变

- `Config: Send + Sync`（`search/config.rs:28-34`）⇒ `Arc<dyn Reranker>` 要求 `Reranker: Send + Sync`（已是）。
- `Searcher` 是 `'static + Clone + Send + Sync`（`search/searcher.rs:20-30, 249-252`）且**并发读**被 NFR-10 覆盖 ⇒ 精排器**必须能在 `&self` 下推理**。
- `TextRerank::rerank` 需要 `&mut self`（`reranking/impl.rs:126-132`）⇒ 与 `LocalEmbedder` 同款：**`Mutex<TextRerank>`**（`embed/local.rs:34-37` 是可直接照抄的先例）。

⚠️ **这会立刻撞上 R43**：`Mutex` 会让「并发检索 + 精排」退化成串行编码。⇒ **§4.4.2 明确「本 Step 不做精排池化」**，并把窗口与默认关闭作为**上游的缓解**（R45）。

### 3.4 精排位于融合之后 ⇒ 与 `mode` 无关

`parts.reranker.rerank(...)`（`:316`）在 `mode` 分支之外 ⇒ 三种 mode（`Bm25` / `Vector` / `Hybrid`）都会经过精排。⇒ **D-S7-09：这是期望行为**（否则 `--modes bm25` 无法做精排 A/B），但必须**显式写明**，否则「`bm25` 模式的分数变了」会被误判成回归。

### 3.5 模型 2.19GB ⇒ 不进 CI

`bge-reranker-v2-m3` 合计 ≈2,187 MB（前置 ②）。⇒ 沿用项目既有纪律：

- 单测**不得**依赖真模型 ⇒ 需要**注入接缝**（Step 6 的 S6-T7 先例：`search/config.rs:334-345` 的 `EmbedderCtor` 函数指针 + `resolve_embedder` 拆策略与构造）；
- 端到端精排用例 `#[ignore]`；
- **性能数字一律本地 release 跑**，不进 CI（CI 共享 runner 的数字不具可引用性）。

### 3.6 `local-rerank` 用独立 feature（不塞进 `local-embed`）

`local-embed` 是**默认 feature**（`crates/core/Cargo.toml` `default = ["local-embed"]`）。精排模型 2.19GB ⇒ 不能让它跟着默认 feature 一起被拉进构建/下载路径。⇒ 新增 **`local-rerank = ["local-embed"]`**（复用同一个 `fastembed` 依赖，**不新增依赖树节点**）。⚠️ `dep:fastembed` 已被 `local-embed` 启用 ⇒ **无需**再写一遍。
⚠️ **本文初版写「显式写 `dep:fastembed` 会报重复」—— 该断言不成立（评审 F6②，作者已独立实测复现）**：`local-rerank = ["local-embed", "dep:fastembed"]` 在 `cargo metadata --features local-rerank` 下 **exit 0**，feature 被记为 `['local-embed','dep:fastembed']` —— **冗余但合法**（`fastembed` 是 **optional** 依赖 ⇒ `dep:` 语法成立）。真正会报错的是把 `dep:` 用在**非 optional** 依赖上（**对照实测**：`dep:bincode` ⇒ exit **101**，`feature \`local-rerank\` includes \`dep:bincode\`, but \`bincode\` is not an optional dependency`）。⇒ 结论：**统一只写 `["local-embed"]`**（与 §4.4.5 的 toml 块一致），并把这条实测**写进附录 C**。

---

## 4. 详细设计

### 4.1 总览：三件事各自的落点

| 事 | 落点 | 破坏性 |
| --- | --- | --- |
| **① 真实精排器** | 新增 `crates/core/src/rerank/local.rs`（`LocalReranker`，`Mutex<TextRerank>`）+ `local-rerank` feature | 否（纯加法） |
| **② 窗口放开** | `Reranker::candidate_window`（provided）+ `search_parts` 的 `candidate_k` / `take` / 截断 / `explain` 时机 | 否（provided 方法） |
| **③ 可观测** | `Metrics.rerank_window` / `rerank_elapsed`（纯加法）+ `Explain.rerank_score`（⚠️ 加字段） | `Metrics` 否；`Explain` 是 |

```text
改造后的 search_parts（关键差异标 ⬅）
  ├ :118  let window      = parts.reranker.candidate_window(k);          ⬅ 新增
  │       let candidate_k = k.saturating_mul(3).max(window).max(10);     ⬅ 联动（D-S7-03）
  ├ 两路召回（各 candidate_k）
  ├ 融合 → fused
  ├       let take_n = window.max(k).min(fused.len());                    ⬅ 原为 k
  ├       循环 fused.take(take_n)：**只填 text / source / metadata / fused_score** ⬅ explain 推迟（D-S7-06）
  ├       let t = Instant::now();
  │       let hits = parts.reranker.rerank(query, hits, k)?;              ⬅ 入参变成 take_n 条
  │       metrics.rerank_elapsed = t.elapsed();                          ⬅ 采集点（NFR-12）
  │       metrics.rerank_window  = take_n;
  ├       对返回的 ≤k 条**补齐 explain**（matched_terms + lane rank/score）⬅ D-S7-06
  └ 截断到 k（`hits.truncate(k)`）+ `metrics.fused = hits.len()`
```

### 4.2 窗口的接口形态（D-S7-01 / D-S7-02）

#### 4.2.1 trait 侧

```rust
// crates/core/src/rerank/mod.rs
pub trait Reranker: Send + Sync {
    /// 精排**希望拿到**的候选条数（供编排层在召回**之前**决定候选池）。
    ///
    /// 语义：返回值 `w` 表示「请把融合结果的前 `max(k, w)` 条交给我」。
    ///
    /// ⚠️ 默认实现返回 `k` —— 即**不放开窗口**。这保证：
    /// ① 既有实现（`NoOpReranker` 与下游自定义实现）**一行不改**；
    /// ② 未装精排器时，`candidate_k` / 截断 / `hits` **与 Step 6 逐位一致**（S7-T1 / T4）。
    ///
    /// ⚠️ 上层会把它与 `candidate_k = max(3k, w, 10)` **联动**：返回 `w > 3k` 时
    /// 候选池会随之放大（否则窗口被静默封顶，见设计 §2.2）。
    fn candidate_window(&self, k: usize) -> usize { k }

    fn rerank(&self, query: &str, hits: Vec<Hit>, top_n: usize) -> Result<Vec<Hit>>;
}
```

#### 4.2.2 默认值 `R = 20`（拟）

| `R` | 收益上界 | 成本（12K / 单对前向 ~50ms 量级的**粗估**，**未实测**） |
| --- | --- | --- |
| `k = 10` | 只重排 10 条（今天） | ~0.5s |
| **20（拟默认）** | 窗口翻倍 ⇒ 给精排 10 条「新面孔」 | ~1s |
| 50 / 100 / 200 | 业界常见档 | ~2.5s / ~5s / ~10s |

⚠️ **上表的「秒」是量级示意，不是实测**（`#23` 前置 ② 的估算是「单对 20~50ms」，但那是在 512 token 满长、CPU、未批处理的假设下）。**真实值由 S7-04 实测**，而**默认 `R` 由决策门（§4.6.4）定**。⇒ 本文的 `20` 是**「拟」值**、**可回退**（同 NFR-13 的先例）。

> 🔑 **「500ms」这个拟阈值的出处，以及它与默认 `R` 的联动（回应评审 F4）**：
> 需求 NFR-12 落的「精排段 P99 **≤ 500ms（拟）**」**对应的是上表 `k = 10` 档的量级上界（≈0.5s）**，**不是 `R = 20` 的** —— 按上表，`R = 20` 粗估 **≈1s，会超过该拟阈值**。
> ⇒ **两个「拟」必须联动**：§4.6.4 的决策门选出 `R*` 后，**要么** `R*` 使 `rerank_elapsed` P99 落回 500ms 内（拟阈值沿用），**要么** 按 `R*` 的实测**重新取阈值**（那就同时改需求 NFR-12 的数值）。
> ⚠️ **不得把「默认 `R`」与「500ms」各自独立引用** —— 那会得出「默认配置必然违约」的自相矛盾结论。

#### 4.2.3 与 `candidate_k` 的关系（写进 rustdoc 的不变式）

```text
take_n        = min(max(k, window), fused.len())
candidate_k   = max(3k, window, 10)          // 恒 ≥ take_n 的上界
metrics.rerank_window = take_n               // 实际交给精排的条数
```

**不变式**：
1. `window ≤ candidate_k`（否则窗口被静默封顶 ⇒ S7-T3 断言 `candidate_k ≥ window`）；
2. `metrics.rerank_window ≤ k` ⟺ 精排**没有**可用的额外候选（此时 `R` 放开失效）；
3. `NoOp` 下 `take_n == k`（默认 `candidate_window` 返回 `k`）⇒ 全链路零回归。

### 4.3 编排层的改造（`crates/core/src/query/searcher.rs`）

#### 4.3.1 `candidate_k` 联动（D-S7-03）

```rust
// :118 之前（必须早于 :180 的两路召回）
let window = parts.reranker.candidate_window(k);
let candidate_k = k.saturating_mul(3).max(window).max(10);
```

⚠️ **顺序是硬要求**：`:118` 的 `candidate_k` 已被 `:183` / `:194` / `:210` / `:215` 四处消费，且 `:245` 的 `fuse(&lanes, candidate_k)` 也用。

⚠️ **两处口径随之变化，必须在 rustdoc 里写明**：

1. `Metrics::vector_shortfall` 的分母是 `candidate_k.min(allowed)`（`:230-232`）⇒ 窗口变大 ⇒ 该指标**跳档**（不是回归，是口径变化）⇒ **跨窗口不可横比**；
2. 低选择度 + `--filter` 档位：`candidate_k` 变大 ⇒ **召回成本上升**（`allowed ≤ 8192` 时走精确扫描，成本 `O(N)`；`allowed > 8192` 时走 ANN，`ef` 与会话宽度不变）⇒ **精排与过滤叠加的延迟不做承诺**（非范围，如实登记）。

#### 4.3.2 截断与回捞

把 `:286` 的 `.take(k)` 改成 `.take(take_n)`。**其余不变**（快照回捞、`doc()` 查找、`doc_id` 断言）。

#### 4.3.3 `explain` 组装推迟（D-S7-06）

拆成两步（**结果不变**，只改时机）：

```rust
// 第一步（窗口内每条都做，但很便宜）：
let mut proto: Vec<Hit> = fused.into_iter().take(take_n).map(|(chunk_id, fused_score)| { … 
    Hit { chunk_id, doc_id, score: fused_score, text, source, metadata,
          explain: Explain { fused_score, ..Default::default() } }   // matched_terms / rank 留空
}).collect();

// 第二步：精排 → 截断
let mut hits = parts.reranker.rerank(query, proto, k)?;
hits.truncate(k);

// 第三步（只对最终 ≤k 条）：补齐 explain —— matched_terms + lane rank/score
for h in &mut hits {
    h.explain.matched_terms = matched_terms(parts.analyzer, query, &h.text);
    h.explain.bm25_score   = bm25_rank.get(&h.chunk_id).map(|(_, s)| *s);
    h.explain.bm25_rank    = bm25_rank.get(&h.chunk_id).map(|(r, _)| *r);
    h.explain.vector_score = vector_rank.get(&h.chunk_id).map(|(_, s)| *s);
    h.explain.vector_rank  = vector_rank.get(&h.chunk_id).map(|(r, _)| *r);
}
```

**⚠️ 已知代价**：精排器**看不到** `explain`（入参里是空的）。⇒ **不变式写进 trait rustdoc**：「`rerank` **不得**依赖 `hits[i].explain`（它可能只有 `fused_score`）；需要正文用 `text`」。这是**有意的接口收缩**，换掉 `(R−k) × analyze_doc(整段)`。

**回归护栏**：`R ≤ k`（默认）时，两次组装合起来与今天的单次组装**逐位一致**（S7-T6 逐字段断言）。

#### 4.3.4 `Metrics` 采集点（NFR-12 的前提）

```rust
// crates/core/src/query/metrics.rs（纯加法，保持 Copy）
/// 精排段耗时（`rerank()` 调用本身；不含回捞组装）。
pub rerank_elapsed: Duration,
/// 实际交给精排的候选条数（= `min(max(k, window), fused.len())`）。
///
/// ⚠️ 与 `vector_shortfall` 同理：窗口变化会让**上游**口径变，故本字段是
/// 「本次是否真的放开了窗口」的**直接证据**（NFR-07）。
pub rerank_window: usize,
```

**为什么必须进 `Metrics`**：这正是 **T7-23 / D-S5-05** 的先例——Step 5 之前 `query::Metrics` 只经 `tracing` 输出，**外部拿不到实例 ⇒ 无法单测、无法被 bench 采集**。NFR-12 要的是「精排段 P99」，没有这个字段就只能拿端到端 `took` 反推，而 `took` 里混着回捞与两路召回。

#### 4.3.5 与 `mode` 的关系（D-S7-09）

精排在融合之后（§3.4）⇒ **三种 mode 都经过它**。文档与被影响的行为必须在三个地方写明：

1. `Reranker` trait rustdoc；
2. `SearchResponse.hits` / `Hit.score` 的 rustdoc（「精排生效时 `score` 的语义」）；
3. `CHANGELOG` 的 **Changed / ⚠️ 破坏性** 段。

### 4.4 `LocalReranker`（`crates/core/src/rerank/local.rs`）

#### 4.4.1 构造与缓存

```rust
pub struct LocalReranker {
    inner: Mutex<TextRerank>,     // TextRerank::rerank 需 &mut self（同 LocalEmbedder）
    window: usize,                // D-S7-01 的 R
    max_length: usize,            // D-S7-08（默认 512）
}

impl LocalReranker {
    /// 构造并触发模型下载（首次 ≈2.19GB，见 #23 前置 ②）。
    pub fn new() -> Result<Self>;                 // window = DEFAULT_RERANK_WINDOW(20)
    pub fn with_window(self, window: usize) -> Self;   // 可配 R（CLI 流入口）
    pub fn with_params(window: usize, max_length: usize) -> Result<Self>;  // 标定用
    // ⚠️ **E1（S7-01 实现期改口径）**：此处原写 `pub fn with_max_length(self, max_length: usize) -> Self`
    //    （构造后 builder）—— **该形态必然撒谎**：`max_length` 在 `TextRerank::try_new` 时就烧进
    //    tokenizer 的 `TruncationParams`（`fastembed/src/common.rs:181-184`）⇒ 构造后改字段**不生效**。
    //    ⇒ 改为**构造期**入口 `with_params`（已实装）。
    //    ⚠️ 而 `with_window` **保留**构造后 builder：`window` 不触碰模型（只被 `candidate_window` /
    //    `id()` 读）⇒ 构造后改**是**生效的。两者性质不同，不是遗漏。
}

pub const DEFAULT_RERANK_WINDOW: usize = 20;       // 「拟」值，见 §4.2.2
pub const DEFAULT_RERANK_MAX_LENGTH: usize = 512;  // = 库默认（HasMaxLength）
```

**缓存目录**：与 `LocalEmbedder` 同源（`embed/local.rs:19-24` 的 `~/.cache/helix-index/models`），**复用同一函数**（不复制一份——否则两处路径漂移会让「模型下到哪去了」变成谜）。`with_cache_dir(…)` + `with_show_download_progress(false)` 同 embedder。

#### 4.4.2 批次、保序与 tie-break

```rust
fn rerank(&self, query: &str, hits: Vec<Hit>, top_n: usize) -> Result<Vec<Hit>> {
    // 1. **空输入**早退（避免 fastembed 对空 documents 报 EmptyTokenizations）
    //    ⚠️ **E2**：原写「零/单条早退」，实装改为**只有空输入**早退（**单条不早退**）。
    //       理由：否则「`score` 是否被精排替换」会**依赖候选条数**，与 D-S7-05 的
    //       `explain.rerank_score.is_some()` 信号自相矛盾（R46）。
    // 2. documents = hits.iter().map(|h| h.text.as_str()).collect()
    // 3. let mut model = self.inner.lock()?;                       // Mutex（R43 同源）
    // 4. let scored = model.rerank(query, &documents, false, None)?;  // batch = None ⇒ 库默认 256
    // 5. 按 `scored[i].index` **回填**到原 hits（⚠️ 不是按顺序 zip —— 见下）
    //    ⚠️ **E5**：契约 = `scored` **恰好覆盖** `hits`（每项一个、`index` 均在范围内）。
    //       违反时两条防御路径**刻意不同**：`index` **越界** = 结构违反（分数会写到错的 hit 上）
    //       ⇒ `debug_assert!`（dev 快速失败）+ `warn!` + 忽略；**覆盖不足** = 保持**输入（融合）分**
    //       + `warn!` —— **不加** `debug_assert`（加了该语义就**不可测**）。
    //       为什么「保持」而非「丢弃」或「取最低分」：丢弃会**静默改条数契约**（本函数只做排序+截断）；
    //       取 `0.0` 会**伪造**一个与 σ 饱和真值（`σ(−89) == 0.0`，E4）**不可区分**的分数；
    //       保持则可由 `explain.rerank_score == None` 识别（正是 D-S7-05 的信号定义）。
    // 6. 排序：σ(score) 降序，**并列时 chunk_id 升序**（D-S7-07）
    // 7. 截断到 top_n
}
```

⚠️ **必须按 `RerankResult::index` 回填**：`RerankResult { document: Option<String>, score: f32, index: usize }`（`reranking/init.rs:152-156`）的 `index` 是**输入切片中的下标**。**不能**假设「返回顺序 = 输入顺序」（它按分数降序）⇒ 若按位置 zip，会把分数配到错误的 chunk 上，**静默错排**。⇒ **S7-T5 用「分数量级与 id 反序」的假精排器专钉这一点**。

**`batch = None` ⇒ 库默认 `256`**（`reranking/mod.rs:2`）。⚠️ **不做多 session 池化**：

| 理由 | 说明 |
| --- | --- |
| **与 R43 同源** | `Mutex<TextRerank>` 与 `Mutex<TextEmbedding>` 是同一个结构性问题：并发读被串行化。做池化 = 把 R43 的债务再复制一份，**且没有 Q3 的答案**（不同 session 是否改变数值，仍挂起） |
| **成本已足够高** | 精排段本身就是秒级；再叠一层池化抢核，收益不确定而内存翻倍（R36 的教训：E2 峰值 RSS **+96.4%**） |
| **可后置** | 精排是**默认关**（D-S7-04）⇒ 它不是热路径；等 Step 8 / R43 的复审一并处理 |

⇒ **明确列入非范围**，并登记为 **R45 的残余**（「精排器与查询侧编码共享同一个串行瓶颈，且精排更贵」）。

#### 4.4.3 分数变换（D-S7-05）

```text
Hit.score        = σ(logit) = 1 / (1 + e^(−logit))       // ∈ [0, 1]（⚠️ E4：闭区间——f32 下精确饱和；对外排序契约）
Explain.fused_score = 融合分（**不变**）                    // 保留「召回阶段怎么看」
Explain.rerank_score = Some(logit)                        // 模型原始输出（诊断用）
```

**为什么 `σ` 而不是原样 logit**：`Hit.score` 在三种 mode 下已经是**模式相关**的量（`bm25` 分 / 余弦 ∈ [−1,1] / RRF ∈ (0, 0.04]）。把**无界、可负、量级 ±10** 的 logit 直接放进去，会让下游的「score 阈值」类代码以完全不同的量纲工作。`σ` 是**单调变换 ⇒ 排序信息零损失**，而**原始 logit 不丢**（进 `rerank_score`）。

**为什么必须留 `rerank_score`**：换 `score` 的语义是**静默行为变更**，而 NFR-07 的纪律是「**降级/变换不得静默**」（Step 2 的 `GraphStatus`、Step 6 的 `default_embedder` 硬失败都是同一条）。`is_some()` 就是「本次 `score` 是精排分」的信号。

⚠️ **`Explain` 加字段是对外破坏性变更**（§3.2）⇒ 影响面表登记 + CHANGELOG 显式标注。

#### 4.4.4 身份与可观测（D-S7-08 / D-S7-10）

| 项 | 做法 |
| --- | --- |
| **不进 `ConfigFingerprint`** | 精排不改索引内容（§2.8）。D-S7-10 |
| **身份字符串** | `LocalReranker::id() -> String`（⚠️ **不是** `&'static str`）：`"bge-reranker-v2-m3@rozgo;max_len=512;window=20"`。理由：`max_length` / `window` 是**运行期**参数，`&'static str` 表达不了（这正是 Step 6 **F3** 指出过的同类问题的另一种形态） |
| **落点** | ① `tracing::info!` 一行（`rerank_window` / `rerank_elapsed` 一并）；② CLI `search --rerank-window` 时打印一行「精排器: …」；③ `Metrics.rerank_window` |
| **模型出处** | rustdoc 记 **repo + sha256 + 取用日期**（前置 ② 的结论）：`rozgo/bge-reranker-v2-m3` / `sha256:84b66c78…8945` / 2026-09-14 |

#### 4.4.5 feature 与依赖

```toml
# crates/core/Cargo.toml
[features]
# ⚠️ 独立于 local-embed：模型 2.19GB 不能被默认 feature 拉进构建/下载路径。
#    `dep:fastembed` 已由 local-embed 启用 ⇒ 这里**只写 local-embed**；
#    再写一遍是「**冗余但合法**」（非报错）——实测见 §3.6 与附录 C。
local-rerank = ["local-embed"]
```

- **不新增依赖树节点**（`fastembed` 6.0.2 已同时提供 `TextEmbedding` 与 `TextRerank`）；
- `LocalReranker` 全部代码 `#[cfg(feature = "local-rerank")]`；`rerank/mod.rs` 的 `pub use local::LocalReranker;` 同 gate；
- ⚠️ **未启用 feature 时的行为**：`LocalReranker` **不存在**（编译期），而不是「运行时静默退化」——与 Step 6 的 D-S6-05（`--vectors` 硬失败）同一条纪律。

### 4.5 CLI 接线

```rust
// crates/cli/src/bench.rs（新增）
/// 精排窗口 R（V2 Step 7 / T7-01）。**不传 = 关闭精排**（= NoOp，与现状逐字一致）。
///
/// - `--rerank-window N` ⇒ 装载 LocalReranker（window = N），并放开候选池
/// - 与 `--runs > 1` **互斥**：`--runs` 会刻意重建图，而精排 A/B 要求冻结图（§3.1）
///   ⇒ 两者同给时**报错**，不静默
#[arg(long, value_name = "R")]
pub rerank_window: Option<usize>,
```

```rust
// 同处（**S7-03 实现期补入 §4.5**：本文原先只在附录 B 第 5 步的命令里用它、没有定义）
/// 精排 tokenizer 的截断长度（默认 512）。⚠️ 它**烧进** tokenizer 的
/// `TruncationParams`（`TextRerank::try_new` 时生效，`fastembed/src/common.rs:181-184`）
/// ⇒ 是**构造期**参数，做不成构造后 builder（同勘误 E1）⇒ 必须与 `--rerank-window`
/// **同时给**（单独给 → 守卫 1）。服务 **R48** 的 512 vs 1024 对照档。
#[arg(long, value_name = "N")]
pub rerank_max_length: Option<usize>,
```

```rust
// crates/cli/src/main.rs 的 SearchArgs（新增，与 bench **参数面完全对称**：
// 同一套 `resolve_rerank` / 守卫 / `build_reranker`，不复制语义）
#[arg(long, value_name = "R")]
rerank_window: Option<usize>,
#[arg(long, value_name = "N")]
rerank_max_length: Option<usize>,
```

⚠️ **四条守卫必须显式挡掉**（**S7-03 实装后回填**：第 1、2 条是实现期补的，第 3、4 条是本文原有的「两条脚枪」）：

| # | 情形 | 处置 | 为什么 |
| --- | --- | --- | --- |
| 1 | `--rerank-max-length` **单独给**（没给 `--rerank-window`） | `bail!` | 它只覆盖**已开启**的精排 ⇒ 静默忽略会让用户以为「改了截断长度」而实际什么都没发生（NFR-07） |
| 2 | `--rerank-window 0` 或 `--rerank-max-length 0` | `bail!` | 「看起来像关掉、实际是开了但空转」：窗口 0 ⇒ `take_n = max(k, 0) = k`（白加载 2.19GB 模型）；长度 0 ⇒ 输入被整段截空。要关精排的正确方式是**不传** `--rerank-window` |
| 3 | **`--runs > 1` + `--rerank-window`** | `bail!` | ⚠️ **脚枪之一**：`--runs` 的重建逻辑在 `bench.rs:436-446` 刻意传 `None` 跳过 sidecar ⇒ 每轮一张新图 ⇒ **精排 A/B 的差值被图漂移污染 0.6%**（§3.1）⇒ 不静默、不取其一 |
| 4 | **`--rerank-window` 在未编译 `local-rerank` 时** | `bail!` + 重编提示 | ⚠️ **脚枪之二**：照抄 `--analyzer charabia` 的既有做法（`bench.rs:717-719`）——**不得**静默退回 NoOp（那会让「精排档位跑通了」的结论建立在空转上） |

🔴 **顺序是硬要求**：**参数面守卫（1~3）必须排在 feature 守卫（4）之前**，且四者都排在加载任何资源（快照 / 模型）之前。两条理由：

① 同一份错配置在「默认构建」与「`--features local-rerank` 构建」下报**不同的错**是不可接受的；
② CI 的 smoke 走的是**默认构建**（未编译 feature）⇒ 只有把参数面守卫放前面，「互斥」这类断言**才可能被 CI 覆盖** —— 这正是「**守卫前移到模型之前 ⇒ 立刻 CI 可覆盖**」的收益（同「半向量守卫必须拒绝」的先例）。

#### 实现期口径（S7-03 实装同步，2026-09-15）

- **`--rerank-max-length` 的归属**：本文原先只在附录 B 第 5 步的命令里用它、§4.5 未定义 ⇒ 本次补入（评审拍板）。它**从属**于 `--rerank-window`（守卫 1）。
- **`RerankSpec` 三态**：`Off` / `On { window, max_length: Option<usize> }`。`max_length` 的默认值**不在解析期填** —— 真源 `DEFAULT_RERANK_MAX_LENGTH` 只在 `local-rerank` 下导得出来 ⇒ 填充推迟到 `build_reranker` 那一层。
- **共享接缝（实现期发现）**：`QueryExecutor::with_reranker` 只收 `Box`，而 bench 要**每 mode × 每 run** 装配一次 searcher（4 个入口）⇒ 想要复用同一个 2.19GB 实例，只能自己写转发 trait 的包装类型（`Reranker` 将来加方法会**静默漏转发**，`candidate_window` 漏了就是窗口静默失效）。⇒ 新增 `QueryExecutor::with_reranker_arc(Arc<dyn Reranker>)`（**新增公开方法**，纯加法）；字段 `reranker: Box<dyn Reranker>` → `Arc<dyn Reranker>`（私有字段，不动公开面），`with_reranker(Box)` 的**签名与语义一字未改**。
- **身份串的可见性（实现期发现）**：`Reranker` trait **没有** `id()`（它只在 `LocalReranker` 上）⇒ 装箱成 `dyn Reranker` 后就取不到。CLI 侧改用 `RerankerHandle` 在**构造时**取出身份串，用于 stderr 一行与 `bench --json` 的 `rerank` 块（A/B 必须自证跑的是哪一档，同 `brute_fallback` 的既有要求）。
- **⚠️ 用户可见面变化（非本文原设计）**：`bench` 输出新增一行 `精排: …`；`search --metrics` 追加 `| 精排=N条/X.XXXms`（出口 S7-02 已加的 `Metrics.rerank_window` / `rerank_elapsed`，此前**没有任何 CLI 出口**）；`search --explain` 追加 `| rerank=<原始 logit>`（D-S7-05 的对外落点）。
- **⚠️ 行号引用已同步**：本 PR 改了 `bench.rs`（新增两个参数 + 守卫 + 装配注入）⇒ 全文按**实测**
  更新了 10 处 `bench.rs` 行号引用（见头部勘误块的 **S7-05 清单第 5 项**，那里同时登记了
  **定义面**上那处（`architecture-design.md:1628`）的同类漂移与处置）。
- ⚠️ **版本号仍不升（v0.2）**：本节只同步「**已实装的事实**」，不触碰决策 / 预算 / 风险面 ⇒ 正式升版随 **S7-05**（同 E1~E7 的处置）。

### 4.6 H3 + NFR-12 的标定实验设计（**S7-04** 的输入；结论由 **S7-05** 回填）

#### 4.6.1 协议（对齐 `perf-ab-calibration` 的四条抗噪声规则）

| 项 | 取值 | 理由 |
| --- | --- | --- |
| **图** | **同一张冻结图**（`--index`，`--runs 1`） | §3.1（第一条约束） |
| **质量轴** | 全量 **320 query × 1 次**（`hybrid` / `vector` / `bm25` 三路） | 质量指标不需要重复（确定性）；全量保证与 P5 可比 |
| **延迟轴** | 固定子集（如 40 query）× **`--reps 20 --warmup 3`** | 精排是秒级 ⇒ 全量 × 20 reps 不可行（320 × 20 × 1s ≈ 1.8h） |
| **档位** | `R ∈ {k(10), 20, 50, 100, 200}` + **`max_length ∈ {512, 1024}`** 的对照组 | H3 的拐点曲线 + R48 的截断影响 |
| **交错** | 档位间**顺序交错**（10/20/50/100/200/10/20/…），按**控制组归一** | 跨时段不可相减（既有教训） |
| **控制组** | `R = k = 10`（等价 NoOp 的行为上界？⚠️ **不等价**，见下） | 见 §4.6.2 |
| **运行范围** | 记录核数 / 是否插电 / 是否在 CI（**一律不在 CI**） | 既有纪律 |

#### 4.6.2 ⚠️ 控制组不能用 `R = k`（一处容易搞错的地方）

`R = k = 10` 与 `NoOp` **不等价**：

- `NoOp` 走 `candidate_window` 默认 ⇒ `win = k`，但**不调用 ONNX**；
- `R = 10` ⇒ `win = max(k, 10) = 10`（**同样的候选**），但**会真的跑一遍 cross-encoder** ⇒ 排序可能变、延迟必然变。

⇒ **控制组有两层**：

| 控制组 | 用途 |
| --- | --- |
| **A：精排关（NoOp）** | 效果基线的**唯一**参照（验收标准 3 的「已复现的基线」） |
| **B：`R = k = 10`（精排开）** | 隔离「**只看 10 条**」与「**看更多**」的差异 ⇒ 回答「**提升来自窗口、还是来自模型**」 |

⚠️ 若只有 A 与 `R=50`，则「提升」既可能来自**模型**、也可能来自**窗口** ⇒ **归因不了**（与 Step 6 的 E1/E2/E3 设计动机相同）。

#### 4.6.3 判据

| 轴 | 判据 | 说明 |
| --- | --- | --- |
| **效果** | `hybrid MRR@10(1)` 的**提升 > 抖动带** | 抖动带 = **同一冻结图**上把同一档位跑 2~3 遍的极差（**先测出来再比**，不预设） |
| **归因** | 分别报告 `Δ(MR, 精排 vs NoOp)` 与 `Δ(MR, R=20 vs R=10)` | 前者含模型+窗口，后者**纯窗口** |
| **窗口拐点** | 取「MRR 相对 `R=10` 的增量 < 抖动带」的**最小** `R` | 即「再加窗口没意义」的点 |
| **延迟** | 端到端 `took` P50/P99 **与** `rerank_elapsed` P50/P99 分列 | ⚠️ 只报 `took` 无法归因；只有 `rerank_elapsed` 才是 NFR-12。**NFR-12 拟阈值（≤500ms）的出处与「与默认 `R` 联动」见 §4.2.2 补记** |
| **内存** | 峰值 RSS（`/usr/bin/time -l` **直接包二进制**） | R44 的读数；⚠️ 不可包 `cargo run`（会量到 cargo 的 RSS，既有教训） |
| **不可横比声明** | **不同 `R` 的 `score` 不可逐位横比**（§2.5 的 `BatchLongest`） | 只比**序数指标**（MRR / NDCG / Recall）与**条数** |

#### 4.6.4 决策门（可证伪，产出默认 `R` 与 NFR-12 数值）

```text
若  ① Δ(MRR, 精排 vs NoOp) ≤ 抖动带              ⇒ 结论：「精排在本评测集上无可测提升」
                                                    （如实记录 + 分析 R48 的截断影响，**不得粉饰**）
    ② Δ(MRR) > 抖动带 且 拐点 R* 可识别           ⇒ 默认 R = R*，NFR-12 按 R* 的 rerank P99 定值
    ③ Δ(MRR) > 抖动带 但拐点不可识别（单调到 200） ⇒ 默认 R = 50（取「收益/成本」的折中档），
                                                    并把「拐点未现」如实记入 §8.14 局限
```

⚠️ **本 Step 不承诺「一定提升」**：`#23` 前置 ① 已证明 <1% 的差异不可判定，而精排的收益方向虽然先验为正，但**评测集**（人类搜索 query、长段落、512 截断）与**目标场景**（Agent query、短 chunk）**不同构** ⇒ **如实记录 + 分析原因**是允许的结论，**粉饰不是**。

### 4.7 确定性探针（R47 / NFR-06）

三个层次，**必须分开测**（混在一起就归因不了）：

| 探针 | 内容 | 期望 | 若反例 |
| --- | --- | --- | --- |
| **P1（同进程重复）** | 同一 query、同一 `R`，连续 N 次 ⇒ (`chunk_id`, `score`) 逐位一致 | ✅ 期望成立（同批组成、同会话、同线程数） | NFR-06 必须加限定词「不含精排」，并登记风险 |
| **P2（跨进程，同 `R`）** | 两次独立进程、同一 `R`、同一图 ⇒ 逐位一致 | ✅ 期望成立 | 同上；且**标定实验的分数轴完全不可用**（只能比序数指标） |
| **P3（批组成敏感性）** | 同一 `(query, doc)` **单条**打分 vs 混在 `R` 条里打分 ⇒ logit 是否逐位相同 | ⚠️ **可能不同**（§2.5 的 `BatchLongest`） | **不必修**（数学上等价），但「**不同 `R` 的分数不可横比**」必须写进文档 |

⚠️ **P3 是本文最容易被忽略的一条**：它不破坏任何验收项，却决定了**报告怎么读**。⇒ 单独成节、单独成测、单独写进 §4.6.3 的「不可横比声明」。

---

## 5. 决策记录（D-S7-01 ~ D-S7-10）

### D-S7-01 H3：精排窗口的形态与默认值 ✅ 建议：可配 `R`，默认 **20（拟）**

- **形态**（可配 `R`，不写死 `candidate_k`）**定案**：`candidate_k`(3k) 会让 rerank 成本 ≈ `O(3k)` 次 568M 参数前向，交互场景不可行（`#23` 前置 ③ 的算术）。
- **数值**：`R = 20` 是**「拟」值**，**由 S7-04 的决策门（§4.6.4）标定、S7-05 回填**。先例：NFR-13（v1.10 落「拟 ≤20ms」→ v1.12 定稿）。
- **可回退**：若评审认为不该先落数值，改为「默认 = `k`（等于不放开）+ 要求显式给 `R`」——代价是所有效果结论都要求用户必须传参，且 NFR-12 的默认档悬空。

### D-S7-02 窗口的接口形态 ✅ 建议：trait provided 方法

见 §2.3 的三路径对比。**选 A（`candidate_window`）**：非破坏性 + 与 D-S5-01「策略归后端」同构 + NoOp 零回归。

### D-S7-03 候选池联动 ✅ 建议：必须联动

见 §2.2 / §4.3.1。**这是本设计的「空转防护」核心**：只改 `take` 会让 `R=100` 静默退化为 30。

### D-S7-04 精排默认开 / 关 🔴 需拍板 ✅ 建议：默认关

| 维度 | 默认关（建议） | 默认开 |
| --- | --- | --- |
| 「零配置可用」体验（G4） | 不变 | **首次检索要下 2.19GB**（embedder 才 96MB） |
| NFR-02（P99 < 20ms） | 不受影响 | **名义失效**（端到端涨到百毫秒级） |
| FR-18 的验收 | 仍可达成（`--rerank-window` 显式开启） | 同上 |
| 与 `parallel_build` 翻转（T7-21）的先例可比性 | ⚠️ **不可比**：那次是「**大收益 + 零成本**」；这次是「**收益未证 + 高成本**」 | — |

⇒ **默认关**。⚠️ 但**必须由评审拍板**：若评审要「默认开 + 小 `R`」，改动量很小（`build_config` 多一行），但**验收标准 1~4 全部要在开着的状态下重测**。

### D-S7-05 `Hit.score` 的语义 🔴 需拍板 ✅ 建议：`σ(logit)` + `Explain.rerank_score`

见 §4.4.3。**替代方案**（评审可选）：

- **B1**：`score` 保持融合分，另加 `explain.rerank_score` ⇒ ⚠️ **`hits` 不再按 `score` 降序**，与 `SearchResponse.hits` 的既有承诺矛盾 ⇒ **不建议**；
- **B2**：只看 `σ(logit)`，不加 `rerank_score` ⇒ 少一处破坏性变更，但**「分数被替换」变成静默** ⇒ 与 NFR-07 纪律冲突。

### D-S7-06 `explain` 组装推迟 ✅ 建议：推迟

见 §2.4 / §4.3.3。收益：去掉 `(R−k) × analyze_doc(整段)`；代价：精排器看不到 `explain`（写进 trait rustdoc 的约束）。

### D-S7-07 tie-break ✅ 建议：并列按 `chunk_id` 升序

见 §2.6。理由：NFR-06 明文「tie-break by chunk_id」，且全项目一致（`retriever/bm25.rs:129-134` 是全序）。

### D-S7-08 `max_length` 🔴 需拍板 ✅ 建议：保持 512，标定加 1024 对照

见 §2.7 / R48。**不预先调大**的理由：① `RerankInitOptions` 的 512 是**库默认**，改它=引入一处「与上游默认不同」的漂移；② 每对前向成本随长度**近似平方**增长；③ **无数据**。⇒ 先测，再决定。

### D-S7-09 精排对单路 mode 是否生效 ✅ 建议：生效

见 §3.4 / §4.3.5。理由：否则 `--modes bm25` / `--modes vector` 无法与 hybrid 做**同条件对照**（而「精排对哪一路更有效」正是 Step 9 自适应融合的输入）。

### D-S7-10 精排器是否进 `ConfigFingerprint` ✅ 建议：不进

见 §2.8。⚠️ **反面记录**：若将来精排器开始**影响索引内容**（例如「精排后改写 chunk」），本条必须重审。

---

## 6. 影响面与兼容性

| 面 | 变更 | 破坏性？ | 迁移 |
| --- | --- | --- | --- |
| `Reranker` trait | **新增 provided 方法** `candidate_window`（默认 `k`） | **否** | 自定义实现**一行不改**，行为不变 |
| `Reranker::rerank` 的语义约束 | rustdoc 补「**不得依赖 `hits[i].explain`**」（D-S7-06，**读侧**）+ **写侧契约**（⚠️ **E6**，PR #52 评审意见 1）：实现**可以且应当**写 `hits[i].explain` 归还原始分（`σ` 不可逆 ⇒ 编排层自己算不出），且编排层按 D-S7-06 补齐 `explain` 时**必须保留**精排器写入的字段（否则「谁后写谁生效」会把该信号**冲掉**） | ⚠️ **契约收紧**（编译期无感） | CHANGELOG 说明 + 库内唯一实现自查 |
| **`Error`**（⚠️ **E6**：**设计期漏列**，实现期新增的**公开面**） | 新增变体 **`Rerank(String)`**（精排模型初始化 / 推理失败）；**不复用** `Embedding` —— 两者落点与代价差一个数量级（96MB vs 2.19GB），混在一起会让「哪一步炸了」只能靠读字符串猜 | ⚠️ **是**（`Error` **非** `#[non_exhaustive]` ⇒ 下游若对它**穷尽 `match`** 会编译失败；与 `Explain` 加字段同族 / R35） | CHANGELOG `⚠️ 破坏性` 段。**评审 #52 已同意不补 `#[non_exhaustive]`**（那会让**所有**下游 `match` 都要改，破坏面比新增一个变体更大） |
| `NoOpReranker` | **无改动** | 否 | — |
| `LocalReranker` | 新增（gate `local-rerank`） | 否（纯加法） | — |
| `Metrics` | 新增 `rerank_window` / `rerank_elapsed` | **否**（有 `Default`，保持 `Copy`） | — |
| `Explain` | 新增 `rerank_score: Option<Score>` | ⚠️ **是**（字段全 `pub`，字面量构造会断） | CHANGELOG `⚠️ 破坏性` 段；库内 1 处构造点已同步 |
| `Hit.score` 的语义 | 精排生效时 = `σ(logit)`（**排序契约不变**） | ⚠️ **是（语义）** | 文档 + `explain.rerank_score.is_some()` 作为信号 |
| `query::SearchParts` / `Config` | **不改**（D-S7-02 选了 trait 方法） | 否 | — |
| 批量构建 / 快照格式 / `FORMAT_VERSION` | **不改**（精排不改索引内容） | 否 | 老快照照常加载 |
| `crates/core/Cargo.toml` | 新增 feature `local-rerank`（**无新依赖节点**） | 否（默认关） | — |
| `helix search` / `helix bench` | 新增 `--rerank-window`（默认不传 = 关闭） | 否（默认行为不变） | — |
| 效果指标 | ⚠️ 精排开启后与 P5 基线**不可横比**（§3.1 的图漂移 + D-S7-05 的分数语义） | — | 文档显式声明（§4.6.3） |

---

## 7. 测试计划（S7-T1 ~ S7-T12）

| # | 测试 | 覆盖 | 进 CI？ | 落点 |
| --- | --- | --- | --- | --- |
| **S7-T1** | `candidate_window` 默认 = `k` ⇒ `candidate_k` / 交给精排的条数 / `hits` **与改动前逐位一致** | D-S7-02 的零回归 | ✅ | `query/searcher.rs` 单测（间谍精排器记录入参） |
| **S7-T2** | `window > k` ⇒ 交给精排的条数 == `min(window, fused.len())`，返回条数 == `min(k, …)` | H3 的核心 | ✅ | 同上 |
| **S7-T3** | **空转防护**：`window = 100, k = 10` ⇒ 间谍向量后端收到的 `k` 参数 **≥ 100**（即候选池真的放大了） | D-S7-03 | ✅ | 同上（复用 `SpyVectorIndex` 的既有形态） |
| **S7-T4** | **NoOp + `window = R`** 的结果与 NoOp + `window = k` 逐位一致 | 窗口对 NoOp 路径零影响 | ✅ | 同上 |
| **S7-T5** | 假精排器（分数与输入顺序**反序**）⇒ 结果按「分数降序、并列 `chunk_id` 升序」；且分数**按 `index` 正确回填**（不是按位置 zip） | §4.4.2 的错排防护 + D-S7-07 | ✅ | `rerank/local.rs`（注入接缝）+ `query/searcher.rs` |
| **S7-T6** | NoOp 下 `explain` 的五个字段与改动前**逐字段一致** | D-S7-06 的回归护栏 | ✅ | `query/searcher.rs` |
| **S7-T7** | `Metrics.rerank_window` / `rerank_elapsed` 被填；NoOp 时 `rerank_window == k`；`metrics.took == took` 仍自洽（I7） | NFR-07 / 口径自洽 | ✅ | `tests/step7_rerank_observability.rs`（新）+ 既有 `step5_query_observability.rs` 口径不破 |
| **S7-T8** | **P1/P2（§4.7）**：同进程重复 N 次 + 跨进程两次 ⇒ (`chunk_id`, `score`) 逐位一致 | R47 / NFR-06 | ⛔ `#[ignore]`（需 2.19GB 模型） | `tests/step7_rerank_local.rs`（新） |
| **S7-T9** | **P3（§4.7）**：同一 `(query, doc)` 单条 vs 批内 ⇒ logit 是否逐位相同（**记录结论，不预设**） | §2.5 的 `BatchLongest` | ⛔ `#[ignore]` | ⚠️ **E3**：`rerank/local.rs` **模块内部**（公开面只有 `σ(logit)`，σ **多对一** ⇒ 证明不了 logit 逐位相同） |
| **S7-T10** | 真模型 smoke：`R = k = 10` ⇒ 集合不变、仅顺序可能变；`R = 50` ⇒ 集合可增补后截断到 `k` | FR-18 的行为基线 | ⛔ `#[ignore]` | 同上 |
| **S7-T11** | **A/B 前置自证**：冻结图连续两次加载，`bm25` 三项 `Δ = 0` 且 `hybrid` 逐位一致 | §3.1 的空转防护（同 Step 6 F1 一族） | ✅（用**已有**快照 fixture，无精排） | `scripts/eval_rerank.sh` 的前置检查 + `tests/graph_persist.rs` 已有同族用例 |
| **S7-T12** | `explain.rerank_score.is_some()` ⟺ 精排生效；`score` 与 `rerank_score` 的 `σ` 单调关系 | D-S7-05 | ✅（用假精排器） | `query/searcher.rs` + `rerank/local.rs` |

> **CI 口径**：`S7-T8 / T9 / T10` 需 2.19GB 模型 ⇒ `#[ignore]`（沿用项目既有模式）。
> **性能数字一律本地 release 跑**，不进 CI。
> ⚠️ **不得**把「`#[ignore]` 用例未跑」写成「已覆盖」——`eval-report.md` §8.14 必须**显式记录**哪些 `#[ignore]` 用例**已在本地跑过、输出是什么**。

---

## 8. 实施任务拆分（S7-01 ~ S7-05）

| # | 任务 | 依赖 | 量级 | PR 切分建议 |
| --- | --- | --- | --- | --- |
| **S7-01** | `Reranker::candidate_window`（provided）+ `LocalReranker` + `local-rerank` feature + 注入接缝 + **S7-T5 / T8 / T9 / T10 / T12** | 无 | M | **PR 1**（内核 + 单测；**不动编排**，可独立评审） |
| **S7-02** | 编排层窗口打通：`candidate_k` 联动 + `take_n` + 截断 + `explain` 推迟 + `Metrics` 采集点 + `Explain.rerank_score` + **S7-T1~T4 / T6 / T7** | S7-01（trait 方法） | M | **PR 2**（**本 Step 的风险集中点**，单独立 PR 便于评审） |
| **S7-03** | CLI 接线（`search` / `bench` 的 `--rerank-window`）+ 两条脚枪的显式拦截 + `scripts/eval_rerank.sh` + **S7-T11** | S7-02 | S | **PR 3** |
| **S7-04** | **标定实验**（R 扫描 + `max_length` 对照 + 延迟/内存分解）⇒ `eval-report.md` **§8.14** + 决策门结论（默认 `R`） | S7-03 | M | **PR 4**（**数据 PR**，只动 `eval-report.md` + 脚本） |
| **S7-05** | 四处定义面回写（**NFR-12 数值定稿**、NFR-06/NFR-05 限定词、架构 §14.5 R44~R48、`docs/README.md`、`plan-v2.md`）+ **CHANGELOG** | S7-04 | S | **PR 5**（收尾） |

> **PR 切分原则**（沿用 Step 4/5/6 的教训）：
> ① **内核与编排分开**——S7-02 是本 Step 唯一会碰热路径的改动，单独一个 PR 便于「零回归」被逐行检查；
> ② **数据与结论同 PR**（S7-04），但**结论不改代码**（默认 `R` 的「拟」值已在设计里落，回填的是**数值**）；
> ③ ⚠️ **不要让 S7-04 与 S7-01/02 绑在一个 PR 里**——那会让「精排在本评测集上无提升」这个可能的结论**无法合并**（Step 6 的 A2 就是这么改过来的）。
> ④ 🔑 **S7-04 标定 → S7-05 回填（分工口径，全文统一；评审 F2）**：凡「**标定 / 实测 / 决策门 / 分别计时**」一律属 **S7-04**；凡「**定义面回写 / NFR-12 数值定稿**」一律属 **S7-05**。

---

## 9. 风险与未决问题

### 9.1 新增风险（**建议写入架构 §14.5**，编号 R44 ~ R48）

| # | 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- | --- |
| **R44** | **精排器常驻内存远超检索路径预算**：`bge-reranker-v2-m3` FP32 权重 **≈2.17GB**（`model.onnx.data` 2,271,088,656 B，sha256 已核）+ 激活 | NFR-05 的「峰值 RSS **372MB**（检索路径，batch 1）」**在精排开启时失效**（差一个数量级）⇒ 用户按 372MB 选机器会在开启精排后被 OOM kill | ① 本文建议给 NFR-05 补限定词「**不含精排器**」（同 Step 6 的 Q8 方案 C 先例）；② 精排**默认关**（D-S7-04）⇒ 不开就不付；③ S7-04 实测峰值 RSS 入 `eval-report.md` §8.14 | ⏳ **长期开放**：不给精排侧的独立 RSS 预算（与 R41 同族，**只给形态不给数**）。⚠️ 与 R41 的区别：R41 是**建库**路径峰值，R44 是**查询**路径**常驻**增量 |
| **R45** | **精排延迟主导**：窗口放开后成本 ≈ `O(R)` 次 cross-encoder 前向；若默认开，端到端 P99 从 ~3ms 涨到百毫秒级 ⇒ NFR-02 名义失效 | 交互体验（NFR-02 / 「Agent 关键路径几十次调用」） | ① 默认关（D-S7-04）；② **NFR-12 独立口径**（不并入 NFR-02）；③ 默认 `R` 由标定（§4.6.4）决定；④ 精排器与查询侧编码共享 `Mutex` 串行瓶颈（同 R43）⇒ **明确不做池化** | ⚠️ **保持开放**：`R` 与延迟的取舍是**用户侧**决策；本 Step 只给「可配 + 有数据」 |
| **R46** | **`Hit.score` 的语义随开关而变**（融合分 → `σ(logit)`），且 `hits` 的**并列次序**规则也随之改变 | 下游若用 `score` 设阈值 / 跨配置比较 / 缓存排序结果 ⇒ **静默失准**；与 R35 一族（对外可观测的破坏性变更） | ① `Explain.rerank_score` 让变换**可观测**（D-S7-05，NFR-07 精神）；② CHANGELOG 显式 `⚠️ 破坏性`；③ `Hit.score` 的 rustdoc 重写为「**当前排序依据**」并写明三种 mode × 精排开关的取值 | ⚠️ **识别成本仍在用户侧**：内核只能说清「这次 `score` 是什么」，无法阻止下游误用 |
| **R47** | **精排数值的可复现性未证实**：tokenizer 用 `PaddingStrategy::BatchLongest`（`common.rs:174-180`）⇒ **同一 `(query, doc)` 在不同批次组成下 padding 长度不同** ⇒ 浮点结果**可能不逐位相同**；ONNX 多线程归约亦然 | ① NFR-06（相同 query → 完全相同结果）**可能不成立**；② 标定实验的**分数轴不可横比**（只能比序数指标） | ① §4.7 三个层次探针 P1/P2/P3（S7-T8 / T9）；② §4.6.3 的「不可横比声明」；③ 若 P1/P2 出反例 ⇒ NFR-06 加限定词「不含精排」并升需求版本 | ⚠️ **测了才知道**：P3 的反例**不必修**（数学等价），但**改变报告怎么读** |
| **R48** | **512 token 截断使长段落的精排判据失真**：`HasMaxLength for RerankerModel = 512`（`reranking/init.rs:17-19`），而 T2Ranking 段落最长 **76,895 字符**（`bench.rs:757`）⇒ 精排只看得到段落前段 | **可能把「方法不行」错判为「精排无提升」**（结论错，而代码没错）；反过来若调大 `max_length`，成本近似平方增长 | ① D-S7-08（保持 512 起步 + `1024` 对照档）；② §4.6.4 的判据②明写「拐点不可识别时要如实记入局限」；③ `max_length` 进精排器**身份字符串**（D-S7-08/§4.4.4） | ⏳ **开放**：截断对长段落的定量影响要等标定；若确认为主因，**为评测集单独调大**（而不是改产品默认） |

### 9.2 未决问题

| # | 问题 | 由谁回答 | 阻塞谁 |
| --- | --- | --- | --- |
| **Q1** | 默认 `R` 取多少？ | **S7-04** 标定（§4.6.4 决策门） | S7-05 的 NFR-12 定稿 |
| **Q2** | 精排**默认开 / 关**？ | 🔴 **本文 D-S7-04，需评审拍板** | S7-04 的协议（开着测还是关着测）与 §6 影响面 |
| **Q3** | NFR-12 的**数值形态与取值**？ | 形态本文落（「端到端 P99 + 精排段 P99」双列、「拟」值），数值 S7-04 回填 | 需求 v1.16 的定稿 |
| **Q4** | `R` 与 `--filter` 叠加时（低选择度 + 精确扫描）的行为？ | **本文列为非范围**（§4.3.1 附注 + R45 的残余）；若评审要求覆盖 ⇒ S7-04 加一档 | 无（不阻塞） |
| **Q5** | NFR-05 是否补「**不含精排器**」限定词？ | 🔴 **本文建议补（随需求 v1.16 落）**，需评审确认 | §6 与需求版本 |
| **Q6** | 单路 mode（`bm25` / `vector`）下的精排是否达标要求？ | 本文 D-S7-09 建议**生效但不单列达标要求**（验收标准 3 只针对 `hybrid`） | 无 |
| **Q7** | 是否要在本 Step 做**精排池化**（多 session 并发）？ | 本文**明确不做**（§4.4.2，理由：R43 债务 + R36 的内存教训 + 精排默认关）——若评审反对，需**先答挂起的 Q3**（Step 6 §11.5） | 无 |

> ⚠️ **与 Step 6 的挂起项的关系**：**Q3（不同 `sessions`/`intra_threads` 是否改变向量数值）仍是挂起状态**，而本 Step 的 R47 探针 P3 是**同族问题在精排上的版本**。本文**不重复回答**它；但若 S7-T9 在精排上得到「批次组成会改变数值」的实测，那**会强化**「Step 6 Q3 必须先答」的判断（R43 的复审触发条件）。

---

## 附录 A：新增 / 变更 API 一览 + 不变式

```rust
// crates/core/src/rerank/mod.rs
pub trait Reranker: Send + Sync {
    fn candidate_window(&self, k: usize) -> usize { k }        // ⬅ 新增（provided）
    fn rerank(&self, query: &str, hits: Vec<Hit>, top_n: usize) -> Result<Vec<Hit>>;
}

// crates/core/src/rerank/local.rs（gate: feature = "local-rerank"）
pub const DEFAULT_RERANK_WINDOW: usize = 20;                   // 「拟」值
pub const DEFAULT_RERANK_MAX_LENGTH: usize = 512;

pub struct LocalReranker { /* Mutex<TextRerank>, window, max_length */ }

impl LocalReranker {
    pub fn new() -> Result<Self>;
    pub fn with_window(self, window: usize) -> Self;            // 运行期参数（不触碰模型）⇒ 可后置
    pub fn with_params(window: usize, max_length: usize) -> Result<Self>;
    // ⚠️ **E1**：`max_length` 只能**构造期**给（烧进 tokenizer）；原写的构造后
    //    `with_max_length(self, …)` 是「改了不生效」的假象。
    pub fn id(&self) -> String;                                 // ⚠️ String，不是 &'static str
}

// crates/core/src/query/metrics.rs（纯加法，保持 Copy）
pub struct Metrics {
    /* … 既有 12 个字段 … */
    pub rerank_window: usize,
    pub rerank_elapsed: Duration,
}

// crates/core/src/query/response.rs（⚠️ 加字段 = 破坏性）
pub struct Explain {
    /* … 既有 6 个字段 … */
    pub rerank_score: Option<Score>,   // 精排生效时为模型原始 logit
}

// crates/core/src/query/searcher.rs（⚠️ **S7-03 实现期补**：新增公开方法，纯加法）
impl<'a> QueryExecutor<'a> {
    pub fn with_reranker(mut self, reranker: Box<dyn Reranker>) -> Self;       // 签名与语义**未变**
    pub fn with_reranker_arc(mut self, reranker: Arc<dyn Reranker>) -> Self;   // ⬅ 新增（共享实例）
    // 字段 `reranker: Box<dyn Reranker>` → `Arc<dyn Reranker>`（**私有字段**，不动公开面）。
    // 动机：`bench` 要「每 mode × 每 run」装配一次（4 个入口），而复用 ≈2.19GB 实例只能自己写
    // 转发包装 —— 那会在 `Reranker` 加方法时**静默漏转发**（`candidate_window` 漏了就是窗口静默失效）。
}

// crates/cli/src/{main,bench}.rs（`search` 与 `bench` **参数面完全对称**）
//   --rerank-window <R>      不传 = 关闭精排（= NoOp）；与 bench 的 --runs > 1 互斥
//   --rerank-max-length <N>  从属参数（必须与 --rerank-window 同时给）；默认 512
//   ⚠️ 四条守卫的**顺序是硬要求**：参数面（max_length 从属 / `0` 拒绝 / `--runs` 互斥）必须排在
//      feature 守卫之前，且四者都在加载任何资源（快照 / 模型）之前 —— 理由见 §4.5 的守卫表。
```

**不变式（写入 rustdoc，三条以内）**：

1. `take_n = min(max(k, candidate_window(k)), fused.len())`，且 `candidate_k = max(3k, candidate_window(k), 10) ≥ take_n`（窗口**不会被候选池静默封顶**）。
2. `Reranker::rerank` **不得依赖** `hits[i].explain`（`matched_terms` / lane rank 在精排**之后**才填 —— D-S7-06）。
3. 精排生效时 `e.hits` 按 `score` **严格不增**排序，`score` 并列时按 `chunk_id` **升序**；且 `hit.explain.rerank_score.is_some()` 是「`score` 已被精排替换」的**唯一**信号（D-S7-05 / D-S7-07）。

---

## 附录 B：执行命令

```bash
# ⚠️ 全部为**本地** release 实测；延迟数字不进 CI（CI 共享 runner 的数字不具可引用性）

# 0) 守门（含 shell 展开检查）
make fmt && make lint && make test && make deny && make shell

# 1) 建一张**冻结图**（T2Ranking 12K，单 chunk 口径，与 P5 可比）
#    ⚠️ 这一步只做一次；之后所有 A/B 都吃这张图的 sidecar（图落盘即冻结 = NFR-06 口径）
cargo run -p helix --release -- build \
  --input data/t2-corpus.jsonl --vectors --single-chunk --output /tmp/t2-frozen.idx

# 2) 【空转防护】先自证「冻结」成立，再谈 A/B（§3.1 / S7-T11）
for i in 1 2 3; do
  ./scripts/eval_quality.sh --index /tmp/t2-frozen.idx --runs 1 --modes bm25,vector,hybrid \
    --json /tmp/s7-freeze-$i.json
done
#    判据：三次的 bm25 三项必须 Δ = 0；hybrid 的 MRR/NDCG 必须逐位一致。
#    若不一致 ⇒ 快照没被真正复用（图每轮重建）⇒ **后面所有数据作废**。
#    ⚠️ 必须 --runs 1：bench 的 --runs > 1 会**刻意跳过 sidecar 重建图**（bench.rs:436-446）。

# 3) 控制组 A：精排**关**（NoOp）——效果基线的唯一参照
./scripts/eval_quality.sh --index /tmp/t2-frozen.idx --runs 1 \
  --modes bm25,vector,hybrid --json /tmp/s7-A-noop.json

# 4) 控制组 B + 各档位：R = k(10) / 20 / 50 / 100 / 200（顺序交错，见 §4.6.1）
for R in 10 20 50 100 200 10 20 50 100 200; do      # 交错两轮
  cargo run -p helix --release -- bench \
    --index /tmp/t2-frozen.idx --runs 1 \
    --modes bm25,vector,hybrid --rerank-window "$R" \
    --queries data/t2-queries.jsonl --k 10 --json "/tmp/s7-r${R}-$(date +%s).json"
done
#    ⚠️ 只比**序数指标**（MRR / NDCG / Recall）与**条数**；
#       **不同 R 的 score 不可逐位横比**（R47：PaddingStrategy::BatchLongest）。

# 5) `max_length` 对照档（R48）
cargo run -p helix --release -- bench --index /tmp/t2-frozen.idx --runs 1 \
  --modes hybrid --rerank-window 20 --rerank-max-length 1024 --json /tmp/s7-ml1024.json

# 6) 延迟轴：固定子集 × 重复（精排是秒级 ⇒ 不能全量 × 20 reps）
cargo run -p helix --release -- bench --index /tmp/t2-frozen.idx --runs 1 \
  --modes hybrid --rerank-window 20 --reps 20 --warmup 3 --json /tmp/s7-lat-r20.json

# 7) 内存（R44）：⚠️ 必须**直接包二进制**（包 cargo run 会量到 cargo 的 RSS，既有教训）
/usr/bin/time -l target/release/helix search --index /tmp/t2-frozen.idx \
  --mode hybrid --rerank-window 20 --k 10 "如何实现支持中文的 BM25 检索"

# 8) 数据落表 ⇒ docs/devel/eval-report.md §8.14（新增小节）
#    ⚠️ 必须记录：运行范围（核数 / 插电 / 非 CI）、抖动带、决策门结论、
#       以及**哪些 #[ignore] 用例真的跑过**（不得把未跑当已覆盖）。
```

---

## 附录 C：依赖源码核实记录（本文技术前提）

**`fastembed-6.0.2`**（`~/.cargo/registry/src/*/fastembed-6.0.2`）

| 位置 | 事实 |
| --- | --- |
| `src/models/reranking.rs:6-11` | `RerankerModel` 枚举：`BGERerankerBase`（默认）/**`BGERerankerV2M3`**/`JINARerankerV1TurboEn`/`JINARerankerV2BaseMultiligual` |
| `src/models/reranking.rs:28-33` | `BGERerankerV2M3` ⇒ `model_code = "rozgo/bge-reranker-v2-m3"`、`model_file = "model.onnx"`、**`additional_files = ["model.onnx.data"]`**（⚠️ **非 BAAI 官方库**；官方库**没有** ONNX） |
| `src/reranking/mod.rs:1-2` | `DEFAULT_MAX_LENGTH = 512` / **`DEFAULT_BATCH_SIZE = 256`** |
| `src/reranking/init.rs:11-19` | `pub struct TextRerank { tokenizer, session, need_token_type_ids }`；`impl HasMaxLength for RerankerModel { MAX_LENGTH = 512 }` |
| `src/reranking/init.rs:22` | `pub type RerankInitOptions = InitOptionsWithLength<RerankerModel>` |
| `src/reranking/init.rs:152-156` | **`pub struct RerankResult { document: Option<String>, score: f32, index: usize }`**（`index` = 输入切片下标 ⇒ 回填必须用它） |
| `src/reranking/impl.rs:44` | `pub fn try_new(options: RerankInitOptions) -> Result<TextRerank>`（`hf-hub` feature 下） |
| `src/reranking/impl.rs:126-132` | `pub fn rerank<S: AsRef<str> + Send + Sync>(&mut self, query: S, documents: impl AsRef<[S]>, return_documents: bool, batch_size: Option<usize>)` —— ⚠️ **`&mut self`** ⇒ 必须 `Mutex` |
| `src/reranking/impl.rs:134` | `batch_size.unwrap_or(DEFAULT_BATCH_SIZE)` ⇒ 传 `None` = **256** |
| `src/reranking/impl.rs:143` | `documents.chunks(batch_size)` ⇒ **按批编码**（批次组成影响 padding 长度） |
| `src/reranking/impl.rs:145-148` | `tokenizer.encode_batch(inputs, true)` + `encodings.first().len()` ⇒ **批内等长**（依赖 padding 已配） |
| `src/reranking/impl.rs:215-224` | `top_n_result` 由**全部** scores 构造并 `sort_by(\|a,b\| a.score.total_cmp(&b.score).reverse())` ⇒ **返回全量排序**；`sort_by` 稳定 ⇒ 并列保持**输入顺序** |
| `src/common.rs:174-180` | **`.with_padding(PaddingParams { strategy: PaddingStrategy::BatchLongest, … })`** ⇒ ⚠️ **批内 padding 到最长** ⇒ 同 (query,doc) 在不同批次组成下 logit 可能不逐位相同（R47） |
| `src/common.rs:181-184` | `.with_truncation(TruncationParams { max_length, .. })` ⇒ `max_length` 生效位置（⚠️ **第 2 轮统一**：原写 `181-185`，`185` 是同一 builder 链上的 `.map_err`；现与 E1 / `crates/core/src/rerank/local.rs` 对齐为 **`181-184`**） |
| `src/common.rs:262-270` | `init_session_builder(execution_providers, intra_threads)`：`None` ⇒ `available_parallelism()` = **用满所有核** |
| `src/init.rs:61-101`（`impl InitOptionsWithLength`）的 **`:71`** / **`:77`** / **`:100`** | `with_max_length` / `with_cache_dir` / `with_show_download_progress`（`RerankInitOptions = InitOptionsWithLength<RerankerModel>` 走这份）。⚠️ 同名方法另有一份在 `:106-140` 的 `impl InitOptions`（**不是**我们走的路径） |

> ⚠️ **行号更正（2026-09-14 评审 F5）**：本表初版的 `impl.rs` 行号整段漂移（`rerank` 记作 `:110`、`unwrap_or` 记作 `:118`、`chunks` 记作 `:127-132`、`encode_batch` 记作 `:134-141`、`top_n_result`/`sort_by` 记作 `:186-198`）⇒ **已按本机 `fastembed-6.0.2` registry 实测更正**（`:126-132` / `:134` / `:143` / `:145-148` / `:215-224`；该文件共 **227** 行）。**语义全部未变**（`&mut self`、全量稳定排序不截断、`index` 回填、`BatchLongest` 均已复核）。§2.5 / §2.6 正文引用的 `:134-181` / `:186-198` 同源偏移，已一并更正为 `:143-212` / `:215-224`。
> ⚠️ **同类漂移不止 `impl.rs`** —— 作者按「**一类缺陷要全仓扫**」把本文**全部 fastembed 行号引用**核了一遍，另更正 **`reranking/init.rs` 3 处**（`:14-19` → **`:11-19`**、`:16-18` → **`:17-19`**、`:21` → **`:22`**、`:149-155` → **`:152-156`**，共 4 处）与 **`src/init.rs` 1 处**（`:63-70` / `:106-113` → **`:61-101` 的 `:71` / `:77` / `:100`**），以及 §3.3 正文的 `impl.rs:110` → **`:126-132`**（评审 F5 未点到、由作者全仓扫描发现）。**核过仍准确、未改的**：`reranking/mod.rs:1-2`、`impl.rs:44`、`common.rs:174-180` / **`:181-184`**（第 2 轮统一，原 `181-185`）/ `:262-270`、`lib.rs:130` / `:132`、`models/reranking.rs:6-11` / `:28-33`。
>
> ✅ **`fastembed` 重导出了 `TextRerank` / `RerankerModel` 吗**（决定 `use` 路径）—— **已核实（2026-09-14）**：本机 registry 实测 **`src/lib.rs:130` `pub use crate::models::reranking::RerankerModel;`**、**`src/lib.rs:132`** 起 `pub use crate::reranking::{ OnnxSource, **RerankInitOptions**, RerankInitOptionsUserDefined, **RerankResult**, **TextRerank**, UserDefinedRerankingModel };`**（注释 `// For Reranking` 在 `:129`）⇒ **与 embedder 侧同款，都是从 crate root 重导出**。**S7-01 可直接 `use fastembed::{RerankerModel, RerankInitOptions, RerankResult, TextRerank};`**。
> ⚠️ 本条初版标「**本文未核实**」并嘱咐开工时先 grep —— **评审已代答，作者已独立复核属实**；原「旁证推理」段（用 `embed/local.rs:9` 反推）已不再需要，删除。

**本地缓存 / 体积**（前置 ② 的实测）

| 项 | 值 |
| --- | --- |
| 仓库 | `rozgo/bge-reranker-v2-m3`（HF） |
| 合计体积 | **≈2,187 MB**（`model.onnx.data` 2,271,088,656 B / `tokenizer.json` 17.1MB / `sentencepiece.bpe.model` 5.1MB / `model.onnx` 108KB / 配置 <2KB） |
| **sha256** | **`84b66c787b9b98977a16d5c993a3959210a214c98fc3466da263c949c2068945`**（= 仓库声明的 LFS oid，已实下载校验） |
| 取用日期 | 2026-09-14 |
| 本地缓存 | **未缓存**（`~/.fastembed_cache` 只有 embedder 的 `bge-small-zh-v1.5`）⇒ 首次使用要下 2.14GB |

---

## 附录 D：本文引用的项目内证据

| 主张 | 位置 |
| --- | --- |
| 融合后**先 `take(k)`**、再进精排 | `crates/core/src/query/searcher.rs:286`（take）vs `:316`（rerank） |
| `candidate_k = max(3k, 10)` | `crates/core/src/query/searcher.rs:118` |
| 融合输出上限 = `candidate_k` | `crates/core/src/query/searcher.rs:245` |
| `matched_terms` 对**整段文本**跑 `analyze_doc` | `crates/core/src/query/explain.rs:13-17`；调用点 `crates/core/src/query/searcher.rs:296` |
| `vector_shortfall` 的分母含 `candidate_k` | `crates/core/src/query/searcher.rs:230-232` |
| `Reranker` trait 定义 | `crates/core/src/rerank/mod.rs:16-19` |
| `NoOpReranker` | `crates/core/src/rerank/noop.rs:12-16` |
| 默认装配 = `NoOpReranker` | `crates/core/src/search/config.rs:317-320` |
| builder 已有 `reranker()` 入口 | `crates/core/src/search/config.rs:186-189` |
| `Config::fingerprint()` 四字段（**不含** reranker） | `crates/core/src/search/config.rs:59-70` |
| `Mutex<TextEmbedding>` + `&mut self` 的先例 | `crates/core/src/embed/local.rs:34-37, 66-80` |
| 模型缓存目录（复用） | `crates/core/src/embed/local.rs:19-31` |
| `Hit` / `Explain` 字段 | `crates/core/src/query/response.rs:14-31`（Hit）、`:36-61`（Explain） |
| `Score = f32` / `ChunkId = u32` | `crates/core/src/types.rs:7, 10, 16` |
| `Metrics` 字段与 `Copy` | `crates/core/src/query/metrics.rs:43-92` |
| `Metrics` 只经 `tracing` 的**历史断链**（T7-23 的由来） | `crates/core/src/query/metrics.rs:1-18` |
| `--runs > 1` **刻意重建图**（与冻结图互斥） | `crates/cli/src/bench.rs:436-446` |
| `--analyzer charabia` 的「缺 feature 即报错」先例 | `crates/cli/src/bench.rs:711-724` |
| T2Ranking 段落最长 76,895 字符 | `crates/cli/src/bench.rs:757` |
| `--threads` 的逐位一致前置条件（NFR-10 ①） | `crates/cli/src/bench.rs:178-180`；`check_concurrency_precondition` `:1597` |
| P5 基线（hybrid MRR@10(1) = **0.6922**） | `docs/devel/eval-report.md` §3.1 |
| 图漂移 ~0.6%（`hybrid MRR@10(1) = 0.68815`） | issue **#23** 评论（2026-09-14）/ `eval-report.md` §3.4 |
| 模型体积 / sha256 / 出处 | issue **#23** 评论（2026-09-14） |
| NFR-12 / NFR-05 / NFR-06 / NFR-07 / FR-18 | `docs/devel/requirements-spec.md` §5.2（FR-18）、§5.3.6、§6.1（NFR-12 / NFR-05）、§6.2（NFR-06 / NFR-07） |
| H3 的原文（含 `query/searcher.rs:196` 的**漂移锚点**） | `docs/devel/plan-v2.md` §4.0 |
| R35（破坏性变更的判定先例）/ R41 / R43 | `docs/devel/architecture-design.md` §14.3 / §14.4 |
| 「策略归后端、分派与记账归编排层」先例（D-S5-01） | `docs/devel/v2-step5-design.md` §4 |
| Step 6 的 F1（测量空转）与 Q8（口径限定词）先例 | `docs/devel/v2-step6-design.md` §10.1 / §11.4 |
| 标定方法学（交错 / 控制组归一 / 运行范围 / 不可横比） | 技能 `perf-ab-calibration` |
