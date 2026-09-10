# 更新日志

本项目所有值得注意的变更都记录在此。
格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

> P0~P5 的条目为**逆向补写**（2026-09-03），依据各阶段设计文档与 git 历史整理；
> 此后每次变更即时追加。

## [Unreleased]

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
覆盖」这条**授权链已断**，改指新承接者；`docs/devel/plan-v2.md` Step 2 提交链标注「本 PR 待合并」。
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
