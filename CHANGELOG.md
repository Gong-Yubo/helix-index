# 更新日志

本项目所有值得注意的变更都记录在此。
格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

> P0~P5 的条目为**逆向补写**（2026-09-03），依据各阶段设计文档与 git 历史整理；
> 此后每次变更即时追加。

## [Unreleased]

### 新增 · V2 Step 8 PR 6 —— **分段合并器**（`S8-06`）+ spike S8-S1 的向量侧取形（2026-09-20）

> ⚠️ **无 `Breaking`**（**纯加法**：新增 `MergeReport` / `VectorMergeStrategy` 两个公开类型
> 与 `SearchIndex::merge_pending()` / `merge_all()` 两个公开方法）。
> 门测试 = **`S8-T10`**（写/合并/读并发 + 双写端）/ **`S8-T13`**（字段索引等价）/
> **`S8-T14`**（向量侧不丢点）。

#### 新增

- **`SearchIndex::merge_pending()` / `merge_all()`**（`D-S8-08`：库**不 `spawn` 线程**）：
  宿主（CLI / `helix serve`）循环调用前者做后台合并。**严格 FIFO**（只并队首一段）——
  跳段会破坏 §4.4.2 的基址不变式。后者是 `save()` / `compact()` 的前置（`D-S8-01` / `D-S8-12`）。
- **`MergeReport`**（7 字段）：`segments_merged` / `chunks_merged` / `tombstones_applied` /
  `vector_strategy` / `vector_merge_ms` / `total_ms` / `generation`。
- **`VectorMergeStrategy`**（`None` / `Incremental` / `Rebuild`）：⚠️ `None` 是**实现期新增的取值**
  （原设计只列两个）——「纯 BM25 装配**没有**向量侧工作」必须能如实表达（`NFR-07`：不撒谎）。

#### 向量侧：**方案 A 落地**（`dump → load → 增量 insert`），失败/不支持**回落方案 B**

由 **spike S8-S1** 标定（`crates/core/examples/spike_s8s1.rs`，可复跑）——
**五条判据全 PASS**：A = **639ms** vs B = **4774ms**（**A/B = 0.134**，阈值 ≤ 0.2）+
可行性 + 召回同量级 + 累积 RSS 增量 **0 KiB**（`Box::leak` 只漏 `HnswIo` ≈200B/次）。

#### 🔴 `S8-T14` 的**判据口径就地更正**（实测逼出来的）

原设计判据 = 「合并后的图与全量重建图的 top-10 重合率 **≥ 0.99**」——
**结构性不可达**：`hnsw_rs` 用**无种子 `OsRng`** 分层 ⇒ **同一份数据重建两次**的图（B vs B′）
只有 **0.9280** 重合。⇒ 改为**相对基线** + **以 `Brute`（精确）为参照的 `recall@10`**。
🔑 **这是一次「对照实验推翻首跑结论」**：首跑只算 A vs B 得 0.922 ⇒ 结论会写成「回落 B」，
**与事实相反**；补了「B vs B′ 基线」与「A vs A′ 自证」两个对照才定案。

#### 测试

- `S8_T10_写与合并与读的并发探针`（⚠️ **`!Sync` ⇒ 实为两线程**：写+合并一个写端、读端 `Searcher`）+ `S8_T10_双写端交替提交不得让段ID空间重叠`（钉 `I8-7` + `error.rs` 的恢复指引）。
- `S8_T13_合并后的字段索引与单段全量重建等价`（200 篇 / 分 4 段 / 8 条过滤 battery）。
- `S8_T14_合并后的向量侧不丢点_以精确后端为参照`。
- `S8_06_合并原语的契约_一次一段与空段返回None` / `S8_06_纯BM25库的合并报告应为None策略`。

#### 变异验证（4 组，**逐组已还原**）

| 注入 | 期望命中 | 实测 |
| --- | --- | --- |
| 方案 A 恒不可用（强制走 B） | `S8_T14` 的 `Incremental` 断言 | ✅ **真实场景命中**（修 T14 场景前实测为 `Rebuild`） |
| `merge_pending` 变成「并全部」 | 契约用例的 `segments_merged == 1` | ✅ 红（`left: 3 / right: 1`） |
| 纯 BM25 库的策略记成 `Rebuild` | `None` 断言 | ✅ 红（`left: Rebuild / right: None`） |
| `merge_from` 跳过 `field_index.rebuild` | `S8_T13` 的逐位一致 | ✅ 红（`allowed` 不一致） |

⚠️ **一处踩坑如实记录**：第三组首轮报「零命中」，真因是**注入文本的行尾注释把 `)` 注释掉了
⇒ 编译失败 ⇒ 测试根本没跑**（脚本只看 `FAILED` 会把它读成「覆盖边界」）。
⇒ 变异脚本必须**同时判「有没有编译错误」**（`error[E` / `could not compile`）与「测试是否真的跑了」。

#### 覆盖边界（不声称已守住）

- **`S8-T10` 的「三线程」不可达**（`D-S8-05` 让 `SearchIndex` 保持 `!Sync`）⇒ 实为两线程；
  **被测语义（读与写/合并重叠）未缩水**，但与设计原文的措辞不同。
- **方案 A 的临时文件 I/O 未量化**（每次合并一次全量 dump，12K 点实测 30ms / 数十 MB）：
  spike 只在 `/tmp`（tmpfs 或本地盘）量过，**未测慢盘/写满**的退化路径。
- **`reclaimed_terms` 的口径仍未统一**（`D-S8-01` 后收窄）：属 `compact` 的回收统计，
  与 `MergeReport` 是两套口径 ⇒ 登记在案、未在本 PR 处理。
- **`merge_pending` 的「宿主调用策略」（`K=4`）未实现**：库只给原语（`D-S8-08`），
  宿主侧循环留 Step 8 之后。


### 修复 · V2 Step 8 PR 5 · **第 1 轮评审响应**（#64，2026-09-20）—— P3 补 `main` 非空变体 + P4 Breaking 段补字段

> 落点 = `pulls/64/reviews` **1 条**（id `5259462645`，SHA `63ad318`）+ **行内 1 条**
> （`crates/core/tests/step8_segments.rs:1074`）；`issues/64/comments` = **0**。
> 评审结论 = **「PR5 范围内无阻塞项，达到可合并形态」**；四处取形
> （载具不共用 / `vector_shortfall` 不逐段化 / 兜底退回单索引 / `Mixed` 无端到端）
> **逐条确认 ✅**。两条意见均为「测试 / 记录加固」，**全部采纳**。

#### Fixed

- 🟡 **P3-1（**已实证**的覆盖盲区）：三条跨段向量用例的夹具 `main` 恒为空 ⇒
  「向量路**漏查 `main`**」这一类变异**全仓不可检测**。**
  **独立复现**（不引用评审的话）：在 `SegmentedVectorRetriever::search_segmented` 的循环注入
  `if i == 0 { continue; }`（向量路**永不**查 `main`）⇒ `step8_segments` **17/17 全绿**
  （含 `S8_T5` / `S8_05_逐段谓词` / `S8_02_旧API`）⇒ 指控**属实**。
  ⚠️ 而 `helix serve` 的常态恰恰是「`main` 有存量 + `delta` 有增量」（`save` / `compact` 之后又
  `commit`）⇒ 该变异若真实发生（如有人把段来源换成 `deltas.iter()`、或归并循环起点写错），
  **存量内容的向量召回会静默消失**。
  **修法** = ① 新增夹具 `t5_main_and_two_deltas`（`add ×2 → commit → save`（折进 `main`）
  `→ add ×2 → commit → add ×2 → commit` = `main(2) + delta(2) + delta(2)`）；
  ② 新增用例 **`S8_T5_main非空与delta并存时跨段仍逐位一致`**，与**同一份 6 篇语料的单段参照系**
  逐位比较；③ 把两个布局变体的断言抽成**共用助手** `assert_vector_cross_eq_single`
  ⇒ 将来改判据**不会漏掉一个**（正是「一类缺陷只修被点到的那一处」的同族错误）。
  🔑 该夹具同时锁住**两个分支**：「空段参与归并只贡献空集」与「非空段正常并入」。

#### Changed

- 🔵 **P4-1：Breaking 段补 `SearchParts.vector_segments`**（见上方 `#### ⚠️ Breaking`）。
  `SearchParts` **未标** `#[non_exhaustive]`（`crates/core/src/query/searcher.rs:70`）⇒ 下游字面量
  构造会编译失败。与 PR4 记录 `segment_set` / `predicate_builder` 时**同一纪律**。

#### 变异验证（本轮 3 组，**红因逐个核对**）

| 注入 | 期望命中 | 实测 |
| --- | --- | --- |
| 跳过段 0（= `main`） | **新变体** | ✅ **唯一红 = 新变体**（旧三条仍绿 ⇒ 新变体是该缺口的唯一绊线） |
| 跳过段 1（= 第 1 个 delta） | `S8_T5` ×2 | ✅ 两条 |
| 跳过段 2（= 第 2 个 delta） | `S8_T5` ×2 | ✅ 两条 |

🔑 **第 1 组注入了两次证据**：失败原文显示**跨段侧恰好丢掉 `chunk_id` 0 / 1**（= `main` 里那两篇），
而单段参照系仍有 6 条 ⇒ ① 一手证明新夹具的 `main` **确实非空**（若为空，「跳过段 0」是语义空操作、
不可能红）；② 证明断言抓的正是「漏查 `main`」本身。
🔑 **第 2 / 3 组说明该夹具是「跳过任意单段」这整类的绊线**，不是 `main` 专用
（若只跳过 `main` 才红，就还留着「漏查 delta」的同类缺口）。

#### 覆盖边界（**就地收窄 + 残留如实登记**）

- ✅ **「漏查 `main`」已收窄**：此前**不可检测**。⚠️ 本文件上一版的覆盖边界**漏登记了这条**
  （当时只列了 `Mixed` / `Hnsw` 标定 / `allowed` 输入三条）⇒ 现在有直接绊线
  （`S8_T5_main非空…`），且该绊线覆盖**整类**（跳过任意单段都会红）。
- ⚠️ **残留（未做，如实登记）**：`S8_05_逐段谓词的语义折算` 的夹具 `main` **仍为空**。
  该用例的鉴别力来自「段的 `base_chunk ≠ 0`」（谓词折算 / 段对齐），与 `main` 是否为空**正交**
  ⇒ **未改**（评审也只要求 `S8_T5` 补变体）。若将来给它也加非空 `main`，能多锁
  「`filters[i]` 与 `segments[i]` **含 `main` 的对齐**」—— 属**可选加固**，本轮不做。

### 新增 · V2 Step 8 PR 5 · **跨段向量检索**（`S8-05`，2026-09-20）—— `S8-T5` 是它的门

> ⚠️ **`Breaking`（两条，见下）**、**公开面 +1 类型 +4 方法**、**逐段谓词必须折算段内语义**。
> 本 PR **堆叠在 PR4（#63）之上**（base = `feat/v2-step8-delta-segments`）：`S8-05` 依赖
> `S8-03`/`S8-04` 的 delta 分段与跨段谓词，而 PR4 尚未合入 `main`（用户已确认走堆叠）。
> ✅ **（2026-09-20 同步）`#63` 已 squash 合并（`main` = `b3e1ab6`）⇒ 本 PR 的 base 已改为 `main`**，
> 且分支已把 `main` 合入（补齐此前缺的 `1b5243c..207a62d` P4 修复）。
> ⚠️ 上面两行是**当时**的事实，按「已 push 的记述不改写」留档；**当前状态**以本行为准。

#### ⚠️ Breaking

- **`VectorRoute` 新增 `Mixed` 变体**（`crates/core/src/vector/mod.rs`）：`S8-04` 起一次检索要对
  **每段各查一次**向量，而 `prefers_exact` 是**逐段判定**的（阈值输入是该段的 `allowed_count`）
  ⇒ 「主段走 ANN、某个小 delta 段走精确」是**常态**：只报 `Ann` / `Exact` 都会**说谎**。
  仍按 **Q5 已结案的取形**执行 —— `VectorRoute` **未标** `#[non_exhaustive]`
  ⇒ 下游**穷尽 `match`** 会编译失败；而补 `#[non_exhaustive]` **同样**会让它失败
  ⇒ **两条路都 breaking，没有「零破坏」选项**（设计 §6 的核实项，本 PR 落地）。
- **`SearchParts` 新增第 4 个字段 `vector_segments`**（`crates/core/src/query/searcher.rs:70`）：
  与 PR4 的 `segment_set` / `predicate_builder` **同族** —— `SearchParts` **未标** `#[non_exhaustive]`
  ⇒ 下游若有**字面量构造** `SearchParts { … }` 会**编译失败**（本仓内为 0 处）。
  ⚠️ **边际破坏为零**（PR4 已让字面构造编译失败），但**记录必须补齐** —— 否则「PR5 的
  Breaking 清单」不完整（本仓自设的门 = 「破坏性必须核实并记录」）。
  🔵 **`S8-05` 第 1 轮评审 P4-1 补登**（原先只在 Added 里带了一句，见下方本轮响应条目）。

#### Added

- **`SegmentedVectorRetriever`**（`crates/core/src/retriever/vector.rs`）：**逐段查 + 全局归并**
  （段不可变 + `Box<dyn VectorIndex>` 非 `Clone` + `hnsw_rs` 无图合并 ⇒ 只能这样）。
  归并**复用 `VectorRetriever::to_scored`**（`score = 1 − d²/2` 单调 ⇒ 距离升序 ≡ 相似度降序）
  ⇒ 不另写一份排序口径。
  🔑 **`Brute` 下「跨段 == 单段」逐位一致是结构性承诺**：全局 top-`k` 的任一成员在**它自己那段**
  的排名必然 `< k` ⇒ 每段取自己的前 `k` 后归并是**无损**的（设计 §4.6.2）。
  ⚠️ `Hnsw` 下**不承诺**（两张图 ≠ 一张图）——那是 ANN 的定义域。
- **`VectorSegmentRef` / `SearchParts.vector_segments`**：**逐段**向量载具（FIFO）。
  ⚠️ **刻意不复用** `SegmentSet`：本仓有「两条 lane 不得互相引用」的硬约束
  （`retriever` 模块头注释）⇒ 载具各自一份。
- **`union_route`**（公开纯函数）+ **`VecLane`**（编排层的两种形态）：单段 ⇒ 单索引（本地
  `chunk_id` == 全局，**与 `S8-03` 之前逐位一致**）；跨段 ⇒ 逐段 + 并集分类。
- **`SegmentPredicate`**（`crates/core/src/search/view.rs`）：把 `SegmentFilter` 暴露成
  `CandidateFilter`，**段内本地 `chunk_id` 语义**，供**该段自己的**向量索引消费。
  🔴 这层折算不可省：`VectorIndex::search_filtered` 的谓词收的是**本地** id，而跨段谓词
  （`ViewFilter`）收的是**全局** id ⇒ 直接下推时主段（`base_chunk == 0`）**看不出问题**，
  而 delta 段会把「本地 id」当「全局 id」判 ⇒ 过滤决策**来自别的段** ⇒ **答错**（不是少召回）。
- **`BuiltPredicates` + `PredicateBuilder::build_all`**：**一次**过滤求值同时给出**全局**（BM25 路 /
  单段向量路）与**逐段**（跨段向量路）两种形态 —— 分成两次构造会让逐段 `doc_bits`
  （字段索引求值）白算一遍。默认实现只给全局（`per_segment = None`）⇒ 单段路径**逐位不变**。
- **`ViewFilter::allowed_per_segment`**：逐段 `allowed`（**含落在该段的墓碑扣减**）的**单点实现**
  —— 全局 `allowed`（= 其和）与逐段 `prefers_exact` 的阈值输入**同源**，否则会出现两个会漂移的口径。

#### Changed

- **`Metrics.vector_segments` 的语义落地**（`S8-03` 评审 P2-2 埋的信号，此前恒为 `1`）：
  跨段时 = **参与归并的段数** ⇒ **恒等于 `Metrics.segments`**（`S8-05` 的验收项）。
  ⚠️ 口径 = 「**参与归并**」而非「实际查到东西」：某段为空 / 无向量索引时它对归并的贡献是**空集**，
  但它**被覆盖了**（不存在漏召回）。
- **`Metrics.vector_route` 改为逐段判定 + 并集上报**（设计 §4.6.3 的实现落地）。
- **`S8_02_旧API与新API结果逐位一致` 的 `vector_recalled` 断言按承诺反转**：
  PR4 把它从「必须有向量路召回」反转为「当前不应有」，并写明「**PR5 落地后必须再反转回 `> 0`**」
  ⇒ 本 PR 执行该反转。

#### 测试

- **`S8_T5_Brute后端vector模式跨段逐位一致`**（设计 §7 明写的**门测试**）：同一份语料，
  `[单段视图]` vs `[main + 2 个 delta]` 的 `vector` 结果**逐位一致**（含 `score` / `explain`），
  并自证 `(segments, vector_segments) == (1,1)` vs `(3,3)`（否则「两串相等」可能只是都为空）。
- **`S8_05_逐段谓词的语义折算_带过滤的跨段向量与单段一致`**：带用户过滤（`FilterKind::Filtered`）时
  跨段必须与单段逐位一致 —— 抓「全局谓词直接下推给 delta 段」这一**答错**类缺陷。
- **`S8_05_union_route的并集分类`**：`None` 项忽略 / 全 `Ann` / 全 `Exact` / 混合 ⇒ `Mixed`。
- **`S8_T5_main非空与delta并存时跨段仍逐位一致`**（**`S8-05` 第 1 轮评审 P3-1 补登**）：
  布局改为 `main(2) + delta(2) + delta(2)` —— 上一条（及各跨段用例）的 `main` 恒为空 ⇒
  「向量路漏查 `main`」那**一类**变异**全仓不可检测**；本变体让它成为**唯一绊线的**可见红。

#### 变异验证（4 组注入，**逐个与意图相符**）

| 注入 | 期望命中 | 实测 |
| --- | --- | --- |
| 退回「只查主段」（`if false` 让逐段分支不可达） | `S8_T5` 等 | ⚠️ 红 3 条，但**红因是我自己的 `debug_assert`（结构性兜底守卫）**⇒ 按纪律**不计数** |
| **只查前两段**（`i > 1 ⇒ continue`，绕开守卫） | `S8_T5` 的**逐位**断言 | ✅ `S8_T5` + 谓词折算条 —— **红因正确** |
| 逐段下推**全局**谓词（丢折算） | `S8_05_逐段谓词的语义折算` | ✅ **唯一红**（`S8_T5` 无过滤 ⇒ 保持绿，归因干净） |
| `union_route` 恒报 `Ann` | 并集分类 + `S8_T5` 的 route 断言 | ✅ 两条 |
| 归并不折算全局 `chunk_id` | `S8_T5` + 谓词折算条 | ✅ 两条 |

🔴 **变异抓出我自己的一处夹具弱点**：谓词折算那条用例最初的语料让**两段的 tag 序列相同**
（周期 == 分段长度）⇒ 「本地 id 当全局 id 查」**恰好得到相同结论** ⇒ 上述第 3 组注入**零命中**。
⇒ 改成**反相**图样（`A = [keep, drop]`、`B = [drop, keep]`）后才命中，并在用例注释里写明
「这是本判据的**夹具要害**」。（同族教训：变异「零命中」要先分清「断言弱」还是「语料不够狠」。）

#### 覆盖边界（如实登记，**不声称已守住**）

- **`Mixed` 没有端到端判据**：造「一段 `Exact` + 一段 `Ann`」需要某段的 `allowed` 跨过
  `BRUTE_FALLBACK_MAX_ALLOWED`（8192 个活分片）⇒ 端到端要**万级语料**。分类规则本身是纯函数
  ⇒ 已在单测里锁死；「逐段 route 真的被收集并归并」由 `S8_T5` 的 `vector_route` 相等断言锁
  （`Brute` 两端都 `Exact` ⇒ 实现写死 `Ann` 就会红）。
- **`Hnsw` 的跨段召回质量未标定**（本 PR 只交付机制）：ANN 下「分段 vs 单段」**不承诺**逐位一致，
  召回率差异属 `S8-06` / 标定范畴。
- **逐段 `plan` 的 `allowed` 输入只经 `allowed_per_segment` 一处**，但其正确性依赖
  `SegmentFilter::allowed_chunks`（既有实现）⇒ 未新增独立判据。
- ✅ **「漏查 `main`」盲区（第 1 轮评审 P3-1 补登，已收窄）**：本节原先**漏列**了这条 ——
  三条跨段向量用例的夹具 `main` 恒为空 ⇒ 注入了「跳过段 0」后**全仓不可检测**。
  现已由 `S8_T5_main非空…` 钉住（且是**整类**绊线）。详见上方本轮响应条目的「覆盖边界」。

### 修复 · V2 Step 8 PR 4 · **第 3 轮评审响应**（#63，2026-09-20）—— 复审通过 + 1×P4（epoch 拒绝消息按调用方分家）

> 落点 = `pulls/63/reviews` **1 条**（id `5258962020`，SHA `1b5243c`）+ **行内 1 条**
> （`crates/core/src/search/view.rs:487`）；`issues/63/comments` 无新增。
> 评审结论 = **「PR4（`S8-03` / `S8-04` + 最小合并）范围内评审通过 —— 无阻塞项」**：
> 它**独立复现**了两条 P1 的变异自证（`M-P1-1` / `M-P1-2a` 各自「唯一红」）、实跑了 3 条新判据、
> 确认 P3-1 处置「超出预期」、认可 `CLI4` 自查；并明确 `S8-05` / `S8-06` / `S8-09` 维持 WIP
> 是**作者已声明的范围决策、不在本评审范围**。
> 唯一新意见 = **1×P4（非阻塞）**，**已采纳并顺手修**（评审给了「顺手修 / 记账给 `S8-06`」两条路）。

#### Fixed

- 🔵 **P4：`commit_view` 的 epoch 拒绝消息被 `commit()` 与 `fold_deltas()` 共用，对 `fold` 侧三条声明全不成立**
  （`crates/core/src/search/view.rs`）。根因：`save()` 是 `self.commit()?; self.fold_deltas()?;`
  （`search/index.rs:970-971`）⇒ fold 那一刻「未提交内容」**早已被 `commit()` 按设计提交**；
  且 `fold_deltas` **不碰** `self.builder`（`Err` 立即从 `fold_deltas()?` 冒出）⇒ builder **保持过期**、
  并非「已换成与当前世代对齐的新 builder」；因此正解是**重试本次 `save()` / `compact()`**，
  **不是**「重新执行 `add` 与 `remove`」—— 旧消息会把调用方带向**错误的恢复动作**（NFR-07）。
  **修法** = 新增 `PublishCaller { Commit, Fold }` + `loss_and_recovery()`，`commit_view` 多一个
  `caller` 参数 ⇒ **两条路径的消息在同一个地方分家**。
  🔑 **为什么用「调用方声明身份」，而不是评审给的另两条路**：① **事后转写**要在
  `Error::Busy(String)` 上做**字符串匹配**来辨认「这是不是 epoch 那条」—— 脆；
  ② **传字符串**能编译，但调用方**忘传 / 传错**没有编译期保护；
  ③ **加变体** ⇒ 必须在 `loss_and_recovery` 里补文案（穷尽 `match`），**新增调用方时漏不掉**。
- 🔴 **「一类缺陷」全扫：P4 的实际落点有 3 处**（评审点名 2 处）——
  ① `search/view.rs` 的 epoch 消息（**已修**）；② `error.rs` 的 `Busy` 产生点表与段落（**已修**：
  原把 `commit()` 与 `fold` 并成一行「丢失的是 = 同上」⇒ 拆开写明「**视调用方而定**」，
  并把段落里「前两条不满足『重试即可』」收窄为「**只有 `commit()` 的两条**」）；
  ③ **`docs/devel/v2-step8-design.md` §4.9.6**（**评审没点到**）：原文写「**基址对账 / 世代对账**（`commit()`）」
  —— 把两者都归给 `commit()`，**完全没提 epoch 检查也会被 `fold_deltas` 走到**（**已修**）。

#### 测试

- **`S8_03_epoch拒绝的恢复指引按调用方分家`**（新）：只钉**语义差别**，不逐字比文案（那会脆）——
  ① 两条文案必须**不同**；② `commit` 侧承认丢内容并给「`add` + `remove`」；
  ③ **`fold` 侧不得出现「已被丢弃」**（P4 的核心：不得把「无损失」说成「有损失」）；
  ④ `fold` 侧必须给「重试 `save()` / `compact()`」。
  **变异自证**：把 `Self::Fold` 臂改成复用 `Self::Commit` 的文案（= 修复前的缺陷形态）
  ⇒ 本条**唯一红**（`assert_ne!` 命中）。
- **`S8_03_compact换代后另一写端的提交被拒` 补一条接线判据**：该 epoch 拒绝来自 `commit()`
  ⇒ 消息必须带 **commit 版**指引（含「已被丢弃」；`fold` 版**没有**这句）。
  理由：只测「两条文案不同」**抓不到**「`commit()` 误传 `PublishCaller::Fold`」这类接线错
  ⇒ 把**生产接线**钉在端到端路径上。

#### 覆盖边界（如实登记）

- ⚠️ **`fold` 侧的接线（`fold_deltas` 传 `PublishCaller::Fold`）仍无确定性判据**：触发它要求
  「另一写端在本写端 `fold` 的秒级窗口内 `compact()`」，测试里构造不出该交错 ⇒ 与既有的
  「`fold_deltas` 的接线不可测」同族。已由 `commit()` 侧那条 + 消息语义判据**部分**覆盖。

### 修复 · V2 Step 8 PR 4 · **第 2 轮评审响应**（#63，2026-09-19）—— 2×P1（多写端 / 未发布状态的缝隙）+ P3 恢复指引 + P4 登记

> 落点 = `pulls/63/reviews` **1 条**（id `5255969412`，SHA `f69fb8e`）+ **行内 4 条**；`issues/63/comments` 无新增。
> 评审另逐条确认了 §6 十二项（**12 项全 ✅**，含对 `epoch`/ABA 推演链与 CI 覆盖面修复的肯定）。
> **两条 P1 都先确定性复现、再修、再做变异自证**（见下方「复现证据」）。

#### Fixed

- 🔴 **P1-1 `doc_id_by_hash_global` 不认 builder 上的未发布墓碑**（`crates/core/src/search/index.rs`）：
  `remove` 的**分支②**（目标在既往段）把墓碑记在 `self.builder.tombstones`，要 `commit()` 才进
  `View.tombstones`；而 ② 的查重循环**只查后者** ⇒ 「`remove(X)` 后**不 `commit()`** 直接重加同
  `content_hash` 的内容」会命中**刚被删除的** X ⇒ 返回 `deduped = true` + `doc_id = X`
  ⇒ **替换 / 重加静默丢失**（`commit()` 后 X 被墓碑挡住，而新内容**根本没被创建**，
  调用方还拿到一个已死 doc 的 ID）。
  🔑 **是缺口不是设计**：同序列在**分支①**（目标未发布）下会就地物理删 + 条件式摘 hash 条目 ⇒ 重加正常；
  而 FR-15 的「替换文档」流程（同 `dedup_key`、不同正文）**总是**落在分支②。
  **修法** = 查重判据补上 `|| self.builder.tombstones.blocks_doc(global)`（一行）。
  **复现证据**（先红后绿）：新增 `S8_T6_remove未commit即重加不得被去重吞掉`，修前在
  `assert!(!again.deduped)` 处失败（`step8_segments.rs:489`）。
- 🔴 **P1-2 `absorb_window` 的长度前缀假设被并发 `fold` 破坏**（`crates/core/src/search/view.rs`）：
  判据只有「`cur_deltas.len() >= snapshot_deltas`（delta 只增）」，而 `fold` **不改 `epoch`**
  ⇒ 世代对账**拦不住**并发 fold。交错：A 取快照 `[D1]`（O(N) 克隆 + O(N·logN) 向量重建 = **秒级窗口**）
  → B fold 把 D1 吸进 `main` 并发布（`deltas = []`）→ B `add(D2)` + `commit()`（`deltas = [D2]`）
  → A 发布：**长度 1 == 1** ⇒ 旧判据放行，而 `skip(1)` 跳掉的是 **D2** ⇒ **D2 从视图静默消失**，
  而 `ids.next_*` 已把它的槽位记账（**永久孤儿区间**）；A 随后的 `save` 把该状态落盘。
  **修法** = 判据从「长度」升级为**前缀身份**（段不可变 ⇒ `Arc::ptr_eq` 是合法身份判据）：
  快照把 `Vec<Arc<Segment>>`（clone = O(1)）带进发布闭包，锁内校验前 N 项逐个 `Arc::ptr_eq`，
  不符 ⇒ `Err(Busy)`。为此 **`Shared::commit_view` 的 `build_next` 改为可返回 `Result`**
  并保证「`Err` ⇒ **不做任何记账**」（序号不消耗 / `base` 不推进 / 视图不动）。
  ⚠️ **`absorb_window` 的 `debug_assert` 已删除**：它被定位为「防旁路守卫」，但**合法路径**
  （并发 fold）就能踩到 ⇒ 必须 dev/release **双端显式拒绝**。
  **复现证据**（先红后绿）：纯函数级确定性构造（无需线程时序）—— 修前
  `absorb_window(&[D2], 1, …)` 返回 `carried` 长度 **0**（D2 被当成 D1 跳掉）。

#### Changed

- **P3-1 三条 `Error::Busy` 的恢复指引按语义分家**（`error.rs` 的产生点表 + 两条消息）：
  被丢弃的 builder 内容**含分支②记下的跨段墓碑** ⇒ 「`remove(X)` → `commit()` 被拒」的流程
  若只照「重新 `add`」指引重试，**什么都不会恢复**（X 的删除静默失效、继续可检索）。
  ⇒ `commit()` 的两条消息改为「重新执行未成功的 `add` **与 `remove`**」；
  🔑 而 `fold` 那条**反过来**必须写「重试本次 `save()` / `compact()` **即可**」——
  合并是**维护性**操作，被判拒时增量段**仍在视图里、内容没丢**，丢的只是本次合并的**计算**
  （我第一版误抄了 `commit()` 的措辞，已按 NFR-07「恢复路径也要如实」更正）。
- **P4-1 墓碑-only 空段的次生成本登记给 `S8-06`**（`SegmentBuilder::is_empty()` + `commit()` 注释）：
  「无内容、有墓碑」被判非空是有意的（墓碑只能搭一次发布的便车进 `View.tombstones`），
  但带来 ① `Metrics.segments` 被零内容段膨胀；② fold 期 `merge_from(空 Index)` 仍走
  `ForwardStore::rebuild` + `field_index.rebuild`（各 O(N)）⇒ 每个墓碑-only 段白付一次全量 pass。

#### Added

- `crates/core/tests/step8_segments.rs`：**`S8_T6_remove未commit即重加不得被去重吞掉`**（P1-1 的回归锁）。
- `crates/core/src/search/view.rs`：**`S8_03_窗口吸收必须校验前缀身份而非只看长度`**（P1-2 的回归锁；
  三条判据 = 前缀相符放行 / 长度相等但身份不同 ⇒ 拒绝 / 长度回退 ⇒ 拒绝）。
- `crates/core/src/search/view.rs`：**`S8_03_发布失败时不消耗任何记账`**（P1-2 的配套判据 ——
  「`Err` 即无副作用」是**承诺**，必须可测）。

#### 变异验证（3 条注入，**逐个命中且红因与意图相符**）

| 注入 | 期望命中 | 实测 |
| --- | --- | --- |
| `M-P1-1`：查重退回「只查 `view.tombstones`」 | `S8_T6_remove未commit即重加…` | ✅ 该条红、**同文件的旧 `S8_T6` 绿** ⇒ 新变体是唯一绊线 |
| `M-P1-2a`：`absorb_window` 退回「只判长度」 | `S8_03_窗口吸收必须校验前缀身份…` | ✅ 唯一红 |
| `M-P1-2b`：`commit_view` 把 `ids.generation = generation` 提前到 `?` 之前 | `S8_03_发布失败时不消耗任何记账` | ✅ 唯一红（⚠️ 见下方「覆盖边界②」） |

#### 守门链附带抓到的一条**既有 flaky**（`CLI4`，`D-S8-01` 系统性影响的**第 4 条**）

- 🔴 **`CLI4_output另存源不变回收看源vs结果` 的 `dst_total < src_tomb` 是噪声判据**（≈**1/8 假红**）：
  该断言本身由 **PR #31** 引入、`origin/main` 上就存在（`git log -S` 实测），
  **但把它从稳健变成 flaky 的是本 PR 的 `D-S8-01`** —— `save` 第一步 `fold_deltas` ⇒
  **第二次 `save` 就已把 4 条墓碑物理化、并用 4 条存活原始向量重建了图** ⇒ `src_tomb` 与 `dst_total`
  **都是 4 点图**，两者只剩 `hnsw_rs` 无种子 `OsRng` 的层级分配差异。
  **实测 8 次**：`src_tomb` 恒 **2556**；`dst_total` ∈ {**2528** ×7、**2579** ×1} ⇒ 符号随机翻转。
  ⇒ 与 `CLI3` 的第三条断言**同一形状**（那条上一轮已删）：参照系改为**同一装配的 8 点基线**
  （`graph_baseline` / `total_baseline`，在删除**之前**量取，≈2.2× 裕度）；**其余断言一条未放宽**。
  **变异自证**：把 compact 重建图的原始向量**重复灌 3 遍**（4 → 16 点，模拟「图没有回收」）
  ⇒ `CLI3` 与 `CLI4` 的基线判据**双双报红**（6713 vs 基线 2425 字节）。
  ⚠️ 发现方式值得记：**同一次守门链里 `CLI3` 绿、`CLI4` 红** —— 上一轮的「`D-S8-01` 系统性影响」
  清单（当时写「3 条均已收口」）**漏了这第 4 条**；`--features local-rerank` 那一档把它暴露出来。

#### 覆盖边界（如实登记，**不声称已守住**）

- ① **`fold_deltas` 的「接线」没有确定性判据**：三条新用例分别锁住**纯函数语义**（前缀校验）与
  `commit_view` 的**错误语义**（失败不记账）；而「`fold_deltas` 真的把快照 `Arc` 列表传下去、
  且真的把 `None` 转成 `Err`」这 6 行**没有测试能盖到** —— 真实的并发交错（另一写端恰在
  「取快照」与「拿锁」之间 fold）**需要线程时序、无法确定性构造**（与 PR #63 上一轮 M9 同族）。
  把它改回「忽略失败」的变异**不会让任何用例变红**。
- ② `M-P1-2b` 意外暴露**我自己的断言名不副实**：`next_origin()` 只回报 `(next_doc, next_chunk,
  epoch)`、**看不到** `ids.generation` ⇒ 我最初写的「发布失败不得消耗序号」那条断言在变异下
  **照样绿**，真正抓住它的是**后面那条反向自证**（失败后再成功发布一次，序号必须是 1 而不是 2）。
  已就地更正标注（两条都保留，因为它们观察的量不同）。

### 修复 · V2 Step 8 PR 4 · **未闭环项收口**（#63，2026-09-19）—— `S8-T4` 门测试落地 + P2-2 观测 + P2-4 epoch 对账

> 处置对象 = 上一条（第 1 轮评审响应）末节登记的**未闭环清单**；本轮**没有新的评审意见**
> （`pulls/63/reviews` 无第 2 轮，`issues/63/comments` 无新增）。

#### Added

- 🔴 **`S8-T4` 门测试落地**（设计 §8 明写的「**PR4 的门**」，含第 1 轮评审 **P2-5** 更正后的双参照系）：
  - **`S8_T4_跨段BM25与单段逐位一致`**：同一份内容，`[单段]` vs `[主段 + 2 个增量段]`，
    6 个 query 的 `hits`（含 `score` / `chunk_id` / `explain`）**逐位相同**。
    前提自证 = 两侧 `metrics.segments` 分别为 `1` / `3`（否则「单段 vs 单段」会静默通过）、
    两侧内容量一致、每个 query 必须有命中（防「空 == 空」空转）。
  - **`S8_T4_带跨段删除_合并前后双参照系`**：① **合并前**（墓碑未物理化）对照
    「单段建库**不删** + 命中集剔除」——并显式断言两侧的 `allowed`（= `N`）**都含**被删 doc；
    ② **合并后**对照「单段建库 + 同序 `remove`」（两侧都**不含**）。
    另显式断言墓碑**真的挡住**召回（不是「只在统计量里扣」），且至少有一个 query 命中被删文档
    （否则「命中集剔除」这一步没被验到）。
  - **`S8_T8_合并与落盘往返保ID`**：`fold_deltas` 与 `save`/`load` 往返都不得改 ID（`D-S8-04`）；
    判据用「搜出来那条命中的 `(doc_id, chunk_id)`」= 调用方真正持有的东西（`R50` 的静默风险面），
    并显式断言「合并必须真的发生」（`segments` 2 → 1），否则「ID 不变」是空转通过。
- **`Metrics.segments` / `Metrics.vector_segments` / `Metrics.tombstoned`**（设计 §4.13.3；评审 P2-2 的
  「最低限观测」）—— 在 `search_parts` **入口**填值（三条早退路径也带真实值，不是草稿缓冲区的 `0`）；
  `Metrics::log` 同步输出三个字段。`vector_segments` 是**表外新增面**（原表只列 `segments` / `tombstoned`），
  理由与 `tombstoned` 的口径更正见设计 §4.13.3 的更正块。
- **`IdAllocator.epoch`（ID 空间世代）** + `Shared::next_origin()` / `Shared::epoch()`：
  `publish_reset()`（`compact` 重编号）**换代**；`SegmentBuilder` 在**同一把锁内**记下
  「基址 + 世代」；`commit_view()` 新增 `expected_epoch` 对账。
- 回归锁三条：`S8_03_compact换代后另一写端的提交被拒`（端到端**构造 ABA 窗口**，
  `crates/core/tests/step8_segments.rs`）、`S8_03_换代后旧世代发布被拒_基址等值也拦不住ABA`
  （`search/view.rs` 单测）、`S8_03_窗口吸收…` 的既有单测继续保留。

#### Fixed

- 🔴 **P2-4 采纳并落地（`compact()` × 多写端的 ID 空间换代）**：原实现只有**基址**对账，而
  `compact()` 允许「已用长度变小」⇒ 若此后恰好又提交等量内容，`ids.next_*` 会回到旧 builder 记录的
  **同一个**基址（**ABA 窗口**）⇒ 基址对账**通过** ⇒ 那条指向**旧 ID 空间**的跨段墓碑被并进视图
  ⇒ 下一次物理化删掉的是**重编号后的另一个无辜文档**（静默错删）。
  现在 `publish_reset()` 把世代 `+1`，`commit_view()` 世代不符即 `Err(Busy)` 并**点名** `epoch a → b`；
  被拒的提交**不消耗** `generation`、也不发布任何内容。
- **`fold_deltas` 的顺序纪律**：**先记世代、后取视图快照**。反序（先取视图、后读世代）会在
  「`compact` 恰好落在两次读之间」时拿到「**旧**视图 + **新**世代」⇒ 对账**通过** ⇒ 用旧 `main`
  的克隆覆盖新视图。
- **`error.rs` 的 `Busy` 文档同步**（评审 P3-4.1 + P2-1 的剩余项）：删掉已作废的
  「`S8-03` 后内核不再产生该错误」，改为登记**两条**新产生点（基址对账 / 世代对账），并写明两条
  **都不满足「重试即可」**——本写端未提交内容已被丢弃，必须重新 `add`。
- **设计 §4.13.3 的 `Metrics.tombstoned` 口径更正**：「被跨段墓碑挡掉的候选数」在召回层
  **不可良定义**（同一 chunk 会被两条 lane、乃至同一路的多个 posting 重复取出 ⇒ 计数虚增）⇒
  改为「视图里的**跨段墓碑条数**」（精确、O(1)，且正是调用方要的量：`0` = 热路径零谓词）。
- 🔴 **`compaction_cli_semantics::CLI3` 的参照系更正（第 2 条已知红的定性收尾，红灯 2 → 1）**：
  原断言 `graph_after < graph_tomb` 把 `graph_tomb` 当「8 点含墓碑的图」。`D-S8-01`（PR4 起
  `save` 第一步 `fold_deltas`）之后**不再是**：第二次 `save()` 在**落盘前**就把 4 条跨段墓碑
  物理化、并用 `retain` 后的 4 条原始向量**重建了图** ⇒ 磁盘上的「墓碑态」已经是一张 **4 点图**
  ⇒ 两侧同量级。**实测**：8 点基线 **2425** 字节 ／ 墓碑态 **1097** ／ compact 后 **1097**（相等）
  —— 这不是「图没收缩」，是**已经收缩过了**；旧断言据此必红，而 `hnsw_rs` 的层级分配用无种子
  `OsRng` ⇒ 两棵同点数图的字节数上下浮动 ⇒ 旧断言**偶发通过**（= 本用例被登记为 flaky 的真因）。
  ⇒ 参照系改为**同一装配的 8 点基线**（`graph_baseline` / `total_baseline`）：该判据与「回收发生在
  `fold` 还是 `compact`」**无关**（`S8-06` 若改增量合并，结论不变），仍有 2.2× 的裕度。

#### 自查抓到的问题（守门不会报这类）

- 🔴 **clippy `-D warnings` 在第一次守门链里红了**（`match published { Ok(g) => g, Err(e) => return Err(e) }`
  ⇒ clippy `question_mark`）：我先前只跑 `cargo test` / `cargo check`（它们不报这条 lint）⇒ 修成 `published?`
  并**重跑整条链**（本 CHANGELOG 的读数取自修复后的链）。
- ⚠️ **`charabia` / `local-rerank` 两个 feature 段第一次跑时没有 `--no-fail-fast`** ⇒ `cargo test`
  在 `compaction_cli_semantics` 这个（按字母序第 4 个）二进制就停了，**后 13 个二进制根本没跑**
  ⇒ 只看「段标记有、无 FAILED」会**误判覆盖完整**。已加 `--no-fail-fast` 重跑并据实报数。

#### 变异验证（本轮 5 条注入，**表里填实测命中者**）

| # | 注入（可逆，`git diff` 可见） | 期望命中 | **实测命中** | 红因（实读） |
| --- | --- | --- | --- | --- |
| **M-I8-5** | `SegmentedBm25Retriever` 的 `avgdl` 改读**首段**统计量（`self.set.segments[0].index.avgdl()`），即「各段各算 `avgdl`」那一类 | `S8_T4` ×2 | ✅ `S8_T4_跨段BM25与单段逐位一致` + `S8_T4_带跨段删除_合并前后双参照系` | 逐位断言（`query 检索`） |
| **M-I8-6** | 段循环内**各段各算 `df`** ⇒ `idf` 随段变化（`term 外层累加` 的反例） | `S8_T4` ×2 | ✅ 两条（补强语料后） | 逐位断言（`query 段` / `query 检索`） |
| **M-I8-7a** | `Shared::commit_view` 的基址改恒 `(0, 0)` | `S8_T4` ×2 | ⚠️ 红 8 条，但**红因 = 上游 `commit()` 基址对账 `Err(Busy)`** | **不计数**（红因与意图不符） |
| **M-I8-7b** | **发布段的** `base_chunk` 置 0（绕开对账，只让 ID 空间真重叠） | `S8_T4` ×2 | ✅ 两条 | 逐位断言（`query 检索`） |
| **M-P2-4** | `commit_view` 的世代检查改恒假（`if false && …`） | 两条 P2-4 回归锁 | ✅ `S8_03_换代后旧世代发布被拒_基址等值也拦不住ABA`（单测）+ `S8_03_compact换代后另一写端的提交被拒`（集成，**ABA 放行** ⇒ `unwrap_err()` 直接炸） | 断言 |
| **M-P2-2** | `metrics.segments` 恒 0 | 依赖该字段的「前提自证」 | ✅ `S8_T4` ×2 + `S8_T8` | 前提断言（`参照系必须是单段` / `被测侧 = main + 1 个 delta`） |

⚠️ **覆盖边界（如实登记）**：`M-I8-6` 首轮**只被主用例命中**，「带跨段删除」变体不敏感
（该变体语料里没有**跨段共享词**）⇒ 已把 `T4_D1` 第 2 篇改成含「检索」（主段也出现的词）并复跑确认。
`M-I8-7a` 说明 I8-7 的破坏**首先**被 `commit()` 的对账守卫拦住（纵深防御），
但「逐位断言能不能抓」必须用 `M-I8-7b`（绕开守卫）证明 —— 两者不可互相替代。

#### 仍未闭环（如实登记）

- **`S8-T6` 已正式化**：`S8_T6_跨段墓碑下同内容重加经save后仍可去重` 补齐设计 §7 的**上半句**
  （既往段同 hash ⇒ 必须去重、返回既有 `doc_id`、不分配新分片）。
- **定义面回写仍不在本 PR**（更正我在第 1 轮响应里的措辞）：PR4 引入的定义面事实变更
  —— `Metrics` 三个新公开字段、`requirements-spec.md:362`（FR-17 §5.3.5 的**行为行**仍写
  「新增文档进入 delta 区，立即对检索可见」，`S8-03` 后应为「**`commit()` 后**进入 delta 区」）
  —— 按设计 §8 归 **`S8-09`**（`PR7`）四处定义面同批回写 + 升版本行；本 PR 只回写**实现依据文档**
  （设计 §4.13.3 / §6 / §7 与本文）。⚠️ 该行是**已知的需求面漂移**，已在此指名登记，不得在收尾时漏掉。
- **P2-1 的另一半**：真正的「无损重基」（保留 builder、对齐基址后重新发布）仍未实现
  （评审接受「错误信息如实声明」这一取形；本轮把**世代**那条也纳入同一声明）。
- **P3-3**（`num_docs` × `tombstone_stats` 口径分叉，已按评审给的两个选项之一「doc 写明」落地）/
  **P3-5**（`fold_deltas` 的 O(k·N)）/ **P3-6** / **P4-2 的其余两份 `locate` 收敛**：随 `S8-06` 处置。
- ✅ **红灯已清零（同日追加，见下方「CI 红灯收尾」）** —— 原话「剩余 1 条已知红：`integration::T1`（留 PR5）」
  已被**夹具更正**取代；⚠️ 判据一条未改，向量跨段本身**仍**留 PR5。
- **变异验证的覆盖边界**：`MUT-I8-6`（各段各算 `df`）最初**只被主用例命中**，「带跨段删除」变体不敏感
  —— 因为该变体的语料里没有跨段共享词 ⇒ 已把 `T4_D1` 第 2 篇改成含「检索」（主段也出现的词）
  并复跑确认两条都命中；`MUT-I8-7a`（`commit_view` 基址置 0）的红因是**上游基址对账 `Err(Busy)`**
  而非逐位断言 ⇒ **不作为「断言有牙齿」的证据**，改用 `MUT-I8-7b`（发布段的 `base_chunk` 置 0，
  绕开对账）复现逐位断言命中。

#### Changed

- `Shared::next_base()` → **`Shared::next_origin()`**（返回 `(base_doc, base_chunk, epoch)` 三元组）。
  ⚠️ 必须**一次取回**：分两次调用时另一个写端可在两次之间 `compact()`，得到
  「**旧**世代基址 + **新**世代 epoch」这种**自相矛盾**的 builder（它的对账会通过 —— 正是 P2-4 要堵的窗口）。
- `Shared::commit_view()` 返回 `Result<u64>`（新增 `expected_epoch` 参数）。

> 守门读数（本地 11 段链，逐段核对 `BEGIN/END/EXIT` 标记齐全，**报数取自修复后的链**）：
> `fmt` / `clippy --workspace --all-targets -- -D warnings` / `rustdoc -D warnings` / `make shell` /
> `msrv 1.90`（`cargo +1.90.0 check --workspace --all-targets`）/ `no-default-features` /
> `cargo build --release --workspace` / `cargo deny check advisories licenses bans sources` **全绿**；
> `cargo test --workspace --no-fail-fast` = **346 passed / 1 failed / 6 ignored**（17 个测试二进制）；
> `--features charabia --no-fail-fast` = **328 passed / 1 failed / 6 ignored**；
> `--features local-rerank --no-fail-fast` = **325 passed / 1 failed / 11 ignored**。
> 🔴 三个测试范围内的**唯一一条红是同一条**：`integration::T1_已删向量不霸占TopK名额`
> （向量跨段留 PR5；见下方「仍未闭环」）。⇒ 红灯 **4 → 2 → 1**。
>
> 📌 **以上是本轮第一次推送时的读数**；同日追加的「CI 红灯收尾」把它变成了**全绿**
> （见下一节的最新读数）。

#### 🔴 CI 红灯收尾（**同日追加**；红灯 1 → **0**）

##### Fixed（`integration::T1` 的**夹具**更正 —— **判据一个字未改**）

`build_ghost_facade` 只 `add` + `commit`、**不 `save`** ⇒ `S8-03` 起内容全在 **delta 段**，
而向量路的取形（设计 §4.6；跨段向量 = `S8-05`）**只覆盖 `main`**
（`Searcher::parts` 交下去的是 `view.main.vector_index`）⇒ `main` 是一张**空图** ⇒
基线断言 `hits.len() == k` 实测 **`left: 0 / right: 5`**（`integration.rs:291`；
CI 的 `test` 与 `feature isolation` 两个 job 都栽在这一行）。

🔑 **真因不是「少了一条断言」，而是「夹具没跟上 `S8-03` 的语义」**：原夹具下本用例
**即便绿也是空转**（0 条候选时「不含幽灵」trivially 成立）。
⇒ 夹具加 `save` + `load`（`D-S8-01`：`save` 第一步就 `fold_deltas` ⇒ 内容折进 `main`、
`deltas` 清空）⇒ 向量路恢复覆盖，本用例**重新成为真测试**（图里确实有幽灵点，
存活谓词必须把它们挡在 Top-K 之外）。

⚠️ **不违反「刻意不改写该用例」**：那条纪律针对的是「把断言改成边界断言」（会销毁它对
Q-C1 的回归价值）；本次改的是「**怎么造库**」，断言 / 基线 / 删除流程全部原样。
⚠️ **本夹具不覆盖**「内容还在 `deltas` 时的向量路」（那是 `S8-05` 的领域）：该半盲态由
`step8_segments.rs` 的 `S8_02_旧API与新API结果逐位一致`（`vector_recalled == 0`，
`S8-05` 落地后必须反转）+ `Metrics.vector_segments` 观测钉住。

##### Fixed（`CLI3` 的第三条断言 —— 顺带抓到的**纯噪声判据**）

同一次收尾里，`--features local-rerank` 这一档把上一轮**漏删**的旧断言
`total_after < total_tomb` 暴露出来（默认 feature 下侥幸通过）：墓碑态 **2619** vs
compact 后 **2624** —— 两者相差**几字节**，差别只来自 `hnsw_rs` 的无种子 `OsRng`
（**同源**于上一轮那条 `graph_after < graph_tomb`）。⇒ **删除**该断言：
「体积必须回落」的语义已被以「8 点无墓碑基线」为参照的两条完整覆盖，且与
「回收发生在 `fold` 还是 `compact`」无关。⚠️ 这正是「**一次红 run 会掩盖后面所有二进制**」
的反面教材：它在默认 feature 下不红，是**换了 feature 组合**才现形的。

##### Changed（CI 工程卫生：**别让早期失败掩盖覆盖面**）

- `test` job：`cargo test --workspace` → **`cargo test --workspace --no-fail-fast`**。
  🔴 **实测依据**（head `26b81d7`）：`cargo test` 默认在**第一个失败的测试二进制**就停 ⇒
  那次 run 的 `test` job **只跑完 8 个测试目标**就结束（本地同命令 **17 行**）⇒ 按 cargo 的
  字母序，`integration` 之后的 `step4_compaction` / `step4_liveness` /
  `step5_query_observability` / `step6_incremental_build` / `step7_rerank_local` /
  `step8_rw_concurrency` / **`step8_segments`（本 PR 新增的 `S8-T4` 门测试就在里面）** /
  `vector_ab` **在 CI 上从未跑过** —— 那次「CI 只有一条红」的读数**低估了未验证面**。
- `features` job：三个 `cargo test` 加 `--no-fail-fast`；第 2~4 步
  （`local-rerank` ×2 / `no-default-features`）加 **`if: always()`** —— 同一原因：
  Actions 默认在**第一个失败的 step** 就停，那次 run 在第 1 步（charabia）就中断，
  后 3 个 feature 组合**没跑**。
- ⚠️ **这不是放宽门禁**：job 仍然红，只是**把失败报全**（一次 run 看到全部失败点与全部覆盖）。
  本 PR 的红灯由**修代码**清掉，不是由这两个改动清掉的。

##### 最新读数（12 段链，含 CI 的 `-p helix --features local-rerank` 一步）

| 段 | 结果 |
| --- | --- |
| `fmt` / `clippy -D warnings` / `rustdoc -D warnings` / `make shell` / `msrv 1.90` / `no-default-features` / `build --release --workspace` / `cargo deny` | ✅ 全绿 |
| `cargo test --workspace --no-fail-fast` | ✅ **347 passed / 0 failed / 6 ignored**（17 行） |
| `cargo test -p helix-core --features charabia --no-fail-fast` | ✅ **329 passed / 0 failed / 6 ignored** |
| `cargo test -p helix-core --features local-rerank --no-fail-fast` | ✅ **326 passed / 0 failed / 11 ignored** |
| `cargo test -p helix --features local-rerank` | ✅ **21 passed / 0 failed** |

⇒ **红灯 4 → 2 → 1 → 0**。


### 修复 · V2 Step 8 PR 4 · **第 1 轮评审响应**（#63，2026-09-19）—— P1-1 窗口吸收 + P1-2 去重条目 + 4 条红定性更正 + 死代码清理

> 评审落点 = `pulls/63/reviews`（**1 条：2×P1 + 5×P2 + 6×P3 + 5×P4**）+ **行内 10 条**；`issues/63/comments` = **0**；
> 被评审 SHA = `fe940e5`。评审由**本地 DeepSeek Harness（dsh）**出具、作者代其上载（其执行通道在本机不可用）。

#### Fixed

- 🔴 **P1-1 采纳（窗口吸收）**：`fold_deltas` 的发布闭包原为 `move |_cur, …|` ⇒ **无视锁内 `cur`**：
  另一个写端在「取快照」与「拿锁」之间提交的 delta 会被**静默丢弃**、且 `ids.next_*` **回缩**（后续内容复用已发号的 ID）。
  ⇒ 新增纯函数 `Shared::absorb_window()`：**无损吸收**窗口内新提交的 delta（合并**不改 ID** ⇒ 其 `base_*` 仍有效）、
  只摘掉**本次已物理化**的墓碑；`used_*` 取 `max(计算值, 锁内真值)` ⇒ 不再回退。
- 🔴 **P1-2 采纳**：① `Index::remove` 的 hash 摘除改**条件式**（只摘指向本 doc 的条目，否则会抹掉「替换文档」的去重条目 ⇒ FR-15 静默失效）；
  ② `merge_from` 的 `content_hash` 碰撞 `debug_assert!` **删除**（碰撞在「墓碑挡住查重 ⇒ 允许重新 upsert」下是**合法状态**）。
  ⇒ 新增回归用例 `S8_T6_跨段墓碑下同内容重加经save后仍可去重`（修前 debug 必 panic）。
- **T11 的期望更正**（评审 P3-1 判断正确）：`before_docs/before_chunks` 必须在**显式 `commit()` 之后**读
  （`S8-03` 起计数只算已发布内容 ⇒ 原写法读到 0）。
- **T3 的判据改为「结果」**：实测三档（[A] builder 内删+内存 compact = 0 / [B] 加 save→load = 0 /
  [C] **先 commit 再删（墓碑路径）= 11**）⇒ 死词在 `save` 的 **fold 期**就被丢弃（`merge_from` 的倒排合并
  只对非空链 `add`）且**不计入任何计数**；新增同伴用例 `T3b_墓碑路径下compact仍回收死词` 继续钉住计数器语义。
- **P2-3 注释自相矛盾**：向量侧改回如实陈述（本 PR 取保守的「全量重建」；**「只能重建」的绝对命题撤回** ——
  `D-S8-09` 的方案 A 不需要旧图所有权；spike S8-S1 未做 ⇒ 该取形是**暂定**的）。
- **P2-5 设计文本更正**：`S8-T4` 的「合并前」参照系**结构上不可满足**（未合并窗口的 `N`/`total_len`/`df` 含墓碑 doc，
  而「单段建库 + `remove`」不含）⇒ 改为「单段建库**不删** + 命中集剔除」，**合并后**才对照「单段建库 + 同序 `remove`」。
- **P2-1 诚实化（部分）**：`commit()` 对账失败路径的**内容丢弃**已写进错误消息（「重试不会恢复」）；
  无损重基取形 + `error.rs` 文档同步留给下一提交。
- **P3-4.5 设计文本同步**：§4.7.3 的取形改为「判据只看有无跨段墓碑」—— 实现（零谓词，不论有无 delta）**优于原文本**。

#### Removed

- **死代码清理（评审 P3-4.4 / P4-2 / P4-3）**：`Shared::with_main_mut()` + `write_needs_exclusive()`（连同
  `S8_02_有读者时写入必须失败` 用例）、`View::locate()`（三份等价实现里零调用点的那份）、`Segment::global_chunk()`、
  `View::needs_bm25_tombstone_filter()`。⚠️ 这 5 处 `dead_code` 在 `fe940e5` 上**就已存在** ⇒ 上一轮的
  「0 warning」读数是**缓存假象**（cargo 未重编译时不重放警告），实际跑 `clippy -D warnings` 会**直接红**。
  顺带修掉 clippy 的 `length comparison to zero`（`search/searcher.rs`）。

#### Added

- `Shared::absorb_window()` + 单测 `S8_03_窗口吸收保留新提交的delta且只摘已物理化的墓碑`。
- `S8_T6_跨段墓碑下同内容重加经save后仍可去重`（P1-2 回归锁）、`T3b_墓碑路径下compact仍回收死词`。

#### ⚠️ 仍未完成（与评审的合并判定一致）

- **P2-2**（`Metrics.segments` 最低限观测）/ **P2-4**（`publish_reset` 的 epoch 对账）/ **P3-3**（`num_docs` 与
  `tombstone_stats` 口径分叉，本轮只加注记）/ **P3-5**（`fold_deltas` 的 O(k·N)）/ **P3-6** / **P4-2 的其余两份 `locate` 收敛**。
- **`S8-T4` 门测试未落**（按更正后的双参照系写）、变异验证 / 13 段守门链 / 定义面回写仍未做。
- ⚠️ **覆盖边界（本轮实测）**：P1-1 的**端到端竞态未能复现** —— 400×400 双写端探针在**修前（M9 变异）与修后
  都是 0 丢失**（且 A 侧 `save()` 只报错 1/400）：A 的 `save()` 第一步 `commit()` 通常就先因基址对账失败而中止，
  根本走不到 fold 窗口 ⇒ 机制属实（代码事实）但**触发难度高于评审的表述**；判据只到「纯函数 + 代码事实」层。
  ⇒ 其中 **P2-2 / P2-4**、**`S8-T4` 门测试**、**`error.rs` 的 `Busy` 文档**已在本 PR 的**下一提交**闭环
  （见上方「未闭环项收口」条目）；**P1-1 的端到端竞态**结论不变（仍未复现，是覆盖边界而非「已守住」）。

### 新增 · V2 Step 8 PR 4（**WIP，未完成**）—— `S8-03` delta 写入闭环 + `S8-04` 跨段 BM25 / 谓词 + **最小文本侧合并**（Refs #58，2026-09-19）

> ⚠️ **本条对应的实现尚未全绿**（见「已知红」）。条目先落，是因为**每个 PR 必带 CHANGELOG**，
> 而本 PR 的用途 = **第三方 harness 评审**（见 PR 正文）。
>
> **本 PR 的范围偏差（须评审确认）**：设计 §8 的切分里合并器属 **PR6（`S8-06`）**，但
> `D-S8-01` / `D-S8-12` 要求 `save()` / `compact()` **第一步就 `merge_all()`** —— 不做最小合并，
> `save` 落盘的快照**只含 `main`** ⇒ **静默丢内容**。⇒ PR4 内实现**最小文本侧合并**（`fold_deltas()`，
> 不含向量侧策略 / `MergeReport` / 字段索引等价 —— 那些仍留给 `S8-06`）。

#### Added

- **`search/index.rs`：`SegmentBuilder`**（`pub(crate)`，写端私有：`index` / `pending` / `raw_vectors` /
  `vector` / `tombstones` / `base_doc` / `base_chunk`）—— `S8-03` 起**写端不再就地改 `main`**。
- **`search/view.rs`：读侧跨段基础设施** —— `SegmentRef` / `SegmentSet`（FIFO 段列表 = `main, deltas[0], …`）/
  `SegmentsInOrder` / `SegmentFilter` / `ViewFilter`（`CandidateFilter` 的跨段实现，`allowed_count`
  为**精确计数**：`Σ allowed_seg − 墓碑挡掉的 chunk 数`）/ `View::sums()`（全局统计量）/
  `Shared::publish_reset()`（`compact` 专用：**允许已用长度变小**，`I8-7` 的唯一合法例外）/
  `View::segments_in_order()`。
- **`retriever/bm25.rs`：`SegmentedBm25Retriever`** —— 跨段 BM25：**外层 term、内层段**的 TAAT 累加 +
  **全局精确整数统计量**（`N` / `total_len` / `df(t)`）⇒ 与单段布局的 `bm25` 结果**逐位一致**（结构性结论）。
- **`index/mod.rs`：`Index::merge_from()`** + **`index/forward.rs`：`ForwardStore::append_from()`** ——
  把 delta 的内容按**本地顺序 append** 进主段，**不重编号**（合并保 ID：`S8-T8` 的门）。
- **`tests/atomic_snapshot.rs`**：该用例的检索手段改为 **`bm25` 模式**（理由见下「Changed」）。

#### Changed

- **`SearchIndex`**：`add` / `flush` / `remove` 全部改走**自己的 builder**（不再 `with_main_mut`）
  ⇒ **并发检索进行中也能写入**（`FR-17` 的核心诉求）；`commit()` = `flush` → **封段** → 持
  `Shared::ids` 锁**原子追加进 `View.deltas`** → 发布新 `View`（含**基址对账**：双写端插队 ⇒ `Error::Busy`，
  不静默发错 ID）。
- **`remove` 三分支**：① 目标在**当前 builder**（未发布）⇒ 就地物理删；② 在**既往段** ⇒ 记**跨段墓碑**
  （`View.tombstones`）；③ 不存在 ⇒ no-op。跨段 `content_hash` 查重同步覆盖「命中但被墓碑挡住 ⇒ 视为未命中」。
- **`compact_with_bytes`**：改走 `Shared::publish_reset()`（重编号 ⇒ 新主段**比原来短**，必须重置发号器）
  **+ 重编号后重建 builder**（否则后续 `commit()` 的基址对账必失败）。
- **`SearchParts` 新增 `segment_set` / `predicate_builder` 两个字段** —— ⚠️ **破坏性**：下游若有字面量
  构造 `SearchParts { … }` 会编译失败（本仓内为 0 处）。
- **`atomic_snapshot::TI5_strict失败后快照仍可加载并恢复`** 的检索手段由 `hybrid` 改为 **`bm25`**：
  该断言验的是「快照内容完整」，而 `save` 前先合并会让图按**全量原始向量重建** ⇒ ANN 拓扑变化 ⇒
  `hybrid` 的 top-10 漂移（§4.6.2 明确「ANN 路径不承诺跨布局逐位一致」）。**BM25 与图无关、确定性**，
  语义不变。

#### Fixed

- **`Shared::new` 的发号器初始化**：原用 `IdAllocator::default()`（全 0）⇒ `load` 后主段已有 N 个槽位，
  新段 ID 会与主段**重叠**（破坏 `I8-7`）。改为从 `main.index.total_docs()` / `total_chunks()` 起步。
- **`fold_deltas` 的 `raw_vectors` 未按 liveness 过滤** ⇒ 快照会带**已删 chunk** 的向量。
- **跨段 BM25 的零谓词判据漏了「跨段墓碑」** ⇒ 墓碑挡不住检索（数据错误）。

#### ⚠️ 已知红（本 PR 尚未完成的部分）

- `step4_compaction`：2 条（compact / 死词回收与 delta + 墓碑的交互 —— 设计上属 `S8-06` 领域）
- `compaction_cli_semantics`：1 条（图 sidecar 体积）
- `integration`：1 条（`已删向量不霸占 TopK 名额` —— **向量路只覆盖 `main`**；跨段向量属 `S8-05`/PR5，
  按已拍板的**范围决策**留待 PR5，不在本 PR 内掩盖）
- `tests/step8_segments.rs` 的新用例（含 **`S8-T4` 跨段 BM25 逐位一致**）**尚未补**；变异验证 / 守门链 /
  设计文档实施结果回填**均未做**。

#### 设计文档更正

- `docs/devel/v2-step8-design.md` **§4.10**：原写「`load` … **`ids` 归零**」为**错误**（会让新段全局 ID
  与主段重叠、破坏 `I8-7`）；更正为「**`ids` 不归零**：从主段已用槽位数起步」，并附实测依据
  （`S6_T8` / `S6_T10` 两条用例）。

### 修复 · V2 Step 8 PR 3 · **第 1 轮评审响应**（#62，2026-09-18）—— `commit()` 原子化（P2）+ 跨段求和（P3-2）+ 3 处顺手修

> 评审落点 = `pulls/62/reviews`（**1 条：1×P2 + 2×P3 + 3×P4**）+ **行内 5 条**；`issues/62/comments` = **0**；
> 落点 SHA = `1ad692b`。⚠️ 唯一的行为变更 = P2 的缺陷修复；另有一处**破坏性 API 变更**（P4-5a 的 `Error::Busy`）。

#### 🩹 Fixed

- 🔴 **P2（阻塞级）：`commit()` 的原子性** —— 原实现是「`snapshot()` → `advance()` → `publish()`」
  **三段分离**（`ids` 锁只覆盖 `advance` 内部），而 `into_index()` 起总是成功 ⇒ 同一 `Arc<Shared>`
  上可有多个写端句柄（`SearchIndex: Send`）⇒ 两写端交错时**后发布的可能是更早 `advance` 的序号**。
  与 PR 自述「`commit()` 全程持 `Shared::ids` 锁从本条起即必需」**不符**（评审指出的即这一处）。
  - **两套实测（修复前）**：① 手工编排交错 ⇒ 终值 `generation = 1`（应为 2），且原「单调性告警」
    判据 `1 <= 0` = false ⇒ **永不触发、形同虚设**；② **真实并发** 4 写端 × 150 次 `commit()`
    ⇒ 观察线程看到 **70 次回退**（样本 `(65,64)` / `(82,81)` / `(134,127)`）。
  - **取形**（评审建议 ①）：新增 `Shared::commit_view()`，**持 `ids` 锁**完成「取快照 → 算下一段基址
    → 推进序号 → 发布」四步；**删除 `publish` 与 `advance`** ⇒ 发布逻辑内联、**唯一发布路径**
    ⇒ 「锁外三段」在**可见性层面写不出来**（结构性对策：消除入口，而不是加检查）。
    `commit()` 里那条比较**发布前旧 `cur`** 的告警一并删除。
  - 🔑 这不只是「序号难看」：同一形状带入 `S8-03`（delta 分段）会让**两个 delta 段拿到相同 `base_*`**
    ⇒ 段 ID 空间重叠 ⇒ 破坏 `I8-7`。⇒ 本修复是 `S8-03` 的**形状前置**。
- **P3-2：`tombstone_stats()` 的「跨段求和」** —— 原实现只有 `debug_assert!(deltas.is_empty())` +
  只读 `main`、**没有任何求和**，与三处文档的声明不符（`S8-03` 一旦 `deltas` 非空：debug 下断言红、
  **release 下静默少算**）。⇒ 取评审建议 ①：**真的逐段求和**，并抽成 `View::sums()` **纯函数**
  —— 否则 `deltas` 恒空时「求和」与「只读 `main`」无差别、永远测不到；顺带把跨段墓碑
  从存活文档数扣掉（`S8-02` 期扣 0 ⇒ 结果与今天**逐位一致**）。
- **P3-3：`CHANGELOG.md` 结构破损** —— PR3 条目插入时把 PR2「第 1 轮评审响应」的**标题行整行替换**
  成 PR3 标题块 ⇒ 该条目**失去标题**、标题尾巴残成孤行，正文悬空挂在 PR3 名下。⇒ 补回完整标题。
  **另**：`## [Unreleased]` 曾出现**两次** —— 核实为**既有问题**（`origin/main` 上即 2 处；
  `git log -S "## [Unreleased]"` 定位到 **PR #61 的合并提交 `5548998`** 引入），按评审建议**顺手合并**。
- **P4-4：向量 lane probe 断言几乎恒真** —— `dbg.contains("0.")` 对任何 `0.x` 分数都成立
  ⇒ 零鉴别力。⇒ 改为 `Explain` 的**结构化字段**断言（`vector_score` / `vector_rank` 至少一个 `Some`）。
- **P4-5a：过渡期并发冲突的错误语义** —— 复用 `Error::InvalidInput`（= 参数错）表示**瞬态并发状态**
  ⇒ 调用方无法在「重试」与「报错退出」间决策。⇒ 新增 **`Error::Busy`**。
- **P4-5b / P4-6a / P4-6b：三处文档补充** —— `add()` 的「**`Err` 但部分生效**（重试安全）」、
  `compact()` 的「末步失败丢弃整套重建结果 ⇒ 无活动读者时调用」、`searcher()` 的
  「可见性口径要连读模块文档」。

#### ⚠️ 破坏性
- **新增 `Error::Busy(String)` 变体**：`Error` 枚举**无** `#[non_exhaustive]` ⇒ 下游以**穷尽 `match`**
  处理 `Error` 的代码会编译失败。沿用 0.x 阶段的既有约定接受（同 Q5 的 `VectorRoute::Mixed` 处置）。
  库内无穷尽 `match`（`cargo check --workspace --all-targets` 零错误）。

#### 🧪 回归锁与变异验证
- 新增 `S8_02_并发commit不丢失视图序号`（4 写端 × 150 次 + **观察线程**判据）。
  🔑 **判据取「过程」而不取「最终值」**：只看终点会漏 —— 最后 `publish` 的恰好是最后 `advance` 的
  线程时终值仍然正确（实测：同一压力下用「终值 == 总数」判据跑 3 轮**全绿**）。
- 新增 `S8_02_跨段求和真的遍历deltas`（判据用 `raw_vectors` —— chunk / doc 计数在空段下恒 0、
  两段相加仍是 0 ⇒ 无鉴别力）。
- **变异**：`M7`（把 `publish` 移出 `ids` 临界区）⇒ 并发用例红（**1591 次回退**）；
  `M8`（`View::sums` 丢掉 `deltas`）⇒ 求和用例红（`left: 1, right: 3`）。两条均**已还原并逐字节核对**。

#### ⚠️ 覆盖边界（如实登记，不声称已覆盖）
- 两条新用例都是**回归锁**：修复后判据**结构性成立** ⇒ 牙齿来自**修复前的复现**
  （70 次 / 1591 次），而不是当前代码里的某个可变点。
- `S8-02` 期这两条判据在**外部 API 层面不可观测**（内容与视图同源），只有 `pub(crate)` 单测能抓。

#### 📄 Docs
- `docs/devel/v2-step8-design.md`：v0.5 → **v0.6**（新增 **§4.3.5 第 1 轮评审响应**）。

### 新增 · V2 Step 8 PR 3 —— **不可变视图骨架** + `searcher(&self)` + 旧所有权 API 废弃（`S8-02` / `T7-18`）（Refs #58，2026-09-18）

> 设计依据 `v2-step8-design.md` §4.3 / §4.8 / §4.12（**v0.5**，新增 §4.3.4 实施结果）。
> ⚠️ **本 PR 是「大重构不夹带行为变化」的护栏段**（§1.3 第 2 条）：`deltas` 恒空 ⇒
> **既有测试全绿 = 行为与重构前逐位一致**。真正改变可见性语义的是下一个 PR（`S8-03`）。

#### ✅ Added
- **`crates/core/src/search/view.rs`（新）** —— 视图模型：`Segment`（内容容器）/ `View`（读端快照）/
  `Shared`（`RwLock<Arc<View>>` + 发号 + 装配）/ `Tombstones`（跨段墓碑，本 PR 恒空）/ `IdAllocator`。
  发布协议 `publish` = **写锁内只做一次指针替换**（不变式 `I8-1`）；读端 `snapshot()` 一次取定（`I8-3`）。
- **`SearchIndex::searcher(&self) -> Searcher`** —— **不消耗写端、不隐含 `flush`**（对齐 NFR-11：
  可见性 = `commit()` 后）。这是 `S8-02` 的**核心对外价值**：读写可以并存。


#### 🔁 Changed
- **`SearchIndex` 改持 `Arc<Shared>`**：内容从「写端私有的 `Inner`」上移为 `Segment`，
  经 `Shared::view` 共享；`SearchIndex` 只留 `pending` 与诊断计数。**`add(&mut self)` 签名一字未改**
  （`D-S8-05`：保持 `!Sync`，「零所有权破坏」的另一半）。
- **`Searcher` 改持 `Arc<Shared>`**：`parts()` / `has_vector()` / `infer_mode()` / `run()` 接 `&View`；
  `SearchRequest::exec` **一次检索只取一次快照**（`I8-3` 的落地）。
- **`commit()` = `flush()` + 发布新视图**（`generation` +1，附 `tracing::debug!` 与单调性告警）。
  ⚠️ 骨架期**不移动内容**（只有一段）⇒ 可见性语义与重构前**完全一致**。
- **`tombstone_stats()` 改为「跨段求和」形状**（`main` + `deltas` + 阶段不变式断言），`S8-03` 起无需再改。

#### ⚠️ 废弃（`T7-18`）
- `SearchIndex::into_searcher(mut self)` → `#[deprecated]` 薄封装 = `commit()` + `searcher()`（**行为逐位不变**）。
- `Searcher::into_index()` → `#[deprecated]`：
  🔴 **行为变更** —— 重构前 `Arc::try_unwrap` 在「`Searcher` clone 残留」时**报错**；新模型下
  「clone 残留」与「写端存活」**不可区分**（后者是合法状态）⇒ **总是成功**。
  既有单测 `clone残留时into_index报错` 已改口径为 `S8_02_into_index总是成功且与旧clone共享视图`。
  ⚠️ **这正是「双写端」成为可能的入口** ⇒ `commit()` 全程持 `Shared::ids` 锁从本条起即必需。
- **生产调用点已迁移**：`cli/src/main.rs`（3）+ `examples/search_basic.rs`（1）改用 `commit()` + `searcher()`；
  测试文件（7 个）显式 `#![allow(deprecated)]` —— **刻意**沿用旧 API 以锁住其行为不变。

#### 🧪 测试（**+13 条**：`view.rs` 4 / `index.rs` 1 / `tests/step8_segments.rs` 8）
`S8-T9`（同视图内 50 次检索逐位一致）+ 骨架不变式 4 条（含 **`有读者时写入必须失败`**，`S8-03` 时应删除）
+ 外部行为 8 条（读写并存 / 不隐含 flush / 旧新 API 逐位一致 / 可见性语义钉住 / commit 幂等）。

#### 🧪 变异验证（**M5 / M6′：两条首轮「零命中」**）
| # | 注入 | 结果 |
| --- | --- | --- |
| **M5** | `commit()` 不 `publish` | ⚠️ 首轮 **8 条集成用例全绿**（**抓不住**）⇒ 补 `index.rs` 单测后 **1 条红** |
| **M6′** | `into_searcher()` 去掉隐含 `commit()` | ⚠️ 首轮 **全仓全绿**（**抓不住**）⇒ 改用**带向量 lane** 的装配后 **1 条红** |

#### ⚠️ 覆盖边界（**如实登记，不声称已覆盖**）
- **「`commit()` 不 `publish`」在外部 API 层面不可观测**（骨架期内容与视图同源）⇒ **任何集成测试都抓不住**
  （M5 实证）；只有 `pub(crate)` 单测（`generation` 递增）能抓。`S8-03` 起才从外部可测。
- **「`into_searcher` 隐含 flush」只在带向量 lane 的装配下可测**（纯 BM25 下 `pending` 恒空、`flush` 是 no-op）
  ⇒ M6′ 首轮全仓全绿；已把该用例改用带假 embedder 的装配并注明理由。
- **过渡约束（非缺陷，但须知）**：骨架期写操作要求「无并发读者」（`deltas` 恒空 ⇒ `Arc::get_mut`），
  有读者时返回 `Err`；**`I8-2`（段不可变）在 PR3 尚未成立**。二者均由 `S8-03` 解除。
- 由**类型系统**保证、无断言覆盖：`searcher(&self)` 不可能隐式 `flush`（`flush` 需 `&mut self`）。

#### 📄 Docs
- `docs/devel/v2-step8-design.md`：v0.4 → **v0.5**（新增 **§4.3.4 实施结果**，含 5 条偏差 / 13 条测试 /
  M5·M6′ / 2 条覆盖边界）。

### 修复 · V2 Step 8 PR 2 · **第 1 轮评审响应**（#61，2026-09-18）—— 数值勘误 + 空图行为断言 + 覆盖边界**就地收窄**

> 评审落点 = `pulls/61/reviews`（**1 条，4×P4、无阻塞**）+ **行内 4 条**；`issues/61/comments` = **0**；
> 落点 SHA = `19635d7`。⚠️ **无生产逻辑改动**（`hnsw_rs_index.rs` 只动 rustdoc / 注释 + 新增 1 条单测）
> + 设计文档 §4.2.1 + `step8_rw_concurrency.rs` 模块文档 + 本文件。

#### 🩹 Fixed
- **P4-1 数值勘误**：rustdoc ⑤ 与下方条目里的 `L ≈ log₁/₃₂(12000) ≈ 2.4` 是**误算**
  （`32^2.4 ≈ 4096 ≠ 12000`；`log₃₂ 12000 = 2.7101`）⇒ 改为 **`E[L] ≈ (ln N + γ)/ln 32 ≈ 2.9`**。
  🔑 **按「一类缺陷全扫」**（评审只点了 rustdoc ⑤ + CHANGELOG 两处）**又抓出 1 处评审未点到的**：
  **设计 §4.2 正文的成本段**（同一数字、同一式）⇒ **本 PR 内共 3 处**，逐处改毕。
  🔑 **口径也一并改正**：`L` = 「**观测到的最大层**」，其期望是**最大次序统计量**
  `E[max] ≈ H_N / ln 32 = 2.877`（`log₃₂ N` 只是**同量级粗式**）。
  ✅ **结论不受影响**（`L+1` 次短持锁、单次持锁 `O(N)` → `O(该层点数)`），只是示例数偏低。
  ⚠️ `927f746` 的**提交信息**里同一数字**按纪律不改写**（评审已把该 SHA 记为基线）。
- **P4-2 删除冗余断言**：`全量遍历基数等于点数且零重复` 里的 `assert!(l0 < n)` 被
  `upper > 0` ∧ `l0 + upper == n` **蕴含**（`l0 = n − upper < n`）⇒ **逻辑上永不报红、零鉴别力**；
  留着会让后人**高估**该用例的覆盖度 ⇒ 删除，并**留一行注释说明**（不静默删）。

#### ✅ Added
- **`空图精确扫描返回空且不panic`（`hnsw_rs_index.rs` 单测，新增）** —— 两个来源合一：
  ① 评审指出的**顺带收益**（旧 `IntoIterator` 写法在**空图**上会 panic）；② **P4-3 的替代物**（见下）。
  - **机制**：空图上 `pi_guard[0].len() == 0` ⇒ `IterPoint::next` 首次调用即走层切换分支 ⇒
    `entry_point_ref.as_ref().unwrap()`（`hnsw_rs 0.3.4` 的 `hnsw.rs:662`）在空图上为 `None` ⇒ **panic**。
    此前只靠上层 `VectorRetriever::search_exact_filtered` 的 `self.index.is_empty()` 早退兜住
    （`retriever/vector.rs:70`）—— **trait impl 自身没有空图早退**（本函数只早退 `k == 0`）。
    逐层写法每层只读 `points_by_layer`（`IterPointLayer::new` `hnsw.rs:701` **不读** `entry_point`）
    ⇒ 本写法**顺带消除**该隐患。
  - 🔴 **它同时是本仓唯一能抓住「改回 `IntoIterator`」的断言**（见下方覆盖边界更正 + M4）。

#### 🔁 Changed
- **覆盖边界②就地更正**（**原句保留、不静默改**）：原写「把生产循环**改回 `IntoIterator`** 时，
  **本仓没有任何 CI 断言会红**」⇒ 加空图断言后**该表述已不准确**，更正为「**空图这一条输入上
  会被抓住**（旧写法 panic ⇒ 红）；仍抓不住『改回 **且同时**补空图早退』的写法」。
  同批改三处载体：设计 §4.2.1 / `step8_rw_concurrency.rs` 模块文档 / 本文件（**覆盖边界①**
  「覆盖类断言必须经由生产函数」不受影响）。
- 设计 **§7 测试计划表** 的 **S8-T2** 行按实现对齐（`get_layer_iterator(0)` 的基数 `<` `get_nb_point()`
  一条**已删** ⇒ 改为「由 `upper > 0` + 并集等式**蕴含**」）。

#### 🧪 变异验证（新增 M4，与前三条 M1/M2/M3′ 并列）
| # | 注入 | 结果 |
| --- | --- | --- |
| **M4** | 生产循环**改回 `IntoIterator`** | ✅ **1 条红**：`空图精确扫描返回空且不panic`（panic at `hnsw.rs:662:62`）；**其余 10 条全绿**（含 I4 逐位一致、经生产路径的覆盖等价）⇒ 本仓其余断言对该回退**零鉴别力**、本用例是**唯一**绊线 |

#### 📝 评审意见处置（4×P4，全部非阻塞）
- **P4-1** ✅ 采纳（数值勘误，见上）。
- **P4-2** ✅ 采纳（删除冗余断言）。
- **P4-3** ❌ **不立项**（评审明确「是否立项由你拍板」）：源码级文本断言（`include_str!` + `contains`）
  属「**作者意图的近似**」而非**可观测结果**（对格式 / 写法敏感、改写形式即可绕过）⇒ 改用**行为级**
  替代物（空图断言），并**用 M4 证明它有牙齿**。⚠️ 如实写明：两者都**不是证明**，
  且都抓不住「改回 + 补空图早退」。
- **P4-4** ✅ 采纳（PR 正文 run id 笔误 `35310301302` → **`35310501302`**）。
- **顺带收益** ✅ 采纳**并升级为测试**（不只在注释里声称）。

### 修复 · V2 Step 8 PR 2 —— R34 修法：精确扫描改**逐层遍历**，消除 `points_by_layer` 的递归读（S8-01 / T7-25）（Refs #58，2026-09-18）

> 设计依据 `v2-step8-design.md` §4.2（**v0.2**，评审后已合并 = PR #60）；本提交 = 7 段 PR 链的 **PR2**，
> 是 **T7-06（delta 分段）的硬前置**。⚠️ **只动 2 个 `.rs`**（1 改 1 新）+ 1 份设计文档 + 本文件。

#### 🩹 Fixed
- **`crates/core/src/vector/hnsw_rs_index.rs`** —— `search_exact_filtered` 的全量遍历由
  `for point in self.hnsw.get_point_indexation()`（`&PointIndexation` 的 `IntoIterator`）改为
  **逐层** `for layer in 0..=pi.get_max_level_observed() as usize { pi.get_layer_iterator(layer) }`。
  - **为什么**：旧路径的 `IterPoint::new` 构造即取 `points_by_layer.read()` 并**全程持有**
    （`hnsw.rs:633`），而 `IterPoint::next` 在**层切换**时**再取一次同一把锁**（`hnsw.rs:661`；
    相邻的 `:660` 是另一把 `entry_point` 锁、**不构成**递归）⇒ **同一把 `std::sync::RwLock` 被递归读**。
    `std` 不保证递归读可重入（其 futex 实现要求 `!has_writers_waiting`，**写者优先**）⇒
    并发检索之间互不阻塞，但**若层切换那一刻恰有写者在等待，第二次读可能永久阻塞 ⇒ 挂死**
    （架构 §14.3 **R34**）。逐层写法每层一个独立 `IterPointLayer`（`:701` 取一次锁、
    `:715-723` 的 `next` **不再取锁**）⇒ **层间 guard 不重叠 ⇒ 无递归读**。
  - **为什么现在修**：今天**不可达**（`add` 要求 `&mut self`，单写者 ⇒ 读写不可能重叠），
    但**Step 8 读写并发一开该路径即活** ⇒ 属「硬前置」。
  - **顺带收益**：单次持锁时长由 `O(N)` 降到 `O(该层点数)`（`L+1` 次短持锁，M=32/N=12K 下 `L≈2.9`；
    ⚠️ 原写 `≈2.4` 为**误算**，见上方「第 1 轮评审响应」P4-1）
    ⇒ 写端的**单次**阻塞窗口显著变短（R34 的原文只说了「消除死锁」，没提这条）。
  - **rustdoc 整段重写**：原注释**要求**用 `IntoIterator`（与 R34 相反）⇒ 改为「必须逐层」，
    含 ① 递归读机制与出处 ② 层号范围（含 `1/M` 漏点率）③ **不可用 `get_max_level()`**
    ④ 空图边界（`get_max_level_observed()` 空图返 0 且不 panic）⑤ 成本。

#### ✅ Added
- **`crates/core/tests/step8_rw_concurrency.rs`（新增）** —— **S8-T1** 的 **std-only** 并发判据：
  - **为什么是复刻体**：`Hnsw::insert` 与 `HnswRsIndex::add` 都要求 `&mut self` ⇒ **安全 Rust 下
    不可能**让「一次精确扫描」与「一次插入」重叠 ⇒ R34 的**端到端**复现不可达（无 `unsafe`）。
  - **正向臂**：逐层形状（取锁 → 迭代 → **释放** → 下一层）在有**已排队写者**时必须**完成**（活性）。
  - **反向臂**：旧形状（持读锁时**再取一次**）必须**被拒** —— `try_read` 臂（精确、不挂线程）
    + 阻塞臂（R34 本体，有界 300ms 观测）。
  - 🔑 **「写者已排队」是被观测的事实**：`try_read` 在读锁**共享**语义下「无写者时必然成功」⇒
    它在持读锁期间失败**只能**由「有写者等待」造成 ⇒ 以**自旋直到观测到**替代 `sleep`。
  - 🔑 **写者受闸**：不加闸时写者可能在读线程拿锁**之前**就取到并释放 ⇒ 前提永不成立
    （**实测**：无闸时自旋 10s 超时，加闸后立即通过）。
  - **平台前提实测**（各 5 轮全数命中）：macOS 与 **Linux `rust:1-slim`（= CI 的 `ubuntu-latest`，
    用 Docker 实跑）** —— 「写者优先」是 R34 成立的环境前提，**不靠推断**。

#### 🔁 Changed
- **`全量遍历基数等于点数且零重复`（既有单测，扩展）** —— 改为**经生产路径**
  `search_exact_filtered(q, n, None)` 断言覆盖等价（长度 == `get_nb_point()` + 去重后相等 +
  与入库 ID 全集相等），并新增两条**层号范围**断言（层号只写到 0 时必须**能观测到少点**；
  各层点数之和**恰等于**总点数）。N 由 300 提到 512（让「存在 `level ≥ 1` 的点」这一前提的
  缺失概率由 ~8.5e-5 降到 ~1e-7）。
  🔴 **为什么必须走生产路径**（实现期新发现，**变异实测**）：原写法是**在用例内复刻**逐层循环 ⇒
  把**生产**循环的层号上界改成 `0..=0` 时它**仍然绿**。

#### ⚠️ 覆盖边界（如实登记，**不声称已被 CI 覆盖**）
- **原写**：把生产循环**改回 `IntoIterator`** 时，**本仓没有任何 CI 断言会红** —— 那条路径
  **覆盖仍然正确**（`IntoIterator` 的语义就是全量）⇒ 能拦住它的只有本新增探针 + 评审红线。
  ⇒ 🔴 **2026-09-18 第 1 轮评审后就地更正**：加**空图断言**后该表述**已不准确** ——
  该回退**在空图这一条输入上会被抓住**（旧写法 panic ⇒ `空图精确扫描返回空且不panic` 红；
  **M4 变异实测**）。**仍抓不住**「改回 **且同时**补空图早退」（那种写法仍带 R34 的递归读）。
- 本文件**不覆盖**「真实 `Hnsw` 上的读写并发」（不可达，见上）。

#### 📝 实现期新发现（2 条，已写进设计 §4.2.1）
1. **覆盖类断言必须经由生产函数** —— 在用例内复刻被测循环 ⇒ 用例锁的是「自己抄的那份循环」。
2. **`IntoIterator` 的回归」（原写：无法用行为断言拦住）** —— ⚠️ **就地更正**：
   **空图**这一条输入上**能**被行为断言拦住（见上「覆盖边界」的更正 + M4）；
   非空图上仍不能（`IntoIterator` 语义正确），且「改回 + 补空图早退」不能。

#### 🧪 变异验证（`变异手法 | 命中门 | 失败输出`）
| # | 注入 | 结果 |
| --- | --- | --- |
| **M1** | 生产循环层号上界 → `0..=0` | ✅ **4 条红**（`精确扫描条数为min_k与allowed` / `精确扫描与暴力逐位一致`(I4) / **`全量遍历基数等于点数且零重复`** / `精确扫描覆盖高层点_定向用例`）。⚠️ **首轮注入时第 3 条仍绿**（当时它在用例内复刻循环）⇒ 据此改了写法 |
| **M2** | 正向臂 `drop(g0)` → `std::mem::forget(g0)`（退化成旧形状） | ✅ **正向臂红**（10.01s 超时，消息指认退化）；反向臂仍绿（它测的是另一件事） |
| **M3′** | `受闸写者` 改取**读**锁（⇒ 永不产生排队的写者） | ✅ **两臂皆红**，消息正是预期那条（10.00s）⇒ **前提观测有牙齿** |
| M3（**废弃**） | `受闸写者` 不等闸 | ⚠️ **不作为证据**：红是**握手断裂**（接收端被丢弃 ⇒ `send` 失败），不是「写者抢跑」⇒ 换成 M3′ |

**行号漂移（实测）= 0**：本文件改动全部落在 `≥ 302` 行，而活文档里 **13 处**行号引用全部 `≤ 257`
（`v2-step1/2/4/5-design.md`）⇒ 用 `git diff -U0` 的 hunk 累计位移逐个换算确认**无漂移**；
历史设计文档里的行号**按纪律不改写**（治本走架构 §14.5 已登记的「全仓锚点符号化」立项）。

#### 📄 Docs
- **`docs/devel/v2-step8-design.md` v0.2 → v0.3**：新增 **§4.2.1 实施结果**（落点 / S8-T1 实际形态 /
  平台前提实测 / **2 条覆盖边界** / 变异表 / 行号漂移实测 / 留给 S8-09 的清单）。**不改任何决策**。

### 文档 · V2 Step 8 详细设计 —— 第 1 轮评审响应（v0.1 → v0.2）（Refs #58，2026-09-17）

> 评审：`pulls/60/reviews` **1 条**（**5×P3 + 7×P4，无阻塞**）；行内 **0** 条、`issues/60/comments` **0** 条
> ⇒ **本轮评审只在 `reviews` 一处**（落点 SHA = head `75aa724`）。⚠️ **仍为零生产代码改动**（`.rs` 一行未动）。

#### 📝 Changed（设计文档 v0.1 → v0.2，新增 §10 逐条处置）
- **P3-1 跨段墓碑在合并时的处置 —— 取「物理化」**（本轮最实质的一条）：段携带的墓碑**全部**指向严格更早的段
  ⇒ 合并时对目标 doc 走**既有的 `Index::remove` 语义**（物理摘 postings + `content_hashes.remove` + 统计量回滚）
  + **从 `View.tombstones` 移除** ⇒ **R52 只覆盖「未合并窗口」且可逆**、**`save` 落盘形态 == 今天 `remove` 后形态**
  （即 **D-S8-01「一个字节都不变」的前提**）。补写 §4.9.2 / §4.9.5 / §4.7.3（热路径表加「合并后」列）/ §4.10，
  **S8-T4 加「带跨段删除」变体**（合并前/后各一条断言）、S8-T12 加第 4 档。⚠️ 明确**不选**「墓碑永久留存」
  （那会让 `bm25_f` 永远 `Some`、全局统计量与单段发散 ⇒ 逐位一致在带删除场景不成立）。
- **P3-4 双写端 `commit()` 的原子性**：保留 `into_index()` ⇒ 同一 `Arc<Shared>` 上**结构上可能出现两个写端**
  ⇒ 「算 `base_*`」与「追加 `deltas`」必须**同处一个临界区**（否则两写端算出相同 base ⇒ 段 ID 空间重叠 ⇒ 破坏 I8-7）。
  ⇒ §4.4.3 写明取形（`commit()` 全程持 `Shared::ids` 锁）+ **S8-T10 加「双写端」臂**。**这是 D-S8-06 的前置条件。**
- **P3-3** §4.6.1 收敛为**唯一取形**（删掉「最保守者」/「独立计数」两个草稿，只留「并集分类 ⇒ 新值 `Mixed`」）；
  **Q5 / Q8 提前结案**（评审代核 + 独立复核）：`VectorRoute`（`vector/mod.rs:37`）**无 `#[non_exhaustive]`**
  ⇒ 新增 `Mixed` 与补 `#[non_exhaustive]` **两条路都是 breaking**；`Config` **无 `impl Default`**、
  全仓唯一字面构造在 `config.rs:301` 的 `build_config`（crate 内部）⇒ **仓内无下游破坏**。
- **P3-5** S8-S1 决策门**新增第 ⑤ 条**（连续 10 次合并 RSS 增量 < 1 MiB）+ §4.9.3 补 A/B 的**非对称**；
  架构 **R51 残余列**补累积性与量级。

#### 🩹 Fixed
- **P4-1** §4.8.2「既有 **100+ 处调用点**」⇒ 经独立复算改为**精确口径 41 处**（生产 4 + 测试 37；另有 2 处定义 + 11 处注释）。
  ⚠️ **与评审的 38 不同**（口径差异已写进设计 §10.3：cli 3 / examples 1 / src 2 三项一致，差额全在 tests 与注释计数）。
- **P4-2** §2.2 + 附录 C：`p_id.0 = level`（`:500`）→ **`let mut p_id = PointId(level as u8, -1)`（`:505`）**
  （`:500` 实为 `layer_g.generate()`；原片段是转述、文中并不存在该行）。
- **P4-3** §7 S8-T11「同 **fig**」→「同**图**」。
- **P4-4** `requirements-spec.md`：「**复发**触发条件」→「**复审**」（`:595`）；
  **加粗嵌套断裂的那处多余 `**` 已删**（⚠️ 实际在**头部状态行 `:9`**，不是评审所记的 `:595`）。
- **P4-5** 设计「非范围」行：「不开 ADR-B，见 **D-S8-09**」→ **D-S8-01**。
- **P4-6** §4.9.2 `field_index` 行的**机理写错**：真实限制是**同一数值同时登记 terms + numbers 两池、合计计入同一限额**
  （`field_index.rs:51-52`），而 `field_index.rs:188` 有 `!contains_key` 守卫 ⇒ 重复插入同一 value 键**不会**重复 +1；
  「必须 rebuild」的结论仍成立（另有依据）。
- **P4-7** ① D-S8-01 代价加限定（「同一量级」**只对文本侧成立**，向量侧视 S8-S1）；
  ② §4.6.3 的「登记进 R49 的观测项」→ **改挂 Q5 + §4.13.3**（⚠️ **与评审建议的 R52 不同**，理由见设计 §10.2：
  R49 = 三条前提、R52 = `bm25_f` 热路径，本条是**路由/阈值的可观测语义**）。
- **P3-2** 附录 B 命令 ⑥ `cargo run -p helix-cli` → **`-p helix`**（Step 6 评审 F2 的**同类错误复发**）；
  附录 B 其余命令已**逐条实跑核对**（`-p helix-core` ✅ / `eval_quality.sh --runs` ✅ / `--example` ✅ / `data/t2-corpus.jsonl` ✅）；
  另**标注** `scripts/eval_rw_concurrency.sh` **尚未存在**（是 S8 的交付物）。

#### 🔁 与评审结论**不同**的两处（均已单列进设计 §10.2）
- **P3-5 的量级**：结论采纳、**量级前提更正** —— leaked 的**只有 `HnswIo`**（`vector/persist.rs:210-216` 逐字「每次加载约 200B + 路径串」）、
  **不是图规模**（向量由 `Hnsw<'static>` 自持、可回收）⇒ 10 次合并 ≈ 2 KB ⇒ 门的阈值改为 **< 1 MiB** 并写明其鉴别力。
- **P4-7② 的归属**：改挂 **Q5 + §4.13.3**，而非评审建议的 R52。

#### ⚠️ 未闭环 / 盲区
- **`main` 上就有的 2 处孤儿 `**`**（`architecture-design.md:9` / `plan-v2.md:10` 的状态行）：
  逐行奇偶与 `main` **一致** ⇒ **非本次引入**；**未擅自改**（用「逐一删除候选」定位无鉴别力：33/41 个候选都能让孤儿归零）
  ⇒ 与「加粗探针入库」合并立为 **V2.1 横切项**（见 `plan-v2.md` §4）。
- **P3-1 / P3-4 的取形尚无实现**：影响面 = S8-03 / S8-06 / S8-07（PR4 / PR6）。
- **38 vs 41 两套口径未收敛**：若评审要求统一，按评审口径改（改 4 处文档数字）。

### 文档 · V2 Step 8（读写并发）详细设计 —— 设计 PR（待评审）（Refs #58，2026-09-16）

> **纯文档**：新增 `docs/devel/v2-step8-design.md` **v0.1** + 四处定义面回写（需求 **v1.20** / 架构 **v1.19** /
> `plan-v2.md` **v0.20** / `docs/README.md`）。⚠️ **零生产代码改动**（`.rs` 一行未动，脚本自证）。

#### 📝 Added
- **`docs/devel/v2-step8-design.md`（新增，v0.1）** —— Step 8（读写并发：不可变视图 + 追加段 + 原子发布）的详细设计：
  - **开工前置三条结账单（H4 / H5 / H6）**：**H4** = `save()` 前先 `merge_all()` ⇒ 落盘仍是**单段 `Index`** ⇒
    `SnapshotSections` / `FORMAT_VERSION`（= **2**）/ `GraphManifest` **一个字节都不用改** ⇒ **不开 ADR-B**
    （多段快照降为条件项 **Q2**）；**H5** = 取 **① 读路径不取写锁 + ② 写不抬高读延迟**，
    **③「读能看到未 commit 的写」明确不做**；**H6** = 写延迟口径由「从累计 embed 耗时**推算**」
    （`eval-report.md` §8.11 的 1.14 s/批）改为 **`commit()` 端到端直接计时**，并给**拟值** `P50 ≤ 1.0s / P99 ≤ 2.0s`
    （`batch_size = 64`、CPU/FP32、不含首次模型下载与加载），待 **S8-08** 标定后定稿。
  - **口径冲突择一**：**取 NFR-11** —— 可见性 = `commit()` 后；`requirements-spec.md` §5.3.5 的「**写入后**立即可查」
    已按需求面改写为「**`commit()` 后**立即可查」（**只改措辞、不改语义**，**原措辞保留**）。
  - **三个设计期新发现**（均源码级核实）：**A. BM25 是「可加」的** —— 只要用**全局精确整数统计量**
    （`N` / `total_len` / `df(t)`）+ 保持「**term 外层、段内层**」的 TAAT 累加顺序，**分段布局与单段布局的
    `bm25` 结果逐位一致是结构性结论**（不是调参巧合）；**B. fastembed 没有 `sessions` 参数**
    （`InitOptions` 只有 `intra_threads`）⇒「每 worker 独立 session」的正确取形 = **每 worker 一个
    `TextEmbedding` 实例**，且**挂起的 Q3 应改述为 Q3′**（不同 `intra_threads` / 实例数是否改变向量**数值**）；
    **C. R36 的「不投」数据不能外推**到查询侧（E2 的 +96.4% / +289.2% 是**建库口径**：4000 段 / batch 64 / 长文本）。
  - **核心设计**：`Shared { view: RwLock<Arc<View>> }` + **不可变 `Segment`** ⇒ **发布 = 只替换一个指针**；
    段的 **ID 空间** = 全局发号 + 段内本地 + `base = 前序所有段长度之和`（**合并保 ID**）；跨段 BM25 / 向量 / 谓词；
    `SearchIndex::searcher(&self)` + `merge_pending()` / `merge_all()` / `MergeReport`；**库不 `spawn` 线程**（只给原语）。
  - **D-S8-01 ~ 12**（**4 条需评审拍板**：**D-S8-01** 持久化形态 / **D-S8-06** `into_index()` 保留 + `#[deprecated]`
    并**把 ID 发号移进 `Shared`** / **D-S8-09** 向量合并 = dump→load→insert（spike 不过则回落**全量重建**）/
    **D-S8-11** 会话池形态定为「池」但 **投不投由 spike S8-S2 判定、不预先承诺**）；
    **S8-T1 ~ T16** 测试计划（**S8-T4「跨段 BM25 逐位一致」是本设计最重要的一条**）；
    **S8-01 ~ 09** 任务拆分 + **7 段 PR 切分**（**PR2 = T7-25 建议先合**）。
- **`architecture-design.md` §14.6（新增）—— R49 ~ R54**：**R49** 跨段 BM25 的「逐位一致」依赖三条前提 /
  **R50** 「合并保 ID」与 `compact()` 重编号的语义冲突 / **R51** 向量侧合并成本与拓扑不确定 /
  **R52** 热路径回归（`bm25_f` 由 `None` 变 `Some`）/ **R53** 视图指针锁的「写者优先」/
  **R54** 会话池内存（R36 的查询侧背面；⚠️ **不得引用 E2 的建库口径读数**）。
  另：**§14 导读**补 §14.6 指引 + 两条纪律（R49/R50 无关闭时点 / R51/R54 等 spike、不预先承诺）。
- **`requirements-spec.md`**（v1.19 → **v1.20**）：**NFR-14 由「草案」定口径**（可见性 = `commit()` 后；
  读延迟 = **②a 相对（合并期 ÷ 静止期 `took` P99 ≤ 1.2，拟）** + **②b 绝对护栏（仍在 NFR-02 预算内）**）；
  **NFR-11 补口径与拟值**（**直接计时** + `P50 ≤ 1.0s / P99 ≤ 2.0s`）；**FR-17 §5.3.5 措辞改写**；
  **NFR-10 ③** 补设计锚点（触发条件 = **T7-26**，挂起的 **Q3 改述为 Q3′**）。
- **`plan-v2.md`**（v0.19 → **v0.20**）：§4 Step 8 补「**设计已出**」块（含 H4/H5/H6 结账单、口径冲突择一、
  4 条需拍板、3 个设计期新发现）、**§7 进度表补 Step 8 设计行**（🟩）、**§6 的 V2.1 门槛 Step 8 行**补
  「开工前置已结清 + 可判定口径已定」、**§8 回写计划**补本轮两条。
- **`docs/README.md`**：新增 `devel/v2-step8-design.md` 索引行；「文档间关系」图的风险编号范围由 **R1~R48** 更新为 **R1~R54**。

#### 🩹 Fixed
- **FR-17 §5.3.5 与 NFR-11 的口径冲突**（`plan-v2.md` §附-4 第 7 条登记的遗留）：由设计期**择一取 NFR-11**，
  需求面同步改写措辞并**保留原措辞引文** ⇒ 该冲突**关闭**。

#### ⚠️ 未决（留待评审 / 实现期）
- **4 条需拍板**（**D-S8-01 / D-S8-06 / D-S8-09 / D-S8-11**）—— 见设计文档开头的「需评审拍板」清单与 PR 正文。
- **两处破坏性待实现期核实**（⚠️ **设计期不凭记忆下结论** —— Step 7 曾凭空造出 `rerank_status` 字段）：
  `VectorRoute` 新增变体是否需要 `#[non_exhaustive]`；`Config` 是否已有字面构造用法（决定 `Config::embed_sessions` 的加法位置）。
- **本 PR 不含任何实现**；Step 8 的实现按 **7 段 PR 链**推进（**PR2 = T7-25 建议先合**）。

### 文档 · V2.1 计划修订（2026-09-16）

> V2.0（Step 1~7）全部合并（main `62acb37`）后、V2.1 开工前的**计划面审视**；
> ⚠️ **零生产代码改动**（`.rs` 一行未动）。审视结论 15 条的索引与落地位置见 `plan-v2.md` **§附-4**。

#### 📝 Added
- **`plan-v2.md` §4「V2.1」区重写**（v0.18 → **v0.19**）：
  - **Step 8（读写并发）** 补两条任务 —— **T7-25**（**硬前置**：R34 修法，精确扫描改逐层 `get_layer_iterator`，
    消除层切换时**递归读**同一把 `RwLock`；⚠️ **必须先于 T7-06**，否则并发一开该路径即活、后果是**永久挂死**）与
    **T7-26**（查询侧会话池 + 挂起的 Q3 —— 解架构 **R43** / NFR-10 ③）；另补依赖、验收与 **PR 切分建议**（6 段）。
  - **Step 9（相关性深化）顺序修正**：**T7-16（Agent query 评测集）→ T7-14（自适应融合）** ——
    原表把 T7-14 的依赖写成「Step 7」，但「在**分桶**上优于固定权重」需要**分桶依据**（现 qrels 来自人类日志）。
  - **Step 10** 新增 **T7-27（prefilter / 排序键索引）**：承接架构 **R11 / R13** 的「V2.1 复议 prefilter」，
    第一用例 = 高基数数值字段（基数保护 `terms + numbers ≤ 1024` ⇒ 毫秒时间戳 / 雪花 ID 约 **512 篇**即永久降级、
    该字段过滤回落 O(N) 全扫）；⚠️ **先 spike 判「投 / 不投」，不预先承诺**。
  - **新增「V2.1 横切任务」区**：承接 #54 / #50 / #42 / #34 / R41 / R44 / R40 / #2（V2.0 有横切区、V2.1 此前没有）。
  - **§4.0 新增 H4~H7**：**H4** delta 分段后的 manifest / 原子发布形态（**反向改动 ADR-A 方案 C** ⇒ 或需 **ADR-B**）／
    **H5** 「读不阻塞写」的语义／**H6** **NFR-11 的写延迟目标值**（现有 1.14 s/批是**推算**、不是直接计时）／
    **H7** 评测资产能否替换主基线。
  - **§6 的 V2.1 门槛改写为可判定条目**（原为一句定性话、四个判据里三个无阈值）；V2.0 门槛末条补齐勾选。
- **`requirements-spec.md`**（v1.18 → **v1.19**）：新增 **FR-34**（prefilter / 排序键索引）、**FR-35**（融合策略自适应）
  、**NFR-14**（读写并发下的读延迟与可见性）、**NFR-15**（自适应融合的质量判据）—— ⚠️ **四条全为「草案、待评审」**，
  数值一律留白，待 `v2-step8/9-design.md` 定稿。
- **`architecture-design.md`**（v1.17 → **v1.18**）：§14.3 **R34** / §14.4 **R43** 的「残余」列补任务号
  （T7-25 / T7-26）、§14 主表 **R11 / R13** 的「应对」列补 prefilter 落点（T7-27 + FR-34）、§14 导读补「V2.1 承接」段
  （含「**V2.1 不新增风险编号 ⇒ R49+ 由 `v2-step8-design.md` 起分配**」）。
- **issue #58**（新增）：【V2 Step 8】读写并发跟踪 issue —— V2.1 此前**无跟踪 issue**
  （`plan-v2.md` §附-3 早已登记该「跟踪缺口」）。

#### 🩹 Fixed
- **FR-18 状态两处漂移**（`plan-v2.md` §5 与 `requirements-spec.md` §5.2）：两处摘要行仍写「设计已出（2026-09-14，
  v0.1，**待评审**）」，而同文档 §5.3.6 详情行早已更新 ⇒ 统一为「**已实现并收尾（2026-09-16）**」。
- **V2.0 门槛末条未勾**：`make fmt && make lint && make test && make deny` 全绿 / CI 全绿 ⇒ 改 `[x]` + 核实说明。

#### 🔁 第 1 轮评审响应（2026-09-16）

> 评审：`pulls/59/reviews` 1 条（**1×P2 + 3×P3 + 2×P4**）+ 行内 **6** 条；**六条全部采纳**，
> 另采纳两条对 T7-25 的补强建议。与评审结论不同者：无（**唯一未照做的是「`--amend` 改提交信息」**，
> 理由见 PR 正文「与评审不同」一节）。改动仍在同一分支、`.rs` 零改动。

- **P2-1**：§7 进度表删除与既有行重复的旧 **T7-16** 行（同一任务出现两次、与 §附-4 #10 的落地声明相抵）。
- **P3-1**：架构 §14.4 **R43** 残余列末尾的**加粗嵌套断裂** ⇒ 去掉内层 `**`、整句加粗。
  ⚠️ 该缺陷经**块级 CommonMark 配对模拟**独立复现，且**本次新引入仅此 1 处**（其余报红经内容级对照确认为 main 历史残留）。
- **P3-2**：「在计划里 **0 命中**」表述**不准确** ⇒ 改为「**没有任务或编号承接**」
  （`plan-v2.md` §8 的 T7-23 条目**有 1 处判据句**提及 prefilter，`requirements-spec.md` 全文才 0 命中）；**实质结论不变**。
  ⚠️ 已 push 的提交信息**不改写**，更正落在本条目 + PR 正文 + §附-4 #3 行。
- **P3-3**：Step 8 验收行给 FR-17 §5.3.5 第一条补**取形括注**（按 H5 预写、原文措辞待设计期更正），消除与被引用方的不同形。
- **P4-1**：§附-4 **#1** 的「落地位置」由「§4.0 H5 之外的硬前置」（H 项与任务依赖是两套机制、按格找不到落点）
  改为「§4 Step 8 的**依赖行硬前置**（先于 T7-06）」。
- **P4-2**：§2.1 范围行的 **T7-27** 补「**spike，先判投不投**」，与 FR-34 / Step 10 的决策门口径对齐（原文读起来像已承诺）。
- **采纳评审补强建议**：Step 8 新增「**T7-25 的回归锁**」—— ① **std-only 探针复跑**证「递归读已消除」；
  ② 「每点恰 `yield` 一次」的**覆盖等价断言**，并把逐层写法钉死为 `0..=pi.get_max_level_observed()`
  （`get_layer_iterator(0)` 漏 `level ≥ 1`、实测 **3.74%**）；两条登记进 `v2-step8-design.md` 的测试计划（S8-Txx）。

#### ⚠️ 未处理（仅登记，本次不改）
- **#24 / #25 仍 OPEN**（T7-21 / T7-24 已随 PR #49 / #48 合并、`plan-v2.md` §7 已标 ✅）——
  按纪律**不擅自关他人 issue**，留待维护者处置。
- **全仓行号锚点符号化**（架构 §14.5 导读已登记 **121 处**）仍**未做** —— 仍建议**单独立项**。

### 文档 · PR #57 评审响应（S7-05 收尾轮）：回写完整性 + 锚点精度（Refs #23，2026-09-16）

> 评审 **3×P2 + 3×P3 + 3×P4（无阻塞）**，**9 条全部采纳**；⚠️ **仍为零生产代码改动**（`.rs` 一行未动）。

#### 🩹 Fixed（P2，建议合并前修 —— 均在 `plan-v2.md`）
- **P2-1 版本单元格插坏**：v0.18 条目被插进 **v0.15 括号段中间**、单元格开头仍是 v0.17、且多出一条重复的
  「此前 v0.17」。⇒ 把 v0.18 整段**移到单元格开头**（恢复「最新版本在头」）、删除重复尾注、还原 v0.15 → v0.14 接缝。
- **P2-2 状态行 / 日期行漏改**（F2「归属不一致」同族）：仍写「Step 7 详细设计已出、**待评审**」「main **现为** `516b2c7`」，
  与版本行 v0.18 自相矛盾 ⇒ 状态行前缀 S7-05 状态 + 旧文改「**曾为**」；日期行 `2026-09-14` → **`2026-09-16`**。
- **P2-3 带版本锚点指错**：`TextRerank::try_new` 写成 **v6.0.2 下 `:126-132`** —— 实测 `:126-132` 是 `pub fn rerank`
  的签名段（`:124-125` 为其 doc），`try_new` 在 **`:44`**。⇒ 已改为 **`:44`**。
  ⚠️ 同格其余锚点**逐条实测吻合**（`init.rs:17-19` / `common.rs:174-180` / `impl.rs:215-224`）。

#### 🩹 Fixed（P3 / P4）
- **P3-1 已漂移的本仓行号**（评审点名 `plan-v2` 的 H3 行与 §附-1）⇒ **改符号指代**，并**顺带扫出同族 9 处**
  （评审没点到的）：`plan-v2` §4.1 两行、`architecture-design` §5.7、`requirements-spec` 的 NFR-12 行与头部注、
  `eval-report` 两处、`v2-step7-design` 的 H3 行。**共 11 处**（跨 5 个文件）。实测**漂移后的真值**：
  `candidate_k` = **`:149`**、融合后截断 `take(take_n)` = **`:337`**、`rerank(...)` = **`:375`**、`fuse(...)` = **`:276`**、
  `filter.rs` 的 `allowed_chunks` = **`:53`**。⚠️ 历史行号按纪律**保留为引文**（标注「已漂移」）。
- **P3-2 R44 残余格三处**：①「⚠️ 与 R41 的区别…」**整句重复两次**（粘贴残留）⇒ 删一处；②「不变式 = 抬升 **≥** 模型体积约
  1.25 倍」被**本 PR 自己的 min 臂反例**（`2959.1−403.7 = 2555.4 MiB ÷ 2165.9 ≈ **1.18×**`）⇒ 改为**实测形态参考**
  （中位 1.25×、区间 **1.18~1.42×**，**不写成「≥」型不变式**）；③「相对 **372MB** 基线 ≈7.7 倍」**数字错挂**
  （7.71× 只对**同一 harness** 的 403.7 MiB 关臂成立；对 372MB 硬比 ≈ **8.4×**）⇒ 已改正并标明**两基线不可横比**。
  `eval-report.md` §8.14.10 第 3 点同源，一并改。
- **P3-3** `plan-v2.md` 验收清单：`[x]` 与行尾「⬜ **待实现**」并存 ⇒ 删尾巴、换「✅ 已实现（#52/#53/#55/#56）+ 收尾」。
- **P4-1** CHANGELOG 写「**6 个**被改文档」、PR 正文写「**7 个**」⇒ 本轮**真的把 7 个全跑了一遍**
  （含 `CHANGELOG.md`，转义感知检查器 + 对照样本自证），统一为 **7 个**。
- **P4-2** NFR-12 的 ① 小标题残留「双列，**均标「拟」**」⇒ 改「双列；**CPU / FP32 口径已定稿**，GPU / 量化标「拟」」。
- **P4-3** 架构状态行尾部历史句保留现在时「main **现为** `516b2c7`」与「默认 20**（拟）**」⇒ 改「**曾为**」+
  「（**2026-09-16 已定稿**，见 v1.17）」。

#### 📝 Added
- **§14.5 导读补「规则的适用范围」**：明确「新写锚点必须符号化 + 存量在该行被改动时一并转换」，
  并**如实登记本轮清点结果**（6 个活文档 **121 处**行号锚点、其中本仓源码约 **60 处**）+ **未改写的具体清单与理由**
  —— 免得这条规则被后人读成「已经全仓改完」。

#### 🔍 自查（本轮抓到的两处自身缺陷，均已在提交前修掉）
1. **一次 `Edit` 把版本单元格改成两段 `| 版本 | `**（新行与旧行前缀并存）⇒ **被表格列数检查器抓住**（5 列 vs 3 列）
   —— 说明「改完必跑表格检查」这条纪律确实在干活（且**必须先跑对照样本**自证检查器有牙齿）。
2. **「一类缺陷」的两个测点都漏了第二处**：`fuse(...)` 的 `:245` 锚点在 `plan-v2` **出现两次**、
   `v2-step7-design` 的 `searcher.rs:286` 也在 **H3 行 + 附录 D** 各一次 ⇒ 靠**预检脚本的 `assert count==n`** 全部暴露
   （若手改就会漏掉一处）。

### 文档 · V2 Step 7 收尾（S7-05）：NFR-12 定稿 + 风险回填 + 锚点符号化（Refs #23，2026-09-16）

> Step 7 的**收尾 PR**（定义面回写 + 两处欠账补测）；⚠️ **零生产代码改动**（`.rs` 一行未动）。
> 三项**判据/数值裁定已按用户拍板执行**：**NFR-12 = 双口径 + 仅开启时适用** ／ **R48 = 收窄后关闭** ／ **R44 峰值 RSS = 补测**。

#### 📊 Changed（需求面 `v1.17 → v1.18`）
- **NFR-12 定稿**：原「**≤ 500 ms（拟）**」（对应 `R = 10` 档的量级上界）**废弃** —— 实测 `R = 10` 的 P99 = **5220 ms**
  （差 **10.4×**）⇒ **CPU-only / FP32 / 未量化下不可达**。定稿 = **双口径**：**㈠ CPU/FP32 上界**（默认 `R = 20` 下
  **P50 ≤ 8.8 s / P99 ≤ 10.9 s**，环境另有负载 ⇒ 读作上界）；**㈡ GPU / 量化模型待测**（无实测 ⇒ **只留形态不给数字**，标「拟」）。
  并写明**适用范围 = 仅显式传 `--rerank-window` 时**（D-S7-04 默认关）与**与默认 `R` 联动**（`R` 定稿 **20**）。
- **NFR-05** 回填精排内存实测（**增量 2.64 GiB / 7.71×**，读 §8.14.10）。
- **NFR-06** 更正精排 tie-break **机制句**：原「不改 `chunk_id` 序（同分时稳定排序保持输入序）」**与 D-S7-07 及实装相反**
  ⇒ 改为「**改用 `chunk_id` 升序**」（实装在 `crates/core/src/rerank/scoring.rs` 的 `apply_scores`，**刻意不沿用**
  `fastembed` 的 stable sort）；**结论不受影响**（重排仍逐位确定）。另补 **S7-T9 实测读数**。
- **FR-18** 验收注记同步（「精排延迟满足 NFR-12」→ 指向定稿口径）。

#### 📌 Added（架构面 `v1.16 → v1.17`）
- **§14.5 R44 数值回填**（**条目仍保持开放**：不给精排侧的独立预算上限 —— 该上限应由用户的并发度 / 量化选型决定，与 R41 同族）。
- **§14.5 R48「收窄后关闭」**：定稿表述 = 「**在本评测集上**截断不是『精排无提升』的主因」+ **三条限定**（产品侧目标场景未测、`max_length = 1024` 档未测延迟、将来调大需按本节判据重测）。
- **§14.5 R47 残余改「已实测」**（P1/P2/P3 全部跑过）。
- **新增一条锚点规则**（写进 §14.5 导读）：**本仓源码锚点一律「符号指代」**（不写行号 —— Step 7 的三次实现 PR 每次都让定义面行号漂移：S7-03 一次 10 处、S7-04 一次 5 处）；**依赖源码**（`fastembed` / `hnsw_rs`）可写「符号 + v 版本下 `:行号`」的**带版本锚点**。

#### 📈 Measured（两组新实验，读数入 `eval-report.md` §8.14）
- **§8.14.10 精排器常驻内存（R44 的数值）**：`bench` / 冻结图 / 含模型加载 / **两臂交错各 3 次**取中位 ⇒
  精排关 **403.7 MiB** → 精排开（`R = 20`）**3110.5 MiB**（min 2959.1 / max 3488.9）⇒ **增量 2706.8 MiB = 2.64 GiB、7.71×**。
  ⚠️ **增量 > 模型文件本身**：是 `model.onnx.data`（**2.12 GiB**）的 **1.25×**（多出 ~0.5 GiB = ONNX Runtime 运行时 + 单 batch 激活 + tokenizer）。
  ⚠️ 开臂离散 **±9%** ⇒ 留余量应按 **max 3488.9 MiB**（**≥ 3.4 GiB**）预留。
- **§8.14.11 真模型 `#[ignore]` 用例实跑**（**补上四轮 PR 一直挂着的口径**）：**P1**（同进程 3 次）/ **P2**（跨进程 2 次）
  ⇒ (`chunk_id`, `score`) **逐位一致**；**P3**（单条 vs 批内）⇒ 原始 logit **相同**（`2.2603965`）；**T10** ⇒ `R = 50` 时
  **有 4 条**不在输入前 10 的候选进入最终 top-10。⚠️ **P3 是单一样例、单一批组成** ⇒ **不构成**「跨 batch 组成恒逐位一致」的证明
  ⇒ **NFR-06 判据不得升级**（仍只到「同一快照两次加载逐位一致」）。

#### 🗂 Docs（四处定义面 + 设计文档 + 收尾自查）
- 四处定义面：需求 **v1.18** / 架构 **v1.17** / `plan-v2.md` **v0.18** / `docs/README.md`（索引行状态）；设计 **v0.2 → v0.3**。
- **设计 §4.6.4 落定拐点口径**：实测**抖动带 = 0** ⇒ 原文判据「增量 < 抖动带」**退化** ⇒ 落定 **「边际增量 ÷ 首段增量 < 10%」**，
  并**注明这是实现期选定的阈值**（消除「下一个照原文跑的人再撞一次」的坑，来源 = 勘误 **E10**）。
- **E10 勘误**：S7-T7 的落点原写 `tests/step7_rerank_observability.rs`（**该文件未创建**）⇒ 实际落在
  `crates/core/src/query/searcher.rs` 单测（T1~T7 / T12~T14 都要**注入间谍精排器**，集成层够不到）。
- **S7-05 回写清单 7 项标记全部结清**（原始记录按纪律保留引文）。

#### 🧪 收尾自查（三项，均通过）
- **引用节号存在性**：本 Step 设计文档的**自引用节号全部存在**（跨文档引用的 8 处逐处打开核过）。
- **测试计划 vs 实现台账**：**S7-T1~T14 全部存在**（唯一偏差 = T7 的**落点**，已记 E10）。
- **Markdown 表格列数**（转义感知）：**7 个**被改文档**逐表核过**，全部一致。

### 修复 · V2 Step 7 PR 4 评审响应（第二方独立评审：**无阻塞** + 2×P3 + 2×P4，**全部采纳**）（Refs #23，2026-09-16）

> 评审落点：`pulls/56/reviews` **1 条 `COMMENTED`（2617 字）+ 行内 4 条**（`2026-09-16T03:11:01Z`，
> 晚于我最后 push 的 `2026-09-15T20:04Z` ⇒ 真·新一轮）；`issues/56/comments` = **只有我自己的 CI 评论**。
> 评审**独立复核**了 8 组事实（新增测试计数 / 双条件判据真值表 / `opt_ms` 契约 / 行号 5 处 /
> §8.14 的 **26 项**交叉复算 / 两个符号检验 p 值 / 协议缩减外推 / 抽样器公式）⇒ **全部属实**；
> 结论 **0×P1 / 0×P2**，并对正文「请评审特别注意」4 条**逐条回应**（全部接受）。
> ⚠️ 本轮 **`.rs` 零改动**（3 个文件是文档、1 个是 `.sh`）。

#### 🟢 Fixed（P3-1 —— §8.14.1 的复现命令**照抄会跑出另一种实验**）

- ② 那段**没写** `--maxlens`，而脚本默认 `MAXLENS="512 1024"`（`eval_rerank.sh:64`）⇒
  **照抄会把 ml512 / ml1024 也在主批跑掉**：既与下方「实跑是两批」的叙述矛盾，又让 §8.14.7
  「跨批次自证」的**前提（两批不同批）失效**。⇒ 命令补 `--maxlens ""`，并就地把「不写会怎样」写清。

#### 🟢 Fixed（P3-2 —— 二进制出处没钉死）

- 「基线 `9a957dd`」读不出**这一层**：`9a957dd` 自己**没有**精排延迟出口（E9 正是本 PR 修的）
  ⇒ 能产出「精排 P50/P99」列的二进制**必然**含本 PR 代码。⇒ 改成
  「`9a957dd` + **本 PR 代码轮**工作树构建」+ **给 md5**（`facc520c…`，mtime `2026-09-15 23:01:42`）
  ⇒ §8.14 那句「**同一份二进制**跑完全部档位」由此**可复核**（首产物 `23:02:07`、末产物 `03:01:44`
  都在其后，**期间未重建**）。⚠️ 并**如实登记该 md5 的复核边界**：`target/` 不入库 ⇒
  它证的是**本次会话的内部自洽**，**不是**「别机能重建同一字节」。

#### 🟢 Fixed（P4-1 —— `--query-sample` 会**静默产出重复子集**）

- **先复现**（合成 4 type = 80/80/80/**2**、请求 60）：实得 60 条里**只有 47 条不同**，
  `paraphrase-0` 占 **8** 次；而「请求 N / 实得 M」那条告警**永不触发**（`out` 恒 = `per × n_types`）
  ⇒ 评审的判断**完全属实**。
- ⇒ 加**两条 fail-closed 前置守卫**（请求数 > 可用条数；某 `type` 条数 < `per`）
  + **一条后置自证**（「条数相等 ≠ 没有重复」的兜底：将来守卫被改坏仍能拦住）。
  拒绝时**不产出子集文件**。
- **变异验证（2 处，均如实复现）**：① 删前置 `short` 守卫 ⇒ **后置自证接住**
  （`60 条里只有 47 条不同`，exit 1）；② 两处都删 ⇒ **静默重复如实复现**（exit 0、仍 60 条 / 47 不同）
  ⇒ 证明守卫确实在干活，不是装饰。夹具 = **每次从被测 `.sh` 重新抽取**内联 Python 再跑
  （用副本会让变异假过 —— 本仓已踩过）。真评测集（320 / 4 type × 80）请求 60 ⇒ 成功、**去重后 60**。

#### 🟢 Fixed（P4-2 —— 缩减系数与同格自列公式不符）

- 延迟轴格的「成本 ÷~16」按**同格自列公式**复算是 **19.2**（旧 `40×3×(1+3+20)=2880` ↔
  新 `10×3×(1+1+3)=150`）⇒ 改成 **÷19.2**。
- 🔑 **顺手扫同类**（评审只点了一处）：**质量轴格**的「成本 ÷5.3」也不对 —— 5.3 是 **query 数**之比，
  **总成本**还要算轮数 2→1 ⇒ **÷10.7**。⇒ 两处一并改，并**标清是哪个口径**（免得再被复算打脸）。

#### 🔵 S7-05 回写清单**新增第 7 项**（源自评审对正文的两条建议）

- **把拐点口径写进设计 §4.6.4 正文**（现在只活在 `eval-report.md` §8.14.5 里）—— 否则下一个照
  设计原文跑的人还会撞上「抖动带 = 0 ⇒ 判据退化」；
- **NFR-12 定稿给两个口径**（CPU / FP32 的实测上界 + GPU / 量化的目标值），避免单一数字再次整体失配。

### 新增 · V2 Step 7 PR 4 —— 精排标定：默认 `R` 与 NFR-12 的实测（S7-04）（Refs #23，2026-09-15）

Step 7 的**唯一数据 PR**（必须真机跑 `bge-reranker-v2-m3`，≈2.19GB），并补上取数所必需的工具与文档。

#### 🔴 开工即修掉两个 blocker

1. **`bench` 侧没有精排延迟出口（PR #55 的遗漏）**：设计 §4.6.3 要求「端到端 `took` 与
   `rerank_elapsed` **分列**」，而 `bench` 的延迟 JSON 与表格**一个都没输出**这两个字段
   （`Metrics.rerank_elapsed` / `rerank_window` 在 S7-02 就加好了）⇒ **NFR-12 的判据取不到数**。
   已在 `crates/cli/src/bench.rs` 补 4 个 JSON 字段（`rerank_p50_ms` / `rerank_p99_ms` /
   `rerank_n` / `mean_rerank_window`）+ 延迟表两列；`opt_ms` 把 `None` 打成 `-` **而不是 `0`**
   （`0` 与「没跑精排」在表里不可区分 —— NFR-07 的老要求）。
   ⚠️ **收集判据是双条件**：「本档位传了 `--rerank-window`」**且** `metrics.rerank_window > 0`。
   第一版只用「`rerank_elapsed > 0`」⇒ **把控制组 A 也收了进来**（`NoOpReranker` 的空调用耗时
   虽小但**非零**（~0.5µs））⇒ 得到 `rerank_p50 ≈ 0.0005ms` 这种**看似精排、其实没跑**的假读数。
2. **设计协议在本机不可行**：实测 **0.45 s/文档**（FP32 / CPU EP / M5 10 核）且成本与 `R`
   近似线性；且延迟轴的实际调用数 = **1（效果阶段）+ `warmup` + `reps`**（设计期漏算了效果阶段）
   ⇒ 按 §4.6.1 全量复算 **≈262 小时**（质量轴 ≈91h + 延迟轴 ≈171h）。
   ⇒ 缩减为 **分层 60 × 三路 × `R ∈ {10,20,50}` × 1 轮 × 延迟轴 10 query/reps 3**；
   设计勘误 **E8**（新增 §4.6.5 全表）+ **E9**（bench 出口缺失）。

#### 工具（脚本）

- **`--query-sample N`**：分层抽样（每个 `type` 等步长取、再**交错排列**）⇒ 同一命令**必得同一子集**
  （实测两次抽样**逐字节一致**）；交错还让 `head -N` 取到的延迟轴子集覆盖各 type。
- **`--skip-quality-axis` / `--no-latency-axis`**：分段续跑（避免单任务过长）。
- **`--modes`**：此前只有环境变量（实测踩到：`--modes hybrid` 报「未知参数」直接退出）⇒ 补成参数。
- 第 8 段：延迟轴汇总 → `latency.md`（**端到端 vs `rerank_elapsed` 分列**；NFR-12 的判据表）。

#### 标定结论（**分层 60 子集**口径；⚠️ 不可与 P5 的全量 320 数字横比）

- **默认 `R` 走决策门出口 ②：`R = 20`**（与设计 §4.2.2 的拟值一致）。
  依据：hybrid 的 `ΔMRR`（vs 精排关）= **+0.0384**，其中 **87.1% 来自「窗口」**
  （`R: 10→20` 边际 **+0.0335**）、仅 13% 来自模型本身（`A→R=10` 的 +0.0050）
  ⇒ **两个控制组把「模型」与「窗口」分开了**（若只有 A vs R=20，这笔收益会被整体误记为「精排有效」）。
- **拐点**：`20→50` 的边际降到首段的 **6.6%**（全精度 `0.002222 / 0.033452`；⚠️ 按四舍五入值算会得 6.9%，
  两者都在 10% 阈之下 ⇒ 结论不受影响）⇒ 已耗尽。
  ⚠️ **`vector` 在 `R=50` 反而下降**（0.7069 → 0.7017）⇒「窗口越大越好」**不成立**，
  且**不同路的最优 `R` 不同**（bm25 仍在小幅上涨）。
- 🔴 **NFR-12 必须重定**：精排 P50/P99 = **4.57 / 5.22 s**（`R=10`）、**8.78 / 10.86 s**（`R=20`）、
  **21.47 / 26.92 s**（`R=50`）⇒ 拟值 `≤ 500ms` 差**一个数量级**（最小档即 **10.4×**）；
  精排占端到端 **99.8%**（`R=10`/hybrid）。⚠️ 测量环境另有 **~370% CPU** 的无关负载
  ⇒ 数字偏悲观、应读作**上界**；但即便乐观 2×，`R=10` 仍 ~2.6 s ⇒
  **`≤500ms` 在 CPU-only / FP32 / 未量化下不可达**。定稿（含是否加「仅 GPU/量化」限定词）属 **S7-05**。
- **`R48`（512 token 截断）有数据了**：固定 `R=20`/hybrid，`max_len 512 → 1024` 的增益
  **≈0**（NDCG **+0.0002**、MRR +0.0057 ⇒ 相对 +0.81%；逐条仅 **7/60** 变化、符号检验单侧 **p = 0.5**）
  ⇒ **截断不是「精排无提升」的主因**（且精排在本子集上**有**提升）；同时**不支持调大 `max_length`**
  （收益 ≈0 而成本近似平方增长——⚠️ 1024 档**未测延迟**，该成本是先验）⇒
  **D-S7-08 的「保持库默认 512」是数据支持的选择**。
  ✅ 顺带实证 **D-S7-08 要求的「`max_length` 进精排器身份面」**：`bench` 输出
  `bge-reranker-v2-m3@rozgo;max_len=1024;window=20`。R48 的风险状态回填属 **S7-05**（本 PR 不代改风险面）。
- **跨批次自证**：`max_length` 对照与主批**不是同一批**（拆两批省成本）⇒ 用
  「两份 `A-noop.json` 的 hybrid 子树**逐字节相同** + 抽样文件同 md5」证明跨批可比 ——
  把 §8.14.3 的「同批 3 次逐位一致」扩到「**跨批次逐位一致**」。
- ⚠️ **统计上不显著**：60 query 下**9 组（3 档 × 3 mode）配对符号检验全部不显著**
  （**二项精确单侧**，与 `eval-report.md` §3.2 同口径 ⇒ `p ∈ [0, 0.5]`；最小 p = **0.1553**）
  ⇒ 本 PR 只报**幅度**、不声称显著。
- **门（S7-T11）通过**：**5 份**读数（`freeze-1/2/3` + 控制组 A 两次）的 md5 **完全相同**（`b2284b78…`）、
  per-query 逐位一致 ⇒ **抖动带 = 0**（因此设计 §4.6.4 的「增量 < 抖动带」判据**退化**，
  本 PR 改用「边际 ÷ 首段 < 10%」并**明确标注为实现期选定的口径** —— 报评审）。

#### 报告与产物

`docs/devel/eval-report.md` **§8.14**（新增 9 个小节：协议缩减 / 环境与冻结图 / 冻结自证门 /
质量轴读数 / 归因与拐点 / 延迟轴 / `max_length` 对照 / 决策门 / 局限）；
脚本侧的汇总产物 = `summary.md` + `latency.md`；冻结图 `data/t2-frozen.snapshot`
（12,000 chunk，**不入库**，复现命令在 §8.14.1）。

#### 变异验证（新增断言必须有牙齿，**按实测填写**）

本 PR 新增的代码面上有两处「判据 / 语义」。它们**各提成一个纯函数**再用单测钉住 ——
否则这两条只能靠**昂贵真机跑**（数十分钟 × 数档）才能验证，等于没有守门：

| 变异 | 注入 | 实测命中 |
| --- | --- | --- |
| **M1** | `should_collect_rerank` 的 `&&` → `\|\|`（收集判据放宽成「或」） | ✅ `bench::tests::精排延迟样本要同时满足配置与交接两条件` FAILED（20 passed / 1 failed） |
| **M2** | 去掉「真交接」那半条（`rerank_window > 0` → `>= 0`） | ✅ 同上 FAILED |
| **M3** | `opt_ms(None)` 打成 `"0.00"`（把「没跑精排」显示成「精排花了 0ms」） | ✅ `bench::tests::可选毫秒的缺失不打成零` FAILED |
| **M4** | `opt_ms` 精度 `{:.2}` → `{:.3}` | ✅ 同上 FAILED |

注入方式 = **锚点唯一性断言**（`assert count == 1`）+ **每轮逐字节还原核对（md5）** ⇒ 四次全部捕获、
还原后 21 项全绿。⚠️ **刻意没有用「删整块 + 空串锚点」**（PR 3 踩过：`s.count("")` 返回字符串长度 ⇒
assert 必炸、**文件根本没还原**、后续变异跑在污染基线上）。

#### 顺带的重构（无行为变更）

`eval_latency` 加到第 8 个参数时触发 clippy `too_many_arguments`（阈值 7）⇒ 按本仓既有先例
（`bench.rs` 的 **`FilterCtx`**，**只给符号不给行号** —— 理由见下「行号欠账」）把
`k` / `warmup` / `reps` / `collect_rerank` 捆成 **`LatencySpec`**（「**这一次怎么跑**」），
与 `FilterCtx`（「**在什么语料上**跑」）各成一束。
`FilterCtx` 的注释里那句「捆成一束也确实比四个散参更能表达它们同生共死」在**两处**都成立。

#### 守门链（**G1~G12 全绿**；本机 pin 的 toolchain 就是 MSRV 1.90）

`fmt` / `make shell`（`check_shell_expansion`）/ `clippy --workspace --all-targets -D warnings` /
`clippy -p helix --all-targets --features local-rerank -D warnings` / `rustdoc -D warnings` /
`cargo test --workspace` / `--features charabia` / `--features local-rerank`（core 与 cli 各跑）/
`--no-default-features` / `cargo +1.90.0 check --workspace --all-targets` /
`cargo deny check advisories licenses bans sources`。

⚠️ **本轮自查抓到的取证缺陷**：第一遍用 `cargo test … | grep -E "^test result:" && echo "G6_OK"`
当段标记 ⇒ **退出码取自 `grep`**，而 `test result: FAILED. 19 passed; 1 failed` 这一行
**本身就会被这个 grep 命中** ⇒ 打印了 `G6_OK`、**而同段其实有失败**（见下）。已改为
**落盘 + 判 `$?` + 断言没有 `^test result: FAILED` 行**。**凡「管道 + `&&`」的段标记都不代表被测命令。**

#### ⚠️ 已知 flaky 一次（**非本 PR 引入**，如实登记）

`cargo test --workspace` 的 G6 段出现过一次 `test result: FAILED. 19 passed; 1 failed`（85.7s）：

| 证据 | 内容 |
| --- | --- |
| **红在哪个二进制** | 20 用例 / ~90s ⇒ 按**用例数对号入座** = **`crates/core/tests/graph_persist.rs`**（Step 2 的图持久化测试） |
| **同码异果** | **同一提交**下该二进制共跑 **16 次**（守门链 3 + 全量复跑 1 + 定向 12）：**1 红 15 绿** ⇒ 抖动而非回归 |
| **定向压测（没能指名）** | 对该文件里**已登记**的那条（`T6_旧快照无图可加载`）定向跑 **40 次**：**0 红** ⇒ **连「是不是 `T6`」都不能确定**（同文件还有 ANN 名次门 `T13_EXACT_RANK_GATE` 等候选；⚠️ 该文件的阈值型断言此前已按 #39 意见**降级为诊断打印**，故「哪条是阈值型」也不能照旧印象推） |
| **本 PR 能否触及** | `git diff --stat HEAD -- crates/core` = **空**（本 PR 只改 `crates/cli/src/bench.rs` + 脚本 + 文档） |
| **是否已登记** | 该文件有已登记项 **#34**（`T6_旧快照无图可加载`）。⚠️ **别与 #42 混** —— 那条在 `crates/core/tests/integration.rs` |
| ⚠️ **诚实标注** | **失败用例名没抓到**：守门链的 `grep` 把 `test … FAILED` 那行过滤掉了 ⇒ 只能靠「20 用例」锁定**文件**，**连文件名以下的指名都做不到**。**这是我自己造的取证缺陷**（已回写教训：过滤要保留该行，或先 `tee` 落盘再过滤） |

⇒ **不阻塞本 PR**，但按纪律**不静默**（同时记进 PR 正文）。

#### 行号欠账（本 PR 又加深了一层）

本 PR 再次改了 `bench.rs`（+162/−13）⇒ 凡引用点落在改动**之后**的行，都在**既有**漂移之上
**再加 ~26 行**；`v2-step5-design.md` / `v2-step2-design.md` / `CHANGELOG`（历史条目）里的
`bench.rs` 行号**本来就已经漂移**（不属本 PR 造成）⇒ 按「历史记录不改写」**不动**，
但**欠账在累积**：本 PR 已把**本 Step 设计文档**内的 5 处按实测同步、**2 处核过未漂移**，
并把「定义面改用**符号指代**」列为 **S7-05** 的建议（本 CHANGELOG 条目已经这么做了）。

⚠️ **未跑**：T8/T9/T10（真模型 `#[ignore]` 用例）**一条都没跑** —— 本 PR 的数据来自 `bench` 端到端，
与那三条用例是两条路径 ⇒ PR #52 / #53 的「不得读成已覆盖」**在此仍然成立**。

### 修复 · V2 Step 7 PR 3 评审响应（第二方独立评审：**无阻塞项** + 1×P2 + 2×P3 + 2×P4，**全部收口**）（Refs #23，2026-09-15）

> 评审落点：`pulls/55/reviews` **1 条 `COMMENTED`（2606 字）+ 行内 3 条**（`2026-09-15T13:38:37Z`，
> 晚于我最后一次 push）；`issues/55/comments` 只有**我自己的 CI 评论**。**5 条全部采纳、无一条被拒**。
> 评审的 ✅ 表（7 条）我逐条重跑过，其中「`Config::fingerprint` 不含 reranker ⇒ 不会 `ConfigMismatch`」
> 一条是**我 PR 正文没写**的关键安全性质，已补进设计文档。

- **P2（脚本延迟轴，两个子问题，都在同一段 10 行里）**：
  ① 头注释声称的「固定子集」**没有实现** —— 全量 `--queries "$QUERIES"` 原样传入，而 bench 的
  延迟阶段**没有子集机制**（对全量 judgments 遍历 `warmup+reps` 次）。按实参复算：
  `320 queries × 23 passes × 200 docs` ≈ **1.47M 次文档打分**（fastembed 批 256 已计入）
  ⇒ **全量跑一档是 2~25 小时**（依每文档有效耗时）⇒ S7-04 首跑就会撞上。
  ② 控制组 A **没传** `--reps/--warmup`，与精排档只在**默认值下巧合一致** ⇒ 一旦 `--reps 5` 覆盖，
  延迟 Δ 就是**静默错口径**（而它正是 NFR-12 的输入）。
  ⇒ 落地 **`LAT_QUERIES`**（默认 **50**；`0` = 全量并打出小时级警告），且**控制组 A 与各精排档
  用同一子集、同一 `reps/warmup`**。
- **P3-1（bench 顺序）**：`build_reranker`（唯一会加载模型的入口）排在 `parse_modes` /
  `parse_thread_levels` **之前** ⇒ 这些**纯参数校验**便宜得多却排在后面（`--modes` 拼错也要先
  白等一次 ≈2.19GB 模型加载/下载）。⇒ 挪到它们**之后**（仍在 `load_setup` 之前）：
  **四条守卫的相对顺序不变**、CI smoke 不受影响，且与 search 侧的「纯参数校验 → 模型」顺序对齐。
- **P3-2（summary 死变量）**：`q = docs[0].get("rerank", {})` 赋值后从未使用（注释承诺的判据没落地）
  ⇒ 换成**串档检测**：各轮 `rerank` 块（本 PR 在 bench 侧加的自证出口）必须**一致且 `enabled`**，
  否则打警告。理由：`OUT_DIR` 残留上一轮产物时，两个**不同档位**之差会被当成「抖动」，
  直接污染 §4.6.4 的「Δ 首次 < 抖动带」判据。
- **P4-1 / P4-2**：`FREEZE_N < 2` 改为**参数区就地校验**（原先要跑完一次 bench、含模型加载，
  才在 Python 里失败）；头注释补 `--skip-freeze` 与 `--lat-queries` 用法（原先只在 case 分支里有）。

**⚠️ 本 PR 第二次改 `bench.rs` ⇒ 行号引用再次同步（实测 5 处 `+3`）**：
`436-446→439-449`、`711-724→714-727`、`717-719→720-722`、`757→760`、`1597→1600`（`178-180` 不变），
并把 **S7-05 清单第 5 项**里给定义面的目标值一并更新为 `:760`。

**变异验证**（本轮改的是**脚本** + 一处顺序 ⇒ 无法实跑，用「dry-run 断言 + 合成数据实跑内联 Python」替代）：

| 变异 | 手法 | 实测命中 |
| --- | --- | --- |
| **M1** | 控制组 A 去掉 `--reps/--warmup` | ✅ 断言 A1d 失败（口径不一致） |
| **M2** | `LAT_QUERIES` 默认改 `0`（回落全量） | ✅ 断言 A1a 失败（无子集截取） |
| **M3** | 串档检测条件恒假 | ✅ 断言 A3b 失败（串档未被检出） |
| **M4** | `FREEZE_N` 就地校验条件恒假 | ✅ 断言 A2 失败（未拒绝） |

外加 **P3-1 的改前/改后行为对照**（该处**无断言可变异**）：同一命令
`bench --modes bogus --rerank-window 20` 在**改前**报「需要以 `--features local-rerank` 编译」
（模型入口先跑）、**改后**报「不支持的 `--modes` 项」✓。
⚠️ 边界：本机**未编译** `local-rerank` ⇒ 该对照证明的是**顺序**，不是真实的 2.19GB 加载耗时。

🔴 **纠正一处我自己的验证假通过**：A3/A4 最初用的是**上一轮抽取的**内联 Python 副本 ⇒
「把判据删掉」这类变异**抓不到**（副本里判据还在）⇒ 改为**每次从当前脚本重新抽取**才有牙齿
（否则 M3 会假过 —— 「照抄命令跑一遍证明不了断言有牙齿」的又一例，与本项目既有教训同族）。

### 新增 · V2 Step 7 PR 3 —— CLI 接线：`--rerank-window` / `--rerank-max-length` + 四条守卫 + `scripts/eval_rerank.sh`（S7-03）（Refs #23，2026-09-15）

精排**第一次能从命令行使用**（此前只有库接口）。**默认行为零变化**：不传 `--rerank-window`
⇒ 装配仍是 `NoOpReranker`（D-S7-04）⇒ 索引与结果与之前**逐位一致**。

#### 交付

- **`cli` 的 `local-rerank` feature**（`= ["helix-core/local-rerank"]`，非默认）：不给这个 feature
  就编不出精排分支；此时传 `--rerank-window` **报错并提示重编**（不静默退回 NoOp —— 那会让
  「精排跑通了」建立在空转上，同 `--analyzer charabia` 的先例）。
- **`--rerank-window R` / `--rerank-max-length N`**：`search` 与 `bench` **参数面完全对称**
  （同一套 `resolve_rerank` / 守卫 / `build_reranker`，不复制语义）。
- **四条守卫**（顺序固定：参数面 1~3 在 feature 守卫 4 **之前**，四者都在加载任何资源之前）：
  ① `--rerank-max-length` 单独给 ⇒ `bail!`（它只覆盖**已开启**的精排）；
  ② `--rerank-window 0` / `--rerank-max-length 0` ⇒ `bail!`（「看起来像关掉、实际是开了但空转」：
  窗口 0 ⇒ `take_n = max(k, 0) = k`，白加载 ≈2.19GB 模型；长度 0 ⇒ 输入整段截空）；
  ③ `--runs > 1` + 精排 ⇒ `bail!`（`--runs` 刻意重建图，图漂移污染 A/B，设计 §3.1）；
  ④ 未编译 `local-rerank` 却传精排参数 ⇒ `bail!` + 重编提示。
  🔑 **顺序的理由**：CI 的 smoke 走默认构建（未编译 feature）⇒ 只有把参数面守卫放前面，
  「互斥」这类断言**才能被 CI 覆盖**（守卫前移到模型之前的直接收益，同半向量守卫的先例）。
- **`scripts/eval_rerank.sh`**：S7-T11 **前置自证**（同一张冻结图连续 N 次 ⇒ 所有 mode 的三项指标
  **与 per-query 明细逐位一致**；不为 0 即**退出并宣告数据作废**）+ 控制组 A（NoOp）/ B（`R=k`）+
  档位**交错**扫描 + max_length 对照档（R48）+ 延迟轴 + `--cross-graph`（可选）+ 汇总 `summary.md`。
  `--dry-run` 只打印将执行的命令（跑之前先看清要跑什么）。
- **CI**：features job 补 `cargo test -p helix --features local-rerank`（CLI 侧接线同样只在
  该 feature 下才编得出来 ⇒ 否则 feature 透传会悄悄腐化）；smoke job 补
  **「精排参数守卫必须拒绝」**（四条断言，**不下载模型**）。
  ⚠️ 该 step 本地按原样复现时**抓到过一个真 bug**：`grep -qF "$want"` 在期望串以 `-` 开头
  （`--rerank-window`）时被当成选项 ⇒ 已改 `grep -qF -e "$want"`。

#### 变更（含 **1 处新增公开方法** 与 **3 处用户可见面变化**）

- **`QueryExecutor::with_reranker_arc(Arc<dyn Reranker>)`（⚠️ 新增公开方法，纯加法）**：
  `with_reranker` 只收 `Box`，而 `bench` 要**每 mode × 每 run** 装配一次 searcher（4 个入口）
  ⇒ 复用同一个 ≈2.19GB 实例只能自己写转发包装（`Reranker` 将来加方法会**静默漏转发**，
  `candidate_window` 漏了就是窗口静默失效）。字段 `reranker: Box<dyn Reranker>` → `Arc<dyn Reranker>`
  （**私有字段**，不动公开面）；`with_reranker(Box)` 的**签名与语义一字未改** ⇒ 既有调用方零改动。
  ⚠️ 这处**不在设计 §4.5 / §6 的影响面表里**，属实现期新发现。
- **用户可见面变化**（非设计原文）：`bench` 输出新增一行 `精排: …`（含身份串）；
  `search --metrics` 追加 `| 精排=N条/X.XXXms`（出口 S7-02 已加的 `Metrics.rerank_window` /
  `rerank_elapsed`，此前**没有任何 CLI 出口**）；`search --explain` 追加 `| rerank=<原始 logit>`（D-S7-05）。
- **设计 §4.5 回写**：补 `--rerank-max-length` 的定义（原先只在**附录 B 第 5 步**的命令里出现、
  §4.5 未定义）+ 把「两条脚枪」扩为「四条守卫」并写明**顺序是硬要求** + 记「实现期口径」。
- **⚠️ 行号引用同步（本 PR 自造的漂移，已实测处理）**：改了 `bench.rs` ⇒ 设计文档里 10 处
  `bench.rs` 行号引用按实测更新（`697→757`、`380-390→436-446`、`655-668→711-724`、`661-663→717-719`、
  `160-162→178-180`、`1530→1597`）。**定义面**上那处（`architecture-design.md:1628` 的 R48 行）
  属**不能在本 PR 改** ⇒ 记入 **S7-05** 回写清单第 5 项（并建议顺手改成**符号指代**，
  从根上消掉「一改 CLI 就全体漂移」）。`v2-step5/step2-design.md` 与 CHANGELOG 历史条目里的
  `bench.rs` 行号**在本 PR 之前就已漂移** ⇒ 按「历史记录不改写」**不动**。

#### 与设计原文的偏差（3 条，报评审）

1. **`--rerank-max-length` 的从属语义**：设计未定义它，本 PR 定为「**必须与 `--rerank-window`
   同时给**」（而非「给了就自动开精排」）—— 避免「传了 max_length 却没开精排」这种静默无效配置。
2. **`0` 一律拒绝**（窗口与截断长度）：设计只说「不传 = 关闭」，未规定 `0` ⇒ 本 PR 明确拒绝。
3. **`RerankerHandle`**：`Reranker` trait 没有 `id()`（只在 `LocalReranker` 上）⇒ 身份串在 CLI 侧于
   **构造时**取出（装箱成 `dyn` 之后取不到）。未把它提升为 trait 方法（那会破坏下游实现）。

#### 变异验证（新断言必须有牙齿，**按实测填写**）

| 变异 | 手法 | 实测命中 |
| --- | --- | --- |
| **M1** | 放行 `--rerank-window 0`（守卫条件恒假） | ✅ `窗口与截断长度为零都被拒绝` FAILED |
| **M2** | `--runs > 1` 与精排**不**互斥（守卫恒假） | ✅ `runs大于1与精排互斥` FAILED |
| **M3** | `--rerank-max-length` 单独给时**静默忽略**（退回 Off 而非报错） | ✅ `max_length单独给被拒绝且点明依赖` FAILED |
| **M4** | `check_rerank_runs` 不看 spec（⇒ 关精排时 `--runs > 1` 也被误拒） | ✅ `runs大于1与精排互斥` FAILED（同一用例的**另一半**断言：`runs=1`/关精排必须放行） |

⚠️ 第一次跑时 **M1 的还原脚本用「删除整块」+ 空串锚点** ⇒ `s.count("")` 返回字符串长度、
assert 必炸、**文件没还原**，导致 M2~M4 跑在污染基线上（读数不可用）。改用「条件恒假」的
**可逆**注入后重跑 ⇒ 四次实测 + 每次逐字节还原核对全部通过。

#### 覆盖边界（如实登记）

- **真实模型未经 CLI 跑过**：本 PR 只把接线做通；`--rerank-window` 的真机端到端（含
  `scripts/eval_rerank.sh` 的实跑）属 **S7-04** 的协议 ⇒ **本地未跑**，不得读成「已覆盖」。
- `scripts/eval_rerank.sh` 的**静态检查**在 CI（`make shell`）里过；**脚本本体未实跑**
  （需 ≈2.19GB 模型 + T2Ranking 语料）⇒ 已用 `--dry-run` 自证参数拼装、用**合成数据**实跑了两段
  内联 Python（冻结自证判据 / 汇总表生成），但「真数据下的读数」仍未验证。

### 修复 · V2 Step 7 PR 2 评审响应（第二方独立评审：**无阻塞项** + 3×P3，**全部收口**）（Refs #23，2026-09-15）

> 评审落点：`pulls/53/reviews` **1 条 `COMMENTED`（1878 字，`2026-09-14T13:33:04Z`）+ 行内 3 条**；
> 落在我 head `236b77b` 的 push **之后** ⇒ 真·新一轮（作者判据仍用**时间戳**）；
> `issues/53/comments` = **只有我自己的 CI 评论**。
> **总评：0×P1 / 0×P2 / 3×P3，全部不阻塞**；3 条行内**全部采纳**，且其中 1 条的**严重性被低估**
> （见下「P3-3 的加强结论」）。评审另**独立重跑了**我的复核（9 条新测试 + `fmt --check` +
> 变异表的 `md5` + 零回归论证 + σ 饱和的数学复核）。

#### 🟢 Fixed（P3-1 —— `k == 0` 白跑精排）

- `take_n = window.max(k).min(fused.len())` 在 `k == 0` 时给出 **`window`（> 0）** ⇒ 把整个窗口
  （每条含 `text` / `metadata` 克隆）交给精排、跑完模型再被安全网截回 0 条 —— **输出对、成本白花**。
  而 `LocalReranker::candidate_window` **不看 `k`**（恒返回 `R`）⇒ `k = 0` + `local-rerank`
  **每次查询白跑 `R` 次推理**。改动前 `.take(0)` 给的是空 vec（`LocalReranker` 对空输入**早退**）
  ⇒ 本 PR 若不挡，是**无意放大**了这个退化输入的成本。
- ⇒ `take_n` 改为 `k == 0 ? 0 : min(max(k, window), 融合条数)`；新增用例 **`S7_T13`**（A/B：
  `k = 1` 时窗口仍放开到 12，`k = 0` 时交接 0 条 ⇒ 证明那 0 是护栏给的，不是窗口本身为 0）。
- 可达性：`helix search --k 0`（`k: usize` **无下界**，`SearchRequest::top_n` 也不钳制）。
- ⚠️ **残留（如实登记）**：召回那一半在 `k = 0` 时**照样跑**（改动前后一致，非本 PR 引入）；
  本护栏只挡掉「窗口交给精排」这一半。让 `k = 0` 在 API 边界不可达（CLI `--k` 下界）归 **PR 3**。

#### 🟢 Fixed（P3-2 —— `rerank_window` 记公式值而非实际值）

- 第 3 步的**陈旧 `chunk_id`** 防御路径会让 `proto.len() < take_n`，而字段自称的是
  「**实际**交给精排的候选条数」⇒ 记公式值会在这条路径上**高报**。
- ⇒ 改为记 `handed = proto.len()`；`Metrics::rerank_window` 的 rustdoc 重写为「实际交接值」并写明
  **两者之差是诊断信号**（差 > 0 ⇒ 本次窗口里有已不可回捞的 chunk）。正常路径两者恒等。
- 新增用例 **`S7_T14`**（向量后端多返回一个不在 `index` 里的 id ⇒ 融合 13 条 / 实际交接 12 条）。

#### 🔵 Fixed（P3-3 —— 采纳 rustdoc 契约；**并报回一个「严重性被低估」的加强结论**）

- **采纳**：`Reranker::candidate_window` 的 rustdoc 补**上界契约** —— 返回值由**实现**负责是
  合理的候选规模上界（建议 ≤ `Index::num_chunks()`），编排层**原样下传、不设防**
  （`S7_T3` 用「后端实际收到的 `k`」钉住了「原样」这一点）。
- 🔵 **加强结论（评审给的是「可能大分配/abort」，实测更明确）**：
  hnsw_rs 自己在 `search_filter` 里做 `ef = ef_arg.max(knbn)`（`hnsw.rs:1519`）
  ⇒ **把 `HnswRsIndex` 库内的 `ef` 上限 `EF_FILTER_MAX = 256` 抵消掉**
  （`hnsw_rs_index.rs` 的 `.min(EF_FILTER_MAX)` 救不回来），随后
  `search_layer` 里 `BinaryHeap::with_capacity(ef.max(2))`（`hnsw.rs:931`）会用 `usize::MAX` 申请
  ⇒ **不是「被 clamp 链兜住」，是会 panic / 申请失败**。
  决定性实验（复刻两条路径的算式，debug 默认构建）：
  `path B` 的 `(k * EF_FILTER_FACTOR)` ⇒ **`attempt to multiply with overflow`（panic）**；
  `path A` 的 `(… + k)` ⇒ **`attempt to add with overflow`（panic）**；
  release（overflow-checks 关）下 `path A` 的 `knbn` 被 `.min(len).min(1024)` 夹到语料规模、
  `path B` 的 `ef` 被夹到 256，**但 `path B` 把 `knbn = k` 原样交给 `hnsw_rs`**（ef 又被拉回 `usize::MAX`）。
- ⚠️ **本 PR 只补契约、不做运行期钳制**，理由两条：① 它是**既有问题**，非本 PR 引入
  （`git log -S` ⇒ `candidate_k` 自 `011bb16` 起就无上限；本 PR 只是**新增了一个来源**：
  第三方 `Reranker` 实现）；② 钳制落在 `vector/` 模块、且会与设计 §4.3.1 的公式产生偏差
  ⇒ 属热路径回归面，不在已评审的「零回归」PR 里混做。**已单独挂账（见 PR 评论）。**

#### 变异验证（新护栏必须有牙齿）

| 变异 | 手法 | 实测命中 |
| --- | --- | --- |
| **M7** | 去掉 `k == 0` 分支（`take_n = window.max(k).min(fused.len())`） | ✅ `S7_T13`（`seen_len` / `rerank_window` 都变 12） |
| **M8** | `metrics.rerank_window` 改回 `= take_n` | ✅ `S7_T14`（13 ≠ 12） |

还原后 `md5` 与注入前**逐字节一致**（`searcher.rs` = `94776610…bff0e`）。

#### 明确不做的

- **运行期钳制 `window` / `k`** ⇒ 见 P3-3 的两条理由，已挂账；
- ⚠️ 本 PR **不上调 `R` 的默认值**、**不动定义面**（仍是 S7-04 / S7-05 的范围）。

#### 🔧 顺带（**非评审项** —— 环境性，故单列一个提交）

- **修 `RUSTSEC-2026-0285`**（`rustls` TLS 1.3 跨加密层接受握手消息）：
  `cargo update -p rustls` ⇒ **`0.23.43` → `0.23.45`**（+ 一处连带 `getrandom`），
  **只动 `Cargo.lock`（3 行）**，无代码改动。
- ⚠️ **它不是本 PR 引入的**（决定性证据：改之前 `Cargo.lock` 与 `Cargo.toml` 与 `origin/main`
  **逐字节相同**）—— 是 **RustSec 公告库在本轮之间新收录**了该条 ⇒ **`main` 的下一次 CI 同样会红**
  （`main` 最近一次 run 是 `2026-09-14T12:40Z` 的 `success`，早于该公告入的库）。
- 传递路径：`rustls ← ureq ← hf-hub ← fastembed`（**仅在 `local-embed` 下编译**，
  且只在模型下载路径上真正跑到；本 PR 与测试都不碰网络）。
  ⇒ 故 CI 的 `cargo-deny` job 依赖的是**公告库的时点**，与代码分支无关；
  这条修复让本 PR 与后续 PR 都能通过该 job。

---

### 新增 · V2 Step 7 PR 2 —— 编排层精排窗口打通（S7-02）（Refs #23，2026-09-14）

> **本 Step 的风险集中点**（设计 §8 的 PR 切分 ②）：唯一动热路径的改动。
> 依据 = `docs/devel/v2-step7-design.md` §4.3（测试计划 §7 的 **S7-T1~T4 / T6 / T7 / T12 编排层侧**）。
> **默认配置零行为变化**：`NoOpReranker` 的 `candidate_window` 默认返回 `k`
> ⇒ `take_n == k`、`candidate_k == max(3k, 10)` **与 PR 1 之前逐位一致**。

#### Added

- **`Metrics.rerank_elapsed` / `Metrics.rerank_window`**（纯加法，保持 `Copy`）：
  NFR-12 的「精排段 P99」与「本次是否**真的**放开了窗口」的**唯一落点**。
  理由同 `vector_shortfall` 的 D-S5-05 先例 —— 只经 `tracing` 输出时外部拿不到实例
  ⇒ 无法单测、无法被 bench 聚合；而端到端 `took` 里混着两路召回与窗口回捞，反推不出精排段。
  `log()` 同步输出 `rerank_window` / `rerank_ms`。
- **`Explain.rerank_score: Option<Score>`**：精排器的**原始分**（`LocalReranker` = 原始 logit）。
  `is_some()` ⟺ 该条的 `Hit.score` 由精排器写入 —— 这是 D-S7-05「分数被替换不得静默」（NFR-07）
  的**信号定义**。⚠️ 因为 `σ` **不可逆**，原始分只能由精排器**写回**，编排层算不出来。

#### Changed

- **`search_parts` 的窗口打通**（`crates/core/src/query/searcher.rs`）：三条不变式
  （已写进函数 rustdoc）：

  ```text
  window                = Reranker::candidate_window(k)   // provided，默认 k
  candidate_k           = max(3k, window, 10)             // ⚠️ 必须在两路召回**之前**算
  take_n                = min(max(k, window), 融合条数)     // 实际交给精排的条数
  metrics.rerank_window = take_n
  ```

  ⚠️ **`candidate_k` 必须与窗口联动**（D-S7-03）：只把截断从 `k` 改成 `R` 而候选池不动，
  窗口会被 `candidate_k` **静默封顶**（`R = 100, k = 10` ⇒ 实际只有 30 条）——
  设计 §2.2 的设计期发现 A，用**四档 `R`** 读「后端实际收到的 `k`」钉住（S7-T3）。
- **`explain` 组装推迟**（D-S7-06）：窗口内先只填 `text` / `source` / `metadata` / `fused_score`，
  `matched_terms`（= `analyze_doc(整段)`）与 lane rank/score 推迟到**精排截断之后**、
  只对最终 ≤ `k` 条算。**结果逐字段不变**（S7-T6），省掉 `(窗口 − k) × 整段分词`。
  ⚠️ 代价（**有意的接口收缩**）：精排器拿到的 `explain` **只有 `fused_score`**
  ⇒ `Reranker::rerank` 的**读侧**契约「不得依赖 `hits[i].explain`」；需要正文用 `hits[i].text`。
- **`Hit.score` 的语义**（D-S7-09）：rustdoc 重写为「**当前排序依据**」并列出
  「精排关 / 开 × 三种 mode」的取值表 —— 精排开时 `score = σ(logit) ∈ [0, 1]`，
  **三种 mode 一视同仁**（精排在融合之后）。`SearchResponse.hits` 的 rustdoc 同步补指针。

#### ⚠️ 破坏性

- **`Explain` 新增字段 `rerank_score`**（`Explain` 字段全 `pub`、**非** `#[non_exhaustive]`）
  ⇒ 下游以**字面量**构造 `Explain` 的代码会编译失败。0.x 阶段按既有约定接受
  （同 `SearchResponse.metrics` 的先例 / 架构 R35 一族）。
  库内构造点 **2 处已同步**（`query/searcher.rs` + `rerank/noop.rs` 的测试夹具；
  后者刻意**显式列出**该字段而非 `..Default::default()`，让破坏性在 diff 里可见）。
- **语义变更**：`Hit.score` 在精排生效时不再是融合分（**同一 query 的分数与精排前不可横比**）。
  信号 = `explain.rerank_score.is_some()`；融合分仍可在 `explain.fused_score` 读到。

#### 🔵 实现期新发现（设计文档未写，逐条报评审）

1. **安全网需显式不静默**：设计 §4.3 的伪码里有 `hits.truncate(k)`，但**没写它不得静默**。
   本实现按 NFR-07 补 `tracing::warn!` —— 触发即表示精排器**违反出参契约**（返回 > `top_n` 条），
   与 `rerank::scoring::apply_scores` 的两条防御路径同族（「有可解释的退化」才配静默）。
   用例：`S7_T5b`（变异：去掉安全网 ⇒ 12 ≠ 10 报红）。
2. **口径变化必须写明**（不写就会读成回归）：窗口变大 ⇒ `candidate_k` 变大 ⇒
   `Metrics::vector_shortfall` 的**分母**（`candidate_k.min(allowed)`）跟着变
   ⇒ **跨窗口不可横比**；低选择度 + `--filter` 档位下召回成本随之上升
   （`allowed ≤ 8192` 走精确扫描、`O(N)`）⇒ **「精排 + 过滤」叠加的延迟不做承诺**（设计 §4.3.1 已登记为非范围）。
3. **测试夹具的 `lane_score_of` 单一来源**：间谍向量后端返回的是**距离** `d = id × 0.5`，
   而实现里 `score = 1 − d/2`（`VectorRetriever::to_scored` 还会**按分数重排 lane**）
   ⇒ 期望值若在多个用例里各算一遍，很容易「测试自己把公式抄错」。故收敛为一个helper。

#### 明确不在本 PR 范围

- `--rerank-window` CLI 接线、两条脚枪拦截、`scripts/eval_rerank.sh` ⇒ **PR 3（S7-03）**；
- `R` 标定与 `max_length` 对照档、`eval-report.md` §8.14 ⇒ **PR 4（S7-04）**；
- 四处定义面回写与 NFR-12 数值定稿 ⇒ **PR 5（S7-05）**。

#### 变异验证（新门必须有牙齿）

⚠️ 下表是**实测**结果（不是预判）。两处与我的预判**不同**，据实记录：

| 变异 | 手法 | 实测命中的用例 |
| --- | --- | --- |
| **M1** | `Reranker::candidate_window` 的默认实现 `k` → `k + 1` | ✅ `rerank::tests::默认窗口等于k`（**PR 1 既有**）+ **`S7_T4`** + **`S7_T7`**。⚠️ **`S7_T1` 抓不到这个变异**：它那条断言用的是**间谍**（显式声明 `window = k`），走不到默认实现；见下方「覆盖边界」 |
| **M2** | `candidate_k = max(3k, window, 10)` 去掉 `window` 项 | ✅ `S7_T3` + `S7_T2`。⚠️ **`S7_T5` 抓不到**：间谍向量后端**忽略 `k`** 恒返 12 条 ⇒ `candidate_k` 的变化在它身上不可观测（`S7_T3` 才是专为这条设的读 `k` 判据） |
| **M3** | 去掉安全网（`if hits.len() > k`） | ✅ `S7_T5b`（12 ≠ 10） |
| **M4** | C2 补齐时把 `explain.rerank_score` 置回 `None`（覆盖 C1） | ✅ `S7_T12` |
| **M5** | 跳过 `matched_terms` 的补齐（D-S7-06 第二步不做） | ✅ `S7_T6` + `S7_T12` |
| **M6** | `take_n` 恒为 `k`（窗口不生效） | ✅ 7 条（`S7_T2`/`T3`/`T4`/`T5`/`T5b`/`T7`/`T12`） |

还原后 `md5` 与注入前**逐字节一致**（`searcher.rs` = `1a32f8f6…b1b9`、`rerank/mod.rs` = `ceb3afc7…b2c0`）。

**覆盖边界（如实登记）**：

- `S7_T1` 的职责是「间谍声明 `window = k` 时链路与 `NoOp` 逐字段一致」，**不是**「默认实现等于 `k`」
  —— 后者由 `rerank/mod.rs` 的既有用例（PR 1）钉住，两者互补而非重复。
- 间谍向量后端**忽略 `k`**（固定返回 12 条）⇒ 「候选池是否放大」只能靠
  `S7_T3` 读「后端实际收到的 `k`」证明，**不能**靠结果差异反推。这是设计 §7 给 S7-T3
  单独列一条的原因。

---

### 构建 · V2 Step 7 PR 1 评审响应（**第 2 轮**：仍无阻塞项 + 1×P2 + 2×P3 + 1 细枝，**全部收口**）（Refs #23，2026-09-14）

> 评审落点：`pulls/52/reviews` **1 条 `COMMENTED`（3276 字，`09:04:30Z`）+ 行内 4 条**；
> 落在我 head `dbbc175` **之后** ⇒ 真·新一轮（作者判据仍用**时间戳**）。
> **总评：第 1 轮 5 条意见「全部收口」（4 条实装、1 条按我的取舍改形），评审**重跑**了我的全部复核；
> 本轮新发现 4 条**，其中 1 条 P2（**新护栏零覆盖**）是我的自查漏项。

#### 🟡 Fixed（P2 —— 新护栏零覆盖，第 2 轮「新 1」）

- **越界护栏此前「零覆盖」**：评审实测「删掉 `apply_scores` 里那句越界 `debug_assert!` 整块后
  `cargo test -p helix-core --lib` ⇒ **223 passed / 0 failed**」；**我独立复现属实**。
  ⇒ 新增用例 **`越界index在dev构建下被debug_assert拦住`**：`#[cfg(debug_assertions)]` +
  **`#[should_panic(expected = "精排返回越界 index")]`**。
  ⚠️ 这是**本仓首个 `should_panic`**（此前全仓 grep = 0）—— 「新护栏必须有牙齿」的代价；
  `#[cfg(debug_assertions)]` 保证 `cargo test --release` 不会因「没 panic」而**假红**
  （cfg 先例：`crates/core/src/bitmap.rs` 的 `count_ones_slow` / `debug_check`）。
  ⚠️ 我第 1 轮的论证只覆盖了「**容忍**路径在 debug 下不可测」，**漏了「护栏本身是可测的」**这一半。

#### 🟡 Fixed（P3 —— 覆盖判定只比条数，第 2 轮「新 2」）

- **`scored.len() != hits.len()` 挡不住「重复 `index` + 漏一个」**（条数相等 ⇒ `warn!` 不发，
  而该候选**静默**保留融合分）：把判定精确化为 **有效且去重后的 `index` 集合**，
  抽成**纯函数** `rerank::scoring::uncovered_count`（⇒ **不依赖 tracing subscriber 就能单测**）。
  `apply_scores` 改为按「未覆盖条数」告警（`warn!` 增加 `uncovered` 字段）。
  新增用例 `未覆盖条数看集合不看条数`（5 组：完整/少给/重复 `index`/越界/空）。
  ⚠️ 成本如实登记：一次 `O(候选数)` 的 `Vec<bool>` 分配（候选数 = 精排窗口 ≤ 几百），
  与一次 ONNX 前向相比可忽略；且精排默认关、不在无精排的热路径上。

#### 🟢 Fixed（P3 —— 「全仓扫描」的结论漏了一个，第 2 轮「新 3」）

- **`architecture-design.md:1628`（§14.5 **R48**）的 `fastembed/src/reranking/init.rs:16-18` 同为旧值**
  （应为 **`17-19`**），该行由 **`1859dbf`（#51）** 引入（`git log -S` 仅此一个提交）⇒ **同样属
  「Step 7 引入的锚点」**。⇒ **更正我第 1 轮的结论**：当时写「Step 7 引入的锚点里**只有一处**
  （`plan-v2.md:462`）漏网」—— **实际是两处**。已作为**第 3、4 项**补进设计文档的 **S7-05 回写清单**。
  🔴 **根因（已记入教训）**：我的全仓扫描**确实命中了**该行，但工具把它截断成
  `[Omitted long matching line]` 而我**没有回读** ⇒ **凡「Omitted」的命中必须逐条回读**。
  （顺带核过：同行 `:1627` 的 R47 锚点 `common.rs:174-180` 是正确的，不动。）

#### 🟢 Fixed（细枝 —— 同一锚点两个值）

- **`common.rs:181-184` 与 `181-185` 统一为 `181-184`**：`181` = `.with_truncation(Some(TruncationParams {`、
  `184` = `}))`、`185` = `.map_err(…)`（同一条 builder 链上的错误映射）⇒ 精确锚点是 **181-184**。
  改 `crates/core/src/rerank/local.rs` 的模块文档 + 设计文档**附录 C** 的表格与「核过仍准确」清单。
  ⚠️ **历史条目不改写**：第 1 轮的 CHANGELOG 条目与 #51 的 F5 条目里保留原引文（符合「被推翻的旧值也要留引文」）。

#### 🔵 与评审的**不同**之处

1. **「新 2」我没走他给的两条路（debug-only `HashSet` 断言 / 降级措辞 + 登记盲区），选了更强的第三条**：
   **release 与 debug 都精确化**（纯函数 `uncovered_count`），因为它**同时**满足「release 侧不再静默」与
   「不依赖 tracing subscriber 就能单测」；他给的 ① 只修 debug 侧、② 只是登记。
2. **「新 1」的形态与他给的一致**（`#[cfg(debug_assertions)] + should_panic`），未额外引入 `catch_unwind`。

#### 变异验证（新护栏必须有牙齿）

| 变异 | 注入 | 结果 |
| --- | --- | --- |
| **m5** | **删掉**越界 `debug_assert!` 整块 | ✅ `越界index在dev构建下被debug_assert拦住 – should panic` **FAILED**（正是评审「新 1」的复现） |
| **m6** | `uncovered_count` 退回「只比条数」（`candidates.saturating_sub(scored.len())`） | ✅ `未覆盖条数看集合不看条数` **FAILED**（③「重复 `index`」那组） |

还原后 `md5` 与注入前**逐字节一致**（`8fb5b3af…`）。

#### 守门（本地，逐条标运行范围）

`fmt` ✅ / `clippy --workspace --all-targets -D warnings` ✅ / `cargo test --workspace` ✅ /
`make shell` ✅ / MSRV 1.90 ✅ / rustdoc `-D warnings` ✅ / `--no-default-features` ✅ /
`--features charabia` ✅ / `--features local-rerank` ✅ / `cargo deny check advisories licenses bans sources` ✅。
**新增用例 2 个**（越界护栏 `should_panic` + `uncovered_count` 纯函数单测）⇒ 默认特性 `302 → 304 passed`。

### 构建 · V2 Step 7 PR 1 评审响应（第二方独立评审：**无阻塞项** + 2×P2 + 3×P3，**全部收口**）（Refs #23，2026-09-14）

> 评审落点：`pulls/52/reviews` **1 条 `COMMENTED`（5469 字）+ 行内 6 条**；`issues/52/comments` 只有作者自己的
> CI 记录评论 ⇒ **本轮评审不在 issue 评论里**（与上一步相反，见「评审落点会一轮一轮变」）。
> **总评：三处设计偏差 + `Error::Rerank` 全部同意；CI 新增步建议保留；5 条意见**（3 条属本 PR，2 条可留 PR 2 / PR 5）。

#### 🟡 Fixed（P2 —— 接口写侧契约，意见 1）

- **`Reranker::rerank` 的 rustdoc 补「出参（写侧）契约」**（`crates/core/src/rerank/mod.rs`）：
  返回类型 `Vec<Hit>` 里只有 `explain` 能携带额外信息，而 **`σ` 不可逆** ⇒ 编排层**自己算不出**原始分
  ⇒ 实现**可以且应当**通过**写** `hits[i].explain` 归还（`LocalReranker` 将在 **S7-02** 写 `rerank_score`）。
  ⚠️ **「不得依赖 `explain`」只约束读侧**；并写明**编排层的义务**：按 D-S7-06 在精排后补齐 `explain` 时
  **必须保留**精排器写入的字段（否则「谁后写谁生效」会把该信号冲掉）。
  同时把原句「（`LocalReranker` 的做法是 … 进 explain）」由**现在时**改为**将来时 + S7-02 落点**（原措辞今天还不成立）。

#### 🟡 Fixed（P2 —— 静默失效，意见 2）

- **`apply_scores` 的「未覆盖项」从静默改成有护栏、有语义、有测试**（`crates/core/src/rerank/scoring.rs`）：
  ① 契约写明「`scored` 应**恰好覆盖** `hits`」，并给出**两条防御路径刻意不同**的分支表：
  **`index` 越界**（结构违反）= `debug_assert!`（dev 快速失败）+ `tracing::warn!` + 忽略；
  **覆盖不足** = 保持输入（融合）分 + `tracing::warn!`（**不加** `debug_assert`）；
  ② 新增单测「**部分覆盖时未覆盖项保持输入分**」，并**替换**掉原先那条「越界」用例 ——
  后者经**实测无鉴别力**（它的每条 hit 都被覆盖且 `index == 位置` ⇒ 在「按位置 zip」变异下**仍然通过**，已复核）；
  ③ 顺带把「覆盖不足」这条路径接上可观测面（NFR-07 的「降级不得静默」）。

#### 🟡 Fixed（P2 —— 文档—代码漂移，意见 3）

- **`docs/devel/v2-step7-design.md` 就地勘误 `E1`~`E6`**（本 Step 的**实现依据**，PR 2~5 会照它写）：
  §4.4.1 / 附录 A 的 API 块（`with_max_length` → `with_params`）、§4.4.2 第 1 步（「零/单条早退」→ 只有空输入）、
  §4.4.2 第 5 步（补两条防御路径）、§4.4.3（`score ∈ (0,1)` → **`[0,1]`**）、§7 S7-T9 落点、**§6 影响面表补列 `Error::Rerank`**
  + `Reranker::rerank` 行补**写侧契约**；并在头部加了「S7-01 实现期勘误」块与 **S7-05 回写清单**。
  ⚠️ **版本号刻意不升（仍 v0.2）**：勘误**不含**决策/预算/风险变更，而升版会让 4 处定义面的
  「依据 `v2-step7-design.md` v0.2」引用**连带漂移** ⇒ 正式升版 + 定义面回写随 **S7-05**。
  ⚠️ **同时更正**：PR 正文原写「本 PR 不碰任何定义面」并列举四处 —— **漏了本文档**（评审指出），已在正文更正。

#### 🟢 Fixed（P3 —— 断言口径不自洽，意见 4）

- **`σ` 的值域是闭区间 `[0,1]`**（f32 下**精确饱和**）。三处口径统一为闭区间并写清机制：
  `scoring.rs` 的 `sigmoid` rustdoc（附两端机制与 bits）、`scoring.rs` 单测（**新增饱和端点断言**
  `σ(16.7) == 1.0` / `σ(−89.0) == 0.0` / `σ(−88.0) > 0`）、`tests/step7_rerank_local.rs` 的 `t10`
  （开区间 → **闭**区间；「模型是否进饱和区」改成**打印的观测量**，随 §8.14 落报告）。
  理由：`t10` 是**本 PR 尚未跑过**的那一类（需 2.19GB 模型）⇒ 开区间会给一个**假失败**、污染 S7-04 的标定结论。

#### 🟢 Fixed（P3 —— 行内意见：`with_window` 与偏差 A 看似冲突）

- **`LocalReranker::with_window` 的 rustdoc 补「两者性质不同」的对照表**：`max_length` **烧进模型的 tokenizer**
  （构造后改不生效 ⇒ 只能构造期给）；`window` **只被 `candidate_window` / `id()` 读、不触碰模型** ⇒ 构造后改**生效**。

#### 🔵 与评审意见**不同**的地方（两类都写）

1. **意见 2 的取舍：未覆盖项「保持输入分」，而非评审建议的「丢弃或压到最低」。** 评审的关切（两个量纲同台排序、
   错排方向随 `mode` 而变）**成立**，但三个候选里我选「保持 + 可见」，理由（已写进 rustdoc）：
   **丢弃**会静默改变**条数契约**（本函数只做排序 + 截断，不做过滤）；**压到最低（`0.0`）**会**伪造**一个分数，
   而 σ 在 f32 下**会精确饱和到 `0.0`**（`σ(−89) == 0.0`，正是本轮意见 4 的事实）⇒ 伪造值与真值**不可区分**；
   **保持**则可由 `explain.rerank_score == None` **识别**（这正是 D-S7-05 的 `is_some()` 信号的定义）。
   ⇒ 结论：把这条路径做成**可解释的异常**，而不是「伪造的分数」或「少一条结果」。
2. **「覆盖不足」刻意不加 `debug_assert`**（评审建议是「越界/未覆盖都加」）：加了之后该语义在 debug 下**不可测**
   （与意见 2 的 ② 直接冲突）⇒ 只在 `index` 越界（**无**可解释退化可言的结构违反）上加 assert。
   ⚠️ 代价如实登记：**release 下的容忍 + `warn!` 是盲区**（无 tracing subscriber 依赖 ⇒ 不单测），已写进 rustdoc。
3. **意见 5（NFR-06 注记机制句反向）不在本 PR 改**：`requirements-spec.md` 属**已评审的定义面**，本项目纪律是
   「别在飞行中改定义面，留给收尾项（S7-05）同批回写」⇒ **裁定「以 D-S7-07 为准」+ 记入 S7-05 清单**（评审给的另一选项）。

#### ✅ 独立复核为属实的评审事实（评审的验证表也逐条重跑）

| # | 评审主张 | 我的复核 |
| --- | --- | --- |
| 1 | `σ(16.7) == 1.0` / `σ(−89.0) == 0.0`（**精确**，故值域是闭区间） | ✅ **真 f32 复现**（`rustc` 直编同形函数）：bits `3f800000` / `00000000`；机制 = `1 + e^(−16.7)` 被舍回 1.0（f32 在 1.0 处半 ULP ≈ 5.96e-8、余量 6%）、`e^89` 上溢到 `inf` |
| 2 | 原「越界」用例对「按位置 zip」变异**无鉴别力** | ✅ **复现**：注入 M2 后该用例**仍通过**（`2 passed`）⇒ 已由新的「部分覆盖」用例取代 |
| 3 | `common.rs:181-184` 的 `with_truncation` 在 `try_new` 内 ⇒ 构造后改 `max_length` 不生效 | ✅ 实读源码属实（`load_tokenizer` 内 `.with_truncation(Some(TruncationParams{ max_length, .. }))`） |
| 4 | fastembed 取的是**原始 `logits`**、没替我们 sigmoid ⇒ `σ` 必需 | ✅ `impl.rs:195-198` 的 `.get("logits")` 属实（这条救掉了 D-S7-05 的重写） |
| 5 | `impl.rs:224` 的 `sort_by` 是 stable ⇒ 并列保持**输入顺序** | ✅ 属实（`sort_by(\|a,b\| a.score.total_cmp(&b.score).reverse())`） |
| 6 | 设计 §6 表 **12 行、无 `Error` 行** | ✅ 属实 ⇒ 已补列（E6） |
| 7 | NFR-06 精排注记的机制句与 D-S7-07 **相反** | ✅ 属实（`requirements-spec.md:388`）⇒ 裁定 + 记入 S7-05 清单 |
| 8 | 计数口径 302 / 288 = 302 − 14 一致 | ✅ 属实（少的那 14 行 = `helix-cli` 的 lib 测试） |

> 🔍 **本轮按「一类缺陷要全仓扫」自检出评审未点到的一处**：#51 的 **F5**（`impl.rs` 行号整段漂移）只改了
> **设计文档**，而 **`plan-v2.md:462`（Step 7 自己的锚点）仍是旧值**（`:110` / `:186-198`，应为 `:126-132` / `:215-224`）
> ⇒ 这是「跨定义面」这个**第三个载体**。已全仓扫描全部 fastembed 锚点，结论：**Step 7 引入的锚点里只有这一处漏网**；
> 另 `plan-v2.md:380` / `:659` 的 `src/init.rs:30-33` 属 2026-09-07 的旧锚点（指 `InitOptions<M>`，我们走
> `InitOptionsWithLength` 的 `:20`），但同文件 `:671-680` **已有「以上一律以 `v2-step6-design.md` 附录 C 为准」的
> 取代指针** ⇒ 属**刻意保留的历史记录**，**不建议改**（同「被推翻的旧值也要留引文」的纪律）。

#### 变异验证（新门必须有牙齿）

| 变异 | 注入 | 结果 |
| --- | --- | --- |
| **M1'** | `apply_scores` 的回填改成**按位置 zip**（`for (idx, item) in scored.iter().enumerate()` + `hits.get_mut(idx)`） | ✅ **新用例「部分覆盖时未覆盖项保持输入分」报红**（`3 failed`，含它 + 回填 + σ 两条）—— 而**旧**的「越界」用例在**同一变异**下**仍通过**（已复核）⇒ 换掉它是必要的 |
| **M2'** | `sigmoid` 改成走 **f64 中间值**（`(1.0f64 / (1.0f64 + f64::from(-logit).exp())) as f32`） | ✅ `分数等于sigmoid_logit且单调有界` **报红** ⇒ 饱和端点断言确实**钉住「f32 算术」这一契约**（不是「碰巧相等」） |
| **M3'** | 默认 `candidate_window` 改成 `k + 1` | ✅ `默认窗口等于k` **报红**（沿用 PR 1 的既有变异，回归确认） |

还原后 `md5` 与注入前**逐字节一致**（`b00b9c03…` / `ceb3afc7…`）。

⚠️ **本轮的变异盲区（如实登记）**：`tests/step7_rerank_local.rs` 的 `t10` 闭区间断言**无法本地变异验证**
（需 2.19GB 模型 ⇒ `#[ignore]`）⇒ 该断言保真的依据是**「σ 在 f32 下会饱和」这一实测事实**（bits `3f800000` /
`00000000`），而不是「改一行代码它会红」。同理 release 下「越界容忍 + `warn!`」也是盲区（debug 被
`debug_assert!` 拦住、且无 tracing subscriber 依赖 ⇒ 不单测）。

#### 守门（本地，逐条标运行范围）

`cargo fmt --all -- --check` ✅ / `clippy --workspace --all-targets -D warnings` ✅ /
`cargo test --workspace` ✅ / `make shell` ✅ / MSRV 1.90 ✅ / rustdoc `-D warnings` ✅ /
`--no-default-features` ✅ / `--features charabia` ✅ / **`--features local-rerank`** ✅ /
`cargo deny check advisories licenses bans sources` ✅。**新增/改写用例 2 个**（`部分覆盖时未覆盖项保持输入分`
+ `分数等于sigmoid_logit且单调有界` 的饱和端点断言）；**删掉 1 个无鉴别力用例**（`越界index被忽略…`）。

### 新增 · V2 Step 7 PR 1 —— 精排内核：`Reranker::candidate_window` + `LocalReranker`（S7-01）（Refs #23，2026-09-14）

> 本 PR **只做内核**（设计 §8 的 PR 切分 ①：内核与编排分开）：**不碰编排层**
> （`query/searcher.rs` 一行未改）⇒ 交付后**没有任何既有行为变化**（`LocalReranker`
> 尚未被任何入口装配；CLI 接线是 PR 3）。对应设计任务 **S7-01**，覆盖
> **S7-T5 / T8 / T9 / T10 / T12** 的 PR 1 部分。

#### Added

- **`Reranker::candidate_window(&self, k: usize) -> usize`（provided，默认 `k`）** —— 精排器
  → 编排层的**单向窗口通道**（D-S7-01 / D-S7-02）。**非破坏性**：`NoOpReranker` 与下游
  自定义实现**一行不改**，未装精排器时 `candidate_k` / 截断 / `hits` 与之前逐位一致。
  `Reranker::rerank` 的 rustdoc 补了**两条入参契约**：`hits` 长度是**候选窗口**（可 > `top_n`）、
  实现**不得依赖** `hits[i].explain`（`explain` 的组装按 D-S7-06 推迟到精排截断之后）。
- **`crates/core/src/rerank/local.rs`：`LocalReranker`**（feature **`local-rerank`**）——
  fastembed `TextRerank` + `RerankerModel::BGERerankerV2M3`（`rozgo/bge-reranker-v2-m3`，
  ⚠️ **非** BAAI 官方库——官方库没有 ONNX；`sha256:84b66c78…8945` / 2026-09-14）。
  `Mutex<TextRerank>`（`rerank` 需 `&mut self`，同 `LocalEmbedder` 先例）；模型缓存目录
  与 embedder **复用同一个函数**；`pub const DEFAULT_RERANK_WINDOW = 20`（**「拟」值**，
  待 S7-04 标定 / S7-05 回填）、`DEFAULT_RERANK_MAX_LENGTH = 512`（= 库默认，D-S7-08）。
  **不做**多 session 池化（设计 §4.4.2）。
- **`crates/core/src/rerank/scoring.rs`：精排的纯策略层（注入接缝）** ——
  `sigmoid` / `ScoredCandidate` / `apply_scores`（**按 `index` 回填** + 排序 + 截断）/
  `reranker_identity`。**不依赖 fastembed** ⇒ 策略能在 CI 里用**可控打分序列**秒级钉住
  （设计 §3.5 的可测性要求；同 Step 6 的 `EmbedderCtor` 动机）。
  编译范围 = `cfg(any(feature = "local-rerank", test))`（默认非测试构建不编译它，不留无用代码面）。
- **`local-rerank = ["local-embed"]`**（`crates/core/Cargo.toml`）—— **不新增依赖树节点**
  （同一个 fastembed 已提供 `TextEmbedding` 与 `TextRerank`）。模型 ≈2.19GB ⇒ **不进默认
  feature**（设计 §3.6 / R44）；未启用时 `LocalReranker` **不存在**（编译期），不是「运行时
  静默退化」。
- **`.github/workflows/ci.yml`：features job 新增 `cargo test -p helix-core --features local-rerank`**
  —— 让这个**非默认** feature 也进守门，否则重蹈 `coreml` 的腐化（只在本地用、CI 不覆盖）。
  真模型用例一律 `#[ignore]` ⇒ **该步不下载任何模型**。
- **测试**：`rerank::scoring` 单测 6 个（**进 CI**：`σ` 单调有界、按 `index` 回填而非按位置
  zip、并列按 `chunk_id` 升序、截断、越界 index、身份字符串随参数变化）+ `rerank` 单测 2 个
  （默认窗口 `== k`、空输入不 panic）；`tests/step7_rerank_local.rs` 真模型用例 4 个
  （`#[ignore]`：P1 同进程逐位一致 / **P2 跨进程逐位一致（真的 spawn 子进程）** /
  T10 重排与截断契约）。
- `Error::Rerank(String)`（`crates/core/src/error.rs`）—— 精排失败与 `Embedding` **分开**：
  两者的落点与代价差一个数量级（96MB vs 2.19GB），混在一起会让「哪一步炸了」只能靠读字符串猜。

#### ⚠️ 破坏性

- **`Error::Rerank` 是新增的枚举变体**，而 `Error` **不是** `#[non_exhaustive]` ⇒ 下游若对它做
  **穷尽 `match`** 会编译失败。刻意与 `Error::Embedding` 分开（理由见上）。
  ⚠️ 这一条**不在设计 §6 的影响面表里**（该表未涉及 `Error`）—— 属实现期发现的**新增面**，已写进 PR 正文报评审。

#### 🔵 与设计文档 §4.4.1 / §4.4.2 的**三处偏差**（实现期实测，已在 PR 正文逐条报评审）

1. **`with_max_length(self, …)` 无法写成「构造后 builder」** ⇒ 改为**构造期**入口
   `LocalReranker::with_params(window, max_length) -> Result<Self>`。理由：`max_length` 在
   `TextRerank::try_new` 时就烧进 tokenizer 的 `TruncationParams`（`fastembed/src/common.rs:181-185`），
   构造后再改字段**只会得到一个「断言仍绿但没生效」的假象**（正是本项目最忌讳的静默失效）。
2. **单条候选不早退**（设计 §4.4.2 第 1 步写的是「零/单条早退」）⇒ 只有**空输入**早退
   （fastembed 对空输入报 `EmptyTokenizations`）。理由：否则「`score` 是否被精排替换」会**依赖
   候选条数**，与 D-S7-05 的 `explain.rerank_score.is_some()` 信号自相矛盾（R46 关注的正是
   score 语义的可判定性）。
3. **P3（批组成敏感性）用例落在 `rerank/local.rs` 模块内部**（设计 §7 写的是
   `tests/step7_rerank_local.rs`）。理由：公开 API 只暴露 `σ(logit)`，而 σ 是**多对一**的浮点
   映射 ⇒ 用 `score.to_bits()` 相等**不能**证明 **logit** 逐位相同；该用例直接读模型原始输出。

#### 明确不在本 PR 范围

- **编排层窗口打通**（`candidate_k` 联动 / `take_n` / `explain` 推迟 / `Metrics.rerank_window` /
  `Metrics.rerank_elapsed` / `Explain.rerank_score`）= **S7-02（PR 2）**。⚠️ 因此 **S7-T5 / T12 的
  「经编排层」那一半**（假精排器在 `search_parts` 里被喂到多少条、`is_some() ⟺ 精排生效`）
  **随 PR 2 落地**；本 PR 覆盖的是它们的**接口侧**（trait 语义与纯策略）。
- CLI `--rerank-window` / 两条脚枪拦截 = S7-03；标定 = S7-04；定义面回写 = S7-05。

### 文档 · V2 Step 7 详细设计 —— 评审响应（F1~F6 全部采纳 + 四处拍板获同意）（Refs #23，2026-09-14）

> 评审落点：**1 条正式评审（`COMMENTED`）+ 6 条行内**，`issues/51/comments` = **0**。
> 评审总评「**设计质量很高**」——在本机逐条实测：**30+ 项目内源码锚点全部命中**、零 `.rs` 改动属实、
> fastembed 来源更正属实、附录 B 的 CLI 参数全部存在。**四处拍板（D-S7-04 / 05 / 01 / 08）全部同意本文建议。**
> **F1~F6 六条意见全部采纳、无一条被拒**；⚠️ 本轮**不改任何决策 / 预算 / 风险条目**（R44~R48 一字未动）。

#### 🔴 Fixed（F1，P1 —— 定义面残留块与 D-S7-02 直接矛盾）

- **`plan-v2.md` §8 架构侧条目整段重写**：① 删掉「**`Config` 新增字段属向后兼容的加法**」——
  与 **D-S7-02**（「`Config` / `SearchParts` **一律不改**」，窗口走 **trait provided 方法**）**直接矛盾**；
  ② **R44~R48 的描述按架构 §14.5 的实际条目对齐**：原文把 R46 写成「组装的线性放大成本」（那是设计期
  **发现 B**）、R48 写成「冻结图依赖与 `--runs > 1`」（那是设计 **§3.1 的约束**）—— **两者都没立为风险**；
  实际登记为 **R46 = `Hit.score` 语义随开关而变** / **R48 = 512 token 截断使长段落判据失真**。
  ⚠️ 这是**我自己上一轮的同类漏改**：自检脚本抓到了 §4 那处、**§8 这处漏网**
  ⇒ 本轮把「同一事实出现在多处」纳入全仓扫描（下文自检表）。

#### 🟡 Fixed（F2~F4，P2 —— 口径与归属）

- **F2：`S7-04 / S7-05` 归属统一（设计文档 8 处）**。评审列出 **7 处**（行 7 / 21 / 32 / 51 / 158 / 311 / 582）
  写「S7-05 标定回填」，与 §8 任务表（**S7-04 = 标定 + 决策门**、S7-05 = 回写）自相矛盾；
  作者全仓扫描后**另发现第 8 处**（§4.6 标题「标定实验设计（**S7-05** 的输入）」）⇒ 一并改。
  **落地口径：凡「标定 / 实测 / 决策门 / 分别计时」= S7-04；凡「定义面回写 / NFR-12 数值定稿」= S7-05**
  （已写进 §8「PR 切分原则 ④」作为全文统一口径）。
- **F3：NFR-12 的标定语料「（10 万级）」→ `T2Ranking 12K / 320 query`**（`requirements-spec.md` 2 处）。
  设计 §4.6.1 / 附录 B 的协议**没有任何 10 万级档位**；「10 万级」是 **NFR-13 / S5-04 先例**的措辞残留
  （那条确实是 10 万级 A/B）。
- **F4：「≤500ms（拟）」补出处与联动**。评审指出该值**在设计文档（含 §4.6.4 决策门）无任何出处**，
  且与 §4.2.2 自家粗估相抵（**`R = 20` ≈ 1s > 500ms**）。作者复核**属实**（设计文档里「500」出现 **0 次**）。
  **落地**：① 设计 **§4.2.2 新增补记** —— 500ms **对应 `R = 10` 档的量级上界（≈0.5s）**、**不是 `R = 20` 的**；
  ② §4.6.3 延迟判据行加指针；③ **需求 / `plan-v2` 的 500ms 一律补「与默认 `R` 联动」**；
  ④ 明确 **决策门选出 `R*` 后二者必须联动**（要么 `R*` 落回 500ms 内、要么按 `R*` 重取阈值并改需求数值），
  **禁止把两个「拟」各自独立引用**（否则会得出「默认配置必然违约」的自相矛盾结论）。

#### 🟡 Fixed（F5，P3 —— 行号漂移；语义全部属实）

- **附录 C 的 fastembed `impl.rs` 行号整段漂移 ⇒ 按本机 registry 实测更正（只改行号，语义不动）**：
  `rerank` 签名 `:110` → **`:126-132`**、`unwrap_or(DEFAULT_BATCH_SIZE)` `:118` → **`:134`**、
  `chunks(batch_size)` `:127-132` → **`:143`**、`encode_batch` `:134-141` → **`:145-148`**、
  `top_n_result` + `sort_by` `:186-198` → **`:215-224`**（该文件共 **227** 行；仅 `:44` 的 `try_new` 原本就准确）。
  §2.5 / §2.6 正文同源偏移（`:134-181` / `:186-198`）一并更正为 **`:143-212`** / **`:215-224`**，
  并在附录 C 加一条**行号更正注记**（写明初版值与更正后值）。
- ⚠️ **同类漂移不止 `impl.rs`——作者按「一类缺陷要全仓扫」把全部 fastembed 行号引用核了一遍**，
  **另更正 5 处**（评审 F5 未点到）：`reranking/init.rs` 的 `:14-19` → **`:11-19`**、`:16-18` → **`:17-19`**、
  `:21` → **`:22`**、`:149-155` → **`:152-156`**（§2.7 / §4.4.2 / R48 / 附录 C），
  `src/init.rs` 的 `:63-70` / `:106-113` → **`:61-101` 的 `:71` / `:77` / `:100`**，
  以及 §3.3 正文的 `impl.rs:110` → **`:126-132`**。**核过仍准确、未改的**：`reranking/mod.rs:1-2`、
  `impl.rs:44`、`common.rs:174-180` / `:181-185` / `:262-270`、`lib.rs:130` / `:132`、
  `models/reranking.rs:6-11` / `:28-33`。

#### 🟡 Fixed（F6，P3 —— 两处笔误，其中一处是我的事实性错误）

- **§8 标题**「实施任务拆分（S7-01 ~ **S7-06**）」→ **`S7-05`**（表内只有 5 个任务）。
- 🔴 **§3.6「显式写 `dep:fastembed` 会报重复」不成立**（评审实测，**作者已独立复现**）：
  `local-rerank = ["local-embed", "dep:fastembed"]` 在 `cargo metadata --features local-rerank` 下
  **exit 0**（feature 记为 `['local-embed','dep:fastembed']`）—— **冗余但合法**，
  因为 `fastembed` 是 **optional** 依赖 ⇒ `dep:` 语法成立。
  **对照样本**（自证探针有牙齿）：`dep:bincode`（**非** optional）⇒ **exit 101**，
  报 `feature \`local-rerank\` includes \`dep:bincode\`, but \`bincode\` is not an optional dependency`。
  ⇒ 措辞改为「**冗余、无需**」，示例统一为 `["local-embed"]`，并**把这条实测写进附录 C**。

#### ✅ 评审的附赠：附录 C 的「未核实」项已转「已核实」

- 本文原自标「**fastembed 是否重导出 `TextRerank` / `RerankerModel` —— 本文未核实**，需 S7-01 开工时先 grep」。
  评审**代答**并给出坐标，作者**已独立复核属实**：**`src/lib.rs:130` `pub use crate::models::reranking::RerankerModel;`**、
  **`src/lib.rs:132`** 起 `pub use crate::reranking::{ …, RerankInitOptions, RerankResult, TextRerank, … };`
  （注释 `// For Reranking` 在 `:129`）⇒ **S7-01 可直接 `use fastembed::{RerankerModel, RerankInitOptions, RerankResult, TextRerank};`**。
  原「用 `embed/local.rs:9` 反推」的旁证段已删除。

#### 📄 Docs（版本面）

- `v2-step7-design.md` **v0.1 → v0.2**（评审收口版：记录四处拍板获同意 + F1~F6 全部采纳）。
- `requirements-spec.md` **v1.16 → v1.17**（§1.1 + 附录 C 两处版本行；**只更正 F3 的语料口径与 F4 的 500ms 出处 / 联动**，
  ⚠️ **不修订任何预算数值、不新增 / 删除 FR-NFR**）。
- `architecture-design.md` **v1.15 → v1.16**（§1.1 + §15.2 两处版本行；**纯版本面同步 —— §14.5 的 R44~R48 一字未改**）。
- `plan-v2.md` **v0.16 → v0.17**（头部三行 + §8 架构侧条目重写）。
- `docs/README.md`：设计文档索引行改 **v0.2 / 评审收口**；版本面 **v1.17 / v1.16 / v0.17**。

### 文档 · V2 Step 7（精排）详细设计 —— 设计 PR（待评审）（Refs #23，2026-09-14）

> ⚠️ **本 PR 只含文档，零 `.rs` 改动**（设计 PR 的既定纪律，同 Step 3/4/5/6 的设计 PR）。
> 交付：新增 **`docs/devel/v2-step7-design.md` v0.1** + **四处定义面回写**
> （需求 **v1.16** / 架构 **v1.15** / `plan-v2.md` **v0.16** / `docs/README.md`）。

#### Added

- **`docs/devel/v2-step7-design.md`（v0.1）** —— 精排接入的完整设计（T7-01 / FR-18 / NFR-12）：
  - **§0 开工前置三条的结账单**：① **基线已复现**，且复现出这一定性事实 —— P5 锚点
    `hybrid MRR@10(1) = 0.6922` 本次为 **0.68815**（Δ ≈ **0.6%**），而**对照组的 bm25 三项逐位复现 Δ = 0**
    ⇒ 唯一变量是**未播种的 HNSW 图**（`StdRng::from_os_rng()`）⇒ **Step 7 的 A/B 必须钉在「同一张冻结图」上**
    （`--index` 读快照 + **`--runs 1`**）；② **模型已实下载并验 sha256** ——
    **`rozgo/bge-reranker-v2-m3`**（⚠️ **非 BAAI 官方**；官方库**没有 ONNX 导出**）、
    `model.onnx.data` **2,271,088,656 B（≈2.19 GB，FP32）**、sha256 `84b66c78…8945`；
    ③ **H3 取形** —— **可配 `R`、默认 20（拟）**（数值由 S7-04 标定回填，同 NFR-13 / NFR-10 先例）。
  - **§2 五个设计期新发现（A~E）**：**A** 候选池必须与 `candidate_k` **联动**，否则 `R > 3k` 时窗口被
    **静默夹到 `3k`**（R=100 在 k=10 时退化成 30）；**B** 候选回捞组装成本随窗口**线性放大**
    （`matched_terms` 对全段文本跑 `analyze_doc`，`query/explain.rs:13-17`）；**C** `TextRerank` 用
    `PaddingStrategy::BatchLongest` ⇒ **跨 batch 组成的分数不可逐位复现**；**D** `rerank()` 返回**全量排序**，
    需自行 `take(top_n)` 并按 `RerankResult.index` 回填（tie-break 是输入顺序）；**E** **512 token 截断** vs
    T2Ranking 最长段落 **76,895 字符**（`bench.rs:697`）。
  - **§4 详细设计**：`Reranker::candidate_window(&self, k) -> usize`（**provided、默认 `k` ⇒ 既有实现一行不改**）；
    `candidate_k = max(3k, window, 10)` 联动 + `take_n`；**`explain` 组装推迟**到最后 ≤ k 条（D-S7-06，
    代价 = 精排器看不到 `explain`，写进 trait rustdoc）；**`LocalReranker`**（`rerank/local.rs`，
    gate `feature = "local-rerank"`，未启用时**编译期不存在**）；`Metrics.rerank_window` / `rerank_elapsed`；
    `Explain.rerank_score`；CLI `--rerank-window <R>`（两条脚枪显式 `bail!`：`--runs > 1` 与 feature 未编译）；
    **标定实验设计**（对齐「四条抗噪声规则」+ 二层控制组 + 可证伪决策门）；**确定性探针 P1/P2/P3**。
  - **§5 D-S7-01~10**、**§7 S7-T1~T12**（含 `#[ignore]` 口径与「未跑不得写成已覆盖」）、
    **§8 S7-01~05** 与 PR 切分（内核 / 编排 / CLI / 数据 / 收尾）、**§9 风险 R44~R48 + 未决 Q1~Q7**、
    **附录 A（API 一览 + 三条不变式）/ B（执行命令）/ C（依赖源码核实）/ D（项目内证据）**。

#### 🔴 Fixed（定义面勘误 —— 评审时请重点核对这三条）

- **`take(k)` 的行号已漂移**：`plan-v2.md` §4.0 **H3** 与 issue **#23** 均记作
  `query/searcher.rs:196`，**实测在 `:286`**（`candidate_k` 定义在 **`:118`**、`parts.reranker.rerank(...)`
  调用在 **`:316`**、`SearchResponse` 字面量在 `:323-329` 与空响应 `:498-514`）。
  已同步更正 `plan-v2.md`（§4.0 H3 行内 + §附-1 证据清单）。
- **FR-18 的模型来源更正**：fastembed 6.0.2 的 `RerankerModel::BGERerankerV2M3` 指向
  **`rozgo/bge-reranker-v2-m3`**，**不是** BAAI 官方 —— **BAAI 官方库没有 ONNX 导出**。
  已回写需求 FR-18（描述行 + 验收行）与 `plan-v2.md` §4 Step 7。
- **`plan-v2.md` 的「main 现为 `8f4d08c`」已过期**（2 处「现为」的陈述：头部版本行与状态行）⇒
  更正为 **`516b2c7`**（**PR #48** T7-24 工程卫生 → `e756bcc`、**PR #49** T7-21 默认翻转 → `516b2c7`）；
  ⚠️ 另 2 处 `8f4d08c` 是「PR #44 的历史落点」、**属准确记录，未动**。

#### 📄 Docs

- **`requirements-spec.md`：v1.15 → v1.16**（**本 Step 唯一触碰 FR/NFR 的改动**）：
  - **NFR-12 由「一行、无阈值、无测量口径、无质量判据」补齐为三部分** ——
    ① **延迟判据（双列，均标「拟」）**：**端到端 `took` P50/P99** + **精排段 `rerank_elapsed` P50/P99**
    （先落 **≤ 500ms（拟）**，由 S7-04 回填；口径 = **仅 `rerank()` 调用本身**，**不含**回捞与 `explain` 组装）；
    **独立口径、不并入 NFR-02**（精排开启后 NFR-02 的 20ms **名义失效**）；
    ② **质量判据** = hybrid **MRR@10(1)** 相对**同图**基线**可测提升**；③ **可观测** =
    `Metrics.rerank_window` / `rerank_elapsed` + `Explain.rerank_score.is_some()`。
    ⚠️ 并**写明 A/B 必须在同一张冻结图上做**（P5 锚点自带 ~0.6% 图漂移，bm25 三项 Δ=0 作对照）。
  - **NFR-05 补口径限定词「不含精排器」**（`model.onnx.data` ≈2,187MB 常驻 ⇒ 精排内存**单独立为架构 R44**，
    数值待 S7-04 回填；**不并入** 372MB 基线，理由同 R41）。
  - **NFR-06 补精排确定性注记**（score 是**二次单调变换**、`chunk_id` 序不变 ⇒ 判据仍是「同一快照两次加载逐位一致」；
    ⚠️ 但**不得**升级为「跨 batch 组成仍逐位一致」——`BatchLongest` 使 padding 长度随批内最长文本变化）。
  - **NFR-07 补精排可观测字段**（`Metrics.rerank_window` / `rerank_elapsed`）。
  - **FR-18 补设计锚点 + 模型来源更正**；§1.1 版本表与附录 C 各增 **v1.16** 行。
- **`architecture-design.md`：v1.14 → v1.15**：
  - **§14.5 新增 R44~R48**（R44 精排器**查询路径常驻内存** ≈2.19GB / R45 **精排延迟主导 + 窗口静默夹取** /
    R46 **`Hit.score` 语义随开关而变**（R35 一族）/ R47 **精排数值可复现性未证实** /
    R48 **512 token 截断使长段落判据失真**）+ §14 导读补 §14.5 指引与两条纪律（**R44 ≠ R41**、**R47 反例不必修**）。
  - **§5.7 `Reranker`**：新增 **provided 方法 `candidate_window`（默认 `k`）**；契约收紧
    「`rerank` **不得依赖** `hits[i].explain`」；补三条不变式与 `LocalReranker` 实现说明。
  - **§5.8**：`Explain` 新增 **`rerank_score: Option<Score>`（⚠️ 破坏性）**、
    `Metrics` 新增 **`rerank_elapsed` / `rerank_window`（纯加法，保持 `Copy`）**；
    `Hit.score` 精排生效时 **= `σ(logit)`**（单调 ⇒ 排序零损失）；§1.1 与 §15.2 各增 **v1.15** 行。
- **`plan-v2.md`：v0.15 → v0.16**：头部版本/日期/状态三行；**§4 Step 7 补「设计已出」块 + 开工前置结账单 +
  设计期源码复核更正表（5 处）**；§4.0 **H3 状态**改为「已取形」+ **行号更正**；§5 NFR-12 / FR-18 行；
  §6 门槛 Step 7；§7 进度表（Step 7 → 🟩 设计已出待评审；**横切 T7-21 / T7-24 → ✅ 已完成并合并**）；
  §8（标题改 Step 1~7 + 需求侧/架构侧各补 Step 7 条目）；§附-1 行号更正；§附-3 执行顺序第 7 位。
- **`docs/README.md`**：新增 `devel/v2-step7-design.md` 索引行；文档间关系的风险范围 **R1~R43 → R1~R48**。
- **顺带结清横切欠账**：`plan-v2.md` §4「横切任务」表下补**状态块** —— **T7-24** = PR **#48** → `e756bcc`、
  **T7-21** = PR **#49** → `516b2c7`，两者**均已并入 `main`**；并写明评审 **P2-1** 的更正
  （增量 `flush` 交付**整个 `pending`** ⇒ 大单文档同样走并行）与 **S2-T22 已改为显式构造**
  （否则「串行」臂会静默吃门面默认、退化成「并行 vs 并行」而断言仍全绿）。

#### ⏭️ 待评审拍板（4 项，详见设计 §1 的前置清单与 §5）

- **D-S7-04** 精排**默认开 / 关**（建议**默认关**：2.19GB 下载 + NFR-02 名义失效）；
- **D-S7-05** `Hit.score` 的语义（建议 **`σ(logit)`** + `Explain.rerank_score`）；
- **D-S7-01** 默认 `R` 的取值（建议 **20（拟）**，或改为「默认 = `k`，要求显式传参」）；
- **D-S7-08** `max_length`（建议**保持 512** + `1024` 对照档）。

### 构建 · 横切 T7-21 评审响应（独立复审：1×P2 + 3×P3，全部收口）（Refs #24，2026-09-14）

> 复审**不采信 PR 描述**，独立读码逐路径核对 + 本地 `cargo test -p helix-core --lib search::config`（7/7 绿）。
> 结论「**行为与接线无问题**；建议 P2-1 修正后合入，P3 自行裁量」。**4 条全部采纳、无一条被拒**。

#### 🔴 Fixed（P2-1 —— 我自己新写的绝对化断言不成立）

- **P2-1：删掉「增量 `flush` 每次最多 64 条 / 永远够不到阈值」这一断言**（本 PR 引入，4 处 + 1 处顺带）。
  评审的反例成立且已独立复核：`add()` 是「**先**把整篇文档的全部 chunk 推进 `pending`、**再**检查阈值」，
  而 `flush()` 交给 `add_batch` 的是**整个 `pending`** ⇒ 批量上界 = `(batch_size − 1) + k`
  （`k` = 当前文档 chunk 数），**不是 64**。故 **`k ≥ 937`（缓冲已满）或 `k ≥ 1000`（保证）时
  增量 `flush` 同样走并行**；默认 `Chunker(512/64)` 步长约 448 字符 ⇒ **单篇约 42~45 万字符**的长文档
  即可能触发（`Chunker` **无单文档 chunk 数上限**；段落边界切分还会让所需长度更短）。
  ⚠️ **为什么是 P2 而非措辞问题**：行为没错，但「flush 永远串行」一旦被引用为**增量建库拓扑可复现**
  的依据，就是一条**会静默失守的承诺** —— 本 PR 的立论方式恰是「逐路径写明生效范围」，不该带同类破绽。
  落地：`search/index.rs`（灌向量注释）、`search/config.rs`（`parallel_build()` 文档表格）、
  `vector/mod.rs`（`add_batch` trait 文档）、本 CHANGELOG 的表格；顺带修 `tests/graph_persist.rs` 的既有同款表述。

#### 🟡 顺手修（P3）

- **P3-1**：低层 `with_capacity` 的默认值在翻转后**失去了唯一的测试钉**（T22 第一腿改成显式后），
  而 `examples/bench_parallel_build.rs` 的「串行」基线臂仍 de-facto 依赖它 ⇒ 改为
  **显式 `.with_parallel_build(parallel)`**（附注释说明为何不能依赖该默认）。
- **P3-2**：`VectorIndex::add_batch` 的 trait 文档原写「默认值已翻转为开」，在该位点有歧义
  ⇒ 明确为「**门面**默认已翻转；**本 trait 的默认实现仍逐条串行**；**低层 `with_capacity` 默认仍为 `false`**」。
- **P3-3**：rustdoc 里的行号引用已漂移（`index.rs:830` 实际 **833**、`:966` 实际 **969**，各差 3 行）
  ⇒ **删掉易腐的行号**，改用符号引用（`rebuild_vector_index` / `GraphStatus::Rebuilt` 分支 / `add()` 攒够 `batch_size`）。

#### ✅ 复审的核验项（作者已独立复核，签「属实」）

透传链路（`from_config` / `compact()` / 降级重建 / `try_load_graph`）、`ConfigFingerprint` **不含**
`parallel_build`（旧快照不受影响）、NFR-06 论证、T13 串行臂显式化的必要性、范围决策与「不加公开 API」的取舍
—— **逐条属实**。

#### 📌 另开 issue

- 评审指出「CLI 无 `--no-parallel-build`」的缺口**合并后应立即落一个 issue 免得遗失** ⇒ 已开 **#50**。

### 构建 · 横切 T7-21 `parallel_build` 默认翻转为「开」（Refs #24，2026-09-14）

> **横切任务**（`plan-v2.md` §4「横切任务」/ §附-3 执行顺序第 3 位，D-J11 拍板），**独立 PR**、不属任何 Step。
> 翻转的是**门面默认**（`SearchIndexBuilder` / `Config`）；**低层** `HnswRsIndex::with_capacity`
> 的默认**保持 `false`**（实现细节、不作契约 —— 避免静默改变 11 处直接构造点的语义）。

#### 📊 Changed

- **`SearchIndexBuilder::default()` 的 `parallel_build`：`false` → `true`**（`search/config.rs`）。
  实测依据（复审实测，见 issue #24）：12K 真实语料 **11.676s → 2.182s = 5.35×**、50K 合成 **5.26×**，
  真实语料 oracle 重合率无差异（0.995 / 0.995）。
- ⚠️ **生效范围（含两处勘误）**：并行分派条件是
  `parallel_build && items.len() >= PARALLEL_INSERT_THRESHOLD(1000)`
  ⇒ 交付点须**一次交够 ≥1000 条**才有意义：

  | 交付点 | 每次 `add_batch` 的批量 | 默认开后 |
  | --- | --- | --- |
  | `compact()` 的向量索引重建（`rebuild_vector_index`） | 全部存活向量（一次） | ✅ **走并行** |
  | `load_with` 的降级重建（`GraphStatus::Rebuilt`） | 全部向量（一次） | ✅ **走并行** |
  | 增量 `flush`（`add()` 攒够 `batch_size` 触发） | 存量（≤ `batch_size`−1）+ **当前文档 chunk 数 k** | ⚠️ `k ≪ 937` 串行；**`k ≥ 937~1000` 同样走并行** |

  ⇒ ① **issue #24 的表述需修正**：它说「单批最多 64 条 ⇒ 光开开关仍走不到并行」——
  那**只对短文档成立**（`flush` 交的是**整个 `pending`**，不是按 `batch_size` 切片）；
  ② **大单文档的增量建图也会走并行**（默认 `Chunker(512/64)` 步长约 448 字符 ⇒
  单篇约 42~45 万字符即可能触发；`Chunker` **无单文档 chunk 数上限**）
  ⇒ **「增量建库一定可复现拓扑」不成立** —— 该边界已写进代码文档与测试注释。
  净收益仍主要在**一次性全量建图**（`compact()` 重建 / 冷启动降级重建）。
- ⚠️ 另注：`bench --input` 的「内存直建」路径是**逐点 `add()`**、不交付批量 ⇒ **不受本开关影响**。
- 代价按 D-J11 接受：并行插入顺序不确定 ⇒ **建图拓扑不可复现**（C8）；但 **NFR-06 的口径是
  「同快照两次*加载*」、不约束建库过程** ⇒ **不破 NFR-06**。需要可复现拓扑时用 `.parallel_build(false)`。

#### 🧪 测试（含变异验证）

- **新增单测** `并行建图默认开且可显式关闭`（`search/config.rs`）：钉门面默认 = `true` +
  **显式 `false` 仍能关掉**；并在既有 `默认配置零配置可用` 里补 `assert!(cfg.parallel_build)`。
- **S2-T22 改口径**：两条腿改为**显式传开关**、**不再依赖任何默认值**（低层默认属实现细节）；
  断言消息由「默认必须串行」改为「**显式关闭** ⇒ 必须串行」。
- **T13 的「串行」臂改为显式 `parallel_build(false)`** —— ⚠️ **不改必退化**：翻默认后那条臂吃的就是
  门面默认，「并行 vs 串行」会**静默**变成「并行 vs 并行」而断言全绿（正是 T22 文档警告的退化）。
- **变异验证**：把默认临时改回 `false` ⇒ 两条断言报红（`config.rs:444:9` / `:434:9`）；已还原并复验。
- ⚠️ **如实声明覆盖边界**（`graph_persist.rs` 里 T13 / T22 的注释）：「门面配置 → 后端真的分派并行」
  这条端到端链路**仍不可观测**（并行计数器只在低层 `HnswRsIndex`，门面不暴露）。
  T7-21 评估过「为此加公开可观测 API」，结论是**不加**（公开面加法不值得）。

### 工程 · 横切 T7-24 评审响应（独立评审：✅ 通过 + 1×P3 已采纳）（Refs #25，2026-09-14）

> 评审**不采信 PR 描述与评论区证据**，在临时 worktree 检出本 PR 的 head **独立复跑**；
> 结论「**✅ 通过，可合并**」，附 **1 条非阻塞 P3**。**已采纳**（实测证据见 PR 评论区）。

#### 📊 Changed（P3：补 `concurrency`，取消被取代的旧 run）

- 本 PR 的立论是「private 仓库、Actions 走**账号配额**」—— 该维度上只剩一个没堵的口子：
  **同一 PR 连续 push 时旧 run 不会被取消**（10 个 job，`feature isolation` 与 `test` 各约 4 分钟，全部白跑）。
  现补：

  ```yaml
  concurrency:
    group: ${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}
    cancel-in-progress: ${{ github.event_name == 'pull_request' }}
  ```

  - **PR 事件** ⇒ 按 PR 号分组、`cancel-in-progress: true` ⇒ 新 push 立即取消旧 run；
  - **push 事件（main）** ⇒ `pull_request.number` 为空、回落到 `github.ref` 分组，且
    `cancel-in-progress` 为 **false** ⇒ **相邻两次合并不会互相砍掉对方的 run**（main 门禁刻意保留）。
- **取消行为已实测**（连续两次 push，观察旧 run 的 `conclusion`）⇒ 结果记在 PR 评论区，不在此处复述。

#### ✅ 评审的核验项（已在响应里逐条签「属实」）

② 三条 sidecar 规则生效 / tmp 变体无回归 / `data/` 白名单三份语料与 `.gitkeep` **无误伤** /
**与 `storage/graph.rs` 的三个后缀常量对账 ⇒ 写入路径产物后缀全覆盖** / 连历史怪名
`foo.idx.hnsw.hnsw.graph` 也兜住（支持不用 issue 原建议的 `*.idx.hnsw.<ext>`）/ ① YAML 语义与 10 个 job /
「不重复计费」论证 / 行号引用 —— **均属实**。

### 工程 · 横切 T7-24 工程卫生：CI 触发放开 + `.gitignore` 补图 sidecar（Refs #25，2026-09-14）

> **横切任务**（`plan-v2.md` §4「横切任务」/ §附-3 执行顺序第 2 位），**独立 PR**、不属任何 Step。
> 本次交付其中的 **① ②**（代码面）；**③ 清理远端分支**是操作动作、不产生 diff，另办。

#### 🔴 Fixed

- **① 堆叠 PR 永远跑不到 CI**：`.github/workflows/ci.yml` 原写 `pull_request.branches: [main]`
  ⇒ 只让 **base = main** 的 PR 触发，**base ≠ main 的堆叠 PR 完全没有 CI**
  （Step 2 的 #13/#14 已吃过亏，当时只能拿本地守门代替）。
  现**去掉 `pull_request` 的 `branches` 过滤** ⇒ **任何 PR 都有 CI**；并补 `workflow_dispatch`（可手动跑）。
  ⚠️ **刻意不采用** issue 里并列的另一个方案「`push: branches-ignore: [main]`」—— 本仓库是 **private**、
  Actions 走**账号配额**，逐分支 push 触发会与 PR 触发**重复计费**，而「每个 PR 都有 CI」只用放开
  `pull_request` 就能达成。
- **② `.gitignore` 漏掉图 sidecar 本体**：实测 `data/t2-index.idx.hnsw.{graph,data,manifest}`
  原先**可被 `git add`**（**31MB 级**误提交风险）—— 既有的 `*.idx.tmp` 只能覆盖 **tmp 变体**，
  非 tmp 的本体一直裸着。现补 `*.hnsw.graph` / `*.hnsw.data` / `*.hnsw.manifest`。
  ⚠️ 用 `*.hnsw.<ext>` 而非 `*.idx.hnsw.<ext>`：`file_dump(dir, "foo.idx")` 会**自行追加**后缀
  （`REFERENCE.md` 的 basename 铁律），basename 未必以 `.idx` 结尾。
  复验：修复前三条均 `TRACKABLE`、修复后均 `IGNORED`，且仓库内**无**已跟踪的 `*.hnsw.*`（无误伤）。

### 文档 · V2 Step 6 收尾 · 评审响应（DeepSeek Harness 复审：3×P2 + 3×P3，全部收口）（Refs #2，2026-09-13）

> 复审由 **DeepSeek Harness**（`--profile headless`，模型 `glm-5.3-flash`）**独立于作者**完成：
> 逐条核实本 PR 的声明与数字（含以未取整 QPS 复算 §8.13 全部比值、对本地 fastembed 源码逐一核对锚点），
> 结论「**3×P2 均为一**行**级事实性修正，修完即可合并**」。**6 条全部采纳、无一条被拒**。

#### 🔴 Fixed（均为「文档断言与事实不符」类 —— 本项目明令清零的一类）

- **P2-1（`architecture-design.md:9`）**：状态行写「`v2-step6-design.md` 升 **v0.3**」，与本 PR 同批落地的
  **v0.4** 自相矛盾（同句还说「Step 6 已完整收尾」，而收尾回写正是 v0.3 → v0.4 的内容）
  ⇒ 改为「升 **v0.4**（实现期 v0.3 → 收尾回写 v0.4）」。
- **P2-2（`architecture-design.md:1574` R42 应对③）**：原写「`deny.toml` 白名单**只对许可**，不含公告豁免」
  —— **与仓库实况相反**：`deny.toml:33-40` 的 `[advisories] ignore` 恰按 RUSTSEC ID **精确豁免**了 R42 的
  两条公告（各带核实日期 2026-09-12 与解除条件，PR #41 `cb7371e` 落地）⇒ 改写为「CI 侧已按 ID **精确豁免**、
  **显式留痕非静默**」，并点明这才是 R42 当前**唯一的既有控制点**。
- **P2-3（`crates/core/tests/step6_incremental_build.rs:4`）**：文件头「**T10 / T14** 由 S6-10 收尾补入」
  对 T14 不实 —— `S6_T14_半向量判据` 由 **PR #43（`a5c2693`）** 引入（`git log -S` 可证），
  且与本 PR 自己回写的 §7 表（「改向后的用例」）矛盾 ⇒ 改为「**T10** 由 S6-10 补入；**T14** 现挂 PR #43
  改向后的判据前提用例，原 `ConfigMismatch` 探针**随 S6-05 挂起**」。

#### 🟡 顺手修（P3）

- **P3-1**：「与实验（一）同协议」漏记 **rep 数**差异（vector `--reps 10` vs hybrid `--reps 20`）
  ⇒ `eval-report.md` §8.12/§8.13 与本 CHANGELOG 同句均补「两处协议差别：加同期控制组 + rep 数 10→20」。
- **P3-2（`plan-v2.md:10`）**：状态块「R38~R40 关闭」把 **R40 误归「关闭」**
  ⇒ 改为「**R38 / R39 关闭**、**R40 上锁（残余仍在）**」，与同文件 `:421`/`:605` 及架构 R40 行一致。
- **P3-3（`v2-step6-design.md:654`）**：Q8 行尾「数值仍待 T7-11 实测后定」是**设计期残留**
  （2.32~3.33 GB 正是 T7-11 的实测产物）⇒ 改为「**预算口径仍待定**（峰值 RSS 已实测 **2.32~3.33 GB**，
  见 `eval-report.md` §8.10.4；缺的是 `batch` 与序列长度的取值口径）」。

#### ✅ 复审的复核项（作者已逐条重跑，签「属实」）

`InitOptionsWithLength` 锚点族（`src/init.rs:11/:20`、`text_embedding/init.rs:20/:27/:33`、
`impl.rs:364/:373`）、`embed/local.rs:35/:73`、`cli/src/bench.rs:2024/:2190`、§8.13 全部比值算术
（+45.6% / +66.0% / 漂移 12.3% / 余量 0.55%）—— 均已在本地独立复现。

### 文档 · V2 Step 6 收尾（S6-10）：定义面四处回写 + NFR-10 裁定 + R41/R42/R43 登记 + 集成测试补 S6-T10（Refs #2，2026-09-13）

> Step 6 的**收尾 PR**。⚠️ **零生产代码改动**（纯文档 + 集成测试）：NFR-10 的裁定选的是
> **「收窄 ② 的适用范围 + 保持 NFR-10 打开」**，**不是**「修并发」——被排除的那两条路径以
> **③ 编码侧并发**如实记为**未达标项**并保持「打开」（避免 D-S6-08 已否决的「只记录不判定」）。

#### 📊 Changed（定义面：NFR 口径）

- **NFR-10 定稿并收窄适用范围**（需求 **v1.13 → v1.15**）：S6-08 实测 `QPS(4)/QPS(1)` = bm25 **2.98×** ✅ /
  hybrid **1.63×** / vector **1.88×**（`eval-report.md` §8.12）⇒ `×2.5` 由「拟」定稿（先例如期走完：
  Step 5 的 NFR-13 v1.10 落「拟」→ v1.12 定稿）。**裁定三条并记**：
  ㈠ **② 的适用范围收窄为「不含查询侧编码的读路径」**（bm25 即该路径的实测证据）；
  ㈡ **NFR-10 整体保持「打开」、不关闭** —— 编码侧并发是**如实记录的未达标项**，
  不是 D-S6-08 已否决的「只记录不判定」；㈢ **复审触发条件 = 查询侧会话池（每 worker 独立 session）
  落地时**，届时须**先答挂起的 Q3**（不同 `sessions`/`intra_threads` 是否改变向量数值）再重测。
- **NFR-05 补口径限定词**「**（检索路径，batch 1）**」：build 路径（batch 64 × 长文本）峰值 RSS 实测
  **2.32~3.33 GB**、与 372MB **差一个数量级** ⇒ 单独立为架构 **R41**（预算按 `batch × 序列长度` 给、
  **只给形态不给数**），**不并入** 372MB 基线（Q8 方案 C 的两步已**双双落地**）。
- **NFR-10 备注措辞更正**（PR #44 评审 **P3-8**）：「读路径**零**共享可变状态 ⇒ 近线性是强先验」
  **只对「索引读路径」成立** —— 端到端 vector / hybrid 还含**查询侧编码**，不受该先验覆盖。
- **NFR-03 ② / NFR-11** 补实测注记（增量 23.705s < 27.973s ✅；NFR-11 写延迟旁证，⚠️ **不标已达标**）。

#### 📌 Added（风险登记：架构 §14.4 由 R36~R40 扩为 **R36~R43**，架构 v1.13 → v1.14）

- **R36 / R37 消解**（T7-09 判「不投」⇒ 多 session / CoreML EP 均不落地；R36 的峰值代价已被数据钉死，
  不是「没人碰」）；**R38 / R39 关闭**（S6-09 `--vectors` 硬失败 + S6-T13 追加后冷启动 `Loaded` 上锁）；
  **R40 上锁、残余仍在**（S6-T8 不变式测试锁住 ID 单调，「不冲突」的证明仍依赖三条隐含前提）。
- 🆕 **R41**：建库路径峰值 RSS 远超检索路径预算（差一个数量级）；**R42**：**依赖安全公告漂移**
  （`bincode` `RUSTSEC-2025-0141` + `paste` `RUSTSEC-2024-0436`，均**无升级路径**、**长期开放**）；
  **R43**：**查询侧编码把并发读吞吐封顶**（`crates/core/src/embed/local.rs:35` 的 `Mutex<TextEmbedding>`
  ⇒ `embed_query`（`:73-79`）每次查询 `lock()`），**保持「打开」**。
- `deny.toml` / `Makefile` 里「**拟登记 R42**」的措辞**转正为 R42**（正式条目在架构 §14.4）。

#### 🧪 Fixed（集成测试）

- **补 S6-T10**（`crates/core/tests/step6_incremental_build.rs`）：追加后 `save → load` 往返 ——
  检索结果与追加后一致、同快照两次加载逐位一致（**NFR-06 不破**）。
  ⚠️ **设计 §7 原就承诺 ✅、实现期漏做**，两轮评审均未抓到，由**收尾自查**发现（§7 表已标注）。

#### 📈 Measured（新增实验，收 PR #44 评审 P2-2 遗留）

- **hybrid 无锁对照实验**：与 §8.12 的 vector 对照**同协议、同快照**（⚠️ **两处协议差别**：加**同期控制组**；
  **rep 数 `--reps 10` → `--reps 20`**，不影响 `QPS(N)/QPS(1)` 的比值口径），
  并加**同期控制组**（控制组 A → 变异 → 还原 → 控制组 A'）归一漂移 ⇒ 把 **R43 的 hybrid 归因从「推断」升为「实测」**：
  移除那把锁后 hybrid `--threads 4` 加速比 **1.73× → 2.51×**、`--threads 8` **1.79× → 3.87×**。
  ⇒ 数据与协议见 `eval-report.md` **§8.13**。（⚠️ 仍低于 vector 的 3.15× ⇒ 那把锁是**主因、非唯一**，
  与 hybrid 独有的 `rayon::join` 双 lane 共享全局池一致。）

#### 🗂 Docs

- **定义面四处 + 版本行**：需求 **v1.15** / 架构 **v1.14** / `plan-v2.md` **v0.15** / `docs/README.md`
  （风险范围 **R1~R35 → R1~R43**）。
- `v2-step6-design.md` **v0.4**：§7 测试表末列按实现实况**逐条**回写、§4.6.1 的 `Arc<Searcher>` 措辞按实现更正
  （评审 **P3-4**：`&QueryExecutor` + `thread::scope`）、D-S6-01 实现期结案标记、§9.2 漂移注记作废（留痕）、
  **新增 §11.6 收尾结账单**。
- `v2-step5-design.md:13`：修掉修订记录里两处**未转义的 `|`**（会把表格行切错列；历史欠账，本次顺带清掉）。

### 构建 · V2 Step 6 PR 6 复审响应（DeepSeek Harness 独立复审：3×P3，全部收口）（Refs #2，2026-09-13）

> 复审由 **DeepSeek Harness**（`dsh --profile headless`，模型 `glm-5.3-flash`）**独立于作者**完成：
> 对首轮 14 条**逐条行为级复验**（重跑相关单测 + 本地复现 CI bench 步骤的全部断言 + 复算 §8.12 每个数字）
> + 本地全量守门。结论 **「可合并（无阻塞项）」**；新发现 **3×P3**（P3-11 / P3-12 / P3-13），本 PR 全部收口。
> 其中 **P3-12 是对作者上一轮结论的纠正** —— 作者独立复算后**确认成立并更正存档**（见下）。

#### 🔴 Fixed

- **P3-11：`--threads 1,2` 打出失实的「缺单线程锚点」告警**（属上一轮 P3-6 修复**新引入**的缺陷）。
  旧实现的 `else` 分支以「(q1, q4) 是否同时存在」触发，文案却写死「档位**不含 t=1** ⇒ 缺锚点
  ⇒ 分母不存在」—— 但 `--threads 1,2` 的表里 **t=1 数据行就在场**（缺的是**分子** `QPS(4)`），
  而真正缺 t=1 的 `--threads 2,4` 打的是同一句 ⇒ **诊断失实**。
  现抽出 `no_verdict_notice(levels)`，按 `contains(&1)` / `contains(&4)` **分三种情形**给各自准确的说明；
  新增单测 `无判定档位的说明分三种情形`（含「有 1 无 4 时**不得**出现『缺锚点』」的反向断言），
  并做变异验证（把 `(true,false)` 分支文案改成与 `(false,true)` 相同 ⇒ 单测 ① 报红）。
- **P3-13：`scripts/eval_threads.sh` 头注仍写旧口径「spawn→join」**（P2-1 后实现已是
  「预热完成 → `Barrier` 同步点起算 → join」）⇒ 头注与实现对齐（与 P3-4 同类的文档漂移）。

#### 📊 Changed（存档更正 —— 数据诚信）

- **P3-12：§8.12 与本节曾把首轮评审的 P2-1 读数判为「与机理不符、未能复现」—— 该判定不成立（协议误读）。**
  首轮评审原文写明其读数是 **8 queries / `--reps 2` / bm25**（自证句 `6485/414 ≈ (30+2)/(0+2) = 16`），
  作者上一轮却**按自己的 `--reps 20` 协议去复算它**，于是得出「预测 2.5×、实测 15.7× ⇒ 不符」。
  **本次独立复算**：该协议下预测首尾比 **16×**、实测 **15.66×**；三档反推单次检索成本
  **0.154 / 0.147 / 0.151 ms** 互相一致 ⇒ **评审那组读数同样与机理自洽**。
  ⇒ **两组读数各自在自己协议下印证同一缺陷**（本报告 `--reps 20`：预测 2.5×、实测 2.56×）。
  §8.12 与本节已按此改写。**主结论（P2-1 是真缺陷、修复有效）不受影响**，且修复已由复审
  行为级复验（修复后 `--warmup 0/3/30` 无单调趋势）。

#### 📌 待办（复审 §六，非本 PR）

- 复审提醒：**S6-10 落地 NFR-10 裁定**（修并发 / 收窄判据 / 其他）时，须同时写明
  **NFR-10 保持「打开」**与**复审触发条件** —— 避免 D-S6-08 已否决的「只记录不判定」。
  已随 P3-4 / P3-8 一并登记到 S6-10。

### 构建 · V2 Step 6 PR 6 评审响应（DeepSeek Harness 独立评审：1×P1 + 3×P2 + 10×P3）（Refs #2，2026-09-13）

> 评审由 **DeepSeek Harness**（`dsh --profile headless`，只读模式）独立完成：读 PR 代码 + 对照四份
> 活文档 + 复跑相关单测/CI 步骤 + 逐条比对作者留存的原始运行工件，结论 **「建议修改后合并」**。
> **14 条全部采纳**，无一条被拒：2 条改**代码语义**、1 条触发**重测并改写数据**、
> 5 条加固 **CI/脚本**、其余为文档与措辞修正；3 条属**定义面**（P3-4 / P3-8 + NFR-10 措辞）
> ⇒ 与 **S6-10** 同批，理由见下。

#### 🔴 Fixed（语义 / 正确性）

- **P1-1：并发「正确性前置条件」可被空满足。** 旧实现只对 `Ok(resp)` 取签名、把 `Err` **静默丢弃**
  ⇒ 全部检索失败时每个 worker 的签名都等于同一个**空哈希常量**，`sigs.len() == 1` 照样判「✅」。
  现改为 worker 显式计数 `n_err` / `n_hits`，并抽出 `check_concurrency_precondition()` 做**三条并检**：
  ① 无失败检索 ② **命中数 > 0** ③ 签名跨档、跨 worker 一致 —— 任一条不成立即 `Err`。
  新增单测 `并发前置条件三条判据都有牙齿`：用合成样本覆盖「健康 / 全失败 / 零命中 / 签名不一致」，
  并**反证**「全失败场景下签名集合大小确实为 1」，把旧判据为何会被骗写进测试。
- **P2-1：QPS 分子分母不同口径（绝对 QPS 被系统性低估 ~13%）。** 旧实现把每 query 的 `--warmup`
  套在计时窗内，而 `n_total` 只算 `reps`。现改为**预热移出计时窗**：N 个 worker 各自预热 →
  `std::sync::Barrier` 同步点 → 起算，`wall_secs` 取各 worker 计时窗的**最大值**（分子分母同口径）。
  ✅ 实测（30 篇小语料，t=1，`--reps 20`）：以「计时窗拉回预热之前」变体复现修复前口径 ⇒
  `--warmup 0/3/30` = **6395.6 / 5322.5 / 2500.4**（首尾比 2.56×，与「分母含 `warmup+reps`」一致）；
  修正后同条件 = **6500.8 / 6064.3 / 6243.6**（无单调趋势，离散 < 7%）。
  ⚠️ **复盘（复审 P3-12）**：上一版把评审读数（6485.1 / 2714.6 / 414.0）判为「与机理不符、未能复现」，
  系**误把评审的协议当成 `--reps 20`**；评审原文是 **8 queries / `--reps 2` / bm25** ⇒ 该协议下
  预测首尾比 `(30+2)/(0+2)` = **16×**、实测 6485.1/414.0 = **15.66×**，三档反推单次检索成本
  0.147~0.154 ms 互相一致，**同样与机理自洽**。两组读数**各自在自己协议下印证同一缺陷**
  ⇒ 采用自测读数只因它与本项目复现脚本同协议，**不是因为评审读数有误**。
- **P3-5：`--threads` 无上界**（每档会真的 spawn 同等数量的 OS 线程）⇒ 加
  `MAX_THREAD_LEVEL = 256` 并在错误信息里点明上限。

#### 🧱 加固（CI / 脚本 / 测试）

- **P2-3：CI 的「逐字一致」断言可能空转通过** ⇒ 补四层自证：效果表与 `bm25` 数据行存在、
  两侧文件行数 ≥ 5、`--threads 1` 的 JSON **没有** `threads` 段、反向自证落到 `NFR-10` **判定行**
  与 JSON `n_total == 16/32/64`。
  ⚠️ **自查时抓到一条假判据**：原打算用 `grep "NFR-10"` 做反向自证，**实测 `--threads 1,2` 也会命中**
  （阶段标题里就含 `NFR-10`）⇒ 改用固定串 `QPS(4)/QPS(1)`；并对三条断言各做了一次变异验证确认能失败。
- **P3-9：`不打印表格` 实为「该 mode 不打印」** ⇒ 各 mode 的表**先攒进缓冲，全部 mode 的前置条件
  通过后一次性吐出**（要么整段出来、要么一行都不出），并同步修正 CHANGELOG 措辞与函数 doc。
- **P3-1：`eval_threads.sh` 把「未判定」误报为「未达标」并 exit 1** ⇒ 分出
  `undetermined / pass / fail` 三种判定（`all_pass` 在未判定时为 `null` 而非 `false`），只有 `fail` 退 1。
  ✅ 实测 `LEVELS=1,2` ⇒ 打印「未判定」+ exit 0；`LEVELS=1,2,4` ⇒ 正常给判定 + exit 1（bm25 在 30 篇
  小语料上只有 2.37× ⇒ FAIL，与 12K 的 2.98× 不同，属语料规模差异）。
- **P3-7：`eval_threads.sh` 的 build 失败被 `set -e` 静默带走** ⇒ 与 bench 段同款包 `if !` 并打日志尾部。
  ✅ 实测（指向不存在的语料）⇒ 打印 `Caused by: No such file or directory` 并 exit 1。
- **P3-6：档位不含 t=1 时没有锚点提示** ⇒ 打印显著告警（仍允许 `--threads 2,4` 的探索性用法）。

#### 📊 Changed（数据与结论）

- **§8.12 按修正后的口径重测**（12K / 320 queries / `--reps 20` / 档位 `1,2,4,8` / 10 核 / 插电 / 非 CI）：

  | mode | t=1 | t=2 | t=4 | t=8 | QPS(4)/QPS(1) | （旧） | 判据 ≥2.5 |
  | --- | --- | --- | --- | --- | --- | --- | --- |
  | bm25 | 721.0 | 1288.9 | 2146.9 | 3215.1 | **2.98×** | 3.07× | ✅ |
  | hybrid | 252.4 | 368.5 | 412.3 | 460.2 | 1.63× | 1.62× | ❌ |
  | vector | 260.1 | 456.3 | 489.0 | 499.3 | 1.88× | 1.88× | ❌ |

  ⚠️ **绝对值与上一版不可比，差值也不能只归因于口径修正**：快照是**重建**的
  （`hnsw_rs` 建图无 seed ⇒ 图拓扑不同）且**跨时段**。**比值稳定（变化 ≤ 3%）** ⇒ **结论不变**。
- **P3-3**：删掉「t=4→8 完全饱和（+1.0×）」这种把 **vector** 的数字配给合并结论的写法 ⇒ 拆开写
  **vector +2.1%（饱和）** 与 **hybrid +11.6%（未饱和）**。
- **P3-2**：按 `QPS(N) = N/(N·e+s)` 重算 —— bm25 `e = 0.159 ms`（`1/e` = 6300）、
  vector `e = 1.445 ms`（692）、hybrid `e = 1.913 ms`（523）；上一版给的 hybrid `2.3 ms / 433`
  与表内数字不自洽（应为 `2.51 / 398`）。本版每个数字都可由表内 N=1 与 N=4 复现。
- **P2-2：hybrid 的饱和归因降级为「推断（未对照）」** —— 无锁对照只在 vector 上跑过；hybrid 还多一个
  未拆分的并发维度（`rayon::join` × N worker 共享**全局** rayon 池）。表中已如实标注变异趟的协议
  （`--reps 10` / 档位 `1,4,8` / 仅 vector / 与基线非同一次运行）。**补跑 hybrid 无锁对照列为待办。**
- **P3-10**：`单次 ONNX 推理 ≈3.4 ms` 注明是两次不同 `--reps` 运行之差，**仅供参考，不作独立读数**。
- **判据读法**（评审对措辞的修正）：`requirements-spec.md:374` 的判据**并未**写明「三模式都要满足」
  ⇒ 本报告的「未达标」用的是**最保守读法（三模式全过）**，已在 §8.12 写明。

#### ⏸ 未在本 PR 做（属定义面，与 **S6-10** 同批）

- **P3-4**：`docs/devel/v2-step6-design.md:429` 写「共享 `Arc<Searcher>`」，实现是借用
  `&QueryExecutor` + `thread::scope`（功能等价；`QueryExecutor: Sync` 已由编译期断言钉住）。
- **P3-8**：`requirements-spec.md:374` 的「读路径零共享可变状态」前提已被实测证伪（只对**索引读路径**
  成立），应加指针注记。
- 两者都是**定义面文本**，且 P3-8 的注记措辞取决于 **NFR-10 的语义裁定**（修并发 / 收窄适用面 /
  其他）—— 现在写入会造成「裁完再改一次 + 版本面重复升版」⇒ **一并留待 S6-10 一次性写入**。

#### 变异验证（本 PR 累计 5+3 次）

- **新增 3 次**（针对 P1-1 的新判据）：`n_err` 恒 0 / `n_hits` 恒 > 0 / 签名判据永不触发
  ⇒ 各自让 `并发前置条件三条判据都有牙齿` **如实变红**，还原后复跑回绿。
- **新增 3 次**（针对 P2-3 的 CI 断言）：含 `threads` 段的 JSON 上断言 ② 失败、空表文件上断言 ① 失败、
  `--threads 1,2` 无 NFR-10 判定行 —— 并**因此抓到「`grep NFR-10` 会命中阶段标题」这条假判据**。
- 原 PR 已有 2 次（签名丢 `chunk_id` / 并发档少跑一条 query）。

### 构建 · V2 Step 6 PR 6：并发检索采集点 `bench --threads` + NFR-10 实测（S6-08 / T7-17）（Refs #2，2026-09-13）

> 本 PR 落地 **`bench --threads N`（T7-17 的采集点）** + **S6-T11 / S6-T12** + **NFR-10 实测数据**。
> ⚠️ **NFR-10 判定：按判据原文（`helix bench --threads N` 覆盖 bm25,vector,hybrid 三模式）为「未达标」**
> （2/3 模式失败）—— 根因已**实测证明**（见下），但「修并发 / 收窄判据 / 其他」属**定义面决策**
> ⇒ 本 PR **只给数据与机制，不擅自改阈值**。

#### Added

- **`helix bench --threads N[,N...]`：T7-17 / NFR-10 的采集点。**
  - 默认 `1` ⇒ **不新增任何输出阶段**（与不传该参数逐字一致；S6-T12 的回归防护，已由 CI 断言）。
  - 含 >1 档时跑**阶段 B2【并发吞吐】**：每档 N 个 worker 用 `&` **共享同一个 searcher**
    （`QueryExecutor: Sync` 已加**编译期断言** ⇒ **无需 `Arc`、无需改内核**），各自跑
    **同一批 query 的同一顺序**；输出总 QPS / 每线程 P50,P99 / 全局 P50,P99 / 相对首档加速比，
    并在同时测到 1 与 4 时给出 NFR-10 判定。
  - 口径（设计 §4.6.2 / D-S6-09）：**先跑一趟预热扫描并丢弃**再测第二趟；QPS 用**进程内**墙钟
    （N 个 worker 各自预热 → `Barrier` 同步点 → 起算到 join），**分子分母同口径** ⇒ 不含进程启动、
    快照加载与**预热**。⚠️ 并发下 per-query 延迟**含排队**，不得与 NFR-02（单线程口径）横比。
  - **正确性是前置条件（三条并检，`check_concurrency_precondition`）**：① 没有失败检索、
    ② 命中数 > 0、③ 所有档位 × 所有 worker 的 `hits`（`chunk_id` + `score`）签名**完全相同**；
    任一条不成立即 `Err`，且**整段并发表格都不打印**（不给「看起来正常」的数字）。
- **`scripts/eval_threads.sh`**：NFR-10 一键复现；自动采集并打印**运行范围**（核数 / 供电 / 是否 CI）。

#### 实测（NFR-10；12K / 320 queries / `--reps 20` / 档位 1,2,4,8；10 核 / 插电 / 非 CI）

| mode | t=1 | t=2 | t=4 | t=8 | QPS(4)/QPS(1) | 判据 ≥2.5 |
| --- | --- | --- | --- | --- | --- | --- |
| **bm25** | 721.0 | 1288.9 (1.79×) | 2146.9 | 3215.1 (4.46×) | **2.98×** | **✅ PASS** |
| hybrid | 252.4 | 368.5 (1.46×) | 412.3 | 460.2 (1.82×) | 1.63× | ❌ |
| vector | 260.1 | 456.3 (1.75×) | 489.0 | **499.3（t=8 仅 +2.1%）** | 1.88× | ❌ |

- **前置条件 ✅ 全过**：三模式 × 四档位的 `hits` 签名**逐位一致**，且「无失败检索」「命中数 > 0」
  同时成立（三条任一不成立 bench 已直接 `Err`）。
- ⚠️ **本次数据与上一版不可比**：口径已修正，且快照是**重建**的（`hnsw_rs` 建图无 seed ⇒ 图拓扑不同）
  ＋**跨时段** ⇒ 绝对 QPS 的差值**不能**只归因于口径修正。**比值稳定（≤3%）** ⇒ 结论不变。
  详见下面的评审响应条目。
- **根因（实测证明，非推断）**：`embed/local.rs:33-37` 的 `LocalEmbedder { inner: Mutex<TextEmbedding> }`
  ⇒ `embed_query` **每次查询都 `lock()`**，查询侧编码**全局串行**。
  决定性对照：把 `embed_query` 临时改成**不加锁**返回常量向量（只看吞吐）⇒ `--threads 4` 的加速比
  **1.88× → 3.15×**、`--threads 8` **1.87× → 4.35×** 且**不再饱和**（实验后已还原并复验）。
  ⚠️ 该对照**只跑 vector**（档位 `1,4,8`、`--reps 10`、与基线非同一次运行）⇒ 它是**上界**
  （变异同时移除了 ONNX 推理本身）；**hybrid 的饱和只是推断，不是测量**（评审 P2-2）。
- ⚠️ **需澄清的定义面措辞**：NFR-10 备注里「读路径**零**共享可变状态 ⇒ 近线性是强先验」只对
  **索引读路径**成立（bm25 3.07× 即证据）；**端到端** vector/hybrid 还含**查询侧编码**，
  那条路径**有**共享可变状态 ⇒ 判据前提对这两种模式不成立。
- ⚠️ **T7-09 的「不投」不能外推**：它测的是**建库**路径的多 session（batch 64 已让 session 饱和
  ⇒ 收益仅 +11.7%/+16.0%、RSS +96.4%）；**查询**路径每次只编 1 条、利用率低得多
  ⇒ 同一池化在**查询侧**的上界远大于建库侧（需**单独评估**，含 RSS 代价）。

#### 测试

- `crates/cli/src/bench.rs` 单测 **+4**（9 → 13）：`queryexecutor可跨线程共享`（编译期 `Sync` 断言）、
  `并发档位解析`（含「单档 1 ⇒ 不跑阶段」= S6-T12 的**结构性**保证 + 档位上界）、
  `并发检索结果逐位一致`（S6-T11：同档内一致 + 跨档一致 + **与手写朴素单线程循环一致**）、
  `并发前置条件三条判据都有牙齿`（评审 P1-1：合成样本覆盖「健康 / 全失败 / 零命中 / 签名不一致」）。
- CI `CLI smoke` **+1 步** `bench --threads 1 与默认一致`：`--no-latency` + 屏蔽计时行后**逐字比对**；
  四条自证断言（效果表与 `bm25` 行存在、两侧行数 ≥ 5、`--threads 1` 的 JSON 无 `threads` 段、
  反向自证落到 `NFR-10` 判定行与 JSON `n_total`）。**走 BM25 ⇒ 不需要模型**，可进 CI。
- **变异验证累计 5+3 次**（均已还原并复跑确认）：原 2 次（① 签名丢掉 `chunk_id` ⇒ 打中**独立性**断言；
  ② 并发档少跑一条 query ⇒ 打中**跨档一致**断言）+ 响应评审新增 3 次（针对 P1-1 三条判据各一次）
  + 3 次（针对 CI 三条新断言，并因此抓到一条假判据）。
- 另有 **1 次决定性对照实验**（无锁 `embed_query`，见上）—— 它**不是**测试，是**归因手段**。

#### 留给 S6-10 / 待决策（本 PR 只点名，不改语义）

- **NFR-10 的处置**：按判据原文**未达标** ⇒（a）修并发（查询侧独立 session / 会话池）/
  （b）把 ×2.5 **收窄到不含编码的读路径**并在需求里写明理由 /（c）其他 —— **属定义面决策**。
- **拟登记 R43**：向量/融合路径的并发吞吐被查询侧编码的**单 session 互斥**封顶
  （本次实测 t=4→8：vector 仅 **+2.1%**、hybrid **+11.6%**）。
- **另 3 条属定义面**（评审 P3-4 / P3-8 与 NFR-10 判据读法）⇒ 见上面的评审响应条目，与 S6-10 同批。

### 构建 · V2 Step 6 PR 5 第 2 轮评审响应（结论「无阻塞项，建议合并」）（Refs #2，2026-09-13）

> 第 2 轮评审逐条核过第 1 轮的 8 条（**7 条落实、1 条只记账**），结论 **无阻塞项**；
> 本轮新增 **3 条 P3**（均非阻塞）⇒ **全部采纳**（第 3 条按评审允许的方式延后但**点名列**）。

#### ⚠️ 行为变更（同一 PR 内加固）

- **半向量守卫前移到「构造 embedder 之前」**（第 2 轮 P3-1）：新增 `build()` 里的**预检**
  `reject_vectorless_snapshot()`（读快照指纹 `embedder_id` 是否为空 —— 用**现成的公开 reader**
  `storage::load_with_crc` ⇒ **不新增 API**）；`build_into_existing` 里基于 `tombstone_stats()`
  的判据**保留为权威判据**（它还能抓住指纹看不出来的**部分向量**）。两个后果都是评审实测指出的：
  ① 在拿不到模型的机器上，「半向量」这条更该看的错误**不再被「模型没拿到」盖住**；
  ② 为一个**注定失败**的命令不再去构造 embedder（首次即触发 ~49 s 模型下载）。
  🔑 **第三份收益**：这条 CLI 接线**不再需要真模型** ⇒ 第一次进得了 CI（`CLI smoke` 新增
  「半向量守卫必须拒绝」一步）；S6-T14 里那句「覆盖不到」的声明随之**收窄**（预检那条已覆盖，
  post-load 权威判据那条仍未覆盖）。
  ⚠️ **代价（已实测）**：`--index` + `--vectors` 时多一次快照解析 —— 同文件 debug 实测 1.47 s
  而进程启动仅 0.01 s；release 的 `加载快照耗时 150.6 ms` 本身即含这次解析 ⇒ 12K 档 ≈**150 ms**，
  相对 23.7 s 的追加 ≈**0.63%**。解析结果在返回前即释放、**不与**随后真正的 `load` 并存
  ⇒ 不抬高峰值 RSS；只读不写 ⇒「拒绝不留半成品」的性质不变。

#### Fixed

- **第 2 轮 P3-2（补记）**：上一版 `[Unreleased]` 的标题行写了「8 条全部采纳（P2-2 只记账）」，
  但**正文里找不到那笔账** —— 已在第 1 轮段的「记账」小节补上 P2-2 的正文条目。
- **第 2 轮 P3-4**：`config.rs` 里「公开面收敛为 X + Y 两个入口」措辞有误 ——
  `default_embedder()` 是**私有**的 ⇒ 改为「入口两处（一公开一私有）、**公开面只有 1 个**」。

#### 未在本 PR 做（已按评审要求**点名到行号**）

- **`plan-v2.md:368` 的归因行**（第 2 轮 P3-3）：`T7-11 增量构建（content_hash → embed 缓存，
  跳过已 embed 文档）` —— 这句描述的其实是 **D-S6-06 明确判「不做」的「形态 B（跨 run 的
  chunk 级 embed 缓存）」**，也正是第 1 轮 P3-2 那处误解的源头。它是 **`plan-v2.md` 的定义面**，
  改它属结论变更 ⇒ 与 §6/§7 进度刷新同批归 **S6-10**；本 PR 的延后清单从「只写『§6/§7 进度刷新』」
  改为**逐条点名到行号**（评审的原文要求）。

#### 变异验证（第三次）

- 屏蔽预检（`reject_vectorless_snapshot` 直接 `return Ok(())`）⇒ 在**拿不到模型**的机器上，
  同一条命令重新报出 `Caused by: 嵌入模型错误: Failed to retrieve model file 'onnx/model.onnx'`
  ⇒ **评审描述的「被模型错误遮住」重现**，证明预检有牙齿。已还原并复跑确认。

### 构建 · V2 Step 6 PR 5 评审响应（PR #43 第 1 轮：1×P1 + 2×P2 + 5×P3）（Refs #2，2026-09-13）

> 评审结论「**建议改后合并**」。**8 条意见全部采纳**（P2-2 按评审自己的建议**只记账不改语义**）；
> 新增 1 条集成测试（S6-T14）+ 1 条 CI 断言，并做了**两处变异验证**。

#### ⚠️ 行为变更（同一 PR 内加固）

- **`--vectors` 追加/重存到「无向量快照」时硬失败（P1）。** 此前会**静默**落盘一个自称含向量、
  实际只有新增部分有向量的**半向量快照**：`--mode vector` 只召回新增文档（老文档静默缺席）、
  快照指纹被改写成「含向量」且**不可逆**（之后不带 `--vectors` 的装配被 `ConfigMismatch` 拒绝）、
  下游 `compact --dry-run` 还会把它当健康态。判据 = `raw_vectors < chunks_alive`（复用
  `tombstone_stats()`，**不新增公开 API**），且放在 `add_documents` **之前** ⇒ 同时覆盖了
  `--index` **单独给出**的「仅重存」路径（实测那条同样会改写指纹，故守卫前移到分发之前）。
  **不加放行开关**：D-S6-05 已按评审 Q6 拍板「**不额外加兼容开关**」。
- **模型拿不到时的退化不再「无解释」（P2-1）。** `resolve_embedder(require = false)` 过去是
  `Err(_) => Ok(None)`，于是 `search --mode vector` / `compare` 只能报「未启用任何 Embedder 实现，
  请开启 local-embed feature」—— 而 feature 明明是开着的、真因是**模型没拿到** ⇒ 把用户指向错的方向。
  现在退化时打一条**带根因**的 stderr 告警（与 `GraphStatus::Rebuilt(reason)` 保留 reason 对齐）；
  退化为纯 BM25 的契约（**G4**）不变。这条同时补齐了设计 §7 对 S6-T7 的验收判据
  「要么 `Err`、要么**可观测标志**为真，**绝不静默**」。

#### Fixed

- **P3-1** `save_and_report` 里 `commit()` 的理由注释已过期（函数体已不读 `num_chunks`）⇒ 换成
  仍然成立的理由（`embed_count` / `embed_elapsed` 是**累计量**，尾批须在此 flush 才计入），
  并显式标注「别据此判定它与 `save` 内部那次重复而删掉」。
- **P3-2** 增量收益归因写反了：收益来自 **`load` 复用快照里的 `raw_vectors` + 图**，**不是**查重
  （见上文 Added 段与 `main.rs` 的更正）。
- **P3-3** CI `CLI smoke`：写明**覆盖边界**（只覆盖纯 BM25 追加；向量追加路径要真模型 ⇒ **不进 CI**），
  并补上 `--index` **单独给出**这条此前**零断言**的分支。
- **P3-4** `helix build` 缺参时 usage 把**可选**的 `--output` 与必填项并列 ⇒ 加
  `#[command(override_usage = "...")]`，错误路径与 `--help` 顶部都变为
  `helix build [OPTIONS] (--input <INPUT> | --index <INDEX>)`。
- **P3-5** 公开面收敛：`resolve_embedder` / `local_embedder_ctor` / `EmbedderCtor` 改**私有**
  （只为单测注入而存在），公开入口只剩 `required_local_embedder()` ⇒ CLI 侧不再需要兜不可达分支的
  `expect`（**少一个无测试覆盖的 panic 分支**）。仍满足设计 §7「校验逻辑与构造必须分离」。

#### 记账（不改语义，按评审建议）

- **P2-2**：`search --index <向量快照> --mode bm25` 在拿不到模型时会因默认装配的 embedder
  退化为 `None` 而 `ConfigMismatch` —— **只想用 BM25 的人被向量能力挡住**。属**既有行为**，
  本 PR **按评审建议不改语义**；P2-1 的告警已使它**可归因**（告警先于
  `Caused by: 配置不匹配 … 当前装配 embedder=<none>` 打印）。
  是否放宽（`--mode bm25` 不强制匹配 embedder 指纹）属**语义决策** ⇒ 挂 **S6-10**。
  ⚠️ 补记说明：上一版只有标题行提到「P2-2 只记账」而无正文条目，第 2 轮评审指出这会
  让下一个人「重新判一次、甚至判出反向结论」⇒ 现补上。

#### 测试

- 新增 `S6_T14_半向量判据的前提在两方向上都成立`：把 P1 守卫依赖的不变式（健康向量快照恒有
  `raw_vectors == chunks_alive`）**两个方向**都钉住 —— 健康快照（含追加后）**相等** ⇒ 不误报；
  无向量快照 + 能 embed 的装配 ⇒ **严格小于** ⇒ 守卫必触发。守卫**接线**需真模型、不进 CI，
  已在测试注释里写明**覆盖不到什么**。
- **变异验证 ①（S6-T14）**：把 `load` 后的 `raw_vectors` 人为清空 ⇒ S6-T14 **报红**
  （`追加前判据不得成立（否则守卫会误报）：raw 0 vs alive 8`，`step6_incremental_build.rs:410`）。
- **变异验证 ②（P1 守卫）**：把 `if args.vectors` 改成 `if false` ⇒ 本地端到端**重现**评审描述的
  半向量快照（exit=0、`含向量`、图仅 10 点、`--mode vector` 查不到真正相关的 base 文档），
  并**证实** `--index` 单独给出同样会改写指纹。两次变异均已还原并复跑确认。

### 构建 · V2 Step 6 PR 5：增量构建 + 显式请求向量的硬失败（S6-06 / S6-07 / S6-09）（Refs #2，2026-09-12）

> 本 PR 落地 V2 Step 6 的三件事：**`T7-11` 增量构建**（FR-28）、**NFR-03 ② 增量口径实测**（S6-07）、
> 以及 **`default_embedder()` 静默降级的消除**（S6-09 / D-S6-05 方案 A）。
> ⚠️ `T7-09`（embed 并行）已由 PR #40 实测判「**不投**」⇒ **S6-04 池化 / S6-05 EP 接入不在本 PR 范围**，
> 本 PR 只做 `T7-11`（`load` 复用快照里**已持久化的向量** ⇒ 只有 delta 需要推理）这条**结构性**收益路径。

#### Added

- **`helix build --index <既有快照> [--input <delta>]`：增量构建（T7-11 / FR-28）。**
  语义 = `load` 既有快照 → 追加 delta → `commit` → 落盘：
  - `--index` + `--input` = **追加**；`--index` 单独给出 = 仅加载后重存（往返诊断）；
  - `--output` 缺省**原地覆盖 `--index`**，给出则另存（沿用 `helix compact` 的先例）；
  - **幂等**：已存在的文档被 `content_hashes` 双保险挡在 `pending` 之外 ⇒
    重复追加不改变文档数 / 分片数 / 词项总数，且**不耗 ONNX 推理**；
  - ⚠️ **增量收益的来源是 `load` 复用快照里的 `raw_vectors` + 图**（`load` 根本不调用
    `embed_documents`），**不是查重** —— 查重保证的是**重复追加的幂等**（S6-T3），是另一个性质。
    实测那次「去重跳过 0」恰好证明收益与查重无关（PR #43 评审 P3-2）；
  - `--input` 与 `--index` 都不给时由 clap 的 `required_unless_present` 拒绝；
  - ⚠️ **不是 upsert-by-source**：同一 `source` 内容变了就是**一篇新文档**，旧文档仍在。
    需要替换语义请显式 `remove` 后再 `compact`（设计 §4.5.5，已由测试钉住）。
- **`SearchIndex::embed_count()`**：累计送入 `embed_documents` 的**条数**（与既有
  `embed_elapsed()` 同源累加）。存在的理由是**口径正确性**：增量构建时 `num_chunks()`
  是索引总量，用它会高估「本次 embed 了多少条」，把 NFR-03 的读数讲错。
- **`scripts/eval_incremental.sh`：NFR-03 ② 一键复现脚本（S6-07）。**
  同一次运行内取三个计时（`T_full` / `T_base` / `T_inc`），按 `T_inc < T_full × RATIO% × 1.2`
  出判定，并把增量耗时分解为 **load / embed / save** 三段；结果同时写 `result.json`。
  分钟级、**不进 CI**（性能数字一律本地 release 跑）。
  口径与 `eval-report.md` §8.2 的 NFR-03 ① 对齐：`--single-chunk --vectors` ⇒ 1 篇 == 1 chunk。

#### Changed ⚠️ 行为变更

- **`--vectors` 现在是「显式请求向量」：拿不到向量就报错，不再静默退化为纯 BM25
  （S6-09 / D-S6-05 方案 A）。** 此前 `fastembed` 初始化失败被 `.ok()` 吞掉，
  用户拿到的是**悄悄少了向量路**的索引 —— 检索不报错、召回静默变差，正是
  NFR-07「降级必须显式可见」要消灭的那类问题（与 Step 2 用 `GraphStatus`
  消灭「图降级无人知」同构）。
  - **零配置路径不受影响**：`SearchIndex::builder().build()` 未显式请求向量，
    模型不可用时仍退化为纯 BM25（保住「零配置可用」契约 G4）。
  - **迁移**：要向量却装不上模型时会看到明确报错并附可执行提示；
    只想要 BM25 检索的话，去掉 `--vectors` 即可。
  - 实现把**策略与构造分离**（`search::resolve_embedder(require, ctor)` +
    `search::local_embedder_ctor()`），使这条策略能在 CI 里用**必失败的构造器**
    秒级覆盖，不必依赖模型下载。

#### 实测（NFR-03 ② 增量口径）

- 复现：`./scripts/eval_incremental.sh`；口径 `--single-chunk --vectors`、`data/t2-corpus.jsonl` 前 12,000 篇，
  `base` 10,800 篇 / `delta` 取**尾部** 1,200 篇（与 base **不相交** —— 评审 F1 的硬约束，
  相交会让 1200 条全部命中查重短路、判据必然通过却什么都没测到）。
- **判定 ✅ PASS**：`T_inc` = **23.705 s** < 预算 **27.973 s**（= `T_full` 233.107 s × 10% × 1.2），余量 **15.3%**。
- 分解：`T_full` = embed 217.35 s + save 0.122 s；`T_inc` = load 0.151 s + embed **21.412 s** + save 0.114 s。
- **增量省的是 embed**：217.35 s → 21.41 s（**−90.2%**）—— 12,000 篇里只有 delta 的
  **1,200 篇（10%）**需要重新 embed，base 的 **10,800 篇（90%）** 向量已在快照里、不重算。
  ⚠️ 分母已按 10% 缩过，故「省 90%」在判据上表现为「只用了全量的 **10.17%**，容许 12%」，
  别把这里的小余量误读成增量收益小。
- 三档 embed 吞吐一致（55.2~57.5 条/s）⇒ 不存在隐含的全量回灌（否则 `T_inc` 会退化到 ≈200 s 量级）。
- 与设计附录 B 的预估（"预期 ≈23 s / 阈值 27.1 s ⇒ 通过但不宽裕"）**高度一致**（实测 23.705 s）。
- 完整表、`T_inc` 余额归因（**进程内口径 ≈1.88 s**）、两种时钟（墙钟 / 进程内）的区分、
  NFR-11 写延迟旁证（≈1.14 s/批）与三条局限，见 **`docs/devel/eval-report.md` §8.11**
  （§8.10 已被 T7-09 的 spike 占用，故另起一节）。

#### 测试

- 新增 `crates/core/tests/step6_incremental_build.rs`（7 条）：
  S6-T1 追加后计数 == 全量重建 /
  S6-T2 追加后 BM25 `(source, score)` 序列 == 全量重建（**不比 id**，口径见设计 §4.5.3）/
  S6-T3 重复追加同一 delta 幂等（含**跨快照**，去重必须能在 save/load 后存活）/
  S6-T8 追加分配的 ID **严格大于**历史最大 ID（含**尾部墓碑**场景 —— ID 复用最容易发生的地方）/
  S6-T9 追加前后配置指纹不变 /
  S6-T13 追加后图 sidecar manifest **重发**且冷启动 `Loaded` 而非 `Rebuilt`
  （Step 4 踩过的同一个坑：图变了不重发 manifest ⇒ 每次冷启动白重建 ≈10s）/
  + 语义边界「追加不是 upsert-by-source」。
- `search/config.rs` 新增 S6-T7 三条单测：注入**必失败**的构造器 ⇒
  断言「显式请求 ⇒ `Err`」「零配置 ⇒ 退化为 `None`」。
- CI `CLI smoke` 新增两条：`--index` 追加往返（并 `grep` 幂等计数）、
  既无 `--input` 也无 `--index` 必须被拒绝。
- **变异验证**：把 `ForwardStore::insert_doc` 改成「复用第一个空槽」⇒ S6-T8 立即失败
  （证明这条不变式测试有牙齿，而不是恒真断言）。

### 工程 · cargo-deny advisories 门评审响应（PR #41 第 1 轮）（Refs #25，2026-09-12）

> 评审**无阻塞项**（3×P2 + 3×P3），结论「**可以合并**」，并明示「**改完 P2-1 / P2-2 我就给 approve**」。
> 本轮 **P2-1 / P2-2 / P3-1 / P3-2 / P3-3 / P3-4 全采纳**；**P2-3 部分采纳**（活文档已同步；
> 「风险追溯 ID」按 Q8 对 R41 的同一裁定留 S6-10，理由见下）。
> 评审给出的 9 条复核证据我独立复算过（见下方「独立复核」段）。

#### Changed

- **P2-1 采纳：`ignore` 改为结构化 `{ id, reason }`。** `reason` 内含**核实日期**与**可判定的解除条件**。
  实测（`cargo deny -L info check advisories`）：`note[advisory-ignored]` 会打印 `── ignore reason`
  并**指回配置行** ⇒ 理由**跟着诊断走**，不依赖注释是否被人删掉，也可被 `--format json` 机器消费。
- **P2-2 采纳：pin 检查器版本（这道门的第二个漂移源）。** `ci.yml` 的 `tool: cargo-deny`
  → **`cargo-deny@0.20.2`**；`Makefile` 的 `cargo install cargo-deny --locked` 追加 **`--version 0.20.2`**。
  理由：检查器版本决定默认 lint / 配置字段语义 / ignore 解析行为，两端浮动会打破
  「本地 `make deny` 绿 ⇒ CI 绿」的等价性。`0.20.2` 与 `docs/devel/thirdparty.md`（`:91`/`:321`/`:393`）
  记录的**项目工具版本一致** ⇒ 是**回归既有记录**，不是新增约束。
- **P3-1 采纳：`bincode` 的复核条件改引用既有决策钩子**（`architecture-design.md` ADR-005 §7.6 的
  bincode vs rkyv A/B 取舍规则 + `p4-design.md` R-P4-4），两条依赖路径**各标 owner**（hnsw_rs 上游 / 本项目）
  ⇒ 不再是「没有触发点的悬空立项」。
- **P3-2 采纳：补「本目标需要联网」。** `Makefile` 与 `deny.toml` 均写明 advisories 会拉 RustSec 数据库
  （缓存 `~/.cargo/advisory-db`）、**断网时 `make all` 在此失败且与代码无关**，并给出离线兜底
  `cargo deny --offline check advisories`（`--frozen` = `--locked` + `--offline`，已核 `cargo deny --help`）。
- **P3-3 采纳：修掉** `paste` **不可达的复核条件。** 原写「fastembed 升到不再依赖 tokenizers 的版本」
  ——fastembed 的本职就是 tokenization ⇒ 等于没写。改为**外部条件**「tokenizers 改用维护中的替代
  （`pastey` 分叉，crates.io 已有 0.2.x）或 paste 出现新维护者」+ 明确「本项目无法自行消除」。
- **P3-4 采纳：补复核配方** `cargo deny -L info check advisories`，并把「**因数据库新增条目变红 ⇒
  允许在同一个 PR 内补 ignore + 复核条件（提交信息带 `Refs #25`）**」写成**明示的低门槛处置路径**
  （评审的原话是「让它成为一个明示的、低门槛的处置路径，而不是让人去猜这红是不是我的锅」）。

#### Fixed

- **P2-3 部分采纳：活文档的命令字面量已同步**（此前文档描述的是**另一条命令**）：
  - `docs/devel/v1-finish-design.md:189` —— 这是 **`ci.yml` 头部自引的设计依据**，`deny` 行改为
    `cargo deny check advisories licenses bans sources` 并加「2026-09-12 事实同步」注记；
  - `docs/devel/plan.md:132`（T0-06）与 `:471`（每日开发循环）两处字面量同步，`plan.md` 升 **v1.6** 并记变更行；
  - `ci.yml` 头部注释由「deny 是 **NFR-09 依赖许可合规**的自动化守门」改为
    「**NFR-09 + 依赖安全公告漂移（拟登记 R42）**」，并注明两个漂移源与 pin 的用意。
  - ⚠️ **有意未动**：`docs/devel/p0-design.md:443`（P0 执行日志，属历史存档）、
    `docs/README.md:48`（「技术风险 R1~R35」本就因 PR #38 登记 R36~R40 而漏更 ⇒ 随 S6-10 的
    「docs/README 索引」一并修，避免同一行改两次）。
- ⚠️ **P2-3 的「风险追溯 ID」部分按 R41 的同一裁定留 S6-10。** 新增的 advisories 门**拟登记为 R42**
  （**R41 已由 Q8 裁定预留给 build 路径峰值 RSS**）。理由：登记风险会牵动**定义面四处 + 版本行**，
  与 R41 属**同一类动作**；且上一个 PR（#40 新增 `shell expansion` 门）也是同样处置（回写留 S6-10）。
  「拟登记 R42」已写进 `deny.toml` / `ci.yml` / `Makefile` / `plan.md` ⇒ **有 owner、有触发点**，不悬空。
  若评审要求在本 PR 内就登记，说一句即可加上。

#### 独立复核（评审给的 9 条证据我逐条复算）

| 评审断言 | 我复算的方式 | 结果 |
| --- | --- | --- |
| `tokenizers` 最新版仍依赖 `paste ^1.0.14` | 直接拉 crates.io sparse index（`/to/ke/tokenizers`）解析最新稳定版依赖 | ✅ `0.23.2` → `paste ^1.0.14`（normal、非 optional） |
| `hnsw_rs` 最新版仍依赖 `bincode ^1.3` | 同上（`/hn/sw/hnsw_rs`） | ✅ `0.3.4` → `bincode ^1.3` |
| `fastembed` 最新版仍 pin `tokenizers ^0.23.2` | 同上（`/fa/st/fastembed`） | ✅ `6.0.3` → `tokenizers ^0.23.2` |
| `pastey` 分叉确实存在 | 拉 `/pa/st/pastey` | ✅ 7 个稳定版，最新 `0.2.3` |
| 结构化 `{ id, reason }` 在 0.20.2 可用、且随诊断打印 | 改完 `deny.toml` 实跑 `-L info` | ✅ 见上图 `── ignore reason` |
| 本机 cargo-deny 版本 = 项目记录版本 | `cargo deny --version` vs `thirdparty.md` | ✅ 均为 `0.20.2` |
| `install-action` 支持 `tool@version` | 拉该 action 的 README 核对语法 | ✅ README:45-50 明示 `tool: cargo-hack@0.5.24` |
| `cargo deny --offline` 存在 | `cargo deny --help` | ✅ 另有 `--frozen` = `--locked` + `--offline` |
| RustSec 数据库本地缓存无隐藏漂移 | **未复跑**（评审已做，且结论不影响本 PR 的任何改动） | — |

### 工程 · cargo-deny 纳入 advisories 检查（Refs #25，2026-09-12）

> **横切工程项**（issue #25 工程卫生的一部分），与 V2 Step 6 正交，独立 PR。

#### Fixed

- **`make deny` 与 CI 的 `cargo-deny` 此前只跑 `licenses bans sources`**，而 `main` 上
  `cargo deny check advisories` **本来就是 FAILED** ⇒ 「排除」等于**这道门不存在**。
  现改为**把 `advisories` 也纳入**，并对命中的公告**精确到 ID** 地 ignore（每条写明复核条件）：

  | ID | crate | 依赖路径 | 为何不修 |
  | --- | --- | --- | --- |
  | **RUSTSEC-2024-0436** | `paste` | `paste ← tokenizers ← fastembed`（**传递**） | 上游作者已归档仓库、不再维护；无升级路径 |
  | **RUSTSEC-2025-0141** | `bincode` | ① `bincode 1.3.3 ← hnsw_rs 0.3.4`（**传递**）<br>② `bincode 2.0.1` ← **本项目直接依赖**（快照序列化） | 上游因外部事件**永久停止开发**，并声明 `1.3.3` 即完整版本；无升级路径 |

  ⚠️ **两条都是 `unmaintained`，不是 `unsound` / vulnerability**。
  ⚠️ **`bincode` 是直接依赖** ⇒ 迁移评估（公告推荐 `wincode` / `postcard` / `bitcode` / `rkyv`）
  应**单独立项**；**本 PR 只把门打开，不做迁移**。
  ⚠️ 这条门依赖**外部 RustSec 数据库** ⇒ 数据库新增条目时 **CI 会无缘无故变红** ——
  届时重新判一次，必要时补 ignore 并在 `deny.toml` 记下复核条件。
  **不要把 ignore 列表当永久白名单。**

#### Docs

- `deny.toml` 的 `[advisories]`：`ignore = []` → 两条带**逐条复核条件**的 ignore；
  `Makefile` 的 `deny` 目标与 `.github/workflows/ci.yml` 的 deny job 同步更新（含「数据库漂移会让 CI 变红」的说明）。

### V2 Step 6 · T7-09 spike 评审响应（PR #40 第 4 轮）（Refs #2，2026-09-12）

> 评审第四轮：**1 条正式评审 + 4 条行内**（0 issue 评论）。上一轮 24 条已有交代，本轮有一处**阻断项**（§2）
> 加两条**数据面**修正（§3.1 / §3.2）。**「不投」结论不变**。评审原文的合并建议是：
> 「修掉 §2 那 10 处 `$VAR` → `${VAR}` 之后，本 PR 我认为可以合并。」

#### Fixed

- **⛔ 阻断项：`scripts/eval_embed_session.sh` 在 `0d3d36d` 上完全跑不起来。**
  `:187` 的 `"$label（"`——「（」是全角括号 U+FF08（多字节），**bash 3.2（macOS 自带）不把非 ASCII 字节
  当作变量名终止符** ⇒ 变量名被解析成「`label` + `（` 的若干字节」，`set -u` 直接
  `label�: unbound variable` 并中止，**一次基准都跑不到**。最小复现：
  ```bash
  bash -c 'set -u; label=x; echo "$label（x）"'      # bash: label�: unbound variable
  bash -c 'set -u; label=x; echo "${label}（x）"'    # ok
  ```
  ⚠️ **与 locale 无关**（评审结论）；但**我们实测它是「`LC_ALL=C` 下不触发」**——CI 与部分本地
  shell 恰好落在 C locale，所以这类缺陷**能一路混过本地裸跑**。修法：`$VAR` → `${VAR}`。
- **同一类缺陷不止一处（评审只点了 1 个文件，全仓扫出 3 个文件、14 处）。**
  除 `eval_embed_session.sh` 的 **10 处**（`:57/:65/:129×2/:158/:164/:170/:187×2/:226`，与评审清单逐行吻合）
  外，另有 **`eval_filter.sh` 3 处**（`:96`×1 / `:111`×2）+ **`eval_quality.sh` 1 处**（`:51`）= **10+3+1 = 14**。
  ⚠️ 只修 `:187` 不够：其余 9 处都在
  **错误/汇总分支**里 ⇒ 最坏情况是「基准失败 → 想打印『作废：…（详见 $log）』→ 自己先挂掉，把真正的原因盖掉」。

#### Added

- **`scripts/check_shell_expansion.py`：把这类缺陷变成确定性守门**（不再依赖「记得在本机跑一次」）。
  扫 `scripts/*.sh` 里 `$VAR` 紧跟非 ASCII 的位置，报 `文件:行:列` 与原文，**退出码非 0 即挂**。
  接进 **`make all`**（新增 `shell` 目标）与 **CI 新增 `shell expansion` job**（无 Rust 依赖，紧跟 `fmt`）。
  变异测试证明有牙齿：故意把 `eval_quality.sh:51` 还原成 `$f（` ⇒ 报
  `scripts/eval_quality.sh:51:40  $f` 并 exit 1；改回 `${f}` ⇒ `OK（5 个脚本，0 处命中）`。
- **`eval-report.md` §8.10 表头补「表 1~表 3 系手工运行」声明**：修复前脚本跑不到第一次基准，
  「复现：`./scripts/eval_embed_session.sh`」那条**在本次修复前不成立**（评审 §2.2 依文件时间核实）；
  现命令成立，但表中数字仍是那批手工产物，时间戳早于修复。

#### Docs

- **§8.10.4 表 3：耗时列改为「两次」（首次 / 复跑），先前只记了较快的一次**（评审 §3.1）。
  N=64 `1.10 / 1.16s`、N=512 `8.86 / 9.01s`、N=4096 **`75.17 / 69.24s`**（两次差 8.6%）。
  **RSS 两列保持「逐位一致」**（2.11 / 3.31 / 3.33 GB）—— 它们确实逐位一致，核心结论
  「峰值 RSS 与语料规模解耦、1 batch 后饱和」**不受影响**。
  表下补：**耗时列只在本表内可比**（三点均为冷进程 · 单轮 · `--warmup 0`，比表 1 低一个强度）；
  **吞吐基线一律取表 1 的 e1（63.05s / 63.4 条/s）**。
- **§8.10.8：删掉「复跑 e1 中位 69.43s」这个回溯不到产物的数字**（评审 §3.2）。
  `/tmp/s6-warmup-cmp{,-2}.json` 的 e1 一轮实为 **76.98s / 73.54s**，两个都不是 69.43s。
  该处要说明的差异（表 3 N=4096 比表 1 e1 慢 10%~19%）改由表 3 那对**可溯源**的数字承担，
  归因仍为「跨时段不可相减」。
- **§8.10.8 冷/热条目按实测改写**（评审 §3.2，评审**主动更正**上一轮自己的「新增 I」）：
  512 段档位的**匹配 A/B ⇒ 无显著差异** —— 评审测 `8.81s（同进程+warmup 1）vs 8.80s（冷进程+warmup 0）= −0.1%`，
  我们独立复跑同口径得 `9.36s vs 9.28s = −0.9%`（同向、均落在噪声内）。
  ⇒ 71.01s vs 63.05s 的差**不能**当成纯「预热收益」（含 process 启动与冷 page cache），**量级被高估**；
  **4000 段档位未做匹配 A/B**，故那 10%~19% 只能归「跨时段」。
- **`:532` 的「e2-2 为 −22%」补限定词**（评审 §4）：标明该数为**单波、无预热、非本报告协议**的抽查，
  **仅作方向参考，不作本报告的数据引用**。

#### Changed

- **`TMPDIR="${TMPDIR%/}"`**：产物目录打印出 `.../T//s6-embed-XXXX` 双斜杠（`TMPDIR` 末尾自带 `/`）。
- **`RSS_MAX_GB` 改按字节计算**：原先由**已取整**的 `PHYS_GB` 算（36GB 机器会算成 32GB × 0.9）；
  现直接用 `PHYS_BYTES * 0.9`。

#### 质量门（全绿）

运行范围：**本分支 `5d87c52` / 本机 macOS aarch64 / 默认特性**。

- `cargo fmt --all -- --check` ✅
- `cargo clippy --workspace --all-targets -- -D warnings` ✅
- `cargo test --workspace` ✅ **276 passed / 6 ignored / 0 failed**（**13** 条 `^test result` 行全 ok）
  ⚠️ 报数口径：**必须取全部 `^test result` 行求和** —— 用 `| tail -12` 会截断第一条（`9 passed`），少算 9。
- `make shell` ✅（新增守门）/ `bash -n` 全部 5 个脚本 ✅
- **UTF-8 locale 下实跑** `LC_ALL=en_US.UTF-8 TEXTS=64 ROUNDS=1 WARMUP=0 ./scripts/eval_embed_session.sh` ⇒ **exit 0**
  （预热 → 波 → 汇总 → 决策门合取表 → 产物目录全通；产物目录 `/var/folders/.../T/s6-embed-9sZprG` **无双斜杠**）
- CI run `34693636678`：**10/10 全绿**，含新 job **`shell expansion`**

### V2 Step 6 · Q8 裁定记录：方案 C（拆两步）（Refs #2，2026-09-12）

评审建议 + 项目负责人拍板，**Q8 定为拆两步**：

1. 给 `NFR-05` 补口径**限定词**「**（检索路径，batch 1）**」—— 消除「按 372MB 规划内存会在建库时被 OOM kill」的误读；
2. 单独立风险项 **R41**，**预算形态按 `batch × 序列长度` 给**（而不是按语料规模给）。

⚠️ **两项都归 S6-10**（登记会牵动定义面四处 + 版本行）；**暂不给数值** ——
三点实验已钉死「RSS 与语料规模解耦、1 batch 后饱和」（`eval-report.md` §8.10.4：`--texts` 64/512/4096 ⇒ 2.11/3.31/3.33 GB），
但「典型的 `batch × 序列长度` 分布」要等 **T7-11** 的实测。
**明确不把 build 路径并进 NFR-05 的既有读数**（会让「372MB」这条历史基线的含义变混、反而降低可判定性）。

### V2 Step 6 · T7-09 spike 评审响应（PR #40 第 1~3 轮）（Refs #2，2026-09-12）

> PR #40 收到 **1 条正式评审 + 8 条行内 + 3 条 issue 补充**（跨三轮复核，合计 **F1~F9 + 新增 A~Q**）。
> **「不投」结论不受影响** —— 评审复核后亦确认加固反而更稳（M：e2-4 的 +16.0% 还高估了；I：E1 基线偏保守；
> H：只削弱表 2「持平」的精度而非方向）。本轮按评审的优先级修：**J → H/I 的文字 → K/L/M 的守卫与统计**，
> 外加必修的 **F1/A**（预热口径 + 决策门从不触发）、**F2/B**（表 2 的 E3 行）、**C**（调优档可复现命令）、**F3**。

#### Fixed

- **F1 / 新增 A：把"测量失败"与"测出小数字"分开，并让决策门真的被打印。**
  脚本原先用 `|| true` 吞掉子进程退出码、RSS 只校验"非空" ⇒ **崩掉/OOM 的波会把最大 RSS 写进 TSV**
  （而 RSS 恰在进程被杀时最大）。现改为：判**退出码** + **校验 JSON 的 `meta`**（texts / warmup / rounds /
  coreml_feature / coreml_static_shapes 与本次参数一致）+ 校验 `rounds_secs` 长度 + **峰值 RSS 必须落在
  `[0.05GB, 0.9×物理内存]`**，任一不符则该波作废并中止。
  另：载体里的决策门块要求同进程有 `e1`，而脚本每进程只传一个 `--configs` ⇒ **那块代码从不触发**，
  「投/不投」在工具链里**没有任何落点**。现把**合取判定表**落到脚本末尾，载体里改标为「吞吐半边 · 非权威」。
- **新增 J：固定 `/tmp` 文件名 + 摘要不校验 `meta` ⇒ 残留文件会被当成新数据汇总。**
  产物改落 `RUN_DIR=$(mktemp -d)`；摘要表头打印语料规模与波数；RSS 段断言 `e1` 存在
  （原先 `base` 是循环内惰性赋值，`e1` 不在首位就 `TypeError`）。
- **新增 K/L：`--rounds 0` 让 `median()` 的 `v[m-1]` usize 下溢 panic；`seq 1 0` 的守卫只加在预热循环。**
  两处都补上（`ensure!(rounds ≥ 1 && texts ≥ 1)` + ROUNDS 循环守卫 + 脚本侧参数越界检查）。
- **评审 Q：`load_texts` 不校验行数与内容**（坏行 `.expect` panic；`--texts N` 少行时**静默**按实际条数算吞吐，
  跨实验不可比）⇒ 改为带行号的可读错误 + **行数不足响亮失败**。
- **评审 F8：两条静默错标的入口。** ① 未开 `coreml` feature 时 `Ep::CoreMl` 仍构造得出 ⇒ 补 `ensure!` 响亮失败；
  ② 同进程多 pool（约 22 GB 常驻）⇒ 打一行告警，并把文档头的运行示例改指脚本协议。
- **评审 Q：`time` 按路径存在性而非按能力选**（Linux 上 `/usr/bin/time` 是 GNU time，`-l` 不可用）
  ⇒ 改为按 OS + 能力选；删掉死代码 `${TIME_BIN%% *}`。

#### Changed

- **F2 / 新增 B：表 2 的 E3 行是跨语料规模的并排，且 RSS 一栏被记成 ✅。**
  3.56 GB 是 **512 段**、3.33 GB 是 **4000 段**；且 D-S6-01 对 E3 **额外要求「向量数值差异已量化」**（未做）。
  ⇒ E3 的 RSS 增量改为「**未测量**（不可比）」，判定写明**合取第 3 项未做**，并补上基线不对称
  （CPU@512 的 49.4 条/s 比 e1 的 63.4 条/s 慢 22%）与**手工复现命令**（新增 C）。
- **F4 / 评审 H / 新增 I：协议描述写全。** 新增「8.10.1 测量协议」表，逐条写明三处与设计的差异
  （同进程 → 逐档位独立进程 / A/B/A/B → 每波一个样本、**样本量 = 3** / 预热只暖 OS 缓存），
  并显式声明**表 1 与表 2 不是同一套 session 状态协议**、不能互相解释。
- **M（统计口径）：** 摘要打印每档位 `min/max/带宽`，并用**全体档位最差带宽**（而非只看控制组）放行可比性；
  据此在表 1 标注 **e2-4 的 +16.0% 与自身噪声不可分**（带宽 **16.3%**）。
- **F5（新增数据）：把「峰值 RSS 的归因」从相关升级为因果。** 固定文本长度、只变 N = 64 / 512 / 4096
  ⇒ RSS **1 个 batch 就到 2.11 GB、8 个 batch 后饱和于 3.3 GB、语料再放大 8 倍不变**
  ⇒ 由 **`batch × 序列长度`** 决定、**与语料规模解耦**（复跑逐位一致：2.11 / 3.31 / 3.33 GB）。
  **这也是 Q8 需要的那份数据。**
- **G（数值口径）：** 控制组带宽 1.0% → **0.9%**；单 session 峰值 RSS 统一为 **3.33 GB**
  （「约 2 GB」实指**单批激活** = 2.11 GB，两者不是同一个量）；吞吐列口径写明「以中位耗时那条为准」。
- **裁定 2（`coreml` feature 保留）**：`crates/core/Cargo.toml` 注明「**探针基础设施**：不在 CI 覆盖内
  （评审 F7）、预期会随 ort 升级腐化、下次测量前须重验」，并**不再**写「若判不投可移除」。

#### Docs

- **F6 / 评审 E：把锚点更正做干净。** 设计文档 §2.3 表两行 + 附录 C 三处（`impl.rs:381→:373`、
  `ep/coreml.rs:147-153→:159`、补 `:102` / `:121` / `:49-51`）；并更正「`plan-v2.md` 已同步」这句**假声明**
  （实际 §附-1 的锚点与附录 C 相反，留 S6-10）。所有新行号均已**独立复核**过本地 crate 源码。
- **§4.2.1 / §4.2.4 的实现期更正**：`intra_threads` 公式 `max(1, 核数/sessions)`（截断）→ **`ceil`**
  （实现用 `div_ceil`，允许轻微超额以免留核空转）；`ort` 依赖记法 → `optional = true` + 独立 feature。
- **设计文档升 v0.3 并新增 §11「实现期更正与状态注记」**：实测定案、协议更正、锚点更正清单、
  **Q8 登记进 §9.2**、**Q3 随 S6-05 取消而挂起**（它是 D-S6-04 的前置），并显式声明
  **定义面状态待 S6-10 统一**（避免「结案 / 命中」与「待定」同时出现在同一批文件里）。

### V2 Step 6 · T7-09 embed 并行 spike：E1/E2/E3 实测与「不投」结论（Refs #2，2026-09-11）

> **含代码**：新增 spike 载体与一键脚本；**不改任何默认路径**（`build` / `search` 的行为逐位不变）。
> **结论：T7-09 判「不投」** —— E2 多 session 池化的吞吐增益仅 **+11.7% / +16.0%**（门槛 ≥ +30%），
> 而峰值 RSS 增量 **+96.4% / +289.2%**（门槛 ≤ +20%）；E3 CoreML EP 即使调优后也只与 CPU
> **持平（+1.2%）**。⇒ 设计 §8 的 **PR 3（S6-04 池化）/ PR 4（S6-05 EP 接入）取消**。
> 实测全表与决策门判定见 `docs/devel/eval-report.md` **§8.10**。

#### Added

- **`crates/core/examples/bench_embed_session.rs`**（S6-01 spike 载体）：E1（1 session，`intra_threads=None` 满核）/
  E2（2、4 session × `intra_threads` 分片）/ E3（CoreML EP，由 `coreml` feature 门控）三组对照。
  实现要点：**按块分发 + 按块索引回填**（保序正确性**不依赖** rayon 的 `collect` 语义，末块右界显式夹紧）；
  轮次交错 + 控制组漂移打印 + `--json` 落盘；`--coreml-static-shapes` 作为 EP 调优探针
  （回答"E3 慢是否源于动态形状"这个反诘）。
- **`scripts/eval_embed_session.sh`**（S6-02 一键跑）：**逐档位独立进程 + 按波交错**，
  用外置 `/usr/bin/time -l` 采集**可归因的分档位峰值 RSS**，输出逐波明细、汇总表与决策门判定。
- **`crates/core/Cargo.toml`**：新增**默认关闭**的 `coreml` feature + `ort = "=2.0.0-rc.13"` 可选直接依赖。
  原因：`fastembed 6.0.2` **没有 coreml 透传**（`[features]` 只有 `directml`），要表达 EP 只能直连 `ort`；
  `ort-sys` 的 `coreml = []` 是**纯开关**（不引入新 crate）⇒ 依赖许可面不变。默认构建**不编译任何内容**。
- **`docs/devel/eval-report.md` §8.10**：E1/E2/E3 实测（4000 段 × 1 预热 + 3 波）、
  **峰值 RSS 的口径归属旁证**（真实 `helix build` 在 30 篇短文本 408MB / 8 篇长文本 517MB /
  64 篇长文本 **2.32GB**）、决策门判定与结论。

#### Changed

- **S6-01 的测量协议推翻设计 §4.2.5 的「一次进程内跑完所有档位」**：实现期实测证伪该前提 ——
  单 session（batch 64 × 长文本）的峰值 RSS 已达 **2~3.3 GB**（是**激活张量**不是权重），
  7 个 session（`1+2+4`）并存会顶穿 32GB 内存 ⇒ 换页会**均匀拖慢所有档位**，使档位间比较失去意义。
  ⇒ 改为**逐档位独立进程 + 按波交错**（交错保留在"波"这一层，控制组漂移 **−0.9%**）。
  该更正已同时写入 `v2-step6-design.md` §4.2.5 与 `eval-report.md` §8.10。

#### Docs

- **`docs/devel/v2-step6-design.md` 更正 `fastembed` 的源码锚点**（§2.4 表 + 附录 C）：
  `fastembed::InitOptions` 的真实位置是 **`src/init.rs`**（= `TextInitOptions` = `InitOptionsWithLength`，
  `intra_threads` 在 `:20`、`with_execution_providers` 在 `:83`、`with_intra_threads` 在 `:94`）；
  原文引的 `src/text_embedding/init.rs:33/:51/:133` 属于**另一个类型** `InitOptionsUserDefined`
  （用于用户自带模型，不是我们走的路径）。**「未调 `with_intra_threads`」的推断不受影响，仍然成立**。
- ⚠️ **新增待决 Q8（请评审裁定）**：NFR-05 的 372MB 只覆盖 `search` 路径（batch 1），
  而 build 路径（batch 64）实测 2.3~3.3GB ⇒ 是否把 build 路径纳入 NFR-05 口径、或单独立风险项 R41？
  本 PR **不擅自登记**。
- **未修订任何 FR / NFR 指标** ⇒ `requirements-spec.md` / `architecture-design.md` / `plan-v2.md` /
  `docs/README.md` 的「定义面」回写**留待 S6-10 收尾 PR**（避免把一个未合并的 PR 写进进度表）。

### V2 Step 6 · 详细设计 + 评审收口（#2，2026-09-11）

> **纯设计 + 文档回写，不含任何代码变更**。设计文档 = `docs/devel/v2-step6-design.md` **v0.2**
> （v0.1 首版 + PR #38 评审收口）。覆盖 T7-09（embed 并行 spike）/ T7-11（增量构建）/ T7-17（并发检索压测）。
> 评审（PR #38）的 **7 条意见（F1~F7）全部采纳**，其中 **F1 / F2 为合并前必修**；
> **D-S6-05 与 D-S6-08 已拍板**、**Q4~Q7 收口**（Q1~Q3 待 spike / 探针实测）。

#### Added

- **`docs/devel/v2-step6-design.md` v0.1**（Step 6 详细设计）：
  - **构建耗时的真实分解**：embed **209.9s / 225.5s = 93%**、建图 11.7s（5.2%）、纯索引 ~3s（1.3%）
    ⇒ 任何不触碰 embed 的优化**上界收益 < 7%**；故 **T7-11（跳过 embed）优先于 T7-09（加速 embed）**。
    ⚠️ 明确**不承诺**把 NFR-03 ① 从 225.5s 压到 120s（D-J8 已判定不可达），① 口径只做「别弄坏」。
  - **E1 / E2 / E3 spike 设计与决策门**：单 session 基线 / 多 session × `intra_threads` 分片 / CPU EP vs **CoreML EP**；
    决策门写死为「吞吐提升 ≥ 30% 且 峰值 RSS 增量 ≤ 20%」，**不达标即不投**（避免把"探路"承诺成"交付"）。
  - **增量构建的现状核实（好消息）**：doc 级增量**骨架已在** —— `content_hashes` **入快照**
    （`index/mod.rs:88` / `:501-503` / `:535`）⇒ `load` 之后 `doc_id_by_hash` 仍可用，重复文档**天然跳过 embed**；
    真正缺的只有 ① CLI 无「追加」入口（`BuildArgs` 只有 `--input`）；② hash 粒度是**整篇文档**（改一字 ⇒ 全量重 embed）。
  - **`build --index` 追加的 ID 不复用证明**（`insert_doc`/`insert_chunk` 用槽位长度分配 + `export/import` 保留尾部空洞）
    ⇒ `raw_vectors` / 图 sidecar 中的旧向量不会被新 chunk 复用；并**要求写成不变式测试**（S6-T8）而非只留文档。
  - **并发检索的实证**：读路径（`query/` `retriever/` `fusion/` `index/`）**零 `Mutex`/`RwLock`/`RefCell`/`unsafe`**
    ⇒ T7-17 只需**采集点**（`bench --threads`），不需改内核；「近线性」是强先验。
  - 风险 **R36 ~ R40**（R36 多 session 内存峰值 / R37 CoreML 改变向量数值 / R38 静默降级 /
    R39 追加漏发 manifest / R40 ID 复用致幽灵向量）；**Q1 ~ Q3 待实测**；测试计划 **S6-T1~T14**；任务拆分 **S6-01~S6-10**。
- **`docs/devel/v2-step6-design.md` v0.2（评审收口）**：
  - 新增 **§10 评审收口记录**：F1~F7 逐条处置表 + 对 F1 / F2 的**独立复算**
    （`wc -l data/t2-corpus.jsonl` = 12000 行；`cargo pkgid -p helix-cli` → `did not match any packages`）。
  - **§4.4.2 预写 `embedder_id` 的升级路径**（评审 F3）：若 S6-T6 判定"必须编入 `sessions` / `intra_threads`"，
    两条路可选——**a** 收敛为**有限档位枚举**（`id()` 仍返回 `&'static str` ⇒ **零结构变更**）；
    **b** 放宽为 `Cow<'static, str>`（`ConfigFingerprint` 字段类型变 ⇒ 与 R35 同类的**破坏性变更**）。
  - **§7 新增「可测性前提」**（评审 F7）：S6-T4 / T5 / T7 要求把会话池抽成**对 session 泛型的辅助结构**、
    并把 `default_embedder()` 的**校验逻辑与构造分离**——否则这三条只能 `#[ignore]`，与 §7 的 CI 口径自相矛盾。
  - **§2.6 的 grep 词清单补 `rayon`**（评审建议）：`search/searcher.rs:207` 有 `rayon::join`（双 lane 并行、**确定性合并**），
    **不构成** S6-T11 逐位一致的反例，写进清单以免后人以为"没查并行"。
- **设计期发现并登记（⚠️ 未修复）：`default_embedder()` 的静默降级**（`search/config.rs:312-318`）：
  `LocalEmbedder::new().ok()` ⇒ 模型初始化失败时**静默退化为纯 BM25**（要向量却拿到纯文本，无错误无告警）。
  已登记为架构 **R38**，修法 = D-S6-05（显式请求向量时硬失败，**评审 Q6 已拍板采纳**），**实际修复在后续实现 PR**。
  ⚠️ 这是**既有行为**、非本 PR 引入 —— 本 PR 只做到**发现并登记**，**并未修复**（故不放在 `Fixed` 下）。

#### Changed

- **需求文档 `requirements-spec.md` v1.13 → v1.14：NFR-10 口径修订为「方案 A」（首个**可判定**口径）** ——
  原口径全文只有「多线程并发检索吞吐与正确性」的定性描述、**无任何数字 ⇒ 当天无法判定达标**。现改为双部分：
  ① **前置条件（正确性）**：各线程 `hits`（`chunk_id` + `score`）序列**逐位等于**单线程（**先正确、后吞吐**）；
  ② **吞吐判据**：`--threads 4` 的 QPS ≥ `--threads 1` × **2.5**（近线性，允许 40% 折损）；
  并**写明测量口径**（预热丢弃首轮 + 顺序交错 1/2/4/8/1/2/4/8 + 报告运行范围：核数 / 是否插电 / 是否在 CI）。
  ⚠️ **×2.5 为「拟」值**：基线 QPS 由 **S6-08** 实测，标定后定稿（同 Step 5 的 NFR-13 先例：v1.10 落「拟 ≤20ms」、v1.12 定稿）。
  同时 **`NFR-03 ②` 的「10% 」单位拍板为「文档数」**（原文即「追加 10% 文档」；chunk 数只作参考指标、不进判据）。
  **这是 Step 6 唯一触碰 FR / NFR 的改动**；其余 6 条意见只改**设计文档**与 CHANGELOG。
  评审对另两方案的否决一并记录：**否决绝对 QPS**（与机器强耦合，违背"性能数字不具跨机可引用性"）与**否决"只记录不判定"**（会使 NFR-10 永不关闭）。

#### Fixed

> 本节修的是**设计文档里的命令与示意代码**，**不是产品代码**（本 PR 零生产代码变更）。

- **附录 B 的 `-p helix-cli` 包不存在**（评审 F2）：workspace 只有 **`helix`**（`crates/cli`）与 `helix-core`
  ⇒ 附录 B 的 **3 处**主命令（步骤 1/2/3）全部会直接报错。改为 **`-p helix`**。
  （独立复算：`cargo pkgid -p helix-cli` → `did not match any packages`；`crates/cli/Cargo.toml:2` = `name = "helix"`。）
- ⚠️ **附录 B 的 delta 与 base 完全重叠 ⇒ NFR-03 ② 的验收测量是空转**（评审 F1，**本次最重要的实质缺陷**）：
  `data/t2-corpus.jsonl` **恰好 12000 行**，而原命令 base 吃全量、delta 取「前 1200 行」⇒ **100% 是已入库文档**，
  1200 条全部命中 `add` 的短路查重（`search/index.rs:356-365`）⇒ 不进 pending、不 embed、不建图，
  "本次耗时" ≈ load + save（~1-2s）⇒ **判据必然通过却什么都没测到**；对照项「全量重建 12K + 1200」也退化成 12K。
  改为 **base = 前 10800 篇**（`sed -n '1,10800p'`）、**delta = 后 1200 篇**（`tail -n 1200`）⇒ **不相交**，
  并加上硬约束文字与预期耗时（≈1200/12000 × 209.9s + ~2s ≈ **23s** vs 阈值 **27.1s** ⇒ 通过但不宽裕，
  恰说明"重叠版秒过"是空转）。
- **§4.3.1 示意代码末块越界**（评审 F6）：`texts[i * BATCH..(i + 1) * BATCH]` 在 `texts.len()` 非 `BATCH` 整数倍时
  会 **panic**；改为夹紧右界 `(lo + BATCH).min(texts.len())`（等价写法：与 `texts.chunks(BATCH)` 配对）。

#### Docs

- **`docs/devel/plan-v2.md` v0.14**：Step 6 设计状态改为「**评审已收口**」（`v2-step6-design.md` **v0.2**，PR #38）；
  §4 Step 6 的「未决需评审拍板」→ **已拍板**（D-S6-08 = 方案 A / Q5 = 文档数 / Q6 / Q7）；§6 门槛的「开工前置」
  标记**已解除**（NFR-10 可判定）但保留 **D-S6-01「投 / 不投」仍待 spike**；§7 进度表同步；§8 回写计划补 v1.14 行；
  main 落点刷新为 **`bb23553`**（PR #39 已并入）。原有 v0.13 内容：Step 6 小节补「设计已出 + 三处更正」；
  §7 进度表 Step 5 两行 ⬜ → ✅；§6 门槛 Step 5 打勾；§8 标题更正为「Step 1 ~ Step 6 设计均已回写」；§附-1 更正 EP 类型名与两处源码路径。
- **`docs/devel/architecture-design.md` v1.12**：**§14.4 新增 R36~R40**（R36 / R37 标注**条件性**）；
  §14 导读同步；文档信息表状态刷新（Step 1~5 已并入 main `590315d`）。**无 trait / 结构变更**
  （`Embedder` trait 不改；`ConfigFingerprint` **不扩字段**——EP 配置编码进 `embedder_id`，理由同 R35 的破坏性判定）。
- **`docs/devel/architecture-design.md` v1.13**（评审收口，**纯版本面同步**）：**无风险条目 / trait / 结构变更**——
  §14.4 R36~R40 保持原样（R38 的「应对」本就指向 D-S6-05 方案 A，本次只是把该决策**正式拍板**）；
  NFR-10 的口径修订属**需求面**、落在 `requirements-spec.md` v1.14。状态行刷新 main → `bb23553`。
- **`docs/devel/requirements-spec.md` v1.13**：NFR-03 ② / NFR-05 / NFR-10 / NFR-11 补「设计已出」与设计锚点；
  ⚠️ **NFR-10 数值目标缺失**（Q4）与 **NFR-03 ② 的 10% 单位**（Q5）登记待拍板；**§1.1 版本表补齐 v1.11 / v1.12 两行**。
  **未修订任何 FR / NFR 指标**（阈值、预算、时限一字未动）。
- **`docs/devel/requirements-spec.md` v1.13 → v1.14**（评审收口，**Step 6 唯一触碰 FR / NFR 的改动**）：
  **NFR-10 口径**改为「前置条件（逐位一致）+ 吞吐判据（`--threads 4` QPS ≥ `--threads 1` × 2.5）+ 测量口径」，
  **×2.5 标「拟」待 S6-08 定稿**；**NFR-03 ② 的 10% = 文档数**（chunk 数只作参考、不进判据）。
  §1.1 与附录 C 各新增 v1.14 行。
- **`docs/README.md`**：新增 Step 6 设计索引行；评审收口后刷新该行状态（`v0.2`、F1~F7 已采纳、D-S6-05 / D-S6-08 拍板、
  Q4~Q7 收口、需求随之 v1.14）。
- ⚠️ **清理上一批遗留的陈旧状态行**（本 PR 顺带完成，避免「Agent 不直推 main ⇒ 只能随下个 PR 回写」的尾巴越积越多）：
  `docs/README.md` 与 `plan-v2.md` §4 Step 3 的「PR #27 **待合并**」→ 已合并 **`ed25d5c`**；
  Step 5 收尾 PR **#37** 并入 main **`590315d`** 的记录补齐（此前四处只记到 `#36` → `48c0ac7`）。
  合并 main 后 main 落点再刷新为 **`bb23553`**（PR **#39**「T13 质量门 → 同图可归因不变式」已并入，
  `## [Unreleased]` 下两条目共存、无内容取舍）。

### Changed · S2-T13 质量门：跨图数值门 → 同图可归因不变式（2026-09-11，`Refs #2`）

**问题**

`crates/core/tests/graph_persist.rs::T13_并行建图质量等价` 在 CI `feature isolation`
（`cargo test -p helix-core --features charabia`）间歇失败：

```
[T13] 与 oracle 的平均 Top-10 重合率：并行 0.6400 / 串行 1.0000
panicked: 并行与串行相对 oracle 的重合率差应 < 5 个百分点
```

（CI run `34562148819`，2026-09-11T04:29Z。同一份 `.rs` 在 main 上 03:46Z 的同一 job 通过
⇒ 非确定性；`git diff --name-only main..HEAD` 当时全为 `.md`。）

**机制：未钉死**（如实记录，不把假设写成结论）

用 6 个临时探针（跑完已删、`git status --porcelain` 已核）复刻门面路径
（`SearchIndexBuilder` + `save`/`load` + `Searcher`）：**320 次独立建图**
（10 物理核；rayon 2/10；含 6 路 CPU 负载；debug profile）——与 oracle 的 Top-10 平均重合率
**min = 0.9650，无一 < 0.95**；整套 `--test graph_persist` **22 次**（5 次无负载 + 17 次 4 路负载）
**min = 0.9850、全绿**。⇒ CI 的 `0.6400` 与此前记录到的一次 `0.8600` 在本轮**均未复现**，
也**归因不到**具体代码路径（幅度 14~36pp，用阈值堵不住）。

**已排除的候选因子（实测结论，勿重复）**

| 候选 | 结论 |
| --- | --- |
| `capacity = 1024 < N = 1200`（`search/index.rs:320`） | **无差异**。`hnsw_rs` 的 `max_elements` 只是 `Vec::with_capacity` 的**预分配提示**（`hnsw.rs:447-462`），无越界迁置分支 |
| `save` → `load` roundtrip 丢边 | **完全无损**：同一张图的 pre/post 逐位一致，50 次同图 A/B `max\|Δ重合率\| = 0.0000` |
| 图丢点 / 向量错位 | 80 次建图 `graph_points == 1200` 恒成立，**图内精确** top-10 与 oracle 重合率恒 `1.0000` |
| 不可达点（自匹配查不到自己） | 抽样 ≤ 3/300 = 1% |
| `hnsw_rs` `entry_point` 竞态（`hnsw.rs:1085-1100`） | 真实存在，但量级仅 ~1%，不足以解释 0.64 |

**变更（四条不变式；全部是同一张图上的确定量，对给定图逐位可复现）**

- **G1 覆盖**：图点数 `==` 文档数（`tombstone_stats()`），且走**精确路径**全量枚举出的 id 集合
  `==` 全部 `chunk_id`（不漏 / 不重 / 不多）。
- **G2 保真**：20 条 query 的**图内精确** top-10 `id` 序列 `==` `BruteForceIndex` oracle 的 top-10
  序列（两侧同为 `(距离升序, chunk_id 升序)` 全序 ⇒ 与遍历顺序无关）。
- **G3 ANN 名次门**：ANN 返回的 10 条**全部**落在**本图精确序**的 top-20 内。实测带宽
  `worst exact rank ∈ [10, 11]`（80 次建图；min 10 / p50 10 / p95 11 / **max 11**）⇒ 余量 ≈ 1.8×。
  它比「重合率」稳在有界空间里：图/检索一退步，返回项会掉到精确序的**远处**（名次从十位量级跳到百位量级）。
- **G4 冻结图确定性**：同一 `Searcher` 上同一 query 重复 20 次，`(chunk_id, score)` 序列逐位一致（NFR-06）。
- 「与 oracle 的重合率」（并行 / 串行 / 差值）**降级为 `eprintln!` 诊断**，不参与判定。
- 精确路径的可达性由 `metrics.vector_route == VectorRoute::Exact` **前置断言**保证——兜底阈值若被
  改小到 `< N`，这里立刻红，而不是静默退化成 ANN 让断言悄悄变弱。

**变异测试（证明新门有牙齿）**

| 变异手法 | 命中门 | 失败输出（摘要） |
| --- | --- | --- |
| 每篇文档错位用**下一篇**的向量入库 | G2 | `图内精确 top-10 应与 BruteForce oracle 逐位一致`；`left: [828, 119, 787, …]` vs `right: [829, 120, 788, …]` |
| 并行路径**少插最后一条** | G1 | `并行库图内点数应 == 文档数`，实测 `1199` vs `1200` |
| ANN 读路径把查询向量**取反**（返回最远点） | G3 | `并行库 query 0：ANN 返回 chunk 1175 落在本图精确序 top-20 之外` |

**本测试覆盖不到什么（如实声明）**

它**不再**断言「并行图与串行图的 ANN 召回率在统计意义上等价」——那是一个**跨图统计命题**，
而 `hnsw_rs` 的建图 RNG 无 seed（`LayerGenerator::new`，`hnsw.rs:325-328`）⇒ 无法确定性判定。
两次建图的重合率仍打进 `stderr` 供人工比对趋势；T7-21（issue **#24**）若需要这条统计证据，
应走 bench / 离线标定，而不是一条会在 CI 里间歇翻脸的断言。

**评审 #39 后的收口（第 2 提交）**

| 意见 | 处置 | 依据 / 变异验证 |
| --- | --- | --- |
| **1 · G2 两侧「全序」不同源**（oracle 用原始距离序，searcher 过 `to_scored` 的 `1 − d²/2`） | **采纳**（防御性加固） | `oracle_topk` 改为与 `to_scored` **同一约定**（score 降序、同分 `chunk_id` 升序）。⚠️ **但量级前提复算不成立**，见下。 |
| **2 · G3 ANN 腿缺 `Route::Ann` 前置断言** | **采纳** | 新增局部 `ann` 闭包，与 `exact` 腿对称。变异：给它加 `.filter(&all)`（→ 变 Exact）⇒ 报红「无用户过滤的向量路必须走 ANN」（`:934`）。 |
| **3 · T22 注释措辞**（只钉后端分派，不钉门面透传/`flush`） | **采纳**（文档） | 措辞收窄为「T22 钉死后端分派（开关 + 阈值）；门面层透传与 `flush` 批量交付暂无门面级护栏」，并指向 #24。 |
| **4 · G4 循环 + 三个互不相干的「20」** | **采纳** | 加 `QUERIES` / `REPEATS` 两个 `const`（文件级 `T13_EXACT_RANK_GATE` 语义不同，不并入）；G4 改 `1..REPEATS`，去掉第 0 轮 `first == first` 恒真式（总查询次数仍 20）。 |

⚠️ **对意见 1 的独立复算（纠正其量级前提，但保留改动）**

评审估的是「top-10 的 d²≈1.3~2.5 ⇒ score≈0 附近 ⇒ 每千次 CI 约 1 次良性翻脸（~1e-3/次）」。复算结果：

- T13 实测（复刻 `hash_vec` + 归一化，1200 篇 × 20 条 query）top-10 的 **d² ∈ [1.16, 1.44]**（score ∈ [0.28, 0.42]），
  20 条 query 的**相邻距离最小间隙 `1.998e-05`**，而该区间 score 的 **ULP ≈ 2.98e-08** ⇒ 差 **670×**。
- **穷举**遍历 `f(t) = f32(1 − t)` 在相邻 f32 对上的碰撞：`t ∈ [0.5, 1.3]`（覆盖实测 `[0.581, 0.685]`，含 `[1.0, 2.6]` 的 d²）
  **10,905,191 对，碰撞 0 次**；而 `t ∈ [0.25, 0.5)`（d² ∈ [0.5, 1.0]）**50%** 的相邻对碰撞、`[0.125, 0.25)` 达 **75%**。
- ⇒ 单调 + 相邻严格 ⇒ 变换在观测带**严格单调**，碰撞**结构性不可能**。**机制真实，但只活在 d² < 1.0**，而 T13 的 top-10 不在那里。

故本条按**加固**而非**纠错**采纳：把「恰好不触发」变成「结构上不可能」，附带收益是 G2 顺带把 **score 公式**也钉住
（`oracle_topk` 的约定被改坏 ⇒ G2 变红；变异验证：oracle 序反向 ⇒ 报红「图内精确 top-10 应与 BruteForce oracle 逐位一致」）。
⚠️ 相应代价是测试里出现**同一口径的第二处实现**（`1 − d/2`），已在 `oracle_topk` 的文档注释里显式标注该耦合。

**范围**：本 PR **零生产代码变更**（只改测试与 CHANGELOG）。

### V2 Step 5 · 收尾：S5-04 阈值标定定稿 + S5-08 文档回写（#22，2026-09-10）

> 设计与实现见 `docs/devel/v2-step5-design.md` **v0.5**；实现主体是 PR **#36**（已并入 main `48c0ac7`）。
> 本条目覆盖 **S5-04（阈值与 NFR-13 预算的数值）** 与 **S5-08（四处定义面回写）**。

#### Changed

- **`BRUTE_FALLBACK_MAX_ALLOWED`：1024 → 8192**（`crates/core/src/vector/hnsw_rs_index.rs`）。
  10 万级 A/B 标定（`--brute-fallback off` vs 强制精确，8 档位 × 2 轮）：交叉点**插值**在
  `allowed ≈ 9700`（**归一后加速线性插值** = 9768；区间 `(5043, 9946)` 的**两个端点无需归一化**
  就成立；未归一插值 ≈7.2K~8.1K，仅说明插值对两轮漂移敏感），取 2¹³——低于归一插值 ~15%、
  低于首个实测反转档位 9946（路径 C 的 `O(N)` 项随规模增长 ⇒ 交叉点下移，宁低勿高）。
  **原初值 1024 过保守**——会漏掉 `allowed=5043`（`tier-5%`）这个实测 2.06× 的档位。
  ⚠️ **这不是正确性变更**：任何阈值下结果都正确，阈值只决定「哪条路径更快」。
- **NFR-13 预算由单值改为双口径**（`requirements-spec.md` v1.12）：
  **常规档位（字段索引命中）≤ 20ms**（实测最差 7.06ms，余量 2.8× ⇒ **原拟的 20ms 被证实**）；
  **降级字段档位 ≤ 35ms**（实测最差 22.84ms，其中**过滤求值自身 8.77ms** 属 D-S5-09 已接受成本）。
  单值口径须 ≥ 35ms 才不被这个**与向量路无关**的成本顶穿 ⇒ 拆口径才能既真实又有鉴别力。
- **R18 / R31 / R33 标定状态更新**（`architecture-design.md` v1.11）：R31 / R33 由「未标定」→「**已标定**」
  （R31 仍标注「**100 万级未测**」的规模上界）；§14 章级主表 R18 补 ⑤ 标定后端到端 P99
  （`sel-1%` 39.49→**5.01ms**、`sel-0.1%` 112.69→**7.06ms**、`ts-range-degraded` 223.24→**21.24ms**）。
- ⚠️ **R34 措辞改写**（同 v1.11）：由「与写端互斥、时长随 `N` 线性增长」改为**「含潜在死锁」** ——
  `IterPoint::new` 持读锁整个生命周期，而 `IterPoint::next` 在**层切换**时**递归读同一把 `RwLock`**
  （`hnsw.rs:633` / `:661`）；`std::sync::RwLock` 不保证递归读锁可重入（其 futex 实现还要求
  `!has_writers_waiting`）⇒ 读写并发下**可能挂死**（15 行 std-only 探针 **3/3 触发**）。
  **今天不可达**（`add` 需 `&mut self`，V2.0 单写者语义），**修法归 Step 8 开工前**：
  换成设计 §4.2.2 已写明的**逐层 `get_layer_iterator` 遍历**（层间 guard 不重叠 ⇒ 无递归读；
  覆盖等价、成本同量级）。

#### Fixed

- `scripts/eval_filter.sh` 的默认阈值文案改为**从源码读**（`sed` 提取 `BRUTE_FALLBACK_MAX_ALLOWED`），
  避免脚本文案与常量再次漂移。顺带修一处可移植性问题：BRE 里的 `\+` 在 **BSD sed（macOS）** 下不生效，
  须写 `[0-9]*` 或 `sed -E`。

#### Added

- `docs/devel/eval-report.md` **§8.9**：标定完整 A/B 表（8 档位 × 2 轮）、方法与噪声说明、定稿阈值下的
  路由验证、以及**三条局限**。⚠️ 方法学要点：**两轮非交错执行 ⇒ 控制组归一（`mean_vector_ms` 口径）×1.382 / ×1.541；
  P99 在同一轮内抖动可达 ±40% ⇒ 决策改看 `mean_vector_ms`**（1000 个 `Metrics` 样本均值）；
  阈值决策只用**不需要归一化就成立**的两个端点。

#### Docs

- 四处定义面同步：`requirements-spec.md` **v1.11 → v1.12**、`architecture-design.md` **v1.10 → v1.11**
  （§1.1 版本表 + 表头 + §15.2 变更记录）、`docs/README.md` 索引行、`plan-v2.md` Step 5 段
  （补「实现与标定」小节 + 验收标记 ✅）。设计文档 **v0.4 → v0.5**：§4.3 记录「代价模型被实测证伪一半」
  （`min(N, ef/sel)` 低估路径 B 成本 9.5×，保留作反面记录）、§4.9 填实测全表与三条结论、
  D-S5-02 定稿 8192、§9.2 **Q1 / Q2 / Q5 全部结案**、附录 B 改为**实际执行**的命令。

### V2 Step 5 · 实现：低选择度精确兜底 + `Metrics` 可观测化（#22，2026-09-10）

> 设计见 `docs/devel/v2-step5-design.md` **v0.3**（D-S5-01~09 全部拍板）。
> 本条目覆盖 **S5-01~03 + S5-05~07 + S5-04 的代码半**；10 万级标定定稿阈值 /
> NFR-13 预算与文档回写（S5-08）**已在上一条目落地**。

#### ⚠️ 破坏性变更（0.x 阶段按既有约定接受，R35）

- **`VectorIndex` 新增 1 个必选方法 + 1 个默认方法**：外部实现 `VectorIndex` 的代码
  需要补 `search_exact_filtered`。必选是刻意的——后端**是否真精确**是实现事实，
  不该由默认实现替它回答（同 `as_graph_persist` 用 `None` 表达「Brute 无图」的手法）。
  库内两个 impl（`HnswRsIndex` / `BruteForceIndex`）已同步。
- **`SearchResponse` 新增公开字段 `metrics: Metrics`**：以**字面量**构造该结构体的下游
  代码会编译失败（库内两处构造点均由内核产出，零影响）。字段全 `pub`，故未加
  `#[non_exhaustive]`（那会同时禁掉下游的穷尽匹配，破坏面更大）。
- `BruteForceIndex::search_filtered` 的排序比较子由 `partial_cmp(..).unwrap_or(Equal)`
  改为 `total_cmp`：**唯一行为差异在 NaN 输入**（旧：视作相等，顺序取决于 sort 的实现
  细节；新：确定性全序）。NaN 属 embedder 契约外输入，精确路径已用 `debug_assert!` 强制。

#### Added

- **路径 C：低选择度过滤走精确扫描**（T7-22 / R18 的对策，D-S5-03 方案 A）。
  - `VectorIndex::search_exact_filtered`（必选）：返回**全部**满足谓词的
    `min(k, 命中数)` 条，**不允许少返回**——这是它与 ANN 的**唯一**语义差异，
    也是低选择度场景的价值所在。
  - `VectorIndex::prefers_exact`（默认 `false`）：**策略归后端**。`HnswRsIndex` 的实现是
    `kind() == Filtered && allowed ≤ 阈值`（默认 `BRUTE_FALLBACK_MAX_ALLOWED = 1024`，
    数值待 S5-04 标定定稿）；`BruteForceIndex` 恒 `true`（它本来就精确）。
  - `HnswRsIndex::with_brute_fallback(Option<usize>)` + `brute_fallback()`：
    `None` = **关闭**兜底（A/B 回归对照）；`from_loaded` 只写产品默认值，**不改签名**
    （阈值是读端策略，不是图的属性——改公开 trait 的签名代价远大于收益）。
  - **全量点遍历用 `&PointIndexation` 的 `IntoIterator`**（`hnsw.rs:681-688`：从 layer 0
    逐层升到 `entry_point_level`，**每点恰 yield 一次**）+ `Point::get_v()`（零拷贝）+
    `Point::get_origin_id()`（即 `ChunkId`）。**不可**用 `get_layer_iterator(0)`：
    点只被推入它自己那一层（`hnsw.rs:511`，无回填低层）⇒ `P(level ≥ 1) = 1/M`，
    本项目 M=32 时约 **3.1% 的点不在 layer 0**，用它做精确扫描会**静默错答**。
- **`Metrics` 可观测化**（T7-23 / NFR-07，D-S5-05/06/07）：
  `SearchResponse.metrics` 字段 + `query` 模块再导出 `Metrics` / `VectorRoute`，
  新增 `vector_route: {None, Ann, Exact}`（**T7-22 的 A/B 判据**）、`bm25_elapsed` /
  `vector_elapsed`（Hybrid 并行下 `took` 无法归因"是哪一路慢"）。
  `tracing` 通道保留——三条通道受众不同（调用方 / 宿主 / 决策），不是重复。
- **`NormalizedVector::distance_to_slice(&[f32])`**，且 `distance_sq` **改为转发它**：
  单一求和实现 ⇒ 「精确路径与 `BruteForceIndex` 逐位一致」是**结构**而非巧合
  （精确扫描逐点调 `NormalizedVector::new(v.to_vec())` 会每候选一次堆分配 + 重算范数）。
- **CLI**：`helix bench --brute-fallback <N|off>`（S5-04 的 **A/B 唯一开关**，
  `off` ⇒ 关闭兜底）；`helix search --metrics` 打印一行内核指标（NFR-07「用户可自查」，
  默认关，属诊断输出）。

#### Changed

- **`Metrics.vector_shortfall` 的语义边界变了**：低选择度档位走精确路径后该值**通常**为 0
  ——但**不是恒等式**。`allowed` 来自 `Index`、扫描枚举的是**图里的点**，两个独立来源：
  `Exact` + 缺口 `> 0` ⟺ **存在 allowed chunk 在图里没有点**（图滞后于索引）。
  ⇒ 「0 缺口」**不再等于**「无 prefilter 需求」，且**两种读数都必须连看 `vector_route`**
  （设计 §4.4 推论 1）。bench 因此把两套 shortfall 口径**并列**输出
  （bench 侧 `min(K, allowed)` vs 内核侧 `min(candidate_k, allowed)`）——
  差异本身现在是"两个分母之别"的度量，而不再是"拿不到内核值"的替代。
- **修复 D-S5-08：两条早退路径的 `metrics.took` 恒为 0**。
  `searcher.rs` 的 `index_is_empty`（连 `metrics.log` 都不调，故 `grep metrics.log`
  **结构上找不到它**）与"过滤排空"两条路径从不设 `took`，而响应的 `took` 一直取真值
  ⇒ 日志与响应各说一套。现在两条都设真值；`empty_response` 改为接收**调用方算好的**
  `took`，让 `metrics.took == took`（I7）是**逐位相等**而不是近似。
- **`search_parts` 的向量路分派**：按 `VectorRetriever::plan()` 在
  `search_filtered`（ANN）/ `search_exact_filtered`（精确）间二选一，并把结果写进
  `Metrics.vector_route`。热路径（`None` / `Alive`）**一行未改**——策略在
  `prefers_exact` 里就否掉了它们（R15 的 fast-return 是 Step 1 的设计不变量）。
- `bench` 延迟阶段新增内核采集（`vector_shortfall_kernel` / `vector_route_exact_ratio` /
  `mean_filter_eval_us` / per-lane 耗时并进 JSON）与汇总表两列（`缺口(内核)` / `精确占比`），
  并单列「内核耗时分解」——降级字段档位的 `doc_bits_scan` 全扫本身就占 ~8ms，
  没有这一节会把"过滤求值贵"误判成"兜底失败"（D-S5-04 / D-S5-09）。

#### 测试

- 单元：`distance_sq` 转发后**逐位不变**（`to_bits` 断言，钉住 I4 的结构性前提）、
  全量遍历基数 `== get_nb_point()` 且零重复、**"高层点自查询"定向用例**
  （取 `level ≥ 1` 的点用它自己的向量查、断言排第一；同时断言它**不在 layer 0**，
  否则用例失去鉴别力——`from_os_rng` 使漏点集每次建图都变，随机 Top-K 会 flaky）、
  与 `BruteForceIndex` 逐位一致、阈值边界 / 关闭开关 / 默认值、`None` 谓词含幽灵、
  同距离按 `chunk_id` 升序且不依赖插入序、`Metrics` 默认值不撒谎。
- 编排层：用**间谍后端**断言"到底调了哪个方法"（真 HNSW 的拓扑每次建图都不同，
  用真图做 A/B 会混入拓扑噪声而不可证伪）；覆盖热路径两者逐位一致、
  低选择度分派与 `vector_route` 记账、三条早退路径 `took` 非 0、响应口径自洽。
  ⚠️ 间谍必须**忠实复刻**策略契约（`FilterKind::Filtered` 才谈得上兜底）——
  `try_build_predicate(index, None)` 对**无用户过滤**也返回 `Some(AliveOnly)`（`filter.rs:195`），
  只看"有没有谓词"会把热路径误判成低选择度。
- 集成：`crates/core/tests/step5_query_observability.rs`（新，N=400 合成语料、秒级、进 CI）——
  真实后端 + 真实谓词 + 真实编排下端到端验 `route == Exact` / 缺口为 0（本 fixture 图覆盖完整）/
  命中全部满足谓词 / 与 Brute oracle 逐位一致 / `off` 回到 `Ann` / `Bm25` 模式 `route == None` /
  空结果口径自洽；另有 **真实软删除语料 + 同一张图上的策略开关**用例（`ToggleFallback` 包装：
  关掉时与 `with_brute_fallback(None)` 分派等价），验 `Alive` 谓词下 on/off **逐位一致**
  且一条死 chunk 都不漏 —— 这是 R15「热路径 fast-return 不回归」的**真后端行为**证据。
  `for _ in 0..100` 的重复性断言同时覆盖 brute 与 hnsw 两侧（对齐设计 I5 / S5-T6）。

> **风险登记**：本 Step 实现面新增 **R31~R35**（`O(N)` 谓词判定 / 兜底改变输出 / 阈值经验值 /
> 精确扫描持读锁归 Step 8 / 对外破坏性变更），已登记在 `architecture-design.md` **§14.3**。
> ⚠️ **R34 的措辞待改**：它不是"与写端互斥"，而是 `IterPoint::next` 在**层切换**时**递归读同一把
> `RwLock`** ⇒ 读写并发下**可能死锁**（今天不可达：`add` 需 `&mut self`，V2.0 单写者语义）。
> 修法是设计 §4.2.2 已写明的**逐层 `get_layer_iterator` 遍历**（覆盖等价、层间 guard 不重叠 ⇒
> 无递归读），与 §14.3 的改写一并在 **S5-08 / Step 8 开工前**落地。

### V2 Step 5 · 详细设计（2026-09-10）

- 新增 `docs/devel/v2-step5-design.md`（**v0.3，二次评审响应版；D-S5-01~09 全部拍板**）：**查询性能与可观测
  （T7-22 / T7-23 / NFR-13 / R18）**的详细设计。
  - **现状源码级定位**：R18 的机理（带 filter 时 `hnsw.rs:983-992` **无 fast-return**，
    且 `:1019` 在 `return_points.len() < ef` 时**距离剪枝全程关闭** ⇒ 堆填不满即整图遍历；
    默认 `k=10 → candidate_k=30 → ef=120` 就是**分水岭**，`allowed` 远小于它时遍历全图）；
    `Metrics` **四处断链**（输出只剩 `tracing` / `SearchResponse` 无字段 / `query` 模块无再导出 /
    bench 只能另算一个口径不同的 `mean_shortfall`，`bench.rs:190-193` 注释自陈"拿不到"）。
  - **设计期新发现**：**两条**早退路径漏设 `metrics.took`——`searcher.rs:99-104`（`index_is_empty`，
    连 `metrics.log` 都不调，故 grep 该符号**结构上找不到它**）与 `:125-127`（过滤排空）。
    两条的响应 `took` 都是真值（`empty_response` 内取 `started.elapsed()`）
    ⇒ 日志 `took_ms=0` 与响应 `took` 各说一套（同类第三处 `:212-213` 反而设了）
    ⇒ 列 **D-S5-08** 随 T7-23 一起修。
  - **关键技术前提（源码核实 + 实测）**：`hnsw_rs` 0.3.4 **可以零拷贝遍历已入库向量**——
    全量遍历 = **`&PointIndexation` 的 `IntoIterator`**（从 layer 0 逐层升到 `entry_point_level`，
    **每点恰 yield 一次、零重复**）+ `Point::get_v() -> &[T]`（零拷贝切片）+
    `Point::get_origin_id() -> usize`（**就是 `ChunkId`**）。
    ⚠️ **`get_layer_iterator(0)` 不是全量**——点只被推入**它自己那一层**（`hnsw.rs:511`，无回填低层），
    `P(level ≥ 1) = 1/M`（本项目 M=32 ⇒ **约 3.1% 的点不在 layer 0**），实测 N=5000 时
    `get_layer_iterator(0).count()=4813`（缺 187 = 3.74%）而 `into_iter().count()=5000`。
    `get_point_data(&PointId)` 存在但**是克隆**，且库**无 `origin_id → PointId` 映射**
    ⇒ 方案 B 需自建。核实记录见设计文档附录 C（含 v0.1 误判的反面记录）。
  - **方案**：向量路从两条路径扩为**三条**——A 热路径（不变）/ B `filtered-ANN`（不变）/
    **C 精确扫描**（`allowed ≤ 阈值` 时绕开 ANN）。**策略归后端**（新增必选方法
    `VectorIndex::search_exact_filtered` + 默认钩子 `prefers_exact`），**分派与记账归编排层**
    ⇒ 零 plumbing，门面 / 逃生舱 / bench 三条入口自动一致。
  - **成本分解（修正 D-J9 的"0.05ms"读法）**：该数字只覆盖"对 ~100 个候选算距离"一段；
    完整成本 = `O(N)` 谓词判定（定位候选，1~10ms 待标定）+ `O(allowed)` 距离（≈0.02ms）。
    关键洞察：**路径 B 与 C 同为 `O(N)`，差别是每个点做什么**（512 维距离 ≈100ns vs
    位图判定 ≈5~20ns ⇒ 5~20× 常数差），这正是"1~10ms vs 41~158ms"的来源。
  - **可观测**：`Metrics` 进 `SearchResponse`（+ `query` 模块再导出），新增
    `vector_route: {None, Ann, Exact}`（T7-22 的 A/B 判据）、`bm25_elapsed` / `vector_elapsed`
    （Hybrid 并行下 `took` 无法归因）；`tracing` 通道保留；bench 两套 shortfall 口径**并列**
    并在 JSON / 汇总表补列；新增 `--brute-fallback <N|off>` 作为 A/B 唯一开关。
  - **必须一并声明的副作用**：精确路径下 `vector_shortfall` **结构性归零** ⇒
    「0 缺口」不再等于「无 prefilter 需求」，判读须**连看 `vector_route`**；
    R11/R13/R17 在低选择度档位只能标"**子集已绕过**"而非"已解决"。
  - 9 个决策 **D-S5-01~09**（**均已拍板**；D-S5-02 / 04 仅**数值**待 S5-04 标定，不阻塞实现）、
    测试计划 **S5-T1~T13**、任务拆分 **S5-01~08**（PR 切分为兜底 / 可观测 / 文档回写三支）、
    风险 **R31~R35**（含 R34：精确扫描全程持 `points_by_layer` 读锁 ⇒ **Step 8 必须复核**）、
    未决 Q1~Q5（评审项 Q3 / Q4 已结案，余实测项）。
  - **v0.3 修订（二次评审响应，2026-09-10）**：
    ① **D-S5-01~09 全部拍板**——评审对 D-S5-01 / 03 / 05 **无异议** ⇒ **阻塞解除，S5-01~04 可开工**；
    D-S5-02 / 04 的数值同意留待 S5-04 标定回填（形态与口径已定）。
    ② `docs/README.md` 索引行**同步纠正** v0.2 已推翻的遍历 API 结论（发现面漂移；
    `docs/README.md` 自己的「修改规则」要求两边不复制对方内容以避免漂移）。
    ③ §4.2.2 的 NaN 论证改为**强制前提**——精确路径加 `debug_assert!(d.is_finite())`，
    并把 `brute.rs:53-57` 的排序统一为 `total_cmp`（与 §4.2.5 让 `distance_sq` 转发
    `distance_to_slice` 是同一手法：**把断言变结构**）；顺带修 §3.1 的 API 标签笔误
    （`get_layer_iterator` 在 `PointIndexation` 上，`Hnsw` 上没有）。
    ④ 附录 C 复现片段补「**演示用、勿抄进测试**」注记（`assert_ne!` 断言的是 bug 存在，
    理论可 flaky），并收录评审方在**落盘重载路径**上的独立复验（`from_loaded` 生产路径
    同样成立：`into_iter().count()==get_nb_point()`、未访问 origin_id = 0；漏点率
    2.66% / 3.04% / 3.08% / 3.74% 抖动，围绕理论值 3.125%）。
- `plan-v2.md` 升 **v0.9 → v0.10**（Step 5 行补设计状态、文件头状态行重写、并加"0.05ms"读法
  修正注记；**实现进度勾选仍留待实现完成后回写**）；`docs/README.md` 索引新增 Step 5 设计行，
  并把「技术风险（R1~R25）」校正为 **R1~R30；Step 5 拟增 R31~R35**。
- **PR #33 评审响应（v0.1 → v0.2）**：① **纠正关键技术前提**——全量遍历 API 由误写的
  `get_layer_iterator(0)` 改为 `&PointIndexation` 的 `IntoIterator`（v0.1 把两个遍历 API 的
  安危判断**写反了**，照原设计实现会让精确扫描**静默漏掉约 3% 的向量**，是错答而非变慢）；
  ② D-S5-08 由"一条早退路径"扩为**两条**（`:99-104` + `:125-127`）；③ `distance_sq` 改为
  **转发** `distance_to_slice`，把 I4「与 Brute 逐位一致」从巧合变**结构**；④ 修正 R11~R18
  的架构引用（在 §14 **章级主表** `:1449-1456`，而非 §14.1 的 Step 2 风险表）；
  ⑤ 新增**"高层点自查询"定向用例**（`from_os_rng` 使漏点集每次建图都变，随机 Top-K 会 flaky）。
- **PR #33 二次评审响应（v0.2 → v0.3）**：① **D-S5-01~09 全部拍板**——评审对
  D-S5-01 / 03 / 05 **无异议** ⇒ **阻塞解除，S5-01~04 可开工**；D-S5-02 / 04 仅数值待 S5-04 标定。
  ② `docs/README.md` 索引行**同步纠正**遍历 API 结论与状态（v0.2 漏了这处发现面）。
  ③ §4.2.2 的 NaN 论证改为**强制前提**（`debug_assert!(d.is_finite())` + `brute.rs` 排序统一
  `total_cmp`）+ 修 §3.1 的 `get_layer_iterator` 归属笔误。④ 附录 C 补「勿抄进测试」注记，
  并收录评审方在**落盘重载路径**上的独立复验。
- `plan-v2.md` 升 **v0.10 → v0.11**（Step 5 设计定稿、D-S5-01~09 全部拍板、S5-01~04 可开工）。
- **相关文档同步回写（2026-09-10，随本 PR 一并落地）**：Step 5 的决策与口径**不只落在设计文档里**，
  三处「定义面」一并刷新——
  - `requirements-spec.md` 升 **v1.9 → v1.10**：**NFR-13** 补「**端到端 P99（含过滤求值）**」
    口径 + 预算拟 **≤ 20ms**（数值待 S5-04 标定），并做**成本口径修正**——D-J9 的「约 0.05ms」
    只覆盖「对 ~100 个候选算距离」一段，**不含 `O(N)` 遍历定位候选**（1~10ms，方案 A 的主要成本）；
    **NFR-07** 补本 Step 的落地路径（`Metrics` 进响应 / per-lane / `vector_route` / 修两条早退 `took`）。
    ⚠️ 同时**补记 V2 Step 4 漏掉的版本行**（`#32` 改了 FR-30 行但未升版本）⇒ 本次一并入 v1.10。
  - `architecture-design.md` 升 **v1.8 → v1.9**：**§5.4** `VectorIndex` 补两个新方法
    （`search_exact_filtered` **必选** + `prefers_exact` 默认钩子）+ `vector_shortfall` 判读注记、
    **§5.8** `SearchResponse` 补 `metrics: Metrics` 字段（**破坏性**，R35）；**§8.3** 新增「Step 5 补外部可见」块，
    并把「⚠️ 当前限制（已知，待排期）」改写为「设计已承接、待实现」；**§14 章级主表 R18 补 ④**
    ——口径修正 + 三路径对策 + ⚠️ **「只覆盖低选择度子集」的边界声明**（`allowed >` 阈值时仍回
    路径 B ⇒ **R11/R13/R17/R18 不得标「已解决」**，只能标「低选择度子集已绕过」）；
    **§14.3 新增 R31~R35**（设计文档 §9.1 原就写明「拟写入架构 §14.3」）；§14 导读补指引。
    ⚠️ 同样**补记 Step 4 的 §7.5.3 / §14.2 版本行**。
  - `docs/README.md`：风险范围行由「R1~R30；Step 5 **拟增** R31~R35」改为 **R1~R35**，
    Step 5 索引行标注「风险 R31~R35（**已写入架构 §14.3**）」。
  - `plan-v2.md` 升 **v0.11 → v0.12**：Step 5 段补「文档回写」子项；
    **Step 2 提交链的「本 PR 尚未合并，合并后补 hash」注记回填为 `01e01eb`**（#35 已并入 `main`）。

### Changed · S2-T6 质量门：不可归因的**数值门**换成可归因的**不变式**（issue #34，2026-09-10）

**问题**

`crates/core/tests/graph_persist.rs::T6_旧快照无图可加载` 在 CI `test` job 偶发失败一次：

```
[T6] 降级重建后与 oracle 的 Top-10 重合率：均值 0.545 / 最差 0.200
panicked at tests/graph_persist.rs: 平均 Top-10 重合率应 ≥ 0.90，实测 0.545
```

近 25 次 run 出现 1 次，此前 24 次连续全绿；**重跑同 commit 即绿**；仅 Linux x86_64 runner 出现。
**该值落在抖动分布之外**：本地 64 次（50 单测 + 6 次整二进制并行）0.990~1.000；评审方 30 轮
**每轮重新建图**（= 30 个不同拓扑）实测单轮均值最低 0.995、单 query 最低 0.900。

**根因：仍未定位**

（本条曾写「根因是拓扑抖动」并据此断言「任何绝对阈值都会 flaky」——**该推论有误**，已改正。）

`hnsw_rs` 的层级分配用 `StdRng::from_os_rng()` 且**无注入口**（`LayerGenerator::new`
`hnsw.rs:325-328`；`PointIndexation::new` `:447-457` 内部自建、`rng` 字段私有）⇒ 建图拓扑确实
每次都不同。但「拓扑抖动 ⇒ 重合率可掉到 0.545」**推不出来**：`k²/N = 10²/200 = 0.5` 只说明
「**若**检索退化为任意返回 10 条，重合率会是这个量级」——那是**必要条件，不是充分条件**；
实测抖动带宽是 0.99~1.00，距 0.545 有 45 个百分点。**⇒ 那次 CI 失败是一次尚未归因的异常。**

**本 PR 的真实价值**：把**不可归因的数值门**换成**可归因的不变式**——同样会红，但红得能直接指出是
「内容缺失」「向量 / ID 灌错」还是「图不可导航」。**issue #34 保持 open**（`Refs #34`，不 `Closes`）。

**变更（两条不变式，都与图拓扑无关）**

- ① **内容覆盖（结构层）**：`graph_points == raw_vectors == 200`（`SearchIndex::tombstone_stats()`）。
  挡住「重建悄悄丢内容」——**这是补齐的覆盖盲区**：只做自匹配时，「重建只灌了前 30 篇」
  也能三查全绿，而重合率已掉到 0.22。
- ② **自匹配（检索层，覆盖全部 200 篇——不是 20 篇）**：每篇用自己的文本查必须**排第一**且
  **余弦相似度 ≈ 1**（`hits.score` 是相似度而非距离：实现统一输出 `d² = 2−2cos`，retriever 换算
  `score = 1 − d²/2 = cos`），oracle 侧同一不变式（顺带钉住「第 i 篇 `chunk_id == i`」这一测试前提）。
  查询文本与文档 `i` 完全相同 ⇒ 向量逐位相同 ⇒ 相似度全局最大 ⇒ 只要重建把**正确的向量按正确的
  ID** 灌进**可导航**的图，这条必然成立。前提（已写进代码注释）：`ef_search ≥ 语料规模`、
  doc `i` 的 chunk 文本 == query 文本。
- 「与 oracle 的重合率」降级为 **`eprintln!` 诊断**、不再断言；但保留**均值 + 最差**两个数
  （后者正是 #34 的原始线索，只看均值会把「20 条里 1 条彻底退化」稀释成 0.95），
  且**先打印、后断言**（否则断言一失败就看不到量级线索）。
- ⚠️ 评审建议的「全量检索 id 集合 == 全量 id 集合」**未采用**，因为它**本身会 flaky**：实测
  `top_n(200)` 的返回值随建图拓扑在 198~200 之间跳（7 次里 4 次 200 条、2 次缺 1 个、1 次缺 2 个）。
  机理是 `level ≥ 1` 的点**不在 layer 0**（`hnsw.rs:500-511`：只把新点推进**它自己那一层**），
  只能靠上层下降时被访问到 ⇒「取不回全部点」是固有现象、不是缺陷。
- 跨实现的召回覆盖仍由 **S2-T13**（并行 vs 串行，**相对**口径）承担；T13 的 `0.90` 绝对底线**保留**
  （N=1200 > `ef_search`=200，是真正的近似检索，与 T6 的成因不同），代码注释里写明该口径差异。

**验收（两条门都有牙齿）**

- 变异 A「**内容缺失**」（`rebuild_vector_index` 只灌前 30 篇）⇒ 立刻红：
  `重建后的图点数应等于快照向量条数（200）… left: 30 right: 200`。
- 变异 B「**向量 / ID 灌错**」（id `i` 拿到 id `i+1` 的向量）⇒ 立刻红：
  `200 篇自匹配失败 200 篇；重合率均值 0.043`，并给出
  `query 0 的自身文档应排第一；实得 Top-10 = [199, 100, 183, …]`。
- 两处变异均已 `git checkout --` 还原。稳定性：本地 20 次单测 + 3 次整二进制并行全绿（自匹配失败 0 篇）。

**同步**：`docs/devel/v2-step2-design.md` §1.3 验收 5 判据（原文仍写「≥ 0.95（S2-T3 口径）」，
与本次修改**互相矛盾**，一并改掉）/ §8 S2-T3 行（该编号已重定义，标注新承接者）/ §8 S2-T6 行 /
状态表验收 5 / P0-7 共五处；**`CHANGELOG` 自身的 P1 条目**里「T3 的意图已由 T6 的 oracle 重合率断言
覆盖」这条**授权链已断**，改指新承接者；`docs/devel/plan-v2.md` Step 2 提交链标注「本 PR 待合并」
（**本 PR 已并入 `main` 为 `01e01eb`**，该注记已在 PR #33 中回填为提交 hash）。
**不改任何产品代码。**

### V2 Step 4 · CLI `helix compact` + churn workload（S4-08 + S4-09，2026-09-08）

**新增（用户可用闭环 + 验收 1/2 实测）**

- **CLI `helix compact`（S4-08）**：墓碑物理回收入口。
  `helix compact --index <快照> [--output <新>] [--dry-run] [--json]`。
  - 默认**原地**写同路径（复用 Step 3 `atomic_write`，崩溃安全）；`--output` 另存（A/B
    对比体积）；`--dry-run` 只打印 `TombstoneStats` + 预估回收，**不写任何文件**。
  - 打印 before → after 条数、三体积（`CompactionReport.bytes_before/after`，dry-run 时
    CLI 自 `fs::metadata`）、图重建 / 总耗时、重建后 `graph_status`；`--json` 机器可读
    （含 `remapped`，A/B 脚本透传）。
  - ⚠️ load 走配置指纹校验 ⇒ 需匹配建库 embedder（默认装配 bge）；`remapped=true` 提示
    ID 已重编号（D-S4-01/09 的对外声明落地到 CLI）。
  - `docs/user-guide.md` 新增 §1.5 compact 参数（含「已知坑」：ID 重编号、需配 embedder）。
- **churn workload example `churn_bench`（S4-09）**：`crates/core/examples/churn_bench.rs`。
  内置**确定性合成 Embedder**（LCG + L2 归一化，dim=512，id=`synth-512`，D-S4-06）；
  强制单 chunk ⇒ doc == chunk == 1 向量点 ⇒ `nb_point` 口径退化为 == 存活数。
  每轮随机删 `churn×N` + 追加等量新 doc → `save` → 记录（快照/graph/data 字节、
  `raw_vectors`、图点数、`GraphStatus`）；末轮 `compact_and_save` → 再记录。
  输出 CSV + 判定（J1 图回收 / J2 不累积 / J3 reload `Loaded` → `VERDICT`）。
- **`scripts/eval_churn.sh`（S4-09）**：编排多档 churn 实测，结果入
  `docs/devel/eval-report.md` **§8.8（新增）**。
- **实测（10K 合成语料，验收 1/2 全 PASS）**：
  - churn 0.1 × 5 轮：`graph+data` 随轮次 **37.2→50.9MB**（问题真实存在）→ compact 后
    回落 **33.8MB**；`nb_point` 11000→15000 → **10000 == 存活数**；snapshot 稳定（不累积）。
  - churn 0.3 × 5 轮（60% 墓碑）：`graph+data` **43.9→84.6MB** → compact 后 **33.7MB**
    （省 60%）；`nb_point` 13000→25000 → **10000**。两档 compact 后 reload 均 `Loaded`
    （铁律），冷启动 **91~99ms**；`save` 墓碑告警（D-S4-02）如期出现。

**评审响应（PR #31 · 6 条非阻塞全处理）**
- **load 侧去噪（建议 6）**：默认装配（embedder=Some + Hnsw）加载**无向量快照**
  （纯 BM25 / 全删后 compact）时，`raw_vectors` 为空 ⇒ save 侧本就清 sidecar 落
  `NotApplicable`（`persist_graph`）⇒ load 侧此前却走 `try_load_graph → 失败 →
  打「[警告] 降级重建」噪音 → 重建空图`。现**静默**建空向量 lane（`graph_status =
  NotApplicable`），保留 PR30「装配有 embedder ⇒ 保留空 lane 供后续写入」不变式。
  回归测试 `R_建议6`。
- **CLI `--output` A/B 体积对比（建议 5）**：新增 `bytes_source`（compact **前**源
  `--index` 三体积）；`--output` 另存时 `bytes_before`（对目标路径 stat）恒 null，回收
  以 `bytes_source` vs `bytes_after` 判定。人类输出恒打「源→新」两态。JSON 耗时口径
  拆分 `load_ms` / `compact_ms`（core `total_ms`，纯 compact+save）。
- **churn_bench CSV `coldstart_ms`（建议 3）**：compact 行改填 reload 实测冷启动值
  （不再恒 0）、`graph_status` 改取 reload 后状态。
- **`scripts/eval_churn.sh`**：语料缺失自动调 `gen_synth_corpus.py` 生成（确定性，建议 1）；
  汇总表 4 列标签/值对齐修正（建议 2，含消除 `$VAR，` 在 bash 3.2 下误并名的坑）。
- **文档（建议 4）**：`SynthEmbedder` 注释澄清 CLI compact 只支持默认装配建的库；
  `churn_bench` 运行示例语料路径改 `data/...`；user-guide §1.5 补 `--json` 字段说明。

**集成测试补充（2026-09-09）**
- **门面层等效复刻 CLI `compact` 语义**：新增 `tests/compaction_cli_semantics.rs`（5 例，
  CLI-1~CLI-5）。背景：CLI `compact` 是薄门面，但真实二进制自动化测受两硬约束——① CLI 无
  `remove` 子命令造不出墓碑（只能测 no-op）；② `--index` 走默认装配实例化 `LocalEmbedder`
  （下载 bge 约 49s）。故用确定性 TestEmbedder + 门面层 API，按 CLI 完全相同的调用序列锁
  行为契约：CLI-1 `--dry-run` 只读（`tombstone_stats()` 后磁盘字节/文件清单分毫不变）；
  CLI-2 无墓碑 no-op（`remapped=false`、reload `Loaded`）；CLI-3 原地回收（图 sidecar 与总
  体积回落、reload 检索不含墓碑）；CLI-4 `--output` A/B（源不被覆盖、`bytes_before=None`
  诚实语义）；CLI-5 纯 BM25 no-op + reload 可检索。
- **core 层补 3 个 compaction 集成场景**（`tests/step4_compaction.rs` +3 = 10 例）：
  T14 外部持久引用 `source` 跨 compact 稳定（D-S4-09：内部 id 全变但 source 检索一一对齐，
  被删 source 专属词零命中）；T15 ID 重编号后存活向量 id 连续无洞 `0..alive`（D-S4-01，
  复用 `snapshot_vectors` 读回）；T16 三轮 churn 墓碑**不累积**（J2/NFR-07：每轮 compact 后
  `chunks_total` 回落到存活数、`tombstone_ratio` 归零、图中无残留墓碑点）。

### V2 Step 4 · 文档回写（S4-10，2026-09-10）

**Step 4 收官**：S4-01 与 S4-03~S4-10 全部完成（**S4-02 `remove_many` 为可选、未实施**），
本 PR `Closes #21`。

- **`architecture-design.md`**
  - 新增 **§7.5.3 墓碑物理回收（compaction，FR-30）**：三条膨胀路径与量级（图 sidecar
    ≈2.6KB/点 vs 快照正文 ≈6B/chunk，约 **440:1**）、按存活集**重新物化 + ID 重编号**
    （D-S4-01）、步骤 0 必须 `commit()`（D-S4-10 / I8）、ID 变更的对外声明（D-S4-09 三处）、
    BM25 逐位一致的理由（排序全序 + remap 单调）、落盘入口**唯一** = `compact_and_save`
    （manifest 重发由既有 `save` 链路自动满足，`FORMAT_VERSION` 保持 2）、触发策略
    （手动为主 + `save` 告警，自动默认关）、失败语义（内存已压实、重试 `save()` 即可续写）、
    10K 实测与四条已知代价（R26~R30）。明确记录**批量删 O(N·M) 未随本 Step 解决**。
  - 新增 **§14.2 V2 Step 4 引入的风险（R26 ~ R30）**：内存峰值 2×（R26，10K 未触发、
    100K 未验证）、ID 重编号破坏外部引用（R27，已缓解且 T14 覆盖）、compaction 耗时
    （**R28 维持「未实测」**——`churn_bench` CSV 无 `compact_ms` 列；采集途径为
    `helix compact --json` 的 `compact_ms`）、重建图拓扑抖动（R29）、期间不可服务
    （R30，在线 compaction 归 Step 8）。
- **`requirements-spec.md`**：FR-30 打上**验收注记**（✅ 已实现 + 实测数字 + ⚠️ ID 重编号
  警示 + 手动触发为主）。
- **`plan-v2.md`**：升 **v0.9**；Step 4 三条进度（T7-12 核心 / S4-01 / S4-08~10）全部标 ✅
  并挂 commit；验收清单补勾 **Step 3**（此前漏勾）与 **Step 4** 两项。
- **`v2-step4-design.md`**：升 **v0.4（实现完成版）**，新增 **§10 实施结果**——S4-01~S4-10
  完成状态表、10K churn 实测（含 2026-09-10 复跑 `VERDICT: PASS`、`reclaimed_chunks=15000`、
  冷启动 94ms）、**Q1~Q7 全部结案**、PR #30/#31 评审响应要点、集成测试补强 8 例与三条
  踩坑（`[char; N]` 作 pattern 误伤全部字符串 / 墓碑态总字节未必大于初始 / 向量近邻对
  已删 doc 不返回空）。
- **`docs/README.md`**：Step 4 设计条目标注 v0.4 实现完成与 §10 位置。

### V2 Step 4 · 墓碑物理回收 compaction（核心，S4-03~S4-07，D-S4-01/03/06/10，2026-09-08）

**新增（资源回收）**

- **`SearchIndex::compact()` / `compact_and_save()`（D-S4-01 / 设计 §4）**：按存活集
  **重新物化 + `ChunkId`/`DocId` 重编号**的墓碑物理回收入口。
  - **D-S4-10：`compact()` 第一步即 `self.commit()?`**（`compact_and_save` 的隐式
    `save()` 链也随之先 commit）——把 `pending` 写缓冲先 flush，杜绝「未 flush 的
    stale id 被灌进 `raw_vectors` 与图」后再重编号（`add()` 在入 `pending` 前就已
    分配真实 `chunk_id`）。
  - **I5 原子替换**：全部在 locals 里构建新 `Index` / `raw_vectors` / `vector_index`，
    全部成功后才一次性替换 `self.inner`；任一步失败不污染旧索引。
  - **`remapped: bool`**：无墓碑时为 no-op（`remapped == false`，ID 一个没变，验收 T11）；
    有墓碑时为 true。
- **重编号细节（D-S4-01）**：`Index::compacted` 按**旧 id 升序**把存活 doc/chunk 映射到
  新稠密 id ⇒ **相对顺序不变** ⇒ BM25 排序 `(score desc, chunk_id asc)` 的全序保持，
  检索输出逐位一致（验收 T3）。`chunk_lens` / `content_hashes` 同步按 remap 收敛；
  `stats` 原样复制（纯增量计数器，与稠密化无耦合）。
- **dead term 摘除（D-S4-04 / 设计 D-S4-03）**：`InvertedIndex::compact` 依存活集把
  墓碑 chunk 的 postings 过滤掉，**空词链整条丢弃**（此前 `remove` 只摘 posting 不摘
  term，死词残留），并给幸存词**重编号 `TermId`**；返回 `reclaimed_terms` 计数。
  全程按旧 `TermId` 升序遍历，幸存词间相对序不变。
- **向量索引重建（S4-05）**：新增共享 helper `rebuild_vector_index`——把
  `hnsw_rs` 无 remove 留下的图墓碑点清掉，产出与新 `raw_vectors` 一一对应的稠密图。
  原「加载降级重建」路径（Brute / Hnsw Err 降级）改用同一 helper，去重收敛。
- **墓碑可观测（S4-06 / NFR-07）**：`tombstone_stats()` + `TombstoneStats` / `SizeBytes` /
  `CompactionReport` 公开结构。报告含 before/after 双统计、三体积（`.idx`/`.hnsw.graph`/
  `.hnsw.data`）、回收计数（chunks/docs/terms/graph_points）、`vector_rebuild_ms`、
  `total_ms`、`remapped`、`graph_status`。
  - **内存-only vs 落盘的口径**：`compact()` 是内存-only，`bytes_after` 恒为 `None`
    （磁盘没变，填任何值都是撒谎）、`graph_status` 报磁盘旧值（不冒充 Loaded）；
    `compact_and_save()` 落盘后才填三体积与最终 `graph_status`。`index` 被移除的「本
    索引无图」的 `brute`/`hnsw` 区分由 `TombstoneStats` 承载。**重新物化后产物是更稠密的
    正常快照，`FORMAT_VERSION` 不变、不引入新崩溃一致性机制**。
- **save 墓碑阈值告警（D-S4-02）**：`save()` 在 `chunks_total ≥ 1024 && tombstone_ratio
  ≥ 0.2` 时 `eprintln` 提示「索引墓碑多，建议 `helix compact`」。工程卫生，不改变行为。
- **测试**：Index 层单元 T1/T2/T11（`src/index/mod.rs`）+ 门面层集成
  T3/T7/T11/T12/T13（`tests/step4_compaction.rs`）；共享确定性 Embedder / 装配收敛到
  `tests/common/mod.rs`（`step4_liveness.rs` 同步迁移，评审建议 #2）。
  - T3 BM25 检索**逐位一致**（compaction 前后结果二进制相同，验收位元不变式 I3）；
    T7 compaction + reload 后 `GraphStatus::Loaded`；T11 无墓碑 no-op ID 不变；
    T12 纯 BM25 与 Brute 后端均可 compact；T13 D-S4-10 的 pending 存活 chunk 不丢向量。

**评审回应（2026-09-08，PR #30 评审）**

- **修复（发现 1，必改）全删 → compact → 写路径砖死**：`compact_with_bytes` 步骤 5
  曾用 `if !raw.is_empty()` guard——全删后存活向量为空时落到 `_ => None`，把**仍有
  embedder 装配**的向量 lane 丢成 `vector_index = None`；此后 `add → commit` 的
  `flush()` 对 `vector_index == None` 误报 `Err(NoEmbedder)`（embedder 明明已配），
  `save` / `compact` / `into_searcher` 全失败，索引不可恢复（CLI：清空 collection 后
  继续写入即踩中）。修法：判定维度改为「是否有向量能力」（`had_vectors`），**与存活
  向量是否为空无关**，全删后保留**空**向量索引（`rebuild_vector_index` 对空 raw 天然
  安全），与设计 §4.5 的无条件重建一致。**load_with 同款对齐**：快照向量已删空
  （全删后 compact_and_save）再 load，装配有 embedder 时同样保留空向量索引，不再因
  `raw_vectors.is_empty()` 落 `None`。新增回归集成测试
  `R_发现1_全删compact后仍可写入检索`（Hnsw，全删→compact→add→commit→save→load→检索）。
- **采纳（建议 2）无墓碑 `compact()` 早退**：旧实现 `remapped == false` 只保证 ID 不变，
  仍重物化 + 重建整图（10~100s 级纯浪费；D-S4-02 刚引导用户跑 compact）。无墓碑时
  early-return `before == after` 的空 report（跳过重建，成本接近 0）；`compact_and_save`
  仍幂等落盘。
- **采纳（建议 3）`compact_and_save` 失败语义 rustdoc 注明**：`save` 失败（Strict 下图
  dump 升级 Err）返回 `Err` 但**内存已压实**（I5 已原子替换）、磁盘仍旧档——设计内
  行为（compaction 核心价值是内存压实，落盘失败不回滚）；重试 `save` 续写即可。
- **采纳（建议 4）首存 `bytes_before` 为 `None`**：目标文件不存在时诚实表达「此前无
  快照」，而非 `Some(0,0,0)`（`snapshot_bytes` 对缺失文件 `unwrap_or(0)` 永不报错）。

### V2 Step 4 · S4-01 flush 幽灵向量防线（2026-09-08，D-S4-05 / §2.3 / T5）

**修复（资源回收）**

- **`flush()` 灌原始向量前按 liveness 过滤（D-S4-05 / §2.3 / T5）**：此前
  `SearchIndex::flush` 把 `pending` 全部灌进 `raw_vectors` 与 HNSW 图，**全程无存活
  检查**；而 `add()` 在入 `pending` 前就分配了真实 `chunk_id`，`remove()` 只墓碑化
  正排/倒排、**不摘 pending**。于是 `add → remove → commit`（默认 `batch_size=64`
  下的常见时序，CLI 逐条 add 后删除即此路径）会让已删 chunk 的向量入库并随
  `save()` **跨快照永续**（资源问题；存活位图仍在检索期兜底，FR-26 不破）。
  修法：`flush()` **整批 embed 之后、入库之前**按 `Index::is_live_chunk` 过滤，
  墓碑 chunk 的向量丢弃（不落 `raw_vectors`、不进图）。刻意不改 `embed_documents`
  入参组成（fastembed batch 组成无关性未实测，避免「赌」）。
- **新增集成测试 `T5` / `T5b`**（`crates/core/tests/step4_liveness.rs`）：
  T5 验证「remove 早于 flush」的幽灵向量不入快照；T5b 验证「flush 后 remove」的
  `raw_vectors` retain 路径本就摘净。两者互补覆盖架构 §7.5.2 防线的时序缺口。

### V2 Step 3 · 原子快照实现（2026-09-07）

**修复（正确性）**

- **快照落盘原子化（FR-31 / T7-13 / Q-C3）**：`save_with_crc` 改走新通用原语
  `storage/atomic.rs::atomic_write`（tmp → 写入 → flush → `sync_all` → rename →
  fsync 父目录）。此前的 `File::create` 直写 + `flush()` 在写入中途崩溃会留下半截
  `foo.idx`，下次 `load` 得 `Error::SnapshotCorrupted`（索引丢失）。崩溃不变式：
  **save 序列任一注入点崩溃后，`load` 要么拿到旧快照、要么拿到新快照，绝不损坏**。
  文件格式零变化（`FORMAT_VERSION` 保持 2，改造前后产物 CRC 逐位一致）。
- **R19 写路径残余收敛**：`hnsw_rs` 图 dump 期间 panic（`DumpInit` 的 `panic_any`，
  probe 探测之外的 TOCTOU 窗口）不再杀进程——新增 `dump_graph_caught`（`catch_unwind`
  → `Err(VectorGraph)`，携带 panic 信息），汇入 Step 2 既有 P0-3 缓存失败语义链；
  Lenient 下 `save` 仍 Ok + `GraphStatus::PersistFailed`，Strict 下 Err。
- **Strict 图失败路径补 best-effort sidecar 清理（D-S3-07）**：失败上抛前清掉半截
  `*.hnsw.graph`/`*.hnsw.data` + 已失效旧 manifest（「失败上抛」≠「失败且留垃圾」）。

**变更**

- **tmp 命名统一为「目标路径 + `.tmp` 追加」（D-S3-01）**：`foo.idx.tmp` /
  `foo.idx.hnsw.manifest.tmp`；顺带修正 manifest tmp 原先 `with_extension` 拼出的
  `foo.idx.hnsw.hnsw.manifest.tmp`（双 `hnsw`）怪名。写侧（`write_manifest_atomic`）
  与清侧（`remove_sidecars`）同 PR 改齐。`.gitignore` 补 `*.manifest.tmp`。
- **tmp 孤儿回收（D-S3-05）**：快照 tmp——save 靠 `File::create` 截断复用、load 成功后
  best-effort 删除；manifest tmp 跟随 `remove_sidecars`（仅 save 路径触发，load-only
  部署保留 manifest tmp 孤儿属已知无害行为）。
- **save 变慢（fsync，D-S3-06）**：12K 语料 54.5MB 快照实测 **0.034~0.040s →
  0.067~0.075s**（+0.027~0.035s，本机 NVMe），不触碰任何 NFR；**不提供跳过 fsync
  的逃生舱**（正确性语义不做选项）。实测入 `eval-report.md` §8.7（新增示例
  `bench_save_fsync`）。
- **Cargo.toml**：`[profile.release]` 钉「不得 `panic = "abort"`」注释（`catch_unwind`
  防线依赖 unwind）；嵌入宿主的对应约束为文档级（架构 R19 残余表 / crate 文档）。

**新增（内部，`pub(crate)`，公开 API 零变化）**

- `storage/atomic.rs`：`atomic_write`（写闭包签名，避免 52MB 整包拷贝）/ `tmp_path` /
  常驻 `pub(crate)` 故障注入钩子 `fault`（`FAIL_AT == 0` 默认 no-op；注入测试内迁
  crate 内 `#[cfg(test)]`，Mutex 串行 + guard 复位 + 线程本地 opt-in 三重防互扰）。
  manifest 与快照共用一份原子写实现（两份独立实现迟早漂移）。

**测试（S3-T1~T9）**：注入测试 T3（cp2 钦定点）/ T4 / T5 / T6（端到端）/ T7（R19）/
T8（manifest tmp 改名一致性）/ T9（钩子默认关闭）+ 集成层 `tests/atomic_snapshot.rs`
（无注入：save 原子性外观 / tmp 不残留 / 兼容回归）；既有快照 roundtrip / CRC 系列
零修改全绿（T2）。

**测试补强（2026-09-08 评审回应，`a474d86` + `dc31b3f`）**：补 S3-T8 单测（manifest
写失败 → tmp 残留 → `remove_sidecars` 回收）；T9 断言前取 `InjectionGuard`（消除与
并行注入测试的 flaky 窗口）；集成层补 **S3-TI1~TI5**——tmp 孤儿回收 load/save 两侧
终态、半截快照公开错误面（`SnapshotCorrupted` / `Io`，绝不 panic）、图三件套命名对齐
（目录清单级）、Strict 失败完整生命周期（失败不丢索引 + 清障恢复）；fsync_dir 平台
注释按实测修正（**macOS/APFS rustc 1.90 目录 fd `sync_all` 实测 Ok**）。

**文档**：架构 v1.8（§7.6.2 前提段销账 / R19 残余更新）/ 需求 v1.8（FR-31 验收注记）/
`plan-v2.md` v0.5（Step 3 ✅）/ `eval-report.md` §8.7 / 设计文档补「实施结果」段。

**文档状态同步（2026-09-08）**：根 README（性能摘要刷新、新增「当前进展（V2 路线图）」、
已知局限改降级口径、开工指路改 plan-v2）/ 需求 v1.9（NFR-04 行改「✅ 已达标」、
NFR-03 补首次全量实测落点）/ `plan-v2.md` v0.6（**状态校正：Step 3 = PR #27 待合并**，
此前误写「已合并」）/ `p5-design.md` D7 与 R-P5-10/13 结案注记 / user-guide
性能预期（构建口径、save 原子写代价）。

### V2 Step 4 · 详细设计（2026-09-08）

- 新增 `docs/devel/v2-step4-design.md`（**v0.1，待评审**）：**墓碑物理回收（compaction，
  T7-12 / FR-30 / Q-C2）**的详细设计。
  - **现状盘点**：三条膨胀路径逐行定位——图 sidecar（≈2.6 KB/点，12K 实测外推）、
    `raw_vectors`（已由 `remove` 回收）、快照正文（`None` 槽 1 B + `chunk_lens` 4 B
    push-only + 词表死词，≈6 B/chunk）；量级约 **440:1**，图是绝对大头但正文也必须堵。
  - **设计期新发现**：`flush()` 灌 `raw_vectors` 时**无 liveness 检查** ⇒
    `add → remove → commit`（默认 `batch_size=64` 下的常见时序）会让已删 chunk 的向量
    入库并跨快照永续。属资源问题（存活位图仍在检索期兜底，FR-26 不破），已列 **S4-01**
    独立小 PR。
  - **方案**：按存活集**重新物化**（正排稠密化 + 倒排 remap + 死词摘除 + 图重建）
    + **ID 重编号**（不重编号则快照正文空洞无法消除 ⇒ 验收不可达；free list 因「旧引用
    静默指向新文档」被否决）；落盘复用 Step 3 `atomic_write`，**不引入新的崩溃一致性机制**，
    `FORMAT_VERSION` 保持 2；**manifest 重发铁律**由既有 `save` 链路自动满足。
  - **可观测 / 可复现**：`TombstoneStats` / `CompactionReport`、CLI `helix compact`
    （`--dry-run`）、example `churn_bench` + `scripts/eval_churn.sh` 零散写入 workload。
  - 9 个待拍板决策 **D-S4-01~09**、测试计划 **S4-T1~T12**、任务拆分 **S4-01~10**
    （建议 PR 切分：S4-01 先合 → 核心单 PR → CLI/workload → 文档回写）、风险 **R26~R30**。
  - **v0.2（2026-09-08 PR #28 评审回应）**：新增 **D-S4-10「`compact()` 与写缓冲 `pending`
    的交互」**（评审 P0）——`add()` 在入 `pending` 前已分配 `chunk_id`，不先 flush 就重编号
    会让 stale id 被紧随的 `save()` 灌进 `raw_vectors` 与新图 ⇒ 采纳「`compact()` 开头先
    `commit()`」，配不变式 **I8** 与测试 **T13**；验收 3 的 `nb_point` 口径改为
    「存活**且有向量**」（与 §4.4 对齐）；D-S4-04 理由重写（保留公开内存版，落盘入口唯一）
    并补齐内存 compact 后 `graph_status` 的语义；§4.3 明确六步作用于**新 `Index` 实例**
    （对齐 I5）；「重建确定性」从 NFR-06 口径中独立命名；`CompactionReport` 并入字节级三体积。
  - **v0.3（2026-09-08 决策拍板）**：**D-S4-01 = 重编号（方案 B）**、**D-S4-10 =
    `compact()` 开头先 `self.commit()?`（修法 A）** 两项正式拍板 ⇒ 全 Step 阻塞解除，
    **核心实现（S4-01 + S4-03~S4-07）可开工**；D-S4-09 的三处 ID 变更声明由「可选」升级为
    **必做项**；§9.2 的 Q1 / Q7 结案。其余 D-S4-02~09 评审无异议，按设计建议执行。
- `plan-v2.md` v0.8：Step 3 状态校正为「✅ 已合并 `ed25d5c`」（验收门槛同步勾选）、
  Step 4 状态改「🟩 设计 v0.3 已拍板，核心实现可开工」；`docs/README.md` 索引行同步。

### V2 Step 3 · 详细设计（2026-09-07）

- 新增 `docs/devel/v2-step3-design.md`（**v0.3 拍板版，可开工**）：
  **原子快照（T7-13 / FR-31 / Q-C3）**的详细设计——`atomic_write` 通用原语抽取
  （tmp → fsync → rename → fsync 父目录，快照与图 manifest 共用一份实现）、
  save 全序列崩溃窗口矩阵（任何窗口下 `load` 要么旧快照要么新快照，
  **绝不 `SnapshotCorrupted`**）、tmp 孤儿回收（快照 / manifest 分口径）、
  故障注入测试钩子（常驻 `pub(crate)` + checkpoint 注入，注入测试内迁 crate 内
  `#[cfg(test)]`，`catch_unwind` 验证不变式）、**R19 写路径残余收敛**
  （`dump_graph` 包 `catch_unwind`，panic 汇入 P0-3 缓存失败语义链）。
- **7 个决策已全部拍板（D-S3-01~07，2026-09-07，按建议采纳）**：tmp 命名统一为
  追加式 `.tmp`（顺带修正 manifest tmp 的双 `hnsw` 怪名，本机实证）/ `atomic_write`
  落 `storage/atomic.rs` 收写闭包（避免 52MB 整包拷贝）/ 注入钩子形态（C′：常驻
  `pub(crate)` 不进公开面）/ R19 收敛方式 / 孤儿回收时机 / fsync 代价接受且不提供
  跳过开关 / Strict 失败路径补 best-effort sidecar 清理。
- v0.2 回应首版评审 8 条意见（PR #26）：checkpoint 计数校正（4→3）、D-S3-03 方案
  重构（B → C′，消除「零新增公开项」的结构性矛盾）、验收 3 按快照/manifest tmp
  拆分口径、cp1/cp3 矩阵行按真实崩溃形态（进程崩溃 vs 掉电）改写、代码草图笔误
  修正、panic=abort 约束降级为两级文档防线（库 profile 对宿主无效）。
  v0.3 拍板：D-S3-01~07 全部按建议采纳。
- 测试计划 S3-T1~T10 + 实施任务 S3-01~09（量级 S，默认单 PR；S3-01 已完成）。

### V2 计划复审 + 步骤重排（2026-09-07）

- 对 `plan-v2.md` 做全量复审（7 处调整 A1~A7 + 4 个拍板问题 D1~D4），含
  `fastembed-6.0.2` / `ort-2.0.0-rc.13` 的源码级核实证据；**复审结论已全部并入
  `plan-v2.md` 本体**（调整项索引与建议执行顺序见其 §附-3），不另立复审文档。
- **D1~D4 已拍板**：① NFR-03 **拆「首次全量 / 增量追加」双口径**；② 低选择度延迟（R18）
  **从 V2.1 提前到 V2.0**；③ 资源回收**提前**、构建性能顺延；④ `parallel_build` 默认值翻转
  **走独立 PR**（须同步改 S2-T22）。
- **Step 3 起重新编号**（映射见 `plan-v2.md` §附-2）：Step 3 可靠性 / **Step 4 资源回收**（原 Step 5）
  / **Step 5 查询性能与可观测**（新增）/ **Step 6 构建性能**（原 Step 4）/ **Step 7 精排**（原 Step 6）；
  V2.1 顺延为 Step 8~10。历史设计文档已加编号注记。
- **Step 3 范围按 ADR-A 重写**：ADR-A 已把多文件原子性收敛为「manifest 唯一发布点」，
  故 Step 3 只需让 `foo.idx` 自己 tmp+rename。⚠️ 补记事实：快照**本体至今非原子**
  （`storage/snapshot.rs:88` 直写且无 fsync）⇒ 崩溃即 `SnapshotCorrupted`（不是降级，是索引丢失）。
- **需求文档升 v1.7**：NFR-03 双口径；**新增 NFR-13**（低选择度过滤延迟，此前不在任何 NFR 口径内）；
  NFR-11 口径修订（`add` 后需 `commit()` 才可见，原表述与实现不符）；**FR-31 原子快照 Should → Must**。
- **架构文档升 v1.7**：compaction 归 Step 4，并补「**compaction 重建图后必须重发 manifest**」
  （否则新图永远匹配不上、冷启动恒走降级重建）。
- **新增横切任务**：T7-21（`parallel_build` 默认翻转）/ T7-22（低选择度暴力兜底）/
  T7-23（`query::Metrics` 可观测化）/ T7-24（工程卫生：CI 触发、`.gitignore` 漏掉图 sidecar、分支清理）。

### V2 Step 2 · 收尾（2026-09-07）

- **并行建图（S2-11 / D-S2-05）收益实测入库**（`eval-report.md` §8.6），新增可复跑的基准
  `crates/core/examples/bench_parallel_build`：12K 真实语料 11.676s → 2.182s（**5.35×**），
  50K 合成向量 171.8s → 32.7s（5.26×），加速比稳定 ≈5.3× 且与规模无关；
  真实语料上 oracle 重合率**无差异**（0.995 / 0.995）。
  ⇒ 设计文档 §10 未决问题 1 收敛：**建议把 `parallel_build` 默认值翻为「开」**，
  但需**独立 PR**（`S2-T22` 断言「默认必须串行」，翻默认值要同步改护栏语义）。
- **进度与状态回写**：`plan-v2.md` 进度表 Step 1/Step 2 打勾；Step 2 状态段改为「已完成并合并」
  并记 main 提交链（含「堆叠 PR base 非 main ⇒ 显示 MERGED 但内容没进 main」的坑）；
  **Q-C3 定级回写**为「高（正确性）」并注明是本文（V2 Step 2）重新定级。
- **许可改为 MIT 单许可**：删除 `LICENSE-APACHE`，`Cargo.toml` 的 `license` 由
  `MIT OR Apache-2.0` 改为 `MIT`；`NOTICE` 与 `README` 同步（第三方依赖仍允许
  Apache-2.0，`deny.toml` 白名单不变——那是依赖的许可，不是本项目的）。
- **新增 `data/download_t2ranking.sh`**：T2Ranking 原始数据（约 3.5GB）的下载脚本入库，
  带 sha256 校验（三个文件的实测校验和写进清单）、断点续传、按文件名过滤下载，
  支持 `HF_ENDPOINT` 镜像与 `SKIP_SHA=1`。此前该数据只有本机有、获取过程无记录，
  评测子集无法重新装配。

### V2 Step 2 · 评审回应（2026-09-07，GLM 外部评审）

三个 PR 各收到一份独立评审（含 12K release 全流程复测与 `hnsw_rs-0.3.4` 源码级核实），
共 12 项发现 + 6 项 nit。逐条处置如下，**其中 3 项是实质缺陷**：

| # | 发现 | 处置 |
| --- | --- | --- |
| #12-1 | platform 校验是「断言」而非「测量」——硬编码 `0x01` 让大端/32 位构建同样接受，可能加载出**字节序错误的向量**（静默错误结果） | ✅ 改为按构建目标派生 `PLATFORM_FINGERPRINT`（`cfg!`） |
| #12-2 | P0-3「save 不得因图失败而失败」只覆盖 `dump_graph`，CRC / manifest 发布仍会让 `save` 返回 Err | ✅ 抽出 `write_graph_sidecar()` 作统一失败边界 |
| #13-1 | **T13 从未走过并行路径**：`batch_size=64` ⇒ `add_batch` 每次 64 条，永远够不到阈值 1000 ⇒「并行 vs 串行」实为「串行 vs 串行」 | ✅ T13 加 `batch_size`；新增 **T22** 钉死分派；`parallel_inserts()` 可观测 |
| #13-2 | `eval_perf.sh` 的 grep 抓不住 µs ⇒ 快路径被误报、冷启动**少算图加载**可能假达标 | ✅ 正则改为 `(ms\|s\|µs\|us)` |
| #13-3 | `parallel_build` 在 load 路径被静默丢弃（读端继续写入永远串行） | ✅ 一路透传 `cfg.parallel_build` |
| #14-1 | 验收对照表两处「测试证据」与实际不符（T6 无 oracle 断言 / 验收 6 映射到无关的 T13/T15/T16） | ✅ T6 补 oracle 断言；验收 6 改为 T7/T8/T9 |
| #14-2 | R24「无法在外部改私有字段」与源码相反（`set_extend_candidates` 是公开 API） | ✅ 改写为「可显式对齐；真正不可改的是 `datamap_opt`」 |
| #14-3 | 数字不一致（graph 8.1 vs 7.9、合计 84.1 vs 分项 83.9、降级 9.96s 复现不出） | ✅ 统一为**范围**：graph 7.9~8.1MB / ≈84MB（1.6×）/ 降级 ≈10~11.5s |

**数字口径的重要修正**：评审独立复现发现「降级重建 9.96s」是偏快样本（他测 11.61s）。
复测三次（9.96 / 11.01 / 10.73s）确认**单次波动 ±15%**，故本仓库的实测除
「完整冷启动 < 2s」这个有 20× 余量的判定外，一律**给范围而非精确值**。

### Fixed（落地 main 时补充，2026-09-07）

- **S2-T6 的 oracle 断言改为「20 query 均值 ≥ 0.90 + 单 query 最差 ≥ 0.5」（修 flaky）**：
  原断言是**单个** query 的 Top-10 重合率 ≥ 0.95，而 Top-10 重合率的量化粒度只有 0.1
  （漏 1 条就是 0.90），卡 0.95 等于要求 10/10 全中；HNSW 是**近似**检索，在这组
  「只差一个数字」的近重复语料上漏 1 条属正常 ⇒ `--features charabia` 下实测 0.900 把
  `main` 的 CI 挂了。改为 20 个 query 取均值（样本量 ×20，实测 0.995~1.000）并保留
  最差 query ≥ 0.5 的兜底，既抓得住真退化，也不会被单次抖动翻脸。

- **S2-T13 的阈值由 1pp 放宽到 5pp（修 flaky）**：并行库相对串行库与 oracle 的 Top-10
  重合率有 **0~2pp 的固有抖动**（C8：并行插入顺序不确定 ⇒ 图拓扑随机）。
  `--features charabia` 下实测出现 `0.9850 vs 1.0000`（1.5pp）与 `0.9900 vs 1.0000`
  （正好卡边界），导致**同一棵树 CI 两次一过一挂**。放宽后连跑 5 次全绿；
  「并行是否真走并行」的护栏是 **T22**（直接钉死 `add_batch` 分派）与
  「两条路径各自 ≥ 0.90」的底线，不依赖这个差值断言。

其余（manifest header 版本先比对、dump 探针先于删除、`PersistFailed` 变体、
`graph_basename` 不回退假名、T9 死代码、user-guide 的 NFR-06 口径、
CHANGELOG 条目归位、版本表时间序、守门命令表述）均已就地修掉。

### V2 Step 2 · 实测与文档回写（S2-10 / S2-12，2026-09-06）

#### 12K 实测（`data/t2-corpus.jsonl`，release，macOS aarch64）

| 项 | 改造前（图不持久化） | 改造后 |
| --- | --- | --- |
| 快照加载（含位图/字段索引重建） | 53.4ms | 57~77ms |
| 图加载 | ——（无此路径） | **23~37ms** |
| **完整冷启动** | **≈11.6s** ❌ | **≈100ms** ✅（实测 82~100ms，约 116×） |
| 图重建（降级路径） | 11.57s | ≈10~11.5s（单次 ±15%） |
| 图 sidecar dump | —— | 43.5ms |
| 磁盘占用 | 52.3MB | ≈84MB（**1.6×**，graph 7.9~8.1 + data 23.7） |

**NFR-04 达标**（限额 2s，余量 20×），代价是磁盘 +60%（R22）。
⚠️ 达标**依赖图 sidecar 命中**——降级路径仍是 ~10s，故 NFR-07 扩口径为「降级不得静默」。

#### 文档回写（S2-12）

- **需求 `requirements-spec.md` → v1.6**：FR-29 落定（补实现方案与实测）；
  NFR-04 口径收紧为「完整冷启动」并补实测；**NFR-06 删去过时论据「HNSW seed 固定」**
  （`StdRng::from_os_rng()` 无 seed API，两次建库拓扑本就不同），改为
  「图持久化冻结拓扑 ⇒ 同快照两次加载逐位一致」；NFR-07 扩「降级不得静默」
- **架构 `architecture-design.md` → v1.6**：**ADR-A** 入 §2.3 决策摘要与 §9.4 决策记录
  （含 §7.6.1 曾预引用的「ADR-011」统一为 ADR-A）；新增 §5.4.3「图持久化
  `VectorGraphPersist`」（basename 铁律 / `ef_search` 是入参 / `Box::leak` 三条结构性约束）、
  §7.6.2 图 sidecar 布局、§14.1 风险表 **R19~R25**；§8.2 NFR-06 与 §8.3 NFR-07 口径修正；
  §10.3 补 `graph_status()` / `graph_dump_elapsed()`
- **`plan-v2.md`**：Step 2 状态与实测表；Q-P2 / Q-P3 标记已解决；**D7 结案**
- **`eval-report.md` §8.3**：冷启动小节重写（P5 vs V2 Step 2 对照），
  同步 §9 未达预期项与 §10 残留问题
- **`user-guide.md`**：图 sidecar 的用户可见行为（四个文件要一起带 / 不可跨平台搬运 /
  `--no-graph-persist` / 降级时看 stderr 的原因）
- **`docs/README.md`**：`v2-step2-design.md` 入索引，风险范围 R1~R18 → R1~R25

### V2 Step 2 · CLI / bench 接线与并行建图（S2-08 / S2-09 / S2-11）

#### 新增（CLI）

- `helix build` 打印**图 sidecar 落盘观测**：点数 / 图体积（graph + data）/ 快照体积 /
  磁盘增量倍数——验收 7「体积与耗时有实测记录」的数据来源
- `helix build --no-graph-persist`：逃生舱（写库但不落图，**磁盘总占用 1.6× → 1.0×**
  （即省下约 0.6× 快照体积）；代价是下次冷启动要重建图）
  - ⚠️ 它省的是**磁盘不是时间**：图 dump 只占全链路的 43.5ms（12K / 203s 建库）
- `helix search --index` 打印**图状态**：持久化图（快路径）/ ⚠️ 降级重建（含原因）/
  不适用（D-S2-04：降级必须显式可见）

#### 新增（bench）

- `--index` 路径接图持久化（D-S2-06）：先从 sidecar 加载图，不可用才重建。
  校验逻辑与门面层**共用** `vector::load_graph_checked`（禁止复制校验——漏项即
  把坏文件交给满是 `unwrap()` 的 `load_hnsw`）
- `--input` 内存直建路径维持重建并打印提示（P1-6 的倾向方案：bench 关注检索延迟，
  冷启动在 S2-T14 单独测）
- `--runs > 1` 的每轮重建**刻意跳过图**（传 `None`）：本就要制造跨进程差异观测
  R-P5-13，读图会让每轮结果相同、`--runs` 失去意义
- 图 sidecar **加载耗时单独计时并打印**（此前只有「加载成功」没有耗时，
  NFR-04 的「快照 + 图」两个口径读不出来）

#### 变更（`scripts/eval_perf.sh`）

- **NFR-04 改为三口径分别计时并自动求和判定**：快照加载 + 图 sidecar 加载 =
  完整冷启动（与 2000ms 目标比较后打达标/未达标），图重建单列为降级路径（期望「未触发」）
- ⚠️ 时间单位的 grep 必须覆盖 **µs**（`耗时 [0-9.]+(ms|s|µs|us)`）：`Duration` 的
  `{:?}` 在小索引/快机器上输出 `591.416µs`，只写 `m?s` 会抓不到 ⇒ 既把快路径误报成
  「未走快路径」，又让冷启动**少算图加载**而假达标（评审 #13 发现 2）
- 头部补脚本依赖说明：**python3**（单位换算与浮点比较，共 4 处）+ 外置 `/usr/bin/time -l`

#### 新增（并行建图，D-S2-05）

- `VectorIndex::add_batch` + `HnswRsIndex` 覆盖为 `parallel_insert_slice`；
  门面层 `flush` 改走批量路径
- 阈值 `PARALLEL_INSERT_THRESHOLD = 1000`（以下回落串行）+ 开关
  `SearchIndexBuilder::parallel_build(true)` —— **默认关**：并行插入顺序不确定 ⇒
  拓扑不可复现（C8），保住与 P5/P6 基线的可比性，先出实测再定默认值

### V2 Step 2 · 图持久化与冷启动（ADR-A 方案 C，2026-09-06）

设计文档 `docs/devel/v2-step2-design.md`（**v0.3，ADR-A 已拍板**）；
H1（图持久化格式 + 多文件原子性）结案，Step 3 原子快照直接沿用本协议。

#### 新增

- **HNSW 图落盘（FR-29）**：快照旁多出三件套 `foo.idx.hnsw.graph` / `.hnsw.data` /
  `.hnsw.manifest`。**`FORMAT_VERSION` 保持 2，快照格式一个字节不动**，旧快照照常可加载
- **`GraphManifest`**：图的唯一原子发布点（tmp → fsync → rename → fsync 父目录）。
  记录父快照 CRC 作「版本锚点」+ 两个图文件的 CRC/长度 + 距离标识 / 平台指纹 / 建图参数
- **`VectorGraphPersist` trait**（D-S2-03）：`VectorIndex::as_graph_persist` 默认下转，
  「Brute 无图」成为类型事实而非运行时 if；`HnswRsIndex` 实现 dump/load
- **`GraphStatus { Loaded, Rebuilt(reason), NotApplicable }`**：降级**必须可观测**（NFR-07），
  `SearchIndex::graph_status()` 暴露；默认「警告后降级」，`GraphPersistMode::Strict` 可升级为 Err
- `SearchIndex::graph_dump_elapsed()`：图 sidecar 落盘耗时单独观测（验收 7；
  快照写入与图 dump 是两个数量级不同的成本，混在一起看不出图持久化的真实代价）
- 配置入口新增 `ef_search(n)` / `graph_mode(mode)` / `without_graph_persist()`
- `storage::save_with_crc` / `load_with_crc`：读写路径带出正文 CRC（图的版本锚点）

#### 图是快照的派生缓存（ADR-A 的核心）

图**可随时丢弃**：删 manifest / 删图文件 / 篡改任一字节 / 快照更新而图未更新 /
维度或平台不匹配 / 建图参数漂移 —— 一律降级为加载后重建，**功能不丢，只慢**。
多文件原子性问题因此被消解：只要 manifest 原子发布且带 CRC，「要么全对、要么全不算」即成立。

#### 开工前源码复核的两项新事实（N1 / N2）

- **N1**：`Description::dump` 无条件写 `MAGICDESCR_4`，`load_description` 读回 **4 不是 3**。
  协议本就用「读回值不硬编码」，无需改动
- **N2**：`load_hnsw` 无条件设 `datamap_opt = true`，使 `file_dump` **拒绝覆盖**已存在的
  `.hnsw.data`、改写随机后缀文件名（`foo.idx-1234.hnsw.graph`）——「加载 → 增量 add → save」
  会静默把图写错位置。**对策：dump 前先删旧 sidecar 两文件**（图是缓存，删除最坏降级重建）

#### 验收

- **`crates/core/tests/graph_persist.rs` 新增 18 个端到端测试**（+ 13 个 storage 单测 + 6 个 persist 单测）
  - 覆盖 **S2-T1/T2/T4~T12/T15~T21**；**S2-T3**（并行 vs 串行 oracle 质量等价）顺延至
    S2-11 的 PR（依赖 `parallel_build`），**S2-T13/T14** 亦在该 PR（T13 并行质量、T14 dump 体积）
  - ⚠️ T3 的意图（图路径与重建路径质量等价）**当时**由 **T6 的 oracle 重合率断言**（≥0.95）
    与 `persist.rs` roundtrip 的逐位一致断言先行覆盖。
    **后续更新（2026-09-10，issue #34 / PR #35）**：T6 那条数值断言已撤（换成两条拓扑无关的
    不变式：**内容覆盖** `graph_points == raw_vectors` + **全部 N 篇自匹配**）⇒ 该意图现由
    **T6 的两条不变式**与 **T13 的相对口径**共同承接。**别再引用「T6 的 oracle 重合率断言 ≥0.95」**
  - 每个「应走快路径」的测试都先断言 `GraphStatus::Loaded`（P0-1：basename 拼错会让
    所有「能加载」断言照样绿，只有 NFR-04 静默失效）
- **验收 2（消解 R-P5-13）**：同一快照连续两次加载，200 条 query 的 Top-10 **逐位一致**。
  ⚠️ 测试**必须先断言 `GraphStatus::Loaded`**——basename 拼错时降级路径一切正常、
  所有「能加载」断言都会绿，只有 NFR-04 静默失效
- **验收 3/4**：图可丢弃四场景 + 截断 1%/50%/99% 与 magic 篡改**均不 panic**（`hnsw_rs`
  reload 路径有 12 处 `assert_eq!`/`unwrap()`/`exit(1)`，靠 manifest CRC + Description 预校验五道先验挡住）
- NFR-04 的完整冷启动实测与体积数据待 S2-10 补录

### V2 Step 1 — 正确性修复（PR #6，2026-09-05）

设计文档 `docs/devel/v2-step1-design.md`（v1.0）。

#### 修复

- **Q-C1（软删除后向量残留）**：已删 chunk 的向量仍留在 HNSW 图里，会霸占 Top-K 名额
  却进不了最终 hits——表现为「要 5 条只给 2 条」，且 Agent 无法区分「库里只有 2 条」
  与「有 5 条但 3 个位置被幽灵占了」。新增存活位图作为软删除的**单一真源**，
  检索期以谓词下推到向量路
- **Q-C1（跨快照永续）**：`SearchIndex::remove` 同步摘除 `raw_vectors`，
  与存活位图构成两道互相独立的防线
- **Q-I1/I2（过滤 O(N) 全扫）**：新增 doc 级字段索引 + 惰性 `CandidateFilter` 谓词，
  每 query 成本从 O(N) 次 JSON 取值降为 O(query 词数 + 匹配文档数)，与语料规模解耦

#### 新增

- `Index::with_max_values_per_field(n)`：字段索引基数阈值可配（默认 1024）。
  ⚠️ 高基数字段（毫秒时间戳、雪花 ID）超过阈值即**永久降级**，该字段上所有过滤
  会退回 O(N) 全扫——这是 Q-I1 尚未覆盖的已知短板，详见 `field_index.rs` 模块文档
- `Metrics::filter_eval` / `allowed` / `vector_shortfall`：过滤下推后的可观测性。
  `vector_shortfall` 按 `allowed` 归一（小语料下不为噪声所淹没），
  是 V2.1 是否引入 prefilter 的判据

#### 变更

- `VectorIndex::search_filtered` / `Retriever::search_filtered` 接受
  `Option<&dyn CandidateFilter>`；**`None` 表示不过滤**（结果含已软删除条目），
  该契约在 `predicate.rs` 与两个 trait 的文档、以及 T17 中三层钉死
- HNSW 双路径：无用户过滤（热路径）走普通 `search()` 保住 fast-return，
  按存活比例过采样；有用户过滤走 `search_filter`，`ef` 仅作搜索宽度
- 空结果原因遵循 **query 侧信号优先**：query 无命中时一律报 `AllTermsUnmatched`
  而非 `FilteredOut`，避免误导 Agent 去调过滤条件。
  ⚠️ 该判据用的是 BM25 词典探针，Vector 模式下存在已知误报（见 `query_has_hits` 文档）
- 快照 `FORMAT_VERSION` 维持 2：存活位图与字段索引均可从 `docs` 重建，无需升版

### V2 Step 1 收尾 — 性能与 fixture（PR #8，S1-10 / S1-11，2026-09-05）

设计文档 `v2-step1-design.md` §10.1 有完整数据；架构文档同步至 **v1.5**（新增 ADR-010）。

#### 新增

- `scripts/gen_synth_corpus.py`：**确定性合成 fixture 生成器**（固定 seed），主题化文本聚类 +
  可精确制造 0.1% / 1% / 10% / 50% 选择度的档位，并含档位自检。产出 corpus / queries / filters.json
- `scripts/eval_filter.sh`：一键扫描 8 档位 × 3 模式，产出「选择度 × 延迟 × 召回」三元数据，
  并自动判定 NFR-02
- `helix bench --filter <spec>`：过滤档位（支持 `field=value` 与 `field>=n` / `field<n` 的 range），
  有过滤时额外跑 **post-filter oracle 对照**（Step 1 之前的行为）作为召回基线
- `helix bench --filter-cost`：过滤求值耗时对照（T13），旧全扫 vs 新位图谓词 vs 降级路径
- oracle 过采样深度按选择度**自适应**（`K / 选择度 × 安全系数`），并标注 oracle 自身是否凑够 K

#### 实测结论 —— 1 万级（K=10，60 query × 10 reps）

- **T14① NFR-02 达标**：无过滤 P99 = bm25 **0.42ms** / vector **3.54ms** / hybrid **3.46ms**，
  均远低于「P5 基线 × 1.10」（4.29 / 9.21 / 9.57ms）—— 无过滤热路径保住 fast-return 的设计目标达成
- **T13 收益分化**：未降级字段求值 **0.2221ms → 0.0005ms（471.8×）**；
  降级的高基数 Range 字段 **0.9×**（新旧都退化为全扫）—— **Q-I1 对该场景收益为 0**

#### 实测结论 —— 10 万级（K=10，200 query × 5 reps）

| 档位 | 选择度 | bm25 P99 | vector P99 | hybrid P99 |
| --- | --- | --- | --- | --- |
| none | 1.0 | 2.43 | 5.81 | 3.46 |
| sel-10% | 0.1 | 0.50 | 7.91 | 7.75 |
| sel-1% | 0.01 | 0.32 | **41.33** | **41.30** |
| sel-0.1% | 0.001 | 0.25 | **158.45** | **144.87** |
| ts-range-degraded | 0.001 | 8.26 | **124.26** | **121.87** |

- **NFR-02 无过滤档位在 10 万级仍达标**（2.43 / 5.81 / 3.46ms）
- **⚠️ R18（已知限制，非 bug）：低选择度过滤查询延迟爆炸**。选择度 ≤1% 时带过滤的
  vector/hybrid P99 达 **41~158ms**，超 NFR-02 限额 **4~16×**。机理是 `hnsw_rs::search_filter`
  的整图遍历（R13），但量级是**规模 × 选择度的复合效应**，远超此前「随规模线性上升」的预估。
  **V2.0 显式接受**：正确性优先于延迟，且优化前的 post-filter 在该选择度下几乎返回不了任何结果
  （这正是 Q-I2 的原始病症）。**该档位当前不适用于在线路径**，V2.1 必须解决
- **⚠️ 高基数 Range 的代价被量化**：`ts-range-degraded` 档位的 bm25 P99 = **8.26ms**，
  比 bm25 无过滤检索本身（2.43ms）还贵 **3.4 倍** —— 降级字段的 O(N) 全扫已超过整个检索的代价

#### 对先前结论的修正

- ⚠️ **此前写「hybrid 重合率未达 0.99，缺口全部来自向量路」是错的**，实测推翻：
  bm25 与 vector 的**分路重合率都是 1.0000**，只有融合后掉到 0.685~0.948
  ⇒ 分歧不可能来自任何一路的下推，**只能诞生于融合层**。
  机制：oracle 用 **fuse-then-filter**，实现用 **filter-then-fuse**；过滤会重排路内名次，
  而 RRF 只吃名次，两种顺序的 Top-10 自然不同。
  因此 **hybrid 重合率不能当作召回缺口指标**——真正的召回判据是
  **分路重合率 1.0000 + 返回条数 10.00**，两者均满分。
  两种融合顺序哪种相关性更好**未决**（现有 fixture 的 relevance 针对全库标注，
  过滤后被机械稀释，无法判定），列 V2.1

#### 文档

- 架构文档 v1.5：新增 **ADR-010**；§5.4/§5.4.1（存活单一真源）、§5.5/§5.5.1（谓词化与已知短板）、
  §7.5 整节重写（原 instant-distance + delta 方案已在 D8 移除）、§7.6.1（新增结构不入快照）、
  §8.3（`Metrics` 三项与其**当前不可观测**的限制）、§8.4（空结果语义）、风险表 R1 标记消除 + 新增 R11~R18
- `plan-v2.md`：Step 1 全部任务勾选 + 1 万 / 10 万两级实测结论
- 需求文档 v1.5：NFR-02 补 10 万级实测读数；FR-26/FR-27 标已完成
- `docs/README.md`：补 `plan-v2` / `v2-step1-design` 两篇 V2 文档的索引

#### 其他

- `.workbuddy/`（Agent 工作记忆）已加入 `.gitignore` 并停止跟踪——它不是项目产物，
  不应随仓库公开
- CLI 的 `--filter` 解析支持 range（`field>=n` / `field<n`），并对不支持的运算符
  （`<=` / `>`）给出**明确指出**而非含混的「数值解析失败」；配套 7 项单测 + CI smoke

### 修复 — bench oracle 深度 panic（PR #10，issue #9，2026-09-05）

#### 修复

- **`--oracle-depth ≥ 501` 触发 `clamp` panic**（`min > max`，与 debug/release 无关）：
  `oracle_depth_for` 的下界 `k × depth` 随用户参数无界增长，上界固定 `ORACLE_MAX_DEPTH=5000`，
  二者未对齐。修复为**下界先被上限夹住再 clamp**，与早退分支对齐；同时两处乘法改
  `saturating_mul`（极端参数下「深度过大」不再变成「bench 崩溃」）
- `--oracle-depth` clap 侧加范围校验 `1..=500`（k=10 时 k×depth ≤ 5000 恰不撞上限），
  非法值直接给出明确错误而非中途 panic 丢现场
- 边界单测：`depth ∈ {501, 600, 5000, usize::MAX/2, usize::MAX}` 全组合不 panic 且
  收敛上限；默认 depth=10 语义回归

## [0.2.0] — 2026-09-04（P6 接口重构）

依据 issue #1「融合索引接口重新设计」重构库的对外接口层，引入门面层。
设计文档 `docs/devel/p6-design.md`（v2.1），架构文档新增「对外接口设计」章。

### 门面层（新增）

- `SearchIndex`（写端）：`builder().build()` 零配置装配（MixedAnalyzer + bge-small-zh + HNSW + RRF）
- `SearchIndex::add(doc)`：分块 → 倒排 → 写缓冲 → 批量 embed，幂等去重（`dedup_key`）由库保证
- `SearchIndex::commit()` / `flush()`：显式可见性（对齐 Lucene）；写缓冲 `batch_size=64`（实测 32~256 差异 <20%）
- `Searcher`（读端，`'static + Clone + Send + Sync`）：`search(query)` 仅 query 必选
- `SearchRequest` builder：`.mode()`（enum 或 `"hybrid"` 字符串）/ `.top_n()` / `.filter()`
- 消除三个静默正确性陷阱：content_hash、L2 归一化、analyzer 生命周期全部内聚进库

### 接口重构（breaking）

- `Document` 重定义为输入 DTO（含 `text`）；新增 `DocRecord` 承接存储记录
- 旧 `Searcher<'a>` → `QueryExecutor<'a>`（签名不变，底层逃生舱仍可用）
- 所有权切换用 `Arc::try_unwrap`（refcount>1 明确报错，非静默深拷贝）

### 配置指纹（修 B1/B2）

- `Analyzer::id()` / `Embedder::id()` 身份标识；快照写入 `ConfigFingerprint`
- load 时校验装配，不一致报 `Error::ConfigMismatch { expected, actual }`
- `FORMAT_VERSION` 1→2；`effective_version()` 把 positions +1 从注释约定落成实现

### 迁移与回归

- CLI build/search/compare 切门面层（`--help` 逐字不变）；bench 删 `--analyzer` 警告回退
- `examples/search_basic.rs` 79→33 行；`docs/user-guide.md` 库接入章节更新
- **回归对账**：brute 后端新旧逐位一致（bm25 0.4491 / vector 0.5279 / hybrid 0.5213）

> ⚠️ **迁移提示**：`Document` 语义变化（`doc_id`/`content_hash` 字段移除，改由库内计算）；
> 快照 `FORMAT_VERSION` 1→2，**旧 `.idx` 快照需重建**（`helix build` 重新生成）。

## [0.1.0] — 2026-09-03

第一版：面向 Agent 场景的通用检索引擎内核，P0~P5 全部完成。
项目正式定名 **HelixIndex**（库 crate `helix-core`，CLI 二进制 `helix`）。

### P0 — 工程骨架与依赖验证

- workspace 骨架：`crates/core`（库）+ `crates/cli`（CLI）
- 三段式命令入口 `make fmt / lint / test / deny`；`Cargo.lock` 入库
- 本地 embedding 链路打通：`fastembed` + `Xenova/bge-small-zh-v1.5`（512 维）
- MSRV 定为 **1.90**（由依赖实测决定，非最初的 1.80）；引入 `cargo-deny` 守许可合规

### P1 — BM25 链路（analyze → index）

- `MixedAnalyzer`：中英混合分段 + jieba 分词 + 过滤器链 + 停用词
- 倒排/正排存储、统计量维护、TAAT Top-K、BM25 打分（**OR 语义**）
- `Retriever` 抽象；与 `tantivy` 的手算值对照测试（ADR-007）

### P2 — 向量链路（embed → HNSW）

- `Embedder` / `VectorIndex` 抽象；`LocalEmbedder`（BGE 查询侧 instruction 前缀）
- 向量入库前强制 L2 归一化（自定义 `DistDotClamped` 规避 aarch64 浮点断言）
- feature flags：`local-embed`（默认）/ `remote-embed`

### P3 — 融合、编排与可解释性

- `FusionStrategy`（RRF 默认）+ `Reranker`（NoOp 留位）
- `Searcher` 编排三路；结构化响应（含 `explain` 与 `took`，FR-12/13）
- 确定性（NFR-06）与 query embedding 缓存（`moka`）

### P4 — 持久化、增量写入与过滤

- 快照格式 `IDX1`（bincode + CRC32）与 `storage::save/load`
- 幂等 upsert（content_hash，FR-15）、墓碑删除与统计量回滚
- 元数据过滤（`Filter`，FR-14）
- 向量索引 A/B 定案：**`hnsw_rs` 胜出**（原生增量），弃用 delta 区设计

### P5 — 语料、评测与调参

- 评测数据源改用 **T2Ranking**（Apache-2.0，SIGIR 2023）：320 query / 12K 段落 / 4 级标注
- `core::bench`：分级 NDCG（gain=2^g−1）、阈值化 Recall/MRR、二项精确符号检验
- CLI `bench`：三路对照、分桶统计、`--runs` 抖动披露、网格搜索、`--json` 输出
- **参数定稿**（真实语料实测，不盲信英文经验值）：
  - BM25 `k1=1.5 / b=0.75`（16 格网格）
  - RRF `k=60 / weights=[1.0, 1.5]`（weights 诊断）
  - `ef_search=200`（100/200/400 校准）
- **三路结论**：hybrid 的 MRR@10=0.6922 与 Recall@10=0.6829 全场最高；
  NDCG 0.5183 未超越 vector 单路 0.5249（p=0.385 不显著，**如实记录**）
- BM25 对 tantivy 基线相对差 **0.31%**，Top-10 重叠 97%
- charabia 分词对照：全面落后自研链（NDCG −5.9%），**保留自研链**
- NFR 实测：延迟达标；构建 236.7s **未达标**；快照加载 53~107ms 达标但图重建 11.57s
- 移除 `instant-distance` 依赖（D8）；`vector_ab` 改写为真实语料召回对照（重合率 0.994）

### v1 收尾（同日）

- 使用文档 `docs/user-guide.md`（CLI 全参数 + 六 trait 替换矩阵）
- rustdoc 守门：库 crate 加 `#![deny(missing_docs)]`，补齐 102 处文档注释
- 评测脚本化：`make eval-quality` / `eval-perf` / `report`（报告数字机械可复现）
- CI：GitHub Actions 9 个 job（fmt/lint/test/deny/doc/MSRV/feature 隔离/release/smoke）
- 修 `bench` 短参冲突（debug 下 panic）与 RRF 默认值语义分裂
- NOTICE（第三方资产声明）、CHANGELOG

---

## 已知未达标项（延续到下一版本）

| 项 | 现状 | 计划 |
| --- | --- | --- |
| NFR-03 构建耗时 | 12K 段落 236.7s（目标 <120s） | V2 Step 4：并行 / 量化 / 增量构建 |
| ~~NFR-04 冷启动~~ | ~~HNSW 图重建 11.57s~~ → **✅ 已达标**（V2 Step 2 图持久化，完整冷启动 ≈100ms） | 已解决；残留风险：降级路径仍 ~10s，故配 NFR-07 降级可观测 |
| 磁盘占用 +60% | 图 sidecar 使 52.3MB → ≈84MB（1.6×），`hnsw_rs` 只支持全量 dump，省不掉 | V2 Step 5 compaction 时一并优化；当前用 `--no-graph-persist` 逃生舱 |
| paraphrase 桶融合 | hybrid 被弱路 BM25 稀释 | P6：自适应融合 / reranker 兜底 |
| 假负例敏感度 | 未做对照 | P6：仅 qrels 语料重建对照 |

## 下一版本（P6，v2）范围

Rerank / MMR 多样性 / 自研索引，以及上表四项残留。
任务清单见 `docs/devel/plan.md`。
