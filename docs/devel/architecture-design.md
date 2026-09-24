# HelixIndex 架构设计说明书

### —— 面向 Agent 场景的通用检索引擎内核

| 项目   | 内容                   |
| ---- | -------------------- |
| 文档版本 | v1.24                |
| 创建日期 | 2026-09-02           |
| 状态   | **V2 Step 9 设计第 1~2 轮评审响应（`v2-step9-design.md` v0.1 → **v0.2**，2026-09-24）**：**§5.6 补第二个 provided 方法 `fuse_adaptive`** —— `weights_override` 是**决策**钩子，但编排层只持 `&dyn FusionStrategy` 且 `fuse` 读策略私有字段 ⇒ **`Some(w)` 无处可用**（设计第 2 轮评审 **P3-1**）⇒ 再补一个**应用通道**（默认 = `self.fuse` ⇒ **既有实现零改动**）+ 写死「开关关 ⇒ 不调用 `fuse_adaptive`」。⚠️ 不增删风险编号、无 trait 签名 / 结构 / 预算变更；需求侧**不变**（仍 v1.24）。此前 **V2 Step 9 设计已出（`v2-step9-design.md` **v0.1**，2026-09-23，PR9-0，待评审）**：**§5.6 的 `FusionStrategy` 伪码按实现更正**（+ provided 方法）+ **新增 §14.7（R55 ~ R58）**。此前 **V2 Step 8 收尾第 1 轮评审响应（`S8-09`，2026-09-22，设计 `v2-step8-design.md` v0.15 → v0.16）**：**§14.4 的 `R43` 补判后记录**（评审 **P3-1**）—— `NFR-10` ③ 的**复审触发条件已触发并已回答**（Q3′：`intra_threads` 不改变向量数值）⇒ **`R43` 保持「打开」**（spike S8-S2 判「不投」；⚠️ 池化确有增益 1.23× → 2.10× ⇒ FAIL 不得读成「池化无效」；判据③ 阈值在查询侧口径下结构性不可达 ⇒ 口径本身进复审）。⚠️ 不增删风险编号、无 trait / 结构 / 预算变更；需求侧随之 **v1.23**。 此前 **V2 Step 8 收尾（`S8-09`，2026-09-22，设计 `v2-step8-design.md` v0.15）**：**§14.6 的 R51 ~ R54 残余列已按实测回填** —— R51 收窄（方案 A 落地、**图拓扑仍漂移**）、R52 已量化（谓词 **+1 us** / 多段 **+8 us**）、R53 已测（②a 三次运行 ≤ 1.2、**基线写死**，尾部不可复现 ⇒ 保持开放）、**R54 消解**（因 spike S8-S2 **判「不投」** ⇒ 条件未触发，翻转须重开）；**R49 / R50 仍不标「已解决」**。⚠️ 不增删风险编号、无 trait / 结构 / 预算变更；需求侧随之 **v1.22**。 **V2 Step 8 设计第 1 轮评审响应（2026-09-17，`v2-step8-design.md` v0.1 → **v0.2**）**：评审 **5×P3 + 7×P4、无阻塞**；**§14.6 R51 的残余列已按评审 P3-5 补两条** —— ① **累积性**（方案 A 每次合并都 `load_graph` ⇒ 每次多一个 leaked `HnswIo`，而异步合并是常态路径）；② **量级**（⚠️ **评审的量级前提经作者独立复算后更正**：leaked 的只有 `HnswIo`、**≈200B + 路径串 / 次**，**不是图规模** —— 依据 `vector/persist.rs:210-216` 的 P1-2 注释）。⚠️ **本次未新增 / 修改任何风险条目、无 trait / 结构 / 预算变更**（R49 ~ R54 条目正文一字未改，只动 R51 的「残余」列）。此前 **V2 Step 8（读写并发）详细设计已出、待评审（2026-09-16，`v2-step8-design.md` **v0.1**）**：**§14.6 新增 R49 ~ R54**（跨段 BM25 的「逐位一致」依赖三条前提 / 「合并保 ID」与 `compact()` 重编号的语义冲突 / 向量侧合并成本与拓扑不确定 / BM25 热路径 `bm25_f` 由 `None` 变 `Some` / 视图指针锁的「写者优先」/ 会话池内存）+ **§14 导读补 §14.6 指引**；**ADR-A 补一条扩展边界**（`save()` 前先 `merge_all()` ⇒ 落盘仍是**单段** ⇒ `FORMAT_VERSION` 保持 **2**、manifest 仍绑单快照 CRC ⇒ **不开 ADR-B**）。⚠️ **本次只新增 §14.6 的风险条目，既有条目一字未改**；需求侧随之 **v1.20**。**V2.1 计划修订（2026-09-16）**：把 V2.1 的**风险承接关系写实** —— §14.3 **R34 → Step 8 的 T7-25**（精确扫描改逐层 `get_layer_iterator`，**必须先于 T7-06**）、§14.4 **R43 → Step 8 的 T7-26**（查询侧会话池 + 挂起的 Q3）、§14 主表 **R11 / R13 → Step 10 的 T7-27**（prefilter / 排序键索引，**先 spike 判投不投**）；⚠️ **V2.1 不新增风险编号** ⇒ **R49+ 由 `v2-step8-design.md` 起分配**。**V2.0 Step 1~7 实现完毕 + S7-05 收尾（2026-09-16，设计 `v2-step7-design.md` v0.3）**：**§14.5 的 R44 数值已回填**（精排内存增量 **2.64 GiB / 7.71×**，`eval-report.md` §8.14.10）、**R48 收窄后关闭**（截断非「精排无提升」主因）、**R47 已有全部实测（P1/P2/P3）**；**Step 7 的行号锚点已改符号指代**（规则见 §14.5 导读）。此前 **V2.0 Step 1~6 + 横切 T7-21/T7-24 已并入 `main`；Step 7 详细设计**已出并完成评审收口（`v2-step7-design.md` **v0.2**，2026-09-14：评审 **4 项拍板全部同意**、**F1~F6 全部采纳**；**本文档无实质变更 —— §14.5 的 R44~R48、§5.7 / §5.8 的新增 API 一字未改**，本行为版本面同步）**（main **曾为** **`516b2c7`**——Step 6 实现 PR 6 **#44** 曾 squash 合并为 `8f4d08c`，其后 **PR #48**（T7-24 工程卫生）→ `e756bcc`、**PR #49**（T7-21 `parallel_build` 默认翻转）→ `516b2c7`；Step 5 收尾 PR #37 → `590315d`；PR #39「T13 质量门 → 同图可归因不变式」→ `bb23553`）；**Step 6 已完整收尾（S6-10，2026-09-13）**——实现任务 **S6-01~S6-09 全部开工完毕**（**S6-04 / S6-05 判「不投」**），`v2-step6-design.md` 升 **v0.4**（实现期 v0.3 → 收尾回写 v0.4）；**§14.4 扩为 R36~R43**（R36/R37 消解、R38/R39 关闭、R40 上锁、新增 R41/R42/R43）；**Step 6 本步无 trait / 结构变更**。**Step 7（精排）详细设计已出、待评审**（`v2-step7-design.md` **v0.1**，2026-09-14）——**§14.5 新增 R44~R48**；**§5.7 补 `candidate_window`（provided，默认 `k` ⇒ 零破坏）** + **§5.8 补 `Explain.rerank_score`（⚠️ 破坏性）与 `Metrics.rerank_elapsed` / `rerank_window`（纯加法）**；精排器 = `rozgo/bge-reranker-v2-m3`（**非** BAAI 官方）；H3 取形为**可配 `R`、默认 20**（**2026-09-16 已定稿**，见 v1.17）。步骤编号映射见 `plan-v2.md` §附-2 |
| 技术栈  | Rust 1.90+ / 2021 edition |
| 文件名   | `architecture-design.md` |
| 配套文档 | `requirements-spec.md`（需求分析说明书） |

---

## 1. 文档信息

### 1.1 版本与状态

| 版本   | 日期         | 状态  | 说明                          |
| ---- | ---------- | --- | --------------------------- |
| v1.0 | 2026-09-02 | 待评审 | 由 `archive/requirements-and-design_v1.0.md` v1.0 拆分重构而来 |
| v1.1 | 2026-09-02 | 已定稿 | `thirdparty.md` 调研回写：ADR-007（tantivy dev 基线）/ ADR-008（MSRV 1.90 + cargo-deny）、Unicode 分段改用现成库 |
| v1.2 | 2026-09-02 | 已定稿 | P0 执行后回写：bincode 定 2.0.1、fastembed 须 `default-features = false`、模型源修正为 `Xenova/bge-small-zh-v1.5` |
| v1.3 | 2026-09-03 | 已定稿 | P5 评测数据源切换 **T2Ranking** 后的目录树 / CLI 示例 / 阶段门槛同步 |
| v1.4 | 2026-09-04 | 已定稿 | `p6-design` 接口重构：新增第 10 章「对外接口设计（门面层）」+ ADR-009，原 10~14 章顺延 |
| v1.5 | 2026-09-05 | 已定稿 | V2 Step 1（向量软删除 + 过滤下推）回写 |
| v1.6 | 2026-09-06 | 已定稿 | **V2 Step 2（图持久化，ADR-A）回写**：5.4.3 / 7.6.2 / 8.2 / 8.3 / 14.1 |
| v1.7 | 2026-09-07 | 已定稿 | **V2 计划复审 + 步骤重排同步**（复审结论已并入 `plan-v2.md` §附-3）：① §7.5「物理回收归 Step 5 compaction」→ **Step 4** 并补「compaction 后必须重发 manifest」（否则新图永远匹配不上、冷启动恒走降级重建）；② §14.1 R22 对策同步为 Step 4；③ §14.1 **R18 更新**——低选择度兜底由「V2.1 必须解决」**提前到 V2.0**（D-J9：Step 5 / T7-22，新增 NFR-13）；④ §14.1 **R19 残余归 Step 3（S3-e）**；⑤ **§7.6.2 补记事实**——快照本体至今非原子（`storage/snapshot.rs:88` 直写无 fsync），崩溃即 `SnapshotCorrupted`（索引丢失，不是降级）；⑥ §12.2 注明矩阵仅反映 V1，V2 归属见 `plan-v2.md` §5。编号映射见 `plan-v2.md` §附-2 |
| v1.8 | 2026-09-07 | 已定稿 | **V2 Step 3（原子快照）落地回写**（依据 `v2-step3-design.md` v0.3，D-S3-01~07 全部拍板）：① §7.6.2 前提段**销账**——快照本体原子性已落地（`storage/atomic.rs` 通用原语：tmp + flush + `sync_all` + rename + fsync 父目录，快照与 manifest 共用一份实现），Q-C3 正确性欠账清零；② §14.1 **R19 写路径残余收敛**——新增 `dump_graph_caught`（`catch_unwind` → `Err(VectorGraph)`，汇入 P0-3 语义链），残余更新为「panic=abort 宿主约束（文档级）/ panic 噪音 / 读路径（C5 边界外）」；③ 12K fsync 代价实测入 `eval-report.md` §8.7（+0.027~0.035s，不触碰任何 NFR） |
| v1.9 | 2026-09-10 | 已定稿 | **① V2 Step 4 回写补记**（`#32` / `9e2211e` 已落地 §7.5.3 与 §14.2，但**漏记版本行**，本行补齐）：§7.5.3「墓碑物理回收（compaction，FR-30）」+ **§14.2 新增 R26~R30**（compaction 内存峰值 2× / ID 重编号破坏外部持久引用 / 耗时未实测 / 拓扑抖动 / 期间无法服务），10K churn 0.3×5 实测 graph+data 84.6→33.7MB。**② V2 Step 5 详细设计回写**（依据 `v2-step5-design.md` v0.3，D-S5-01~09 全部拍板）：**§5.4** `VectorIndex` 新增两方法（`search_exact_filtered` **必选** + `prefers_exact` 默认 `false`）并补 `vector_shortfall` 判读注记（精确路径下结构性归零 ⇒ 必须连看 `vector_route`）；**§5.8** `SearchResponse` 新增 `metrics: Metrics` 字段（**破坏性**，R35）；§8.3 新增「Step 5 补外部可见」块（`Metrics` 进 `SearchResponse` D-S5-05 / per-lane 耗时 D-S5-06 / `vector_route` D-S5-07 / 修两条早退 `took` D-S5-08）+ 改写「当前限制」为「设计已承接、待实现」；**§14 章级主表 R18 补 ④**——「0.05ms」口径修正（只含算距离、不含 `O(N)` 定位候选）+ 三路径对策 + ⚠️ 只覆盖低选择度子集；**§14.3 新增 R31~R35**（O(N) 谓词判定 / 兜底改变输出 / 阈值经验值 / 精确扫描持读锁归 Step 8 / 对外破坏性变更）；§14 风险表导读补 R31~R35 指引与「R11/R13/R17/R18 不得标已解决」的边界声明 |
| v1.10 | 2026-09-10 | 已定稿 | **V2 Step 5 实现期口径精确化**（PR #36 评审 F1）：`vector_shortfall` 在精确路径下**不是「恒为 0」**——`allowed` 来自 `Index`、扫描枚举的是**图里的点**，两个独立来源，图滞后于索引时该值仍 > 0。**§5.4** 注记与 **§8.3 `D-S5-07` 行**改为**条件式**，并把该条件本身升级为信号：`Exact` + 缺口 `> 0` ⟺ **图未覆盖全部 allowed chunk**（`Exact` + 缺口 `== 0` 才说明「这一档没走 ANN」）。同步精确化实现侧措辞（`query/metrics.rs`、`query/searcher.rs`、集成测试、CLI 图例、`scripts/eval_filter.sh`、CHANGELOG）。⚠️ 本行同时**补正表头版本号**：v1.9 只改了 §1.1 表，表头仍停在 v1.8 |
| v1.11 | 2026-09-10 | 已定稿 | **V2 Step 5 收尾：S5-04 标定定稿 + S5-08 文档回写**（实现已合并 `48c0ac7`；设计文档升 **v0.5**）：**① §14.3 R31 / R33 未标定 → 已标定**——阈值由初值 **1024 改为 8192**（10 万级 A/B 两端点实测 + 归一插值交叉点 ≈ 9700；原初值过保守，会漏掉 `allowed=5043` 这个 2.06× 的档位；R31 的规模上界仍标注「100 万级未测」）；**② ⚠️ R34 措辞改写**——由「与写端互斥、时长随 N 线性」改为「**含潜在死锁**」（`IterPoint::next` 在层切换时**递归读同一把 `RwLock`**，探针 3/3 触发；今天不可达，但 Step 8 照旧措辞复核会设计错），修法 = 设计 §4.2.2 的**逐层 `get_layer_iterator` 遍历**，归 Step 8 开工前；**③ §14 章级主表 R18 补 ⑤**——标定后的端到端 P99（`sel-1%` 39.49→**5.01ms**、`sel-0.1%` 112.69→**7.06ms**、`ts-range-degraded` 223.24→**21.24ms**）+ NFR-13 双口径；**④ §14.3 表头**由「设计已定稿、实现未开工」改为「已完成并合并」（Q1~Q5 全部结案） |
| v1.12 | 2026-09-11 | 已定稿 | **V2 Step 6 详细设计回写**（依据 `v2-step6-design.md` v0.1，D-S6-01~09 待评审）：**§14.4 新增 R36~R40** —— 多 session 内存峰值（R36，**条件性**）/ CoreML EP 改变向量数值致效果基线不可横比（R37，**条件性**）/ `default_embedder()` 静默降级（R38，**今天就存在**）/ 追加路径漏发图 manifest（R39，与 Step 4 同一坑）/ ID 复用致幽灵向量（R40，与 Q-C1 同族、成因相反）；§14 导读补 R36~R40 指引与「R36/R37 为条件性」的说明。⚠️ **本 Step 不引入任何 trait / 结构变更**（`Embedder` trait 一字不改、`ConfigFingerprint` 结构不动——EP 配置改用 `embedder_id` 编码，理由同 R35 的破坏性变更判定），故不触碰 §5.x / §8.x / §10.x；**也不触碰任何 FR / NFR**（NFR-10 的数值目标缺失问题登记为 `v2-step6-design.md` §9.2 Q4，待评审拍板后才修订需求文档） |
| v1.13 | 2026-09-11 | 已定稿 | **V2 Step 6 设计评审收口：架构侧无实质变更，本行仅为版本面同步**（设计文档升 **v0.2**；PR #38 的 7 条评审意见 F1~F7 全部采纳，**D-S6-05 / D-S6-08 拍板**、Q4~Q7 收口）——**未新增或修改任何风险条目（R36~R40 保持原样）、trait、结构**：R38 的「应对」本就指向 D-S6-05 方案 A，本次只是把该决策正式拍板；**未触碰任何 FR / NFR**——NFR-10 的口径修订属**需求面**，落在 `requirements-spec.md` **v1.14**。评审对本文档的复核结论：§14.4 R36~R40 的登记与「R36/R37 条件性」判定**成立** |
| v1.14 | 2026-09-13 | 已定稿 | **V2 Step 6 收尾（S6-10）回写 —— §14.4 由「R36~R40」扩为「R36~R43」**：① **R36 / R37 消解**——T7-09 判「不投」（E2 增益 +11.7%/+16.0% 而峰值 RSS **+96.4%/+289.2%**；E3 调优后仅 **+1.2%**）⇒ 多 session / CoreML EP 均不落地，条件性风险**不发生**（R36 的峰值代价已被数据钉死，不是「没人碰」）；② **R38 / R39 在 Step 6 内关闭**（S6-09：`--vectors` 显式请求向量**硬失败** + 内部测试接缝；S6-T13：追加后冷启动 `Loaded` 且 manifest `nb_point` 吻合）；③ **R40 已上锁、残余仍在**（S6-T8 不变式测试锁住 ID 单调，但「不冲突」的证明仍依赖三条隐含前提）；④ **新增 R41**（**建库路径峰值 RSS 2.32~3.33GB 与 NFR-05 的 372MB 差一个数量级** ⇒ 预算形态按 `batch × 序列长度` 给、**只给形态不给数**；NFR-05 已补限定词「（检索路径，batch 1）」，需求 v1.15）/ **R42**（**依赖安全公告漂移**：`bincode` `RUSTSEC-2025-0141` 两条路径 + `paste` `RUSTSEC-2024-0436`，均无升级路径、**长期开放**）/ **R43**（**查询侧编码把并发吞吐封顶**：`crates/core/src/embed/local.rs:35` 的 `Mutex<TextEmbedding>`；实测 4 线程 vector **1.88×** / hybrid **1.63×**，临时把 `embed_query` 移出被测路径 ⇒ vector **3.15×** ⇒ **阈值本身可达、卡在实现**；**保持打开**，复审触发条件 = 查询侧会话池落地时，届时须先答挂起的 Q3）；⑤ **§14.4 表头状态**由「设计已出、实现未开工」改为「实现完成 + S6-10 收尾回写」。⚠️ **无 trait / 结构变更**（`Embedder` 一字不改、`ConfigFingerprint` 不扩）；NFR-10 / NFR-05 的口径修订属**需求面**，落在 `requirements-spec.md` **v1.15** |
| v1.15 | 2026-09-14 | 待评审 | **V2 Step 7（精排）详细设计回写**（依据 `v2-step7-design.md` **v0.1**，D-S7-01~10 待评审）—— **① §14.5 新增 R44~R48**：R44 **精排器常驻内存**（`model.onnx.data` **≈2.19 GB FP32** ⇒ NFR-05 的 372MB 在精排开启时失效；⚠️ 与 **R41 不是同一件事**：R41 是**建库**路径峰值、R44 是**查询**路径**常驻**增量）/ R45 **精排延迟主导 + 窗口与 `candidate_k` 的静默夹取** / R46 **`Hit.score` 语义随开关而变**（R35 一族）/ R47 **精排数值可复现性未证实**（`PaddingStrategy::BatchLongest` ⇒ 跨 batch 组成的分数可能不逐位相同）/ R48 **512 token 截断使长段落判据失真**（T2Ranking 最长段落 **76,895 字符**）。**② §5.7 `Reranker`**：新增 **provided 方法 `candidate_window(&self, k) -> usize`（默认 `k`）** ⇒ 既有实现**一行不改**、NoOp 路径**逐位零回归**；契约收紧「`rerank` **不得依赖** `hits[i].explain`」（**编译期无感**，D-S7-06 的接口收缩）；新增 **`LocalReranker`**（gate `feature = "local-rerank"`，⚠️ **未启用时编译期不存在**、非运行时静默退化）。**③ §5.8**：`Explain` 新增 **`rerank_score: Option<Score>`（⚠️ 破坏性）**、`Metrics` 新增 **`rerank_elapsed: Duration` / `rerank_window: usize`（纯加法，保持 `Copy`）**；`Hit.score` 在精排生效时 **= `σ(logit)`**（单调 ⇒ 排序零损失，融合分仍留 `Explain.fused_score`）。**④ §14 导读**补 §14.5 指引。⚠️ **本步不引入 trait 破坏**（只加 provided 方法）；**不进 `ConfigFingerprint`**（精排不改索引内容，D-S7-10）；NFR-12 / NFR-05 / NFR-06 / NFR-07 与 FR-18 的修订属**需求面**，落在 `requirements-spec.md` **v1.16** |
| v1.16 | 2026-09-14 | 待评审 | **V2 Step 7 设计评审收口：架构侧无实质变更，本行仅为版本面同步**（设计升 **v0.2**）—— 评审 **4 项拍板（D-S7-04/05/01/08）全部同意建议**、**F1~F6 全部采纳**；**§14.5 的 R44~R48 条目一字未改**（评审逐条核过 R46 / R48 的实际登记内容，确认与设计一致；S7-05 收尾时已按实测回填 R44 / R47 / R48 三项，见 §14.5）；**§5.7 / §5.8 的新增 / 变更 API 一字未改**。评审唯一触及架构语义的条目是 **F1**：`plan-v2` §8 曾把窗口写成「`Config` 新增字段」（与 **D-S7-02**「`Config` / `SearchParts` 一律不改」矛盾）⇒ 已在 `plan-v2` 更正，**架构侧无需改动**。NFR-12 的措辞更正属**需求面**，落在 `requirements-spec.md` **v1.17** |
| v1.17 | 2026-09-16 | 待评审 | **V2 Step 7 收尾（S7-05）**：① **§14.5 R44 数值回填**（精排内存增量 **2.64 GiB / 7.71×**，`eval-report.md` §8.14.10；条目仍开放、不给独立预算上限）；② **R48 收窄后关闭**（本评测集上截断非「精排无提升」主因：`max_len 512→1024` 的 ΔNDCG **+0.0002**；限定「产品侧目标场景未测」）；③ **R47 补全部实测**（P1/P2 逐位一致、P3 单条 vs 批内 logit 相同）⇒ 残余改「已实测」；④ **定一条锚点规则**（§14.5 导读）：本仓源码锚点一律**符号指代**，依赖锚点带版本；并把 R44/R47/R48 三行里 Step 7 引入的锚点按此改写；⑤ 需求侧随之 **v1.18**。⚠️ **无 trait / 结构 / 预算变更** |
| v1.18 | 2026-09-16 | 待评审 | **V2.1 计划修订（架构侧）—— 只补「风险承接关系」，不新增风险条目、不改任何既有条目内容**：① **§14.3 R34 的「残余」列**补落点 —— 已落为 `plan-v2.md` Step 8 的 **T7-25（硬前置，必须先于 T7-06）**；② **§14.4 R43 的「残余」列**补落点 —— 其「复审触发条件 = 查询侧会话池落地时」已落为 Step 8 的 **T7-26**（含挂起的 Q3）；③ **§14 主表 R11 / R13 的「应对」列**补 prefilter 落点（**`plan-v2.md` Step 10 的 T7-27 + 新 FR-34**，第一用例 = 高基数数值字段：基数保护 `terms + numbers ≤ 1024` ⇒ 毫秒时间戳 / 雪花 ID 约 **512 篇**即永久降级）；④ **§14 导读补「V2.1 承接」段**，并钉死一条编号纪律：**V2.1 的设计从 `R49` 起分配**（⚠️ 不要按「Step 6 = R36~R40」的老口诀从 R44 起算）。⚠️ **R34 / R43 / R11 / R13 / R17 / R18 一律不标「已解决」** —— 本次只把「谁在哪一步接」写实；**§1.1 与 §15.2 两处版本表同批加本行** |
| v1.19 | 2026-09-16 | 待评审 | **V2 Step 8（读写并发）详细设计回写**（依据 `v2-step8-design.md` **v0.1**，D-S8-01 ~ 12 待评审、其中 **4 条需拍板**：**D-S8-01** 持久化形态 / **D-S8-06** 旧 API 处置 / **D-S8-09** 向量合并取形 / **D-S8-11** 会话池投不投）—— ① **新增 §14.6（R49 ~ R54）**：**R49** 跨段 BM25 的「逐位一致」依赖三条前提（全局精确整数统计量 / term 外层累加 / 段 ID 空间不重叠 —— 任一被破坏则 `score` 只在**分段布局**下漂移）／**R50** 「合并保 ID」与 `compact()` 重编号的语义冲突（合并若复用 `Index::compacted()` ⇒ 用户侧 `chunk_id` **静默失效**）／**R51** 向量侧合并成本与拓扑不确定（`hnsw_rs` 无图合并 API ⇒ dump→load→insert 或全量重建 ≈ **38.7s/12K**）／**R52** 热路径回归：`bm25_f` 由 `None` 变 `Some`（一旦有跨段墓碑，无过滤查询的 BM25 lane 每条 posting 多一次 `dyn contains()`）／**R53** 视图指针锁的「写者优先」（`std::sync::RwLock` 写者等待会挡住新读者 ⇒ **写锁只可用于指针替换**）／**R54** 会话池内存（R36 的查询侧背面；⚠️ **不得引用 E2 的建库口径读数**）。② **§14 导读**补 §14.6 指引 + 「R49/R50 无关闭时点」「R51/R54 等 spike、不预先承诺」两条纪律。③ **ADR-A 的扩展边界**（设计 §4.10）：`save()` 把 `merge_all()` 作为**第一步** ⇒ 落盘仍是**单段 `Index`** ⇒ `SnapshotSections` / `FORMAT_VERSION`（= **2**）/ `GraphManifest`（`snapshot_crc` + `snapshot_len` + `nb_point`）**一个字节都不用改** ⇒ **不开 ADR-B**（多段快照登记为**条件项** Q2）。⚠️ **本次无 trait / 结构 / 预算变更**（**五 trait 一字不改**；公开面只有加法 + 两处待实现期核实的破坏性：`VectorRoute` 新变体的 `#[non_exhaustive]`、`Config` 是否有字面构造用法）；需求侧随之 **v1.20**（**NFR-14 定口径 + NFR-11 补口径与拟值 + FR-17 §5.3.5 措辞改写**） |
| v1.20 | 2026-09-17 | 待评审 | **V2 Step 8 设计第 1 轮评审响应（`v2-step8-design.md` v0.1 → v0.2）** —— **只动 R51 的「残余」列**（2026-09-17，#60 评审 **P3-5**）：补 **① 累积性**（方案 A 每次合并 `load_graph` ⇒ **每次多一个 leaked `HnswIo`**；后台异步合并是 FR-17 的常态路径 ⇒ 长驻宿主累积）与 **② 量级**（⚠️ **评审的量级前提经独立复算后更正**：leaked 的**只有 `HnswIo`** —— `vector/persist.rs:210-216` 的 P1-2 注释逐字「每次加载约 200B + 路径串」，向量数据由 `Hnsw<'static>` 自持、**可回收** ⇒ **不是图规模**；10 次合并 ≈ 2 KB）。S8-S1 决策门第 ⑤ 条据此定阈值 **< 1 MiB / 连续 10 次合并**。⚠️ **§14.6 的 R49 ~ R54 条目正文一字未改**（含 R51 的风险 / 影响 / 应对三列）；**无 trait / 结构 / 预算变更**；需求侧随之 **v1.21**（两处笔误） |
| v1.21 | 2026-09-22 | 待评审 | **v1.21（2026-09-22）**：**V2 Step 8 收尾（`S8-09`，设计 `v2-step8-design.md` 升 v0.15）—— 只回填 §14.6 的 R51 ~ R54 残余列 + 版本面**：① **R51** 收窄（spike S8-S1 五条判据全 PASS ⇒ 方案 A 已落地；残余 = `HnswIo` 累积 + **图拓扑仍漂移**）；② **R52 已量化**（`bm25_elapsed` 四档 44 / 52 / 53 / 50 us ⇒ 谓词 **+1 us**、多段扫描 **+8 us**）；③ **R53 已测**（`S8-T11` 三次运行 ②a 皆 ≤ 1.2、**基线定义写死**；尾部扰动不可复现 ⇒ 保持开放）；④ **R54 消解**（spike S8-S2 判「不投」⇒ 条件未触发；**将来翻转须重新打开**）。⚠️ **不新增 / 不删除任何风险编号**（R1 ~ R50 一字未改）、**无 trait / 结构 / 预算变更**；需求侧随之 **v1.22**（NFR-14 ② 与 NFR-11 定稿去「拟」+ FR-17 状态漂移修正） |
| v1.22 | 2026-09-22 | 待评审 | **V2 Step 8 收尾第 1 轮评审响应**（`v2-step8-design.md` v0.15 → v0.16，PR7b） —— **§14.4 的 `R43` 补判后记录**（评审 **P3-1**）：`NFR-10` ③ 的**复审触发条件已触发并已回答**（Q3′：`intra_threads` 不改变向量数值）⇒ **`R43` 保持「打开」**（spike S8-S2 判「不投」）；⚠️ 池化**确有增益**（1.23× → 2.10×）⇒ 判据② 的 FAIL **不得**读成「池化无效」；判据③ 的阈值在查询侧口径下**结构性不可达**（≈ 97 MiB/槽）⇒ 口径本身进复审。⚠️ **不新增 / 不删除任何风险编号**（R1 ~ R50 一字未改）、**无 trait / 结构 / 预算变更**；需求侧随之 **v1.23**（`NFR-10` ③ 判后记录） |
| v1.23 | 2026-09-23 | 待评审 | **V2 Step 9 设计（`v2-step9-design.md` v0.1，PR9-0）**：**§5.6 的 `FusionStrategy` 伪码按实现更正** + 补 **provided 方法** `weights_override`（默认 `None` ⇒ 既有实现零改动）；**新增 §14.7**（**R55 ~ R58**）；⚠️ **不改任何既有风险条目与编号，不改 §5.7 的 `Reranker` 形态** |
| v1.24 | 2026-09-24 | 待评审 | **V2 Step 9 设计第 1~2 轮评审响应**（`v2-step9-design.md` v0.1 → **v0.2**，PR9-0）—— **§5.6 补 provided 方法 `fuse_adaptive`**（`weights_override` 的**应用通道**：只有「决策」方法时 `Some(w)` 无处可用）⇒ 默认 = `self.fuse` ⇒ **既有实现零改动**；§14.7 的来源行同步为 v0.2。⚠️ 不增删风险编号、无 trait 签名 / 结构 / 预算变更 |

### 1.2 读者

实现者与代码评审者。阅读前应先了解需求背景，见 `requirements-spec.md` 第 2、4 章。

### 1.3 与需求文档的关系

```
需求分析说明书
    │  FR-xx / NFR-xx 的唯一权威来源
    │  术语表的唯一定义来源
    ▼
架构设计说明书（本文档）
    只引用编号，不重复定义需求
```

本文出现的每个 `FR-xx` / `NFR-xx` 均指向需求文档。需求变更时只改需求文档，本文通过编号保持引用有效。

### 1.4 修订记录

见文末「15.2 变更记录」。

---

## 2. 架构概述

### 2.1 文档目的与范围

本文档定义 HelixIndex 检索内核的模块结构、核心抽象、数据结构、关键算法与技术选型。

**覆盖的需求**：FR-01 ~ FR-23、NFR-01 ~ NFR-09。FR-24（结果去重）、FR-25（token budget 裁剪）为 v2 需求，本版不做设计。

**需求覆盖矩阵**见第 12.2 节。

### 2.2 架构目标与约束

| 来源     | 目标 / 约束                                    | 架构体现                          |
| ------ | ------------------------------------------ | ----------------------------- |
| G2     | 全部核心能力可替换                                  | 第 5 章，六大 trait 抽象             |
| G5     | 为 Rerank / 增量 / 分布式留位                      | FR-18 NoOp 实现、7.5 增量方案、模块边界   |
| NFR-02 | 混合 P99 < 20ms                              | 第 8 章：两路并行、零 IO、缓存            |
| NFR-06 | 结果确定可复现                                    | 第 8.2 节：**图持久化冻结拓扑**（同快照两次加载）+ tie-break；⚠️「seed 固定」是过时论据，已删 |
| 需求文档 8.1 | BM25 手写、内存 + 快照、默认本地 embedding           | ADR-001、ADR-005、ADR-003       |

### 2.3 关键架构决策摘要

| 编号      | 决策                                    | 一句话理由                             | 相关需求   |
| ------- | ------------------------------------- | --------------------------------- | ------ |
| ADR-001 | BM25 倒排索引**手写**，不用 tantivy            | 符合"从 0 开发"目标，且中文调优与扩展完全可控         | FR-03、FR-04 |
| ADR-002 | 向量索引用 `instant-distance`，以主索引 + delta 绕开无 insert API | 纯 Rust 无 C 依赖，增量限制可用工程手段绕过。**P4 与 `hnsw_rs` A/B 复核，达标则删除 delta 区** | FR-08、FR-17 |
| ADR-003 | Embedding 默认本地 `fastembed`，远程作 feature | 无需联网与 API Key，中文效果好，远程作为兜底         | FR-06、FR-07 |
| ADR-004 | 融合**默认 RRF（k=60）**，加权归一仅作备选           | 两路量纲不可比，RRF 只用排名、免疫量纲              | FR-10、FR-11 |
| ADR-005 | 持久化用**内存索引 + 自研 bincode 快照**，不引入嵌入式 KV | 秒级加载、零额外依赖；bincode 3.0.0，**P4 与 `rkyv` A/B** | FR-16  |
| ADR-006 | BM25 检索用 **TAAT** 而非 DAAT/WAND        | 万级规模下实现简单、缓存友好，天然支持 OR 语义          | FR-04  |
| ADR-007 | **`tantivy` 仅作 dev 依赖的 BM25 正确性基线** | 不用它实现功能，**用它证明自己写得对**；自研最大风险是"写错了看不出来" | FR-03、FR-04 |
| ADR-008 | **MSRV 提到 1.90 并用 `cargo-deny` 强制 License 白名单** | NFR-08 的 1.80 已被主流依赖突破；License 友好是硬要求，人工审查不可靠 | NFR-08 |
| ADR-009 | **对外接口采用「门面层 + 共用编排内核」**（`SearchIndex` 写端 / owned `Searcher` 读端） | 复用同一编排实现避免逻辑漂移；重构是"加法"，bench/测试零改动，逃生舱保证能力不丢 | issue #1 |
| ADR-010 | **向量软删除 + 过滤下推**（存活位图作单一真源 + 谓词注入 + HNSW 双路径） | 双写必漂移且向量侧状态无法从快照恢复；无过滤是热路径，不为"几乎全通过"的谓词付整图遍历代价 | `v2-step1-design.md` |
| **ADR-A** | **图持久化：图 = 快照的派生缓存**（H1 的方案 C） | 把图塞进快照要动 `FORMAT_VERSION` 与"多文件原子性"；放外部 sidecar 则可用 **manifest 作唯一原子发布点**（tmp→fsync→rename），把多文件问题降维成单文件问题，且图**可随时丢弃、丢了只慢不错** | FR-29、NFR-04 |

> **ADR-A 编号说明**：V2 Step 2 的设计文档 `v2-step2-design.md` 全篇使用该编号，
> 架构文档 §7.6.1 曾预引用为「ADR-011」，**现已统一为 ADR-A**（避免与 ADR-001~010 的
> 数字序列混淆——ADR-A 是 H1 决策点下的方案级编号，不是新增的第 11 条架构决策）。
> 完整决策过程、7 个决策点与外部评审记录见 `v2-step2-design.md`。

---

## 3. 系统上下文与整体数据流

### 3.1 系统上下文

```
┌────────────────────────────────────────────┐
│  Agent 应用层                                │
│  知识库 RAG / 长期记忆 / 代码助手                    │
│  （查询改写、prompt 组装、多轮对话管理 —— 不在本内核）        │
└───────────────┬────────────────────────────┘
                │  SearchRequest / SearchResponse
┌───────────────▼────────────────────────────┐
│  helix-core（本内核）                          │
│  摄入 → 分析 → 索引 → 召回 → 融合 → 精排(留位) → 输出  │
└────────────────────────────────────────────┘
```

内核边界：**不做查询改写、不做 prompt 组装、不管多轮对话**。这些属于 Agent 应用层。内核只提供 FR-21 的多 query 接口作为承载。

### 3.2 写入路径

```
Document
   │
   ├─► Chunker ──► Vec<Chunk>
   │                  │
   │                  ├─► Analyzer ──► Vec<Token> ──► 倒排索引（可变）
   │                  │                                  └─► 正排存储
   │                  │
   │                  └─► Embedder::embed_documents ──► Vec<Vec<f32>> (L2 归一化)
   │                                                       │
   │                                     ┌─────────────────┴──────────────┐
   │                                     ▼                                ▼
   │                            主 HNSW（不可变）                    delta 区（暴力）
   │                                     └──────── commit() 合并 ─────────┘
   ▼
Snapshot (可选持久化)
```

### 3.3 查询路径

```
Query(raw text)
   │
   ├────────────────► Analyzer::analyze_query ──► Vec<Token>
   │                                                  │
   │                          ┌───────────────────────┴────────────┐
   │                          ▼                                    ▼
   │              [Lane A] BM25Retriever            [Lane B] VectorRetriever
   │                          │                        Embedder::embed_query
   │                          │                              ▼
   │                          │                      VectorIndex::search
   │                          ▼                              ▼
   │                   Vec<ScoredHit>                 Vec<ScoredHit>
   │                          └──────────┬───────────────────┘
   │                          （两路并行，rayon）
   │                                     ▼
   │                            FusionStrategy::fuse (RRF)
   │                                     ▼
   │                            Reranker（v1 = NoOp）
   │                                     ▼
   │                      正排回捞 ──► Vec<Hit> (含 Explain)
   │                                     ▼
   └──────────────────────────► SearchResponse (含 EmptyReason / took)
```

> **融合层不回捞正文**。融合只操作 `(chunk_id, score)`，正文回捞统一在融合与精排之后做一次，避免对候选集做无谓的字符串拷贝。

---

## 4. 模块架构

### 4.1 Cargo workspace 目录结构

```
HelixIndex/
├── Cargo.toml                 # workspace
├── crates/
│   ├── core/                  # helix-core（库，对外交付主体）
│   │   └── src/
│   │       ├── lib.rs          # 顶层 API：SearchIndex / Searcher 门面 + prelude
│   │       ├── error.rs        # Error / Result
│   │       ├── types.rs        # DocId / ChunkId / TermId / Score
│   │       ├── document.rs     # Document（输入 DTO）/ DocRecord（存储记录）/ Chunk
│   │       ├── schema.rs       # 字段定义、权重、Filter
│   │       ├── analyze/        # Analyzer trait / 中英混合分词 / 过滤器 / 停用词
│   │       ├── index/          # Index / inverted / posting / forward / stats
│   │       ├── retriever/      # Retriever trait / bm25 / vector
│   │       ├── vector/         # VectorIndex trait / hnsw / brute
│   │       ├── embed/          # Embedder trait / local / remote / cached
│   │       ├── fusion/         # FusionStrategy trait / rrf / weighted
│   │       ├── rerank/         # Reranker trait / noop
│   │       ├── query/          # SearchIndex / Searcher 门面 + search_parts 编排 + parse / explain
│   │       ├── storage/        # Snapshot 读写 / codec
│   │       └── chunk/          # Chunker 分块策略
│   └── cli/                   # helix（二进制）
├── data/                      # 30 篇 demo 语料 + T2Ranking 评测集转换产物（t2-corpus / t2-queries，见需求文档 9.4）
├── examples/
└── docs/
```

### 4.2 模块职责与边界

| 模块          | 职责            | 禁止                  |
| ----------- | ------------- | ------------------- |
| `analyze`   | 文本 → Token 序列 | 不知道索引的存在            |
| `index`     | 倒排 / 正排 / 统计量 | 不知道融合与排序            |
| `vector`    | 向量存储与近邻检索     | 不知道文本，只认 `Vec<f32>` |
| `embed`     | 文本 → 向量       | 不知道索引的存在            |
| `retriever` | 单路召回          | 不与其他 lane 交互        |
| `fusion`    | 多路合并          | 不回捞正文               |
| `query`     | 编排以上全部；门面层（`SearchIndex` 写端 / `Searcher` 读端）只做装配与生命周期管理 | 不自己实现算法             |

> 这些「禁止」是**代码评审的硬性检查项**。违反边界不会导致编译失败，但会让 trait 抽象失去意义——一旦 `fusion` 开始回捞正文，换融合策略就再也不能独立测试。

> **门面层边界（P6 新增）**：`SearchIndex` / `Searcher` 是 `query` 模块上的门面，只做装配与生命周期管理，**不实现任何算法**——`add` 内部依次调 `Chunker::chunk` → `Index::add` → `Embedder::embed_documents` → `VectorIndex::add`，不碰 postings / BM25 公式 / HNSW 图。底层 `QueryExecutor<'a>`（旧 `Searcher`）原样保留作逃生舱，见第 10 章。

### 4.3 依赖规则

```
query  ──►  fusion / rerank / retriever / index / embed / analyze
retriever ─►  index | vector | embed | analyze
fusion  ──►  (无下游依赖，纯函数)
index   ──►  analyze / storage
vector  ──►  (无下游依赖)
embed   ──►  (无下游依赖)
```

规则：**只允许上层依赖下层，禁止反向依赖与同层横向依赖**（`retriever` 的两路实现之间尤其禁止互相引用）。

---

## 5. 核心抽象设计

### 5.1 基础类型

```rust
// ---------- types.rs ----------
pub type DocId   = u32;
pub type ChunkId = u32;
pub type TermId  = u32;
pub type Score   = f32;

pub struct Document {                // 输入 DTO（对外门面 add 的参数，P6 起才含 text）
    pub text: String,                // 原始文本，由门面层分块
    pub source: String,              // 文件路径 / URL / 标题 —— 溯源用（FR-12）
    pub metadata: serde_json::Value, // 业务自定义 —— 过滤用（FR-14）
    pub dedup_key: Option<String>,   // 幂等 upsert 键（FR-15）；缺省用 xxh64(text)
}

pub struct DocRecord {               // 存储记录（内部），由 Document 派生
    pub doc_id: DocId,
    pub source: String,
    pub metadata: serde_json::Value,
    pub content_hash: u64,           // xxh64(dedup_key.unwrap_or(text))
}

pub struct Chunk {
    pub chunk_id: ChunkId,
    pub doc_id: DocId,
    pub ordinal: u32,       // 在文档内的序号，用于相邻 chunk 合并（FR-24 v2）
    pub text: String,
    pub char_start: usize,  // 原文偏移，用于精确引用（FR-12）
    pub char_end: usize,
}
```

### 5.2 Token 与 Analyzer

```rust
pub struct Token {
    pub term: SmolStr,   // 归一化后的索引词
    pub position: u32,   // 位置，供未来短语查询使用
    pub start: usize,
    pub end: usize,
}

pub trait Analyzer: Send + Sync {
    fn analyze_doc(&self, text: &str) -> Vec<Token>;
    /// 查询侧分词。默认转发 analyze_doc —— 强制两侧一致，避免最常见的坑。
    fn analyze_query(&self, text: &str) -> Vec<Token> {
        self.analyze_doc(text)
    }
}
```

> 拆成两个方法而非一个，是为了**未来允许**查询侧差异化（如同义词扩展、保留停用词）；但默认实现强制一致，从 API 层面杜绝"索引侧和查询侧分词不一致"这类隐蔽 bug。对应风险 R4。

### 5.3 Embedder

```rust
pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize;
    /// 入库侧。BGE 系列不加前缀。
    fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    /// 查询侧。BGE 系列**必须**加 instruction 前缀。
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
    /// 输出是否已 L2 归一化
    fn is_normalized(&self) -> bool { true }
}
```

> `embed_documents` 与 `embed_query` **必须分开**。BGE 中文模型要求查询侧加前缀 `为这个句子生成表示以用于检索相关文章：`，入库侧不加。这一步漏掉，向量检索效果会明显下降——**是最容易踩也最难自查的坑**（代码能跑、分数看着也正常）。对应风险 R2。

### 5.4 VectorIndex

> **V2 Step 1 演进**（ADR-010）：新增 `search_filtered`，把过滤从"检索后 post-filter"
> 下推到 ANN 内部。详细推导见 `docs/devel/v2-step1-design.md` §3 / §5.7。
>
> **V2 Step 5 演进**（**已完成并合并**：实现 PR #36 → `48c0ac7`、收尾 PR #37 → `590315d`；D-S5-01，`v2-step5-design.md` §4.2.1）：
> 新增**必选**方法 `search_exact_filtered` + 默认钩子 `prefers_exact`（默认 `false`）。
> **精确性由类型表达**——策略归后端、分派与记账归编排层（`Metrics.vector_route`，D-S5-07），
> 好处是**零 plumbing**：门面 `Searcher` / 逃生舱 `QueryExecutor` / bench 三条入口自动受益。
> 代价是**对外破坏性变更**（外部实现要补一个方法）⇒ 见 **R35**。

```rust
pub trait VectorIndex: Send + Sync {
    /// 增量插入一条向量（两个实现均支持）
    fn add(&mut self, id: ChunkId, vec: NormalizedVector) -> Result<()>;

    /// 检索最近的 k 条**且通过 `filter` 的**候选，返回 `(chunk_id, distance)`，按距离**升序**。
    fn search_filtered(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<(ChunkId, f32)>>;

    /// 不带过滤的检索（默认转发到 `search_filtered`，`None` 语义见下）
    fn search(&self, query: &NormalizedVector, k: usize) -> Result<Vec<(ChunkId, f32)>> {
        self.search_filtered(query, k, None)
    }

    /// 【V2 Step 5 新增，**必选**】精确的过滤检索：返回**全部**满足 `filter` 的最近 k 条
    /// ⇒ **返回条数恒为 `min(k, 命中数)`**（这是与 `search_filtered` 的**唯一**语义差异）。
    /// 代价是 `O(N)` ⇒ 调用方**须先问 `prefers_exact`**。
    fn search_exact_filtered(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<(ChunkId, f32)>>;

    /// 【V2 Step 5 新增】本后端在**该谓词**下是否应走精确路径（默认 `false` = 零回归）
    fn prefers_exact(&self, _filter: &dyn CandidateFilter) -> bool {
        false
    }

    /// 已入库向量条数（**含已软删除的**；与存活数无关）
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool { self.len() == 0 }
}
```

- `distance` 是**平方欧氏距离**（越小越近）；转相似度由上层负责（见 7.3）
- `filter = Some(f)`：返回的**每一条都必须满足 `f.contains(id)`**。允许返回少于 k 条（过滤后不足），
  **不允许**用未通过过滤的候选凑数
- ⚠️ **`filter = None` = 不过滤，结果可能包含已软删除的 chunk**。这是逃生舱路径的**已知契约不是 bug**：
  `hnsw_rs` 无法从图中物理摘除向量，存活过滤必须由编排层注入谓词完成（`predicate` 模块文档 + T17 断言）
- ⚠️ **`vector_shortfall == 0` 不再等于「无 prefilter 需求」**：走了 `Exact` 的档位它**通常**为 0，
  但**不是恒等式**——`allowed` 来自 `Index`、扫描枚举的是**图里的点**，图滞后于索引时该值仍 > 0
  （那时它反过来是「图未覆盖全部 allowed」的诊断信号）。**两种读数都必须连看**
  `Metrics.vector_route`（D-S5-07；设计文档 §4.4）

#### 5.4.1 软删除：存活状态的单一真源在 `Index`，不在 `VectorIndex`

Q-C1（删除后向量残留霸占 Top-K 名额）的修复。可选方案是把 `tombstone` 加进 `VectorIndex`，
但被否决——**双写必漂移**，且 `load` 时 `raw_vectors` 全量重灌，向量侧状态无法从快照恢复。

因此定案（D-S1-01）：

- 存活位图（`bitmap::ChunkBits`）与 `chunks` 的 `Some`/`None` 在 **`Index::forward` 的同一函数内**维护
  （`insert_chunk` 置位、`tombstone_chunk` 清位），快照 `import` 时 `rebuild`
- 检索时以**谓词**（`predicate::AliveOnly`）传给向量路，向量索引自身不持有存活状态
- `SearchIndex::remove` 额外摘除 `raw_vectors`，与存活位图构成**跨快照永续的两道防线**（T2 刻意绕过防线①验证防线②）

#### 5.4.2 HnswRs 的双路径与 `ef` 策略

`hnsw_rs::search_filter` 有三个反直觉的结构性行为（源码逐行核实，见设计 §3）：

1. **带 filter 时从不 fast-return**，唯一正常终止是候选耗尽
2. **堆未满时距离剪枝全程关闭** ⇒ `allowed < ef` 时**必然整图遍历**（库层机制，调参治不了）
3. `ef = ef_arg.max(knbn)` ⇒ **`knbn` 才是输出条数旋钮，`ef` 只是搜索宽度**

由此定案双路径：

| 路径 | 场景 | 实现 | `ef` | `knbn` |
| --- | --- | --- | --- | --- |
| **A** | 无用户过滤（热路径） | 普通 `search()` + 返回后 `retain(alive)` | `200`（`EF_SEARCH`） | `min(len, ceil(k/alive_ratio)+k).min(1024)` 按删除比例过采样 |
| **B** | 有用户过滤 | `search_filter` + `FilterT` 适配器 | `min(max(4k,k), 256)`（纯宽度） | `k` |

路径 A **必须**走普通 `search()`：用 `search_filter` 跑 AliveOnly 会禁用 fast-return，
改变热路径的成本结构（P99 敏感，T14① 强制验证）。

> ⚠️ 不要用 `ef.min(allowed)` 夹逼——`allowed < ef` 时堆恒填不满、剪枝本就关闭，该 `min` 不生效；
> `allowed ≥ 4k` 时它又不生效。**低选择度下的整图遍历是已知代价，宁可召回不足并用
> `Metrics::vector_shortfall` 暴露，也不假装能调参解决**（R13）。

#### 5.4.3 图持久化：`VectorGraphPersist`（V2 Step 2 / ADR-A，FR-29）

图持久化**没有**放进 `VectorIndex` 契约，而是独立 trait，因为它只对「有图的后端」有意义：

```rust
pub trait VectorGraphPersist: VectorIndex {
    /// 把图 dump 到 `<base>.hnsw.graph` / `<base>.hnsw.data`，返回统计
    fn dump_graph(&self, base: &Path) -> Result<GraphStats>;
    /// 从 sidecar 加载图（base = 快照文件名全名，ef_search 由调用方带入）
    fn load_graph(base: &Path, m: &GraphManifest, ef_search: usize) -> Result<Self>;
}
```

`BruteForceIndex` **不实现**它 ⇒ `VectorIndex::as_graph_persist()` 默认返回 `None`（P0-4），
由类型事实兜住「无图后端要求持久化」的分派缺口，而不是靠运行时 `match` 漏掉。

**三条结构性约束（源码级核实，写错即静默降级）**：

1. **basename 铁律**：`file_dump(dir, basename)` 与 `HnswIo::new(dir, basename)` 会**自行追加**
   `.hnsw.graph` / `.hnsw.data`。要得到 `foo.idx.hnsw.graph`，传入的 `basename` 必须是
   **`"foo.idx"`**（快照文件名全名）。拼成 `"foo.idx.hnsw"` 会落出 `foo.idx.hnsw.hnsw.graph`——
   加载时找不到、静默走重建，所有「能加载」的断言依然绿。**统一走 `graph_basename()`**，
   且测试必须先断言 `GraphStatus::Loaded`。
2. **`ef_search` 是入参，不是字段**：`hnsw_rs` 全 crate 无 `set_ef*`，`ef` 只能逐次作 search 参数传。
   故 `HnswRsIndex` 自己持有 `ef_search` 字段，并由 `from_loaded()` 构造器带入。
3. **`HnswIo` 必须比 `Hnsw` 活得长**（`load_hnsw*` 的 `'a: 'b`）⇒ 只能 `Box::leak`，
   且**必须丢弃返回的句柄**（泄漏是刻意的、有界的：每进程每索引一次）。
   用编译期断言 `const _: () = assert_send_sync::<HnswIo>();` 把 `Send + Sync` 前提钉住。

4. **平台指纹必须按构建目标派生**（C4；评审 #12 发现 1）：`hnsw_rs` 落盘用
   `to_ne_bytes()`（原生端序）+ `from_raw_parts` 裸拷贝 f32，图文件本身是 native-endian。
   若 manifest 的 `platform` 字段写死常量，大端 / 32-bit 构建**同样写 0x01、同样接受 0x01**
   ⇒ CRC 全对、platform 相符，却加载出字节序错误的向量，得到**静默错误的结果**。
   实现为 `PLATFORM_FINGERPRINT = if cfg!(target_endian = "little") && cfg!(target_pointer_width = "64") { LE64 } else { OTHER }`。

详见 7.6.2 节的文件布局与发布协议。

### 5.5 Retriever

> **V2 Step 1 演进**（ADR-010）：`RetrieveRequest` 的过滤载体从 `Option<&Filter>`（领域对象）
> 换成 `Option<&dyn CandidateFilter>`（谓词 trait）。BM25 侧因此不再自己遍历 postings 判过滤。

```rust
pub struct RetrieveRequest<'a> {
    pub query: &'a AnalyzedQuery,
    pub k: usize,
    /// **谓词**而非领域对象：由编排层求值一次后复用（见 5.5.1）
    pub filter: Option<&'a dyn CandidateFilter>,
}

pub struct ScoredHit {
    pub chunk_id: ChunkId,
    pub score: Score,
}

pub trait Retriever: Send + Sync {
    fn name(&self) -> &'static str;
    fn retrieve(&self, req: &RetrieveRequest) -> Result<Vec<ScoredHit>>;
}
```

`name()` 用于 `Explain` 中标识来源 lane，是 FR-13 可解释性的基础。

`filter` 的契约与 `VectorIndex::search_filtered` **完全一致**（含 `None = 不过滤` 的逃生舱语义），
两路共用 `predicate` 模块，避免「同一份过滤在两个 lane 里含义不同」。

#### 5.5.1 为什么换成谓词（Q-I1/I2）

V1 的 `Option<&Filter>` 让 BM25 侧对每个候选 chunk 调 `Filter::matches(metadata)`，
复杂度 **O(语料规模)**——每 query 都要全扫一遍 metadata，与命中多少无关。

改成谓词后，求值被提到编排层**一次**完成（`query::filter::doc_bits`）：

- 字段索引（`index::field_index`）把 `field → value → doc 位图` 预先建好，等值/range 查询
  退化为几次位图交并，成本与语料规模**解耦**
- 位图再包成惰性 `ChunkFilter`，`contains(chunk_id)` 是 O(1) 位测试（内部经 `doc_of` 映射到 doc 位）
- 结果：`allowed_chunks`（旧，全扫 + HashSet）→ `doc_bits + 惰性谓词`（新），T13 实测对照见设计文档 §10

⚠️ **已知短板**：字段索引有基数保护（`terms + numbers` 合计 1024），**高基数字段会永久降级**，
该字段上的过滤回落到 O(N) 全扫。毫秒时间戳 / 雪花 ID 这类最常见的真实过滤场景**约 512 篇**就撞线
（数值字段同时登记 terms 键与 numbers 键，各占一份额度）。这是 V2.1 prefilter / 排序列方案的**第一用例**。

### 5.6 FusionStrategy

```rust
// ⚠️ 本节原为**设计期伪码**；2026-09-23 按**实现**更正（V2 Step 9 / 设计 §6.4）：
//    原写「`weights` 是 `fuse` 的形参」+ 类型名 `ScoredHit` —— 实际**权重在构造期注入**
//    （`fusion/rrf.rs:27-30`）、类型是 `LaneResults = Vec<(ChunkId, Score)>`、且 `fuse` 有 `k` 形参。
//    原伪码**全文引文**在 `v2-step9-design.md` §2.4 **N2**（本项目纪律：被推翻的结论不静默删除）。
pub trait FusionStrategy: Send + Sync {
    fn name(&self) -> &'static str;

    /// `lanes` 按路序给出（`bm25` 在前、`vector` 在后）；`k` = 输出上限（实际传 `candidate_k`）。
    fn fuse(&self, lanes: &[LaneResults], k: usize) -> Vec<(ChunkId, Score)>;

    /// 【V2 Step 9 / FR-35 新增，**provided**】按 query 侧信号**覆盖本次权重**。
    ///
    /// 默认实现返回 `None` ⇒ 沿用策略自身权重 ⇒ **既有实现一行不改**、**逐位一致**
    /// （先例：`Reranker::candidate_window`，§5.7）。
    ///
    /// ⚠️ 返回 `Some(w)` 时 `w` 必须与 `lanes` **一一对应**（**含空 lane 的槽位**）——
    /// 剔路只能**置 0**、**不得删槽**，否则权重会串到另一路（`fuse` 内按 `lane_idx` 取权重）。
    fn weights_override(&self, _ctx: &FusionCtx<'_>) -> Option<Vec<f32>> {
        None
    }

    /// 【V2 Step 9 / FR-35 新增，**provided**】上一条的**应用通道**（**闭合机制**）。
    ///
    /// 默认实现 = `self.fuse(lanes, k)` ⇒ **既有实现一行不改**、**默认逐位一致**。
    /// `RrfFusion` 覆写：先 `weights_override(ctx)` 决策 → `Some(w)` 则以 `w` 跑 RRF
    /// （抽一个**带权重的私有 helper**，与 `fuse` 共用同一段数学）→ `None` 则退回 `fuse`。
    ///
    /// 🔴 **为什么必须多这一个方法**（设计第 2 轮评审 **P3-1**）：只加 `weights_override` 时，
    /// 「`Some(w)` 时**用 `w`**」这句话**没有落点** —— 编排层只持 `&dyn FusionStrategy`、
    /// 而 `fuse` 读的是策略**私有**字段 ⇒ 编排层**消费不了** `w`。四条替代路全部堵死
    /// （改 `fuse` 签名 = 破坏性 / 每查询重建策略 = 编排层做不到 / 策略内部可变性 =
    /// 破坏 `Send + Sync` 与并发逐位一致 / 编排层自己重算 RRF = 绕开抽象、对
    /// `WeightedFusion` 是错的）。
    /// ⚠️ 与 §5.7 的 `candidate_window` 先例的**差别**：后者返回值（`usize`）编排层
    /// **自己能消费**（进 `candidate_k`）⇒ 一个方法即闭环；`weights_override` 的返回值
    /// 编排层消费不了 ⇒ 必须再补一个「消费方」。
    fn fuse_adaptive(
        &self,
        lanes: &[LaneResults],
        k: usize,
        _ctx: &FusionCtx<'_>,
    ) -> Vec<(ChunkId, Score)> {
        self.fuse(lanes, k)
    }
}
```

> ⚠️ **2026-09-24 补 `fuse_adaptive`**（设计第 2 轮评审 **P3-1**）：`weights_override` 只解决「**该用什么权重**」，`fuse_adaptive` 解决「**这个权重怎么进计算**」 —— 两者是**决策 / 应用**的两半，缺一不闭合。
> ⚠️ **本节 2026-09-23 按实现更正**（V2 Step 9 / 设计 §6.4）：原伪码把 `weights` 写成 `fuse` 的形参、用了**不存在的**类型名 `ScoredHit`、且漏了 `k` 形参 —— 三处都与 `crates/core/src/fusion/mod.rs` 不符。

### 5.7 Reranker（FR-18；V2 Step 1 留位 → **V2 Step 7 接入，设计已出、待评审**）

```rust
pub trait Reranker: Send + Sync {
    /// 【V2 Step 7 新增，provided】精排**希望拿到**的候选条数（供编排层在召回**之前**决定候选池）。
    ///
    /// 语义：返回 `w` 表示「请把融合结果的前 `max(k, w)` 条交给我」。**默认实现返回 `k`**
    /// —— 即**不放开窗口**：既有实现（`NoOpReranker` 与下游自定义实现）**一行不改**，
    /// 且未装精排器时 `candidate_k` / 截断 / `hits` 与 Step 6 **逐位一致**。
    ///
    /// ⚠️ 上层会与 `candidate_k = max(3k, w, 10)`（`query/searcher.rs` 的 `candidate_k` 计算处）**联动**：
    /// 返回 `w > 3k` 时候选池随之放大，否则窗口被**静默封顶**（见 §14.5 R45）。
    fn candidate_window(&self, k: usize) -> usize { k }

    /// ⚠️ 【V2 Step 7 契约收紧】**不得依赖** `hits[i].explain`——`matched_terms` 与 lane
    /// rank/score 在精排**之后**才填（编排层把 `explain` 组装推迟到最终 ≤ k 条，D-S7-06），
    /// 入参里可能**只有** `fused_score`；需要正文请用 `hits[i].text`。
    fn rerank(&self, query: &str, hits: Vec<Hit>, top_n: usize) -> Result<Vec<Hit>>;
}
```

**不变式（V2 Step 7，写入 rustdoc，三条）**：

1. `take_n = min(max(k, candidate_window(k)), fused.len())`，且 `candidate_k = max(3k, candidate_window(k), 10) ≥ take_n` ⇒ 窗口**不会被候选池静默封顶**；
2. `Reranker::rerank` **不得依赖** `hits[i].explain`（`explain` 在精排**之后**才组装，D-S7-06）；
3. 精排生效时 `hits` 按 `score` **严格不增**、并列按 `chunk_id` **升序**（D-S7-07）；且 `hit.explain.rerank_score.is_some()` 是「`score` 已被精排替换」的**唯一**信号（D-S7-05）。

**实现（V2 Step 7）**：`NoOpReranker` **无改动**（`rerank/noop.rs`，`take(top_n)` 直通，仍是默认与逃生舱）；**新增 `LocalReranker`**（`rerank/local.rs`，gate **`feature = "local-rerank"`**）——`Mutex<TextRerank>` + `window` + `max_length`，模型 = fastembed `RerankerModel::BGERerankerV2M3` ⇒ **`rozgo/bge-reranker-v2-m3`（⚠️ **非** BAAI 官方；官方库**没有 ONNX 导出**）**，`model.onnx.data` **≈2.19 GB FP32**（内存后果见 §14.5 **R44**）；`id()` 返回 **`String`**（**不是** `&'static str`——`max_length` / `window` 属运行期参数）。⚠️ **未启用 feature 时 `LocalReranker` 编译期不存在**（不是运行时静默退化，同 Step 6 D-S6-05 的硬失败纪律）。**不进 `ConfigFingerprint`**（精排不改索引内容，D-S7-10）；默认窗口 **`R = 20`（「拟」值，由 S7-04 标定回填）**。

> 详细设计见 `docs/devel/v2-step7-design.md`（**v0.1**，待评审）；风险见 **§14.5 R44~R48**。

### 5.8 返回类型

```rust
pub struct Hit {
    pub chunk_id: ChunkId,
    pub doc_id: DocId,
    pub score: Score,
    pub text: String,
    pub source: String,
    pub metadata: serde_json::Value,
    pub explain: Explain,
}

impl Hit {
    /// 直接产出可拼进 prompt 的上下文块，带出处标注（FR-12）
    pub fn to_context_block(&self) -> String { /* ... */ }
}

pub struct Explain {
    pub matched_terms: Vec<String>,
    pub bm25_score:  Option<Score>,
    pub bm25_rank:   Option<u32>,
    pub vector_score: Option<Score>,
    pub vector_rank:  Option<u32>,
    pub fused_score: Score,
    /// 【V2 Step 7 新增，**破坏性**】精排生效时为**模型原始 logit**；
    /// `None` ⟺ 本次 `score` 是**融合分**（D-S7-05）。
    /// **这是「`score` 已被精排替换」的唯一信号**（NFR-07 的「不得静默」，见 §14.5 R46）。
    pub rerank_score: Option<Score>,
}

pub struct SearchResponse {
    pub hits: Vec<Hit>,
    pub total_candidates: usize,
    /// 空结果时说明原因，供 Agent 决策（FR-13）
    pub empty_reason: Option<EmptyReason>,
    pub took: Duration,
    /// 【V2 Step 5 新增，**破坏性**】内核观测指标（D-S5-05）——外部单测 / bench 采集的唯一入口
    pub metrics: Metrics,
}

pub enum EmptyReason {
    NoDocuments,        // 索引为空
    AllTermsUnmatched,  // 词全部未命中 —— 提示 query 可能含幻觉词
    FilteredOut,        // 有候选但被 filter 全部过滤 —— 提示放宽 scope
}
```

> `bm25_rank` / `vector_rank` 用 `Option` 而非默认值：`None` 明确表示"该 lane 未召回此文档"，这正是 4.5 节所说的诊断信号。用 0 或 `usize::MAX` 会丢失这层语义。
>
> 【**V2 Step 5**，设计已定稿、实现未开工】新增 `metrics: Metrics` 是**对外破坏性变更**（字段全 `pub`
> ⇒ 外部字面量构造会编译失败，见 **R35**）。不变式：`metrics.took == took`（口径自洽），
> 且 `metrics.vector_route` 是「兜底是否真的生效」的唯一直接判据（D-S5-07）。
>
> 【**V2 Step 7**，设计已出、待评审】`Explain` 新增 **`rerank_score: Option<Score>`**（**对外破坏性**，与 `metrics` 同属 **R35** 一族）+ `Metrics` 新增 **`rerank_elapsed: Duration` / `rerank_window: usize`**（**纯加法**，保持 `Copy`）。⚠️ **`Hit.score` 的语义随精排开关变化**（D-S7-05）：精排生效时 `= σ(logit) ∈ (0,1)`——**单调变换 ⇒ 排序信息零损失**；融合分**不丢**（仍在 `Explain.fused_score`）、原始 logit 进 `Explain.rerank_score`。`rerank_window == k` ⟺ 窗口**没被放开**（放开是否真的生效的直接证据）。**不进 `ConfigFingerprint`**（精排不改索引内容，D-S7-10）。

---

## 6. 数据结构设计

### 6.1 倒排索引

```rust
pub struct InvertedIndex {
    term_dict:   HashMap<String, TermId>,  // Term → TermId
    terms:       Vec<String>,              // TermId → Term（反查，快照与调试用）
    postings:    Vec<PostingList>,         // TermId → posting 列表
    df:          Vec<u32>,                 // TermId → document frequency
}

pub struct PostingList {
    chunks: Vec<Posting>,   // 按 chunk_id 升序
}

pub struct Posting {
    chunk_id:  ChunkId,
    term_freq: u32,
    positions: Vec<u32>,    // MVP 即存储，供未来短语查询；feature = "positions" 控制
}
```

**设计说明**

- **term dict 与 postings 分离**：`HashMap<String, TermId>` + `Vec<PostingList>`，而不是 `HashMap<String, Vec<Posting>>`。后者在序列化时每个 key 都要重复存字符串，前者只需存一次字符串表，快照体积与加载速度都更优。
- **MVP 就存 positions**：虽然第一版不用，但**事后补需要重建全部索引**。存储成本可接受，收益是未来加短语查询零迁移成本。
- **MVP 不压缩**：posting 用朴素 `Vec<Posting>`。varint / roaring bitmap 列为 P4 优化项——过早优化会掩盖算法本身的正确性验证。

### 6.2 正排存储

```rust
pub struct ForwardStore {
    chunks:   Vec<Chunk>,          // chunk_id 即下标，O(1) 回捞
    docs:     HashMap<DocId, Document>,
    deleted:  RoaringBitmap,       // 墓碑标记
    by_source: HashMap<String, DocId>,  // 幂等 upsert 用（FR-15）
    by_hash:   HashMap<u64, DocId>,     // 幂等 upsert 用（FR-15）
}
```

### 6.3 统计量维护（增量友好，FR-17）

```rust
pub struct IndexStats {
    num_chunks: usize,  // 有效 chunk 数（已扣除墓碑）
    total_len:  u64,    // 所有 chunk 的 term 数总和
}
// avgdl = total_len as f32 / num_chunks as f32
```

**增量维护而非重算**

- 插入 chunk：对每个 term 的 `df += 1`（仅当该 term 首次出现于此 chunk），`total_len += dl`，`num_chunks += 1`
- 删除 chunk：从正排取回原文，**重新分析一次**得到 term 列表，逐个 `df -= 1`，再减 `total_len` 与 `num_chunks`

> 删除时重新分析而非维护反向映射，是用 CPU 换内存：省掉一份 `chunk → terms` 的常驻映射（百万级规模下这是显著开销），而删除是低频操作。对应风险 R5，必须由单测兜住。

### 6.4 向量存储（FR-08 / FR-17）

```rust
pub struct VectorStore {
    sealed: Option<HnswMap<NormalizedVector, ChunkId>>,  // 主 HNSW，不可变
    delta:  Vec<(ChunkId, NormalizedVector)>,            // 未建图的新向量
}
```

详见 7.5 节。

---

## 7. 关键算法设计

### 7.1 中英混合分词（FR-02）

**处理流程**

```
原始文本
  → 按 Unicode 属性切分为「中文段」与「拉丁段」
  → 中文段：jieba-rs 分词
  → 拉丁段：正则切词（[A-Za-z0-9]+）
  → Token 过滤器链：小写化 → 停用词过滤 → 英文词干化（可选）→ 长度过滤
  → Vec<Token>
```

**决策点**

| 项      | 决策                                    | 理由                                                                     |
| ------ | ------------------------------------- | ---------------------------------------------------------------------- |
| 中文分词   | jieba-rs                              | 生态成熟、词典可定制                                                             |
| 中英分段   | `unicode-segmentation`                | **不手写 Unicode 边界判断**：全角符号、假名、emoji 的边界自己判极易出错，一个轻量纯 Rust 依赖换正确性          |
| 文本归一化  | `unicode-normalization`（NFC）          | 避免"é"与"e+́"被当成两个 term，导致索引侧与查询侧不一致                                  |
| 英文词干化  | **第一版不做**       | 词干化（rust-stemmers）会损害专有名词与术语的精确匹配；检索场景宁可牺牲一点召回换精确率。留作可插拔 filter        |
| 停用词    | 中英文合并词表，保守裁剪    | 只去掉真正无意义的虚词。⚠️ 注意"的/了"这类词**不能**在向量路去掉——向量路用的是原文，不受影响；但 BM25 路去掉可显著减小索引 |
| 单字词    | 保留              | 中文单字词（"云""网""库"）常是关键术语                                                 |
| 查询侧一致性 | 强制同一 `Analyzer` | 通过默认实现 `analyze_query → analyze_doc` 保证（见 5.2）                          |

### 7.2 BM25（FR-04）

**公式**

```
score(D, Q) = Σ_{t ∈ Q}  idf(t) · (tf(t,D) · (k1 + 1)) / (tf(t,D) + k1 · (1 - b + b · dl(D)/avgdl))

idf(t) = ln(1 + (N - df(t) + 0.5) / (df(t) + 0.5))
```

**参数**：`k1 = 1.2`，`b = 0.75`（Lucene 默认值，作为起点）

> ⚠️ **中文场景的重要差异**：`dl` 是 chunk 的 **term 数**。中文分词后 term 数显著少于英文（同样一段话，英文按空格切出 200 词，中文分词后可能只有 120 词），导致 `avgdl` 系统性地偏小，`b` 的作用被放大。**1.2 / 0.75 只能作为起点，必须在 P5 阶段用评测集网格搜索重新调参，不能盲信英文经验值。** 对应风险 R6。

**检索语义**：OR（FR-04）。query 中未命中的 term 贡献 0 分，其余 term 正常累加。

**执行方式**：TAAT（Term-At-A-Time），理由见 ADR-006。

### 7.3 向量检索与相似度对齐（FR-09）

**核心矛盾**：`instant-distance` 的 `Point::distance()` 语义是**距离（越小越相似）**，而业务层要的是**相似度（越大越相似）**，且 BGE 模型用的是余弦相似度。

**解法：入库前统一 L2 归一化**

归一化后，欧氏距离 `d` 与余弦相似度 `cos` 满足：

```
d² = |u - v|² = |u|² + |v|² - 2·u·v = 2 - 2·cos(u,v)
⟹ cos = 1 - d² / 2
```

由于 `d ≥ 0` 时 `cos` 关于 `d` **单调递减**，按 `d` 升序排列与按 `cos` 降序排列**完全等价**——排序结果无需转换即可使用，仅在需要展示真实余弦值时做一次换算即可。

附带好处：归一化后**内积等于余弦**，为未来换用内积索引（如 Faiss 的 `IndexFlatIP`）留好前提。

```rust
#[derive(Clone)]
pub struct NormalizedVector(pub Vec<f32>);

impl instant_distance::Point for NormalizedVector {
    fn distance(&self, other: &Self) -> f32 {
        self.0.iter()
            .zip(other.0.iter())
            .map(|(a, b)| { let d = a - b; d * d })
            .sum::<f32>()
            .sqrt()
    }
}
```

**模型选择**：`BAAI/bge-small-zh-v1.5`，512 维。理由：中文效果与体积平衡（ONNX 约 90MB），CPU 推理速度可接受。备选 `BAAI/bge-m3`（1024 维、多语言、同时支持稀疏与稠密），在需要多语言或稀疏检索时切换。

### 7.4 混合融合（FR-10 / FR-11）

**为什么默认 RRF 而不是加权归一**

两路分数的**量纲与分布完全不同**：

|       | 取值范围                                  | 分布特征                  |
| ----- | ------------------------------------- | --------------------- |
| BM25  | [0, +∞) 无上界，中文常见 5~30                 | 长尾、随 query term 数线性增长 |
| 余弦相似度 | [0, 1]，中文 embedding 实际集中在 [0.6, 0.95] | 极度集中、区分度低             |

这种不可比性使归一化**既损失信息又不稳定**：

- **min-max 归一化**：Top-K 内最大值一变，其余所有分数全部改变；且只有一个候选时分母为 0
- **除以 Top-1 分数**：比 min-max 稳定，但 Top-1 分数本身随 query 波动

RRF **只使用排名，完全免疫量纲问题**：

```
score(d) = Σ_{lane i}  w_i / (k + rank_i(d))
```

- `k = 60`（Cormack et al. 2009 原论文推荐值，empirically robust）
- `rank` 从 1 开始；某 lane 未命中该文档则不贡献
- 默认 `w_bm25 = w_vector = 1.0`

**FR-11 加权归一作为可选策略**：若业务确实需要，采用「除以该 lane Top-1 分数」而非 min-max，并在单候选时退化为 1.0。

### 7.5 增量写入与软删除（FR-17 / FR-26）

> **V2 Step 1 重写**：本节原描述 `instant-distance + delta 区` 方案。该方案已在 **D8 决策**中
> 彻底移除（依赖 + `hnsw.rs` + `ImmutableIndex` 变体一并删除，生产路径切 `hnsw_rs`），
> delta 区设计随之取消（T4-06 不再存在）。以下为当前实现。

**选型**：`hnsw_rs` 0.3.4（MIT/Apache-2.0，纯 Rust），依据 ADR-002 记录的 P4 A/B 结论 ——
召回同为 0.970，但原生增量 **1.5ms/条** vs instant-distance 全量重建 **31s/万条**。
它原生支持 `insert(&self, (&[f32], usize))`，构造后可随时单条插入，**无需 delta 区与后台重建**。

#### 7.5.1 软删除的三条路径

`hnsw_rs` 与 `instant-distance` 都**没有 remove API**，删除只能靠墓碑。V2 Step 1 之前只在正排侧
打墓碑（`chunks[id] = None`），向量图里仍留着向量 —— 它会**继续霸占 Top-K 名额**，表现为
「删了 20 篇后 Top-5 只剩 2 条」（Q-C1）。修复后三条路径的一致性维护：

| 路径 | 维护点 | 检索期作用 |
| --- | --- | --- |
| 正排 `chunks: Vec<Option<Chunk>>` | `Index::tombstone_chunk` | 文本回捞时 `None` 直接跳过 |
| 存活位图 `bitmap::ChunkBits` | **同一函数内**紧邻 `chunks` 写入 | 存活状态**单一真源**，检索时以谓词下推到两路 |
| 倒排 postings | 物理摘除 | BM25 路因此**无需**存活过滤，编排层传 `None` |

⚠️ **为什么不给 `VectorIndex` 加 `tombstone`**：双写必漂移；且 `load` 时 `raw_vectors` 全量重灌，
向量侧状态**无法从快照恢复**。所以存活状态只存在 `Index` 一处，检索时以谓词注入（D-S1-01）。

#### 7.5.2 跨快照永续的两道防线（T2 集成测试覆盖）

1. `SearchIndex::remove` 主动 `raw_vectors.retain(...)` 摘除被删文档 → 落盘快照里没有幽灵向量
2. 即便快照里带着幽灵向量（T2 刻意**绕过防线①**、直接走 `storage::save`），`load` 后 `import`
   重建存活位图，检索期谓词仍会滤掉它

**已知代价**：`remove` 每次全量 `raw_vectors.retain`，批量删 N 篇是 O(N·M)。暂不处理 ——
物理回收归 **Step 4 compaction**（2026-09-07 重排前为 Step 5，映射见 `plan-v2.md` §附-2），届时可一并引入 `remove_many(&[DocId])` 批量入口。⚠️ compaction 重建图后**必须重发 manifest**（`nb_point`/`graph_crc` 全变），否则新图永远匹配不上、每次冷启动都走降级重建。

> 若后续需要**物理删除**（而非墓碑），`hnsw_rs` 与 `instant-distance` 都不满足，路径为 `usearch`
> （Apache-2.0，支持 `add` / `remove` / `filtered_search` / `exact_search`，但经 `cxx` 引入 C++ 依赖）；
> `arroy`（LMDB 后端）因与"内存 + 快照"路线冲突而排除。

**P4 A/B 已定案**：结论与 `DistDot` 的归一化语义见 **ADR-002**。补充两点实现层面的坑：

- ⚠️ `hnsw_rs` 的 `DistDot` 在 aarch64 上有 `assert!(dot <= 1.0)` 浮点断言，自匹配时 `dot = 1.0000002`
  会 panic → 本项目使用自定义的 `DistDotClamped`
- ⚠️ crate 名是 **`hnsw_rs`**（下划线）

#### 7.5.3 墓碑物理回收（compaction，FR-30）

> **V2 Step 4 新增**。§7.5.1 / §7.5.2 解决的是「已删文档不占 Top-K 名额」（**FR-26 正确性**），
> 但墓碑**只是被跳过、从未被移除**——磁盘与图里的死数据只增不减。本节解决**长期膨胀**。

**膨胀的三条路径与量级**（逐行盘点见 `v2-step4-design.md` §2.1）：

| 路径 | 增长机制 | 量级 |
| --- | --- | --- |
| 图 sidecar（`.hnsw.graph` / `.hnsw.data`） | `hnsw_rs` **无 remove**，墓碑点随每次 `save` 的 `file_dump` **重复累积** | **≈2.6KB/点**（大头） |
| 快照正文（`.idx`） | 正排 `chunks` 的 `None` 槽位 + `chunk_lens` 是 push-only（`remove` 不回收） | ≈6B/chunk |
| `raw_vectors` | `remove` 的 `retain` 本已摘净；但 `flush` 无 liveness 检查时会灌入幽灵向量（**S4-01 已堵**） | dim×4B/条 |

> 图与正文约 **440:1** —— 治理重点在图；但**不重编号则正文空洞无法消除**（`Vec<Option<_>>`
> 的 `None` 必须靠稠密化挤掉），这是 D-S4-01 选「重编号」而非「原地保留 id」的根本理由。

**方案：按存活集重新物化 + ID 重编号**（D-S4-01）。一次 `compact()` 六步：正排稠密化并生成
`IdRemap` → 倒排 remap + 死词摘除 + `chunk_lens` / `content_hashes` / 字段索引同步 →
`raw_vectors` 过滤 + remap → 向量索引重建 → 三者**一起原子替换**（I5：失败不留半压实）。

两个关键约束：

- ⚠️ **步骤 0 必须先 `commit()`**（D-S4-10，是 `compact()` 的第一行）：`add` 在入 `pending`
  **之前**就分配了 `chunk_id`，不先 flush 的话 `pending` 里仍是旧 id，紧随的 `save` → `flush`
  会把 stale id 灌进 `raw_vectors` 与新图。不变式 **I8**：`compact()` 返回时 `pending` 必为空。
- ⚠️ **ID 会被重编号**（D-S4-01 ⇒ D-S4-09 由「可选」升为**必做**）：跨 compaction 的持久
  引用请用 `source` / `content_hash`，**不要用 `doc_id` / `chunk_id`**。已三处声明：
  `CompactionReport.remapped` 字段、`SearchIndex::compact` 的 rustdoc、`user-guide.md` §1.5。
  无墓碑时**早退且不重编号**（`remapped=false`），避免白重建整张图。

**BM25 为什么不受影响**（I3 逐位一致）：BM25 排序是 `(score 降序, chunk_id 升序)` 的**全序**
（`retriever/bm25.rs`），而 `IdRemap` 对存活集**单调** ⇒ 重编号不改变任何一对的相对顺序。

**落盘与崩溃一致性**：落盘入口**唯一** = `compact_and_save(path)` = `compact()` + 既有 `save()`。
复用 Step 3 的 `atomic_write`（**不引入新的一致性机制**），`FORMAT_VERSION` 保持 2；
§7.5.2 的铁律「重建图后必须重发 manifest」由这条**既有 `save` 链路自动满足**，compaction 不
另开落盘路径。`compact()` 本身是**纯内存**操作，其 `bytes_before` / `bytes_after` 恒为 `None`
（磁盘此时未变，填任何值都是撒谎）。

**触发策略**（D-S4-02）：**手动为主**——`helix compact`（`--dry-run` / `--output` / `--json`）；
`save()` 在墓碑占比 ≥20% 且总量 ≥1024 时只打 `[提示]` 告警，**自动 compaction 默认关**
（把 10~100s 的重建塞进 `save()` 会让写路径耗时不可预测）。

**可观测**：`tombstone_stats()` → `TombstoneStats`；`compact*` → `CompactionReport`
（`before` / `after`、三体积 `bytes_before` / `bytes_after`、`reclaimed_chunks` / `docs` /
`terms` / `graph_points`、`remapped`、`graph_status`、耗时）。

**失败语义**：`save` 失败时本方法返回 `Err`，但**内存已在 `compact()` 阶段压实**（I5 无 undo
路径）。调用方拿到 `Err` 后对同一 `path` 重试 `save()` 即可续写，不必重跑 `compact()`。

**实测**（10K 合成语料 churn 0.3 × 5 轮 = 60% 墓碑；`eval-report.md` §8.8）：图 + data
**84.6MB → 33.7MB**（-60%），图 `nb_point` **25000 → 10000**（== 存活且有向量的 chunk 数），
compact 后 reload `GraphStatus::Loaded`（非 `Rebuilt`）、冷启动 **91~99ms**。
churn 0.1（33% 墓碑）50.9 → 33.8MB（-34%）。

**已知代价**：① 期间内存峰值 ≈2×（新旧 Index 与图并存，**R26**）；② 重建图 O(N)，100K 级
预估 60~100s（**R28**）；③ 重建图引入拓扑抖动（NFR-06 口径不破，**R29**）；④ 期间不可服务
（**R30**，在线 compaction 归 Step 8）。⚠️ 批量删除的 O(N·M)（`remove` 每次全量 `retain`）
**未随本 Step 解决**——`remove_many` 是可选 S4-02，**未实施**。

### 7.6 快照格式（FR-16）

```
┌────────────────────────────────────────┐
│ Header: magic "IDX1" (4B)              │
│         format_version: u32            │
│         crc32: u32                     │
├────────────────────────────────────────┤
│ Section: term_dict   (bincode)         │
│ Section: postings    (bincode)         │
│ Section: forward     (bincode)         │
│ Section: stats       (bincode)         │
│ Section: vectors     (bincode, 可选)   │
└────────────────────────────────────────┘
```

- 序列化用 **bincode 2.0.1**（MIT）。
  > 修正：初版文档称"bincode 1.3 与 `instant-distance` 的 `with-serde` 内部 bincode 对齐"——**该说法有误**。`instant-distance` 0.6.1 的 `with-serde` 只引入 `serde` 与 `serde-big-array`，**不含 bincode**。
  > ⚠️ 另有两处版本坑：**bincode 3.0.0 是玩笑发布**（源码仅一行 `compile_error!`），crates.io 的 `max_version` 不等于可用版本；bincode 2 需显式开 `serde` feature 才有 `bincode::serde`。
  > ⚠️ bincode 2 **不支持 `serde_json::Value`**（`serialize_any` → `AnyNotSupported`）→ 快照对 metadata 用 DTO 存 JSON 字符串。
- **P4 阶段与 `rkyv` 实测对比**：`rkyv` 0.8.18（MIT）为零反序列化格式，配合 `memmap2` 可使加载接近 O(1)，对 NFR-04（冷启动 < 2s）极具吸引力。代价是所有落盘结构需 `derive(Archive)`，格式升级要额外处理版本兼容。**取舍规则：若 bincode 在 1 万 chunk 快照上已满足 NFR-04，则不引入 rkyv。** 快照结构已在 `storage/codec.rs` 抽象，切换不影响上层。
- `format_version` 用于向后兼容检测：版本不匹配时直接报错而非静默读错
- CRC32 校验防止读到半截文件（进程被 kill 的常见后果）

#### 7.6.1 V2 Step 1：新增结构**不入快照**，故 `FORMAT_VERSION` 保持 2

V2 Step 1 引入了两份全新状态，但**都不落盘**：

| 结构 | 是否入快照 | 理由 |
| --- | --- | --- |
| 存活位图 `ChunkBits` | ❌ | 由 `chunks` 的 `Some`/`None` 在 `import` 时 `rebuild` 得到，无信息损失 |
| 字段索引 `FieldIndex` | ❌ | 由 `docs` 的 metadata 全量重建 |

代价是冷启动多一次重建（O(总 chunk 数) + O(总 metadata 键数)），收益是**旧快照可直接加载**
（`SnapshotSections` 结构未动）。这也让本 Step 与 **ADR-A**（图持久化，Step 2，见 7.6.2）**完全解耦**。

⚠️ 字段索引快照后复位的副作用：`degraded` 标志会清零（重建是完整重灌，不存在"被跳过的窗口"，
所以复位是**正确**的）。它与"运行期 `degraded` 粘滞"形成表面矛盾，已在 `field_index.rs` 注明。

#### 7.6.2 V2 Step 2：图 sidecar（**ADR-A**）

> 详细设计与源码级证据见 `v2-step2-design.md`（v0.3，2026-09-06 拍板）。
> 本 ADR 即 plan-v2 §4.0 的 **H1**（图持久化格式 + 多文件原子性），Step 2 与 Step 3 共享。

**决策：图 = 快照的派生缓存，不是真源。**

```
foo.idx                 快照（真源，FORMAT_VERSION 仍为 2，格式一字节未动）
foo.idx.hnsw.graph      图拓扑（hnsw_rs 原生格式，派生）
foo.idx.hnsw.data       图数据（含向量副本，派生）
foo.idx.hnsw.manifest   清单（**唯一原子发布点**）
```

- `FORMAT_VERSION` **保持 2**：图不进快照正文，旧快照照常可加载（加载后降级重建图）
- **唯一原子发布点是 manifest**（小文件，tmp → fsync → rename → fsync 父目录）。
  manifest 记父快照 CRC 作「版本锚点」+ 两个图文件的 CRC/长度 + `dist_id` / `platform` /
  建图参数 ⇒ 「要么全对、要么全不算」，**多文件原子性问题由此消解**
- basename 铁律：传给 `file_dump` / `HnswIo::new` 的是**快照文件名全名**（`foo.idx`），
  `.hnsw.graph` / `.hnsw.data` 由 `hnsw_rs` 自行追加（拼成 `foo.idx.hnsw` 会得到
  `foo.idx.hnsw.hnsw.graph`，降级路径一切正常、测试全绿，只有 NFR-04 静默失效）

**失败模式：图可随时丢弃。** 删 manifest / 删图 / 篡改任一字节 / 快照更新而图未更新 /
dim 或 platform 不符 / 建图参数漂移 —— 一律降级为加载后重建，**功能不丢，只慢**。
这条把 Q-C3（多文件版本错配）从「正确性事故」降级为「性能退化」，也是 Step 3 只需
让 `foo.idx` 自己 tmp+rename、图 sidecar 自动跟随的原因。

> ✅ **前提已落地（V2 Step 3，2026-09-07）**——快照本体原子性此前是本节的正确性欠账
> （`File::create(path)` 直写 + `flush()`，写一半崩溃 ⇒ 半截文件 ⇒ `SnapshotCorrupted`，
> **不是降级重建，是索引丢失**）。现由 `storage/atomic.rs` 的通用原语
> `atomic_write`（tmp → 写入 → flush → `sync_all` → rename → fsync 父目录）补齐：
> **快照与 manifest 共用一份实现**（两份独立实现迟早漂移，漂移的那一份就是下一个 Q-C3），
> 快照格式零变化（原子性是写协议变更，不是格式变更，`FORMAT_VERSION` 保持 2）。
> 崩溃不变式：rename 之前真源从未被触碰 ⇒ 崩溃后要么旧快照要么新快照，`SnapshotCorrupted`
> 在任何窗口都不可达；fsync 代价实测 +0.027~0.035s（`eval-report.md` §8.7，不触碰任何 NFR）。
> 设计与故障注入证明见 `v2-step3-design.md`（D-S3-01~07）。

| 被否掉的候选 | 硬伤 |
| --- | --- |
| A. 图并入快照 blob | 需升 `FORMAT_VERSION`（旧快照全废）且 `HnswIo` **只能从文件路径加载**（无内存 reader 公开 API）⇒ 加载时要先写回临时文件 |
| B. sidecar + 代际目录 | `save(path)` 语义从文件变目录，**对外破坏性变更**；且需 GC 策略 |

---

## 8. 质量属性设计

### 8.1 性能（NFR-02 / NFR-03）

| 手段                                    | 说明                                      | 对应需求   |
| -------------------------------------- | --------------------------------------- | ------ |
| 两路召回并行                                 | BM25 lane 与向量 lane 用 rayon 并行，取较慢者耗时    | NFR-02 |
| 查询路径零磁盘 IO                             | 全内存索引；过滤在融合前用 bitmap 做，不触碰正排            | NFR-02 |
| query embedding 缓存                     | **`moka`** 并发缓存（带 TTL），Agent 多轮循环中大量 query 是重复的 | FR-20  |
| 复用 `hnsw_rs::Hnsw` 实例                 | 避免每次查询重新分配堆内存                           | NFR-02 |
| 批量摄入并行                                 | rayon 并行分词与 embedding                   | FR-19  |
| 融合层不回捞正文                               | 只在最后对 Top-K 做一次正排回捞                     | NFR-02 |
| 过滤下推（V2 Step 1）                        | 字段索引把 `field → value → doc 位图` 预先建好，求值从 O(语料) 降到 O(匹配文档数) | NFR-02 / FR-27 |
| 无过滤热路径保 fast-return（V2 Step 1）         | 无用户过滤时**刻意不走** `search_filter`：它会禁用 `hnsw_rs` 的 fast-return，改变 P99 的成本结构 | NFR-02 |

> Embedding 是最大延迟源：本地 bge-small 单条约 5~15ms，远程 API 则 50~200ms。**若使用远程 embedding，NFR-02 的 20ms 上限不可能达成**，此时应改为监控 P99 并单独上报 embedding 耗时。

### 8.2 确定性（NFR-06）

| 来源         | 处理                                          |
| ---------- | ------------------------------------------- |
| HNSW 图构建随机性 | ⚠️ **「固定随机种子」是过时论据**（已证伪：`StdRng::from_os_rng()` 无 seed API，见 `p5-design.md`）。V2 Step 2 起改为**图持久化 + 冻结拓扑**（ADR-A）：NFR-06 的口径细化为「**同一快照连续两次加载**检索结果逐位一致」 |
| 同分排序不稳定    | tie-break by `chunk_id` 升序                  |
| HashMap 遍历序 | 凡是进入排序的路径，必须先收集再按 `chunk_id` 排序，不直接依赖 HashMap 迭代顺序 |
| 并行执行       | 两路并行不影响各自结果；融合前按 lane 名固定顺序收集               |

验收：同一 query 连续检索 100 次，结果顺序完全一致（见 13.1 单测清单）。

### 8.3 可观测性（NFR-07）

`SearchResponse.took` 为端到端耗时。此外内核内部记录各 lane 耗时与候选数，通过 tracing 输出：

```
search: took=8.2ms bm25=1.4ms(vector=6.1ms parallel) candidates=187 fused=50 took_total=8.2ms
```

**V2 Step 1 新增 `query::Metrics` 三项**（服务于"Agent 需要自查检索质量"这一核心差异）：

| 字段 | 含义 | 用途 |
| --- | --- | --- |
| `filter_eval` | 过滤求值耗时 | 定位 Q-I1 未覆盖到的降级字段全扫 |
| `allowed` | 通过过过滤的候选数 | 低选择度的直接读数 |
| `vector_shortfall` | `min(candidate_k, allowed) - 实得条数` | filtered-ANN 召回静默下降的可观测信号；**V2.1 是否引入 prefilter 结构的判据** |

**V2 Step 2 新增「降级可观测」**（NFR-07 口径延伸）：图 sidecar 不可用时必须显式报告，
不得静默降级——静默会掩盖「图其实一直没生效」这类问题。载体是
`SearchIndex::graph_status() -> GraphStatus`，CLI `search --index` 与 bench 均打印：

| 变体 | 含义 | 语义边界 |
| --- | --- | --- |
| `Loaded` | 从 sidecar 加载成功（冷启动快路径） | 每个「应走快路径」的测试都**必须先断言它**（P0-1：basename 拼错时所有「能加载」断言照样绿） |
| `Rebuilt(reason)` | 降级重建（manifest 缺失 / CRC 不符 / 参数漂移 / 平台不符…） | 结果仍然正确，只是冷启动慢 |
| `PersistFailed(reason)` | **写侧**失败：图没落盘（非 Strict 下 `save()` 仍返回 Ok） | 与 `Rebuilt` 区分：这里什么都没重建，也不是「读不到图」（评审 #12 nit） |
| `NotApplicable` | 不适用（Brute 后端 / 纯 BM25 / `--no-graph-persist`） | 从未发生图持久化 |

> ⚠️ **P0-3 的边界（评审 #12 发现 2 后修订）**：「图是缓存 ⇒ `save` 不得因图失败而失败」
> 覆盖的是**整条写图链路**（dump → CRC → manifest 原子发布 → 失败清理），
> 而不只是 `dump_graph` 一步。快照落盘之后才写图，此时让 `save()` 返回 Err
> 等于「缓存写坏了把主数据一起否决」。`GraphPersistMode::Strict` 是唯一的例外开关。

**V2 Step 5 补「外部可见」（T7-23；2026-09-10 设计定稿，实现待 S5-05~07）**：

| 变更 | 内容 | 决策 |
| --- | --- | --- |
| `Metrics` **进响应** | `SearchResponse` 新增 `metrics: Metrics` 字段（`query` 模块同时再导出 `Metrics`）；`tracing::info!` 通道**保留**——一个给宿主、一个给调用方 | **D-S5-05**（⚠️ **对外破坏性变更**：`SearchResponse` 字段全 `pub`，外部**字面量构造**会编译失败 ⇒ 记 CHANGELOG `⚠️ 破坏性`；库内只有 `searcher.rs:268` / `:413` 两处） |
| **per-lane 耗时** | `bm25_elapsed` / `vector_elapsed`。Hybrid 走 `rayon::join`（`searcher.rs:164-167`），单一 `took` 无法归因「是哪一路慢」 | **D-S5-06**（实现形状：lane 闭包返回 `(Result<Vec<Scored>>, Duration)`） |
| **路径可见性** | `Metrics.vector_route: VectorRoute { None, Ann, Exact }` —— `None` = 未走向量路 / `Ann` = 走了 ANN（可能近似）/ `Exact` = 走了精确扫描（保证 `min(k, allowed)` 条且无召回缺口） | **D-S5-07**。**没有它，「兜底是否真的生效」只能靠延迟反推**；且 `vector_shortfall` 在精确路径下**通常**为 0，但**不是恒等式**（`allowed` 来自 `Index`、扫描枚举的是图中的点，图滞后于索引时仍 > 0——那时它是「图未覆盖全部 allowed」的诊断信号）⇒ 「0 缺口」**≠**「无 prefilter 需求」，**两种读数都必须连看 `vector_route`**（设计文档 §4.4） |
| 修早退 `took` | `searcher.rs:99-104`（`index_is_empty`，**连 `metrics.log` 都不调**，故 grep 该符号结构上找不到）与 `:125-127`（过滤排空）**两条**早退路径漏设 `metrics.took` ⇒ 日志 `took_ms=0` 是**假数据**，而响应 `took` 为真值（同类第三处 `:212-213` 反而设了） | **D-S5-08**（与 T7-23 同主题；观测正确性缺陷） |

⚠️ **当前限制（V2 Step 5 设计已承接，待实现）**：`Metrics` 至今仍只在 `search_parts` 内聚合并经
`tracing::info!` 输出，**不在 `SearchResponse` 里、也不进 `bench::QueryMetrics`**。后果是：宿主不挂
tracing subscriber 就静默丢弃；**无法单测**；**无法被 bench 聚合**。在 D-S5-05 落地之前，
上述三项 Step 1 指标（含新的 `vector_route`）**对外依然无人可见** —— 而 `vector_shortfall`
正是「V2.1 是否引入 prefilter 结构」的判据、Step 6 的 NFR-10/11 实测也要靠它。

### 8.4 空结果的语义（V2 Step 1）

Agent 场景里空结果必须是**可归因**的——调用方要能区分"该建索引了"和"过滤条件写错了"。
编排层按 **query 侧信号优先**判定 `EmptyReason`：

| query 有命中 | filter 有匹配 | 判定 |
| --- | --- | --- |
| 否 | — | `AllTermsUnmatched`（探针：`query_has_hits` 走 BM25 词典） |
| 是 | 否 | `FilteredOut` |
| 是 | 是（但语料空） | `NoDocuments` |

⚠️ **探针的适用边界**：`query_has_hits` 是纯 **BM25 侧**信号（走 `Index::term_id + postings_by_id`）。
在 `SearchMode::Vector` 下它退化成"语料里有没有这些词"，与向量召回无关 → 空结果会误报
`AllTermsUnmatched`。该行为**非 V2 Step 1 引入**，T16 已把它钉成契约。真正区分需要向量路自己的探针（V2.1）。

---

## 9. 技术选型与决策记录

### 9.1 选型表

> 全部版本于 2026-09-02 经 crates.io API 实测；License 与 MSRV 核查见 `thirdparty.md`。

| 用途             | 选型                     | 版本        | License            | 理由                                                                     |
| -------------- | ---------------------- | --------- | ------------------ | ---------------------------------------------------------------------- |
| 语言             | Rust                   | **1.90+** / 2021 edition | —        | MSRV 由依赖决定（bincode 1.85 / clap 1.85 / smol_str 1.89），见 ADR-008           |
| 向量索引           | `instant-distance`     | 0.6.1     | MIT OR Apache-2.0  | 纯 Rust HNSW、rayon 并行构建、`with-serde` 支持序列化。**P4 与 `hnsw_rs` A/B 复核**（见 7.5、ADR-002） |
| 本地 embedding   | `fastembed`            | **6.0.2** | Apache-2.0         | 基于 ort(ONNX Runtime)、同步无 tokio 依赖。⚠️ ① 传递依赖 `ort =2.0.0-rc.13` 为预发布，`Cargo.lock` 必须入库；② **必须 `default-features = false`**，否则默认 feature 会带来 `image → rav1e → libfuzzer-sys`（NCSA 许可）；③ 实际下载的模型仓为 `Xenova/bge-small-zh-v1.5`（HF 未声明 License，**已决策接受**，沿用上游 MIT；残留风险见需求文档 8.3 P5） |
| 中文分词           | `jieba-rs`             | 0.10.3    | MIT                | 生态成熟、词典可定制                                                             |
| 中英分段           | `unicode-segmentation` | 1.13.3    | MIT OR Apache-2.0  | 不手写 Unicode 边界判断                                                       |
| 文本归一化          | `unicode-normalization` | 0.1.25   | MIT OR Apache-2.0  | NFC 归一化，保证索引侧与查询侧一致                                                    |
| 并行             | `rayon`                | 1.12      | MIT OR Apache-2.0  | 两路召回并行、批量 embedding                                                    |
| 序列化            | `bincode`              | **2.0.1** | MIT                | 主流、够快。**⚠️ 不可用 3.0.0**：crates.io 上的 3.0.0 是玩笑发布（源码仅 `compile_error!("https://xkcd.com/2347/")`）。版本无外部约束（`instant-distance` 的 `with-serde` 只含 serde）。P4 与 `rkyv` A/B |
| 小字符串           | `smol_str`             | 0.3.6     | MIT OR Apache-2.0  | Term 存储的 inline 优化，减少堆分配                                               |
| 缓存             | **`moka`**             | 0.12.16   | (MIT OR Apache-2.0) AND Apache-2.0 | **并发安全 + TTL**；替代原计划的 `lru`（非并发安全，需套 `Mutex`，会抵消两路并行收益）。⚠️ 必须显式启用 `sync` 或 `future` feature，否则 `compile_error!` |
| CLI            | `clap`                 | 4.6       | MIT OR Apache-2.0  | derive API                                                             |
| 错误处理           | `thiserror` / `anyhow` | 2.0 / 1.0 | MIT OR Apache-2.0  | 库侧 / CLI 侧                                                             |
| 日志             | `tracing`（+ subscriber） | 0.1 / 0.3 | MIT             | 可观测性（NFR-07）                                                           |
| 校验             | `crc32fast`            | 1.5.1     | MIT OR Apache-2.0  | 快照 CRC                                                                 |
| **测试基线（dev）**  | **`tantivy`**          | 0.26.1    | MIT                | **仅作 BM25 正确性对照，不参与构建产物**，见 ADR-007                                    |
| 基准测试（dev）      | `criterion`            | 0.8.2     | Apache-2.0 OR MIT  | P5 延迟实测                                                                |
| 属性测试（dev）      | `proptest`             | 1.11      | MIT OR Apache-2.0  | 分词一致性、快照 round-trip                                                    |
| **依赖合规（工具）**   | **`cargo-deny`**       | 0.20.2    | MIT OR Apache-2.0  | License 白名单强制校验，见 ADR-008                                              |

**不引入**：`arroy`（耦合 LMDB，与内存+快照冲突）、`redb` / `sled` / `heed`（持久化路线为快照）、`dashmap`（7.x 仍 rc，v1 单写者 + `RwLock` 足够）、`stop-words`（词表不值得一个依赖，且中文需自维护）、`lindera`（词典数十 MB）、`axum` / `tokio`（不做服务层）。完整评估见 `thirdparty.md`。

### 9.2 备选方案对比

**向量索引**

> 2026-09-02 复核后新增 `hnsw_rs` 与 `usearch` 两个候选，并修正 `arroy` 的版本与结论依据。

| 候选                 | 增量 insert | 删除    | 查询时过滤                   | License            | 依赖            | 结论                                              |
| ------------------ | --------- | ----- | ----------------------- | ------------------ | ------------- | ----------------------------------------------- |
| `instant-distance` 0.6.1 | ❌ 无 insert API | ❌ | ❌                  | MIT OR Apache-2.0  | 纯 Rust，无 C 依赖 | ✅ **第一版选用**（最轻）。用主索引 + delta 区绕开限制，**P4 复核** |
| **`hnsw_rs`** 0.3.4 | **✅**     | ❌     | **✅ `search_filter`**   | **MIT/Apache-2.0** | **纯 Rust**    | ⏳ **P4 A/B 原型**。达标则取代前者并删除 delta 区；`DistDot` 要求入库前归一化，与 7.3 方案天然契合 |
| `usearch` 2.26.2   | ✅         | **✅** | ✅ `filtered_search`     | Apache-2.0         | ❌ C++（`cxx`）  | ⏳ P6 备选。仅当**物理删除**成为硬需求时引入（前两者均不支持）             |
| `arroy` 0.8.0      | ✅         | ✅     | ✅                       | MIT                | ❌ LMDB（`heed`） | ❌ 与"内存 + 快照"路线冲突                                |
| 自研 HNSW            | ✅ 可控      | ✅     | ✅                       | —                  | 无             | ❌ 第一版不做。待内核稳定后作为 P6 优化项                         |

**BM25 实现**：见 ADR-001。

**Embedding**：见 ADR-003。

### 9.3 Feature Flags

```toml
[features]
default       = ["local-embed"]
local-embed   = ["dep:fastembed"]         # 本地 ONNX 推理
remote-embed  = ["dep:reqwest", "dep:tokio"]  # 远程 HTTP API
positions     = []                        # posting 存储位置信息
```

`local-embed` 与 `remote-embed` 可同时开启，运行时通过配置选择；都不开启时 `Embedder` 无可用实现，编译期即暴露（见 11.3 `NoEmbedder`）。

### 9.4 决策记录

#### ADR-001：BM25 倒排索引手写，不用 tantivy

- **状态**：已接受
- **背景**：需要 BM25 关键词检索（FR-03、FR-04）。tantivy 是成熟的工业级全文检索库。
- **决策**：手写分词 → 倒排表 → postings → BM25 打分 → 检索的完整链路，只用 `jieba-rs` 处理分词。
- **理由**：① 项目目标包含"把检索链路每个环节写一遍"（G2 的认知目的）；② 中文场景的 `avgdl` 与参数调优需要完全可控；③ 字段权重、查询语法扩展自由。
- **代价**：需自己处理段合并、posting 压缩等工程问题；MVP 阶段不做（见 7.1、7.6）。
- **相关需求**：FR-02、FR-03、FR-04

#### ADR-002：向量索引用 instant-distance，以主索引 + delta 绕开无 insert API

- **状态**：已接受
- **背景**：需要 HNSW 向量索引（FR-08）与增量写入（FR-17），但 `instant-distance` 的 `build()` 一次性消费 points，无增量插入。
- **决策**：选用 `instant-distance`，用「不可变主 HNSW + 暴力 delta 区」支持增量；delta 达 1000 条或总量 10% 时后台重建。
- **理由**：纯 Rust 无 C 依赖、Apache-2.0、rayon 并行构建、`with-serde` 支持序列化。增量限制可用工程手段绕过，代价可控（见 7.5）。
- **代价**：delta 区为线性扫描；重建期间有额外内存占用。
- **相关需求**：FR-08、FR-09、FR-17

#### ADR-003：Embedding 默认本地 fastembed，远程作 feature

- **状态**：已接受
- **背景**：需要中文 embedding（FR-05、FR-06），可选远程 API（FR-07）。
- **决策**：`Embedder` trait 抽象；默认本地 `fastembed` + `BAAI/bge-small-zh-v1.5`（512 维）；远程 HTTP 实现置于 `remote-embed` feature 之后。
- **理由**：本地方案无需联网与 API Key，demo 可离线运行；`fastembed` 基于 ort、同步无 tokio 依赖，不污染运行时。
- **代价**：首次运行需下载 ONNX 模型（国内可能失败，对应风险 R3）；CPU 推理是主要延迟源。
- **相关需求**：FR-05、FR-06、FR-07

#### ADR-004：融合默认 RRF（k=60），加权归一仅作备选

- **状态**：已接受
- **背景**：需要把 BM25 与向量两路结果合并（FR-10、FR-11）。
- **决策**：默认 RRF，`k=60`，两路权重均为 1.0；加权归一作为可选策略实现，采用「除以 lane Top-1 分数」而非 min-max。
- **理由**：两路量纲不可比（BM25 无上界 vs 余弦集中在 [0.6, 0.95]），归一化既损失信息又不稳定；RRF 只用排名，免疫量纲。
- **代价**：RRF 丢弃了分数绝对大小信息，无法表达"这一路明显更可信"。需要时由权重 `w_i` 调节。
- **相关需求**：FR-10、FR-11

#### ADR-005：持久化用内存索引 + 自研 bincode 快照

- **状态**：已接受
- **背景**：需要快照 save/load（FR-16），且需求文档 8.1 明确不引入嵌入式 KV。
- **决策**：平时全内存，save/load 到自定义二进制格式（magic + version + crc32 + 若干 bincode section）。
- **理由**：① 秒级加载，满足 NFR-04；② 零额外依赖；③ bincode 版本无外部约束（`instant-distance` 的 `with-serde` 只含 serde，不含 bincode）。
- **修正（2026-09-02，P0 实测）**：① 初版称"bincode 1.3 与 instant-distance 内部版本对齐"属事实错误；② 中间曾改选 3.0.0，但 **3.0.0 是玩笑发布**（源码仅一行 `compile_error!`），最终定为 **`bincode 2.0.1`**。P4 末与 `rkyv` 实测对比后再定稿。
- **代价**：不支持单条文档的持久化更新，必须整体重写快照（MVP 规模可接受）。
- **相关需求**：FR-16

#### ADR-006：BM25 检索用 TAAT 而非 DAAT/WAND

- **状态**：已接受
- **背景**：需要实现 BM25 检索执行（FR-04）。
- **决策**：TAAT（Term-At-A-Time）——逐个 term 遍历 posting 列表，累加分数到 `HashMap<ChunkId, f32>`，最后取 Top-K。
- **理由**：万级规模下实现简单、缓存友好，且天然支持 OR 语义（FR-04 的硬要求）。
- **代价**：长尾 term 的 posting 长时无剪枝，无法提前终止。DAAT（WAND / Block-Max WAND）列为 P4 优化项。
- **相关需求**：FR-04

#### ADR-007：`tantivy` 作为 dev 依赖的 BM25 正确性基线

- **状态**：已接受（2026-09-02 新增）
- **背景**：手写 BM25 的最大风险不是写不出来，而是**实现有偏差但看不出来**。现有防护是"手算 5 篇文档比对"，覆盖率太低（覆盖不到空 query、全停用词 query、超长 query、df=0 等边界）。
- **决策**：`tantivy` 0.26.1（MIT）**仅作为 dev-dependency 引入**，用于：① P1 阶段建同语料索引，逐条比对 BM25 排序；② P5 阶段作为第三路基线加入对照实验。**不参与构建产物**。
- **理由**：① 工业实现是最现成的 oracle，成本极低而价值极高；② 仅 dev 依赖，不影响分发与 License；③ 不违背 ADR-001——不用它**实现功能**，只用它**验证实现**。
- **代价**：多 50+ 个 dev 依赖，拉长 `cargo test` 的首次编译时间；需容忍 `k1`/`b` 与平均长度口径差异（排序可比，分值不必全等）。
- **验收**：自研 BM25 与 tantivy 在同一评测集上的 NDCG@10 差距**不得超过 5%**，否则必须查清原因再发布 P5 结论。
- **相关需求**：FR-03、FR-04；对应 plan.md T1-15、T5-04

#### ADR-008：MSRV 提到 1.90，并用 `cargo-deny` 强制 License 白名单

- **状态**：已接受（2026-09-02 新增）
- **背景**：① NFR-08 原定 Rust 1.80+，但实测主流依赖的 MSRV 已突破：`bincode` 3.0.0 = 1.85、`clap` 4.6 = 1.85、`smol_str` = 1.89、`criterion` 0.8 = 1.86、`roaring` = 1.90；坚守 1.80 意味着全面锁旧版本。② 项目要求依赖 License 友好，但传递依赖可达数十个，人工审查 `Cargo.toml` 不可靠。
- **决策**：① MSRV 定为 **1.90+**，并在 `rust-toolchain.toml` 固定；② 引入 `cargo-deny` 0.20.2（MIT OR Apache-2.0），配置白名单 `MIT / Apache-2.0 / BSD-3-Clause / BSL-1.0 / CC0-1.0 / Zlib / ISC / Unicode-DFS-2016`，出现其他 License（尤其 GPL 系列）直接失败。
- **理由**：① 1.90 覆盖除 `croaring`（1.95，已排除）外的所有候选依赖；当前 stable 为 1.98.0，1.90 对使用者不算苛刻；② 合规检查自动化，避免"某个传递依赖换成 GPL 而无人发现"。
- **代价**：要求使用者的工具链 ≥ 1.90。
- **相关需求**：NFR-08；对应 plan.md T0-03、T0-06

#### ADR-009：对外接口采用「门面层 + 共用编排内核」，而非重写检索路径

- **状态**：已接受（2026-09-04 新增，依据 `p6-design.md` v2.0 决策点 D-I1~D-I8）
- **背景**：issue #1 要求把接口重构为 `index.add(doc)` / `searcher.search(query)`。现状 `Searcher<'a>` 借 4 个外部引用无法存 struct，最小闭环 79 行含 3 个静默陷阱；但它的编排逻辑（两路召回→融合→回捞→精排）已稳定并被评测验证，**不应重写**。
- **决策**：新增 `SearchIndex`（写端）+ owned `Searcher`（读端，`'static + Clone + Send + Sync`）门面；把原 `Searcher<'a>` 的编排抽成自由函数 `search_parts`，旧类型改名 `QueryExecutor<'a>`（签名不变）作底层逃生舱。门面只做装配与生命周期管理，不实现算法。
- **理由**：① 新旧共用同一 `search_parts`，避免两份编排逻辑漂移；② 重构是"加法"，bench / 集成测试 / 单测零改动，评测可复现性不受威胁；③ 逃生舱保证现有可调能力（brute 后端、BM25/RRF 网格、charabia、裸 embedder）一个不丢。
- **代价**：`Document` 拆为输入 DTO + `DocRecord`（breaking）；快照升 `FORMAT_VERSION` 2 存配置指纹（旧 `.idx` 需重建）；`raw_vectors` 使向量内存翻倍（NFR-05 重测）。
- **相关需求**：FR-12 / FR-14 / FR-15 / FR-17 / NFR-03 / NFR-05；对应 plan.md P6（I-01~I-14）

#### ADR-010：向量软删除用「存活位图 + 谓词下推」，而非给 `VectorIndex` 加墓碑

- **状态**：已接受（2026-09-05 新增，依据 `v2-step1-design.md` v1.0 决策点 D-S1-01~D-S1-12）
- **背景**：两个独立缺陷。① **Q-C1**：软删除只在正排打墓碑，向量图里仍留着向量，继续**霸占 Top-K 名额**——删 20 篇后 Top-5 只剩 2 条，且不报错。② **Q-I1/I2**：过滤是检索后 post-filter 且每候选调 `Filter::matches(metadata)`，复杂度 **O(语料规模)**，与命中多少无关。
- **决策**：
  1. 存活状态的**单一真源**放在 `Index::forward` 的位图（`ChunkBits`），与 `chunks` 的 `Some`/`None` 在**同一函数内**维护；**不给 `VectorIndex` 加 `tombstone`**。检索时以谓词（`predicate::CandidateFilter`）注入两路。
  2. 新增 `field_index`（`field → value → doc 位图`）+ 惰性 `ChunkFilter` 谓词，把过滤从 O(语料) 降到 O(匹配文档数)。
  3. HnswRs **双路径**：无用户过滤走普通 `search()`（保 `fast-return`，按存活比例过采样 `knbn`）；有用户过滤才走 `search_filter`。
- **理由**：
  1. **双写必漂移**——向量侧若也持有存活态，两处状态需要跨快照同步，而 `load` 时 `raw_vectors` 全量重灌，向量侧状态**无法从快照恢复**。
  2. 谓词是**叶模块**（`predicate.rs`），`vector` / `retriever` 只依赖它、不依赖 `index` 或 `query::filter`，分层不破。
  3. 无过滤是**热路径**。`hnsw_rs` 的 `search_filter` 会禁用 fast-return；用它跑 `AliveOnly` 会为「几乎全通过」的谓词付出整图遍历的代价。双路径把成本只对真正需要过滤的查询收取。
- **代价**：
  - 位图与字段索引**不入快照**（`import` 时重建），冷启动多一次 O(N) 遍历；换来 `FORMAT_VERSION` 保持 2、旧快照可直接加载。
  - 字段索引有**基数保护**（`terms + numbers` 合计 1024），高基数字段（毫秒时间戳 / 雪花 ID）**约 512 篇**就永久降级，该字段上过滤回落 O(N) 全扫 —— **Q-I1 对最常见的真实场景收益为 0**。这是 V2.1 prefilter / 排序列方案的**第一用例**。
  - 带过滤的 vector 路 P99 显著高于无过滤（1 万级实测 3.5ms → 9~11.5ms），根因是 `hnsw_rs` 的 `search_filter` 在候选不足时关闭距离剪枝 → 近似整图遍历。**这是库层结构性行为，调参治不了**。
- **相关需求**：FR-26 / FR-27 / FR-28 / NFR-02 / NFR-06；对应 `plan-v2.md` Step 1（S1-01~S1-11）

#### ADR-A：图 = 快照的**派生缓存**（V2 Step 2，H1 的方案 C）

- **状态**：已接受（2026-09-06 拍板，依据 `v2-step2-design.md` v0.3；7 个决策点 D-S2-01~07 已获外部评审同意）
- **背景**：图不持久化（D7）导致每次冷启动都要重建 HNSW 图，12K 语料 **11.57s**，NFR-04（< 2s）不达标。三个备选：
  - **方案 A** 图塞进快照 ⇒ 必须动 `FORMAT_VERSION`（旧快照全废）+ 快照体积翻倍
  - **方案 B** 图独立一文件、与快照并列 ⇒ 两个文件谁先写谁后写？**多文件原子性**无解
  - **方案 C（采纳）** 图作**快照的派生缓存**：`foo.idx` 仍是真源，旁边多出 `foo.idx.hnsw.{graph,data,manifest}`
- **决策**：
  1. **`FORMAT_VERSION` 保持 2**，快照格式一个字节不动；图是 sidecar，随时可删
  2. **manifest 是唯一原子发布点**：图文件先写完并算 CRC → manifest 走 tmp→fsync→rename→fsync 父目录。
     「要么全对、要么全不算」因此成立，**多文件原子性被降维成单文件原子性**
  3. manifest 记**父快照 CRC** 作「版本锚点」——快照变了而图没变，图自动失效
  4. 图**可随时丢弃**：删 manifest / 删图 / 篡改任一字节 / 维度或平台不匹配 ⇒ 一律降级为加载后重建
- **理由**：
  1. 图本就是**可由快照重算**的东西（有 `raw_vectors` 就能重建），把它当缓存而非真源，
     正确性风险从「图错了 = 结果错」降为「图错了 = 慢一点」
  2. `hnsw_rs` 的 reload 路径有 12 处 `assert_eq!`/`unwrap()`，**损坏文件会 panic 而非返回 Err**（C3）。
     只有「图可丢弃 + 五道先验 CRC 校验」才能同时保住「不崩」与「不错」
  3. 方案 A/B 都要引入新的一致性协议；方案 C 的协议只有一条：**manifest 说了算**
- **代价**：
  - **磁盘 +60%**（12K 实测：52.3MB → ≈84MB，1.6×）。`hnsw_rs` 只暴露 `DumpMode::Full`，
    向量必然被复制一份，**省不掉**（R22）。`--no-graph-persist` 是逃生舱
  - 内存不变（只涨磁盘）；图 dump 43.5ms、图加载 23.3ms 均可忽略
  - **达标依赖图 sidecar 命中**——降级路径仍是 ~10s，故必须配 NFR-07 的「降级不得静默」
- **实测（12K / release）**：完整冷启动 76.9ms（快照）+ 23.3ms（图）≈ **100ms** ✅（限额 2s）；
  对照降级路径 56~78ms + ≈10~11.5s ≈ 11s
- **相关需求**：FR-29 / NFR-04 / NFR-06 / NFR-07；对应 `plan-v2.md` Step 2（S2-01~S2-12）

---

## 10. 对外接口设计（门面层）

> 架构级概要。任务级拆分见 `plan.md` 第 11 章；完整设计（业界调研、Before/After、决策点 D-I1~D-I8）见 `p6-design.md`（v2.0 已拍板）。

### 10.1 设计动机

对外接口重构的三个动因（均实测，见 `p6-design.md` 第 1 章）：

1. **静默正确性陷阱**：现状最小闭环 79 行、手接 8 对象、7 步，其中 3 步写错不报错只变差——分词器不一致（R4）、忘算 `content_hash`（FR-15 幂等失效）、忘 `NormalizedVector`（余弦分全错）。这与 11.3 节「配置错误尽早暴露」的原则直接矛盾。
2. **生命周期逼用户当库作者**：`Searcher<'a>` 借 4 个外部引用，无法存 struct / 无法跨线程；仓库自己的 CLI 与 bench 都手搓了「引擎对象」。
3. **批量 embed 不可见**：向量化是批接口，逐条 `add` 会打穿 NFR-03。

门面层的修复方式：把易错步骤移进库「只做对一次」，把生命周期收进 owned 对象，把批量 embed 收进写缓冲。

### 10.2 目标 API 形态

```rust
use helix_core::prelude::*;

let mut index = SearchIndex::builder().build()?;   // 零配置可用
index.add(Document::new("文本").with_source("src").with_metadata(json!({})))?;
index.add("纯文本")?;                               // impl Into<Document> for &str
index.commit()?;                                    // 刷写缓冲，此后可见

let searcher = index.into_searcher();               // 'static + Clone + Send + Sync
let resp = searcher.search("查询")?;                // 只有 query 必选；mode=Hybrid(自动)、top_n=10
let resp = searcher.search_with("查询")
    .mode(SearchMode::Hybrid)   // 也接受 .mode("hybrid")
    .top_n(10)
    .filter(&Filter::eq("lang", "zh"))              // FR-14
    .explain(true)                                  // FR-13
    .exec()?;
```

### 10.3 写端 `SearchIndex`

| 方法 | 说明 |
| --- | --- |
| `SearchIndex::builder()` → `SearchIndexBuilder` | 全部配置点（analyzer / chunker / embedder / fusion / reranker / bm25 / batch_size）；**不调即用默认值** |
| `add(doc) -> Result<AddOutcome>` | 分块 → 倒排 → 写缓冲；返回 `{ doc_id, chunk_ids, deduped }` |
| `add_documents(iter)` | 批量路径（吞吐最优，`helix build` 走这条） |
| `remove(doc_id)` | 墓碑删除 + 统计量回滚 |
| `commit()` | 刷写缓冲（批量 embed + 灌向量索引），此后数据可见（对齐 Lucene） |
| `into_searcher()` | 交出所有权，产出 owned `Searcher`（隐含 flush） |
| `search(&mut self, q)` | 便利法：内部 `commit()` 后立即检索 |
| `save(path)` / `load(path)` | 落盘 / 加载（含配置指纹校验，见 10.6）。`save` 会顺带写图 sidecar（ADR-A） |
| `graph_status()` | 最近一次图 sidecar 的状态：`Loaded` / `Rebuilt(reason)` / `NotApplicable`（NFR-07：降级必须可观测） |
| `graph_dump_elapsed()` | 最近一次 `save` 中图 sidecar 落盘耗时；`None` = 本次未走图持久化（验收 7 观测点） |
| `embed_elapsed()` | 累计 embed 推理耗时（NFR-03 口径） |

**写缓冲与批量 embed**：`Embedder::embed_documents` 是批接口，`add` 逐条 embed 会打穿 NFR-03。因此 `add` 只把 chunk 文本 push 进 `pending`，满 `batch_size`（默认 64，**实测校准** 32/64/128/256）时同步 flush。L2 归一化由 flush 路径内部保证。

**可见性语义**：`add` 后必须 `commit()` 才对检索可见（对齐 Lucene/tantivy），为 P7 的 delta 分段留出语义空间。

**所有权切换**：`into_searcher()` / `into_index()` 只移动 `Arc<Inner>`，零拷贝零锁。⚠️ 若 `Searcher` 被 `clone()` 后再 `into_index().add()`，`Arc::make_mut` 会静默深拷贝整个 `Inner`（快照隔离语义正确，但非零拷贝）——`add` 内加 `debug_assert!(Arc::strong_count == 1)` 提示。

### 10.4 读端 `Searcher`

| 方法 | 说明 |
| --- | --- |
| `search(query)` | 主形态：**只有 query 必选**；`top_n=10`，`mode` 按装配自动推断（有向量→Hybrid、无→Lexical） |
| `search_with(query)` → `SearchRequest` | builder：`.mode(m)` `.top_n(n)` `.filter(&f)` `.explain(b)` `.exec()` |
| `into_index()` | 换回写端继续增量写入 |
| `config_report()` | 打印 analyzer / chunker / embedder 实际取值（可观测性，对应风险 R7） |

`Searcher: 'static + Clone + Send + Sync`（六 trait 均已 `Send + Sync`），可直接放进 axum 的 `AppState`、`Arc<Searcher>` 分发、`move` 进 rayon。`SearchIndex` 本身是 `!Sync`（含可变 `pending`），可 `Send` 不可共享。

### 10.5 Document 输入 DTO 与数据模型

`Document`（对外输入）与 `DocRecord`（内部存储）拆分，见 5.1。要点：

- `content_hash = xxh64(dedup_key.unwrap_or(text))`——`dedup_key` 存在时以它为准，是**替代**非并存
- `dedup_key` 持久化进 `DocRecord`，remove→re-add 幂等判定不变
- 同一逻辑文档应始终用同一 `dedup_key`（时有时无判为不同文档，是调用方责任）

### 10.6 持久化与配置指纹

`SearchIndex::load(path)` 校验快照中的 `ConfigFingerprint { analyzer_id, embedder_id, dim, chunker }` 与当前装配是否一致，不一致 → `Error::ConfigMismatch { expected, actual }`（**双面打印**，绝不静默）。这修掉了 `helix search --index` 写死 `MixedAnalyzer` 导致的静默换分词器（R4）隐患。

快照 `FORMAT_VERSION` 升 2；`positions` feature 通过 `effective_version()` 对 base +1（base 2 / positions 3）——**这是把 `codec.rs` 里「从未实现的注释约定」落成实现**。

### 10.7 逃生舱：底层永远可达

门面是**默认装配，不是唯一路径**。底层 `QueryExecutor<'a>`（旧 `Searcher`）原样保留，需要完全自定义 lane 组装（brute 后端、BM25/RRF 网格、裸 embedder、`--runs` 重建图）时直接用它。完整逃生舱映射见 `p6-design.md` 7.3。

---

## 11. 内部接口与 CLI

### 11.1 底层 API（trait 与逃生舱）

> 门面层（第 10 章）是默认路径；以下底层 trait 装配 API 保留作逃生舱（换 brute 后端、BM25/RRF 网格、charabia 对照等，完整映射见 `p6-design.md` 7.3）。底层 `Searcher<'a>` 已改名 `QueryExecutor<'a>`。

```rust
use helix_core::prelude::*;

// ---- 构建索引 ----
let analyzer = MixedAnalyzer::new()?;
let embedder = LocalEmbedder::new(EmbeddingModel::BGESmallZHV15)?;  // fastembed 6.x 变体名（非 BGESmallZH）

let mut index = Index::builder()
    .analyzer(analyzer)
    .embedder(embedder)
    .build()?;

for doc in documents {
    index.upsert(doc)?;   // 幂等，按 content_hash 去重（FR-15）
}
index.commit()?;          // 合并 delta，构建 HNSW（FR-17）

// ---- 持久化 ----
index.save_to_path("index.helix")?;
let index = Index::load_from_path("index.helix")?;   // 秒级加载（FR-16）

// ---- 检索 ----
let searcher = Searcher::new(index)
    .fusion(RrfFusion::new(60.0))
    .reranker(NoOpReranker);          // v1 留位（FR-18）

let resp = searcher.search(SearchRequest {
    query: "Rust 里怎么实现支持中文的 BM25？",
    k: 10,
    mode: SearchMode::Hybrid,          // Bm25 | Vector | Hybrid
    filter: Some(Filter::eq("lang", "zh")),
})?;

for hit in &resp.hits {
    println!("{:.4} [{}] {}", hit.score, hit.source, hit.explain.matched_terms.join(","));
    println!("{}", hit.to_context_block());   // 直接拼进 prompt（FR-12）
}

if let Some(reason) = &resp.empty_reason {
    eprintln!("空结果原因：{:?}", reason);   // Agent 据此决策（FR-13）
}
```

### 11.2 CLI

```bash
# 建索引
helix build --input data/corpus.jsonl --output index.helix

# 检索（三种模式）
helix search --index index.helix --mode hybrid -k 10 "查询文本"
helix search --index index.helix --mode bm25   -k 10 "查询文本"
helix search --index index.helix --mode vector -k 10 "查询文本"

# 对比三种模式的效果（调试用，最常用）
helix compare --index index.helix -k 10 "查询文本"

# 可解释性输出（FR-13）
helix search --index index.helix --mode hybrid --explain "查询文本"

# 评测（P5）
helix bench --index index.helix --queries data/t2-queries.jsonl
```

### 11.3 错误类型

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("序列化错误: {0}")]
    Codec(#[from] bincode::Error),

    #[error("快照版本不兼容: 文件版本 {found}, 支持版本 {expected}")]
    SnapshotVersionMismatch { found: u32, expected: u32 },

    #[error("配置不匹配: 快照指纹 {expected}, 当前装配 {actual}")]
    ConfigMismatch { expected: String, actual: String },   // P6 新增：load 时校验配置指纹（10.6）

    #[error("快照校验失败，文件可能损坏")]
    SnapshotCorrupted,

    #[error("向量维度不匹配: 期望 {expected}, 实际 {found}")]
    DimensionMismatch { expected: usize, found: usize },

    #[error("嵌入模型错误: {0}")]
    Embedding(String),

    #[error("未启用任何 Embedder 实现，请开启 local-embed 或 remote-embed feature")]
    NoEmbedder,

    #[error("分词错误: {0}")]
    Analyze(String),
}
```

> `NoEmbedder` 与 `DimensionMismatch` 是刻意设计的**运行期早期失败**：把配置错误在第一时间暴露，而不是让错误向量静默进入索引——后者会表现为"检索效果莫名其妙地差"，排查成本极高。

---

## 12. 实施计划与需求映射

### 12.1 阶段划分

| 阶段         | 内容                                                             | 验收标准                                                 | 依赖 |
| ---------- | -------------------------------------------------------------- | ---------------------------------------------------- | -- |
| **P0**     | 工程骨架 + 安装 Rust 工具链 + 验证依赖可获取性                                  | `cargo build` 通过空工程；`instant-distance` / `fastembed` / `jieba-rs` 均可拉取编译 | —  |
| **P1**     | `analyze` + `index` + `retriever/bm25` + 单测                    | CLI `helix search --mode bm25` 在示例语料上出结果；BM25 分值与手算值一致 | P0 |
| **P2**     | `embed`（本地）+ `vector`（HNSW）+ `retriever/vector`                | CLI 向量路出结果；同义改写能被召回                                  | P1 |
| **P3**     | `fusion`（RRF + 加权）+ `query/searcher` + `explain`               | `helix compare` 可对比三路；Explain 输出完整                     | P2 |
| **P4**     | `storage` 快照 + 增量写入（**先 `hnsw_rs` A/B 再定案**）+ 元数据过滤                | save/load 后检索结果完全一致；增量写入后立即可查                        | P3 |
| **P5**     | T2Ranking 评测集装配 + `helix bench` + README                        | Recall@K / MRR@10 / 分级 NDCG@10 / 延迟有实测数据；BM25 参数网格搜索调参 | P4 |
| **P6**     | **接口重构**：`SearchIndex` / `Searcher` 门面 + 写缓冲 commit + 配置指纹（issue #1，见第 10 章） | 最小闭环 ≤15 行；brute 后端新旧逐位一致；CLI 参数输出一字不改 | P5 |
| **P7**（v2） | Reranker 接入（bge-reranker-v2-m3）、MMR 去重、token budget 裁剪、自研 HNSW、delta 分段（FR-17 完整版） | —                                                    | P6 |

**关键路径**：P1 → P2 → P3。这三步完成即具备完整检索能力，P4/P5 是工程化与验证。

### 12.2 阶段 → 需求覆盖矩阵

> 本矩阵反映 **V1（P0~P6）** 的需求归属；V2 需求（FR-26~33 / NFR-10~13）的步骤归属见
> `plan-v2.md` §5（2026-09-07 重排后编号）。

| 需求    | 阶段           | 需求    | 阶段          |
| ----- | ------------ | ----- | ----------- |
| FR-01 | P1           | FR-14 | P4          |
| FR-02 | P1           | FR-15 | P4          |
| FR-03 | P1           | FR-16 | P4          |
| FR-04 | P1           | FR-17 | P4          |
| FR-05 | P2           | FR-18 | P3          |
| FR-06 | P2           | FR-19 | P2          |
| FR-07 | P2（可选 feature） | FR-20 | P3       |
| FR-08 | P2           | FR-21 | v2（P7）      |
| FR-09 | P2           | FR-22 | P1，随阶段演进   |
| FR-10 | P3           | FR-23 | P1，P5 完善   |
| FR-11 | P3           | FR-24 | v2（P7）      |
| FR-12 | P3           | FR-25 | v2（P7）      |
| FR-13 | P3           | —     | —           |

| NFR     | 验证阶段 |
| ------- | ---- |
| NFR-01  | P5   |
| NFR-02  | P5   |
| NFR-03  | P5   |
| NFR-04  | P4   |
| NFR-05  | P5   |
| NFR-06  | P3（单测） |
| NFR-07  | P3   |
| NFR-08  | P0   |
| NFR-09  | P0（`make deny`），每个阶段末复查 |

> 覆盖矩阵的作用是**反向检查**：每一行需求都必须有归属阶段，否则就是漏做。

---

## 13. 测试策略

### 13.1 单元测试（必须有）

| 测试               | 断言                                                        |
| ---------------- | --------------------------------------------------------- |
| BM25 公式正确性       | 构造 5 篇已知文档，手工计算期望分值，逐条比对                                  |
| **索引侧/查询侧分词一致性** | 对同一文本，`analyze_doc` 与 `analyze_query` 输出的 term 序列完全相同     |
| RRF 融合排序         | 构造两路已知 rank，验证融合后顺序与 `w/(k+rank)` 累加一致                    |
| 相似度对齐            | 归一化向量经 `cos = 1 - d²/2` 换算后，与直接点积结果误差 < 1e-5              |
| 快照 round-trip    | save → load 后，全部统计量、postings、检索结果完全一致                     |
| 幂等 upsert        | 同一文档 upsert 两次，chunk 数不翻倍，检索结果不变                          |
| 删除后统计量           | 删除 chunk 后 `df` / `total_len` / `num_chunks` 与全量重建的结果一致   |
| 确定性              | 同一 query 连续检索 100 次，结果顺序完全一致                              |
| BGE 前缀           | 断言 `embed_query` 输出含 instruction 前缀（通过 mock 或对比不同输入的向量差异） |
| **BM25 与 tantivy 对照**（dev 基线） | 同语料下自研实现与 `tantivy` 的 Top-K **排序**一致率 ≥ 95%（分值允许口径差异）；覆盖边界：空 query、全停用词 query、超长 query、`df = 0` 的 term。见 ADR-007 |

> **"删除后统计量" 与 "BM25 手算值" 这两个测试最关键**：增量维护的统计量漂移和公式实现偏差，都是不会报错、只会让效果慢慢变差的问题，必须靠测试兜住。
>
> **tantivy 对照测试是前三者的补强**：手算只能验证 5 个样本，对照测试能覆盖整份语料与边界情况。三者叠加才敢说"BM25 是对的"。

### 13.2 集成测试

- 端到端：摄入 → 提交 → 快照 → 加载 → 检索，结果一致
- 并发：多线程并发检索（验证 `Send + Sync` 与 `Search` 对象复用安全）

> 效果评测（指标定义、对照实验、数据集要求）见 `requirements-spec.md` 第 9 章，本架构文档不重复定义。

---

## 14. 技术风险与应对

| #  | 风险                                          | 影响                                 | 应对措施                                                                  |
| -- | ------------------------------------------- | ---------------------------------- | --------------------------------------------------------------------- |
| R1 | ~~**`instant-distance` 无增量插入 API**~~ **（已消除）** | — | **D8 决策：已换 `hnsw_rs`（纯 Rust、原生增量 `insert`），delta 暴力区设计随之删除**，依赖 + `hnsw.rs` + `ImmutableIndex` 变体一并移除。残余：需物理删除时走 `usearch`（引入 C++ 依赖），见 7.5 |
| R2 | **BGE 查询侧前缀遗漏**                             | 向量检索效果显著下降，且**极难自查**（代码能跑、分数看着也正常） | `Embedder` 强制区分 `embed_query` / `embed_documents`；写单测断言；README 显著位置标注 |
| R3 | **fastembed 首次下载模型失败/极慢**（国内访问 HuggingFace） | 阻塞 P2 阶段                           | ① 支持 `HF_ENDPOINT` 镜像环境变量；② 支持指定本地模型目录；③ `remote-embed` 作为兜底路径        |
| R4 | **索引侧/查询侧分词不一致**                            | 召回率莫名偏低，排查成本极高                     | 默认实现强制 `analyze_query → analyze_doc`；单测断言两侧一致                         |
| R5 | **BM25 增量统计量漂移**                            | df / avgdl 逐渐失真，效果缓慢劣化，**不会报错**    | 增量累加 + 墓碑；单测对比"增量删除后"与"全量重建"的统计量是否一致                                  |
| R6 | **中文 avgdl 偏小导致英文经验参数失效**                   | BM25 排序质量不佳                        | P5 阶段网格搜索调参，不盲信 k1=1.2 / b=0.75                                       |
| R7 | **RRF 的 k 值选择**                             | 融合效果次优                             | k=60 为论文推荐值且鲁棒；若效果不佳，在 {20, 60, 100} 中对比                              |
| R8 | **OR 语义下长 query 噪声稀释**                      | Agent 的长问句中虚词干扰排序                  | BM25 的 idf 天然抑制高频虚词；若仍不足，v2 增加 query term 裁剪（按 idf 阈值）                |
| R9 | **`fastembed` 精确锁定 `ort =2.0.0-rc.13`（预发布）** | 依赖树不可浮动；ort 2.0 正式版 API 变动需等上游跟进   | ① **`Cargo.lock` 必须入库**（plan.md T0-03）；② 5.x 与 6.x 依赖相同，换版本无法规避；③ 极端情况下走兜底路径：直接用 `ort 1.16.3` + `tokenizers` 自研 BGE 推理（约 150 行） |
| R10 | **传递依赖 License 漂移**                       | 某个传递依赖换成 GPL 系列而无人发现，破坏"License 友好"约束 | 引入 `cargo-deny` 白名单校验并接入 CI/本地 `make deny`（ADR-008）                    |
| R11 | **filtered-ANN 低选择度召回不足** —— `hnsw_rs` 结构性行为 | 低选择度过滤查询返回少于 k 条 | 过采样 + `ef` 策略（path B）；`Metrics::vector_shortfall` 暴露；bench 出三元数据；**✅ V2.1 已给落点（2026-09-16）**：`plan-v2.md` Step 10 的 **T7-27**（prefilter / 排序键索引）+ 新 **FR-34**；⚠️ **先 spike 判「投 / 不投」**（判据 = `Metrics.vector_shortfall` 的分布） |
| R12 | **字段索引与正排 metadata 漂移**                      | 过滤结果静默错误 | 单一入口维护 + `import` 重建 + 等价性性质测试（T6/T7）+ 时序测试（T15） |
| R13 | **`search_filter` 在中/低选择度下必然整图遍历** —— 堆填不满 ⇒ 距离剪枝全程关闭（`hnsw.rs:1019`）。**库层机制，不是参数问题** | 带过滤 P99 随语料规模上升 | path B 的 `EF_FILTER_MAX=256` 只限高选择度宽度；中/低选择度无法避免——**接受并测量**（T12）；**✅ V2.1 已给落点（2026-09-16）**：同 R11 —— `plan-v2.md` Step 10 的 **T7-27** + **FR-34**，**先 spike 判「投 / 不投」** |
| R14 | **字段索引内存（高基数字段）**                          | 内存膨胀 | 基数保护（对 `terms+numbers` **合计**生效）；阈值可配（`Index::with_max_values_per_field`）；实测记入 NFR-05 |
| R15 | **无过滤热路径退化**：`search_filter` 会禁用 fast-return | 所有无过滤查询的 P99 与召回受影响 | path A 走普通 `search()` 保住 fast-return 与 `EF_SEARCH=200`；T14 强制验证 P99 与召回重合率 |
| R16 | **直连向量路 / BM25 路绕过存活过滤**（契约 `None = 不过滤`） | "Q-C1 已修复"在这些路径上被绕过而不自知 | 契约写进 trait rustdoc；T17 显式断言；`VectorBackend::Brute` 逃生舱文档标注 |
| R17 | **`hnsw_rs::search` 有固有近似误差** —— 即使 `knbn == len`、`ef=200` 也可能少返回（20 点图 200 次采样：191 次满 / 8 次少 1 / 1 次少 2）。**库层行为，不是过采样参数问题** | ① 存活过滤后仍可能凑不满 k；② 任何对"返回条数"的严格断言都可能 flaky | ① 过采样已含 `+k` 方差余量；② `Metrics::vector_shortfall` 暴露；③ **验收测试配比须让存活数 ≥ 2×K**；④ S1-10 在 10 万级语料测出真实量级：分路重合率 1.0000、平均条数 10.00，K=10 下未观测到可见缺口 |
| **R18** | **低选择度过滤查询的延迟爆炸**（S1-10 实测）。10 万级：选择度 1% → vector P99 **41.33ms**（7.1×），0.1% → **158.45ms**（**27.3×**），降级字段 Range → 124.26ms（21.4×）。机理是 R13 的整图遍历，但量级是**规模 × 选择度的复合**，非单纯线性随规模 | 带过滤查询在选择度 ≤1% 时**违反 NFR-02**（限额 10/20ms，超 4~16×）；10 万级上不可用于在线路径 | ① S1-10 时期**接受**——正确性优先于延迟（项目质量属性优先级），且优化前的 post-filter 在该选择度下几乎返回不了结果；② 由 `Metrics::vector_shortfall` 与 bench 三元数据暴露；③ **2026-09-07 复审（D-J9）已提前到 V2.0**：`plan-v2.md` Step 5（T7-22——**allowed 小于阈值时绕开 ANN 直接精确扫描**），并新增 **NFR-13** 把该场景纳入口径；④ **2026-09-10 Step 5 设计定稿（v0.3）**：**成本口径修正**——D-J9 写的「约 0.05ms vs 158ms」只覆盖「对 ~100 个候选算距离」一段，**不含 `O(N)` 遍历定位候选**（1~10ms，方案 A 的主要成本）⇒ 端到端口径必须计入；对策细化为**三条路径**（A 热路径一行不改 / B `filtered-ANN` 不变 / **C 精确扫描**，策略归后端 `prefers_exact`、记账归编排层），见 `v2-step5-design.md` §4 与 **§14.3**；⑤ **2026-09-10 S5-04 标定定稿**（实现已合并 `48c0ac7`）：阈值 **8192**（两端点实测 + 归一插值交叉点 ≈ 9700，原初值 1024 过保守）；10 万级端到端 P99 实测 —— `sel-1%` **5.01ms**（兜底前 39.49ms）、`sel-0.1%` **7.06ms**（前 112.69ms）、降级字段 `ts-range-degraded` **21.24ms**（前 223.24ms）；**NFR-13 定双口径**（常规档位 ≤20ms / 降级字段档位 ≤35ms，`eval-report.md` §8.9）。⚠️ **该对策只覆盖低选择度子集**：`allowed >` 阈值时仍回路径 B ⇒ 本机理与 R11 / R13 / R17 **不能标「已解决」**，只能标「**低选择度子集已绕过**」（设计文档 §4.4） |

> 项目级风险（工具链未安装、语料与标注依赖、模型域不匹配）见 `requirements-spec.md` 第 8、9 章。
> V2 Step 1 的完整风险表（含触发条件、残余风险、验收挂钩）见 `v2-step1-design.md` §10。
> V2 Step 2（图持久化）的 R19~R25 见下表，完整触发条件与残余风险见 `v2-step2-design.md` §9。
> V2 Step 5（查询性能与可观测）的 R31~R35 见 §14.3，完整触发条件与未决问题见 `v2-step5-design.md` §9。
> V2 Step 6（构建性能与多线程）的 **R36~R43** 见 §14.4 —— **R36 / R37 为条件性**（取决于 embed 并行 spike 的「投 / 不投」判定，**已判定「不投」⇒ 二者消解**），
> 完整触发条件与未决问题见 `v2-step6-design.md` §9。⚠️ 本 Step **不承诺**把 NFR-03 ① 从 225.5s 压到 120s（D-J8 已判定不可达）。
> ⚠️ **R11 / R13 / R17 / R18 不得标「已解决」**：Step 5 的精确路径只覆盖**低选择度子集**
> （`allowed ≤ 阈值`），`allowed >` 阈值时仍回路径 B ⇒ 只能标「低选择度子集已绕过」（设计文档 §4.4）。
> V2 Step 7（精排）的 **R44~R48** 见 §14.5，完整触发条件与未决问题见 `v2-step7-design.md` §9。
> ⚠️ **R44 与 R41 不是同一件事**：R41 是**建库**路径峰值 RSS，R44 是**查询**路径的**常驻**增量。
> ⚠️ **R47 的结论「测了才知道」**：探针若出反例**不必修**（数学上等价），但它**改变报告怎么读**
> （不同 `R` 的 `score` 不可逐位横比，只能比序数指标）。
> **V2.1 承接（2026-09-16 计划修订）**：**R34 → Step 8 的 T7-25**（精确扫描改逐层 `get_layer_iterator`，**必须先于 T7-06** —— 并发一开该路径即活，后果是**永久挂死**）、**R43 → Step 8 的 T7-26**（查询侧会话池 + 挂起的 Q3）、**R11 / R13 → Step 10 的 T7-27**（prefilter / 排序键索引，第一用例 = 高基数数值字段，**先 spike 判「投 / 不投」**）。
> ⚠️ **R11 / R13 / R17 / R18 仍未关闭**（上一条纪律不变）；⚠️ **V2.1 不新增风险编号** ⇒ **R49+ 由 `v2-step8-design.md` 起分配**（⚠️ 不要按「Step 6 = R36~R40」的老口诀从 R44 起算 —— Step 6 收尾已用掉 R43）。
> ✅ **V2.1 的编号分配已兑现**：**R49 ~ R54 见 §14.6**（2026-09-16，来源 `v2-step8-design.md` v0.1），完整触发条件与未决问题见该设计文档 §9。
> ⚠️ **R49 / R50 是「正确性」类、没有「修完就关」的时点** —— R49 的三条前提（全局精确整数统计量 / term 外层累加 / 段 ID 空间不重叠）**无法用测试穷尽**，靠 **S8-T4 的逐位一致断言 + 评审红线**守；R50 是**静默**正确性风险。
> ⚠️ **R51 / R54 是「等 spike」条目**（S8-S1 / S8-S2）⇒ **不预先承诺「投」**（Step 6 的 E2 教训：预先承诺会让「不投」的结论无法合并）。
> ✅ **V2.1 的编号分配已兑现（续）**：**R55 ~ R58 见 §14.7**（2026-09-23，来源 `v2-step9-design.md` v0.1）—— ⚠️ 其中 **R55（桶标签含 qrels ⇒ 标签泄漏）是「评测诚信」类**，与「产品缺陷」不同：它**不改变任何代码行为**，**但会让 `NFR-15` 的结论不可信** ⇒ 判据 **S9-3** 是它的唯一绊线。

#### 14.1 V2 Step 2 引入的风险（R19 ~ R25）

| # | 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- | --- |
| **R19** | **`hnsw_rs` 在损坏输入上 panic / `exit(1)`**（C3；读路径 12 处 `assert_eq!`/`unwrap()`，写路径 `DumpInit` 打不开文件即 `panic_any`） | 崩溃恢复场景下进程直接死 | 读：**五道先验**（manifest CRC + 长度 + 维度 + 平台 + `Description` 预校验）全过才交给 `hnsw_rs`；写：① 前置探测目录可写（第一道防线，管「可预判」）；② **`dump_graph_caught`（V2 Step 3 / S3-e 已落地）**：`catch_unwind` 把写路径 panic 降级为 `Err(VectorGraph)`，汇入 P0-3 语义链（Lenient 警告 + `PersistFailed` / Strict Err + best-effort 清理，D-S3-07），管「TOCTOU 残余」 | **写路径已收敛（2026-09-07）**。残余三条（`v2-step3-design.md` §4.6）：① `panic = "abort"` 构建下 `catch_unwind` 静默失效——本 workspace 由 Cargo.toml 注释钉死；**嵌入宿主是文档级约束**（crate 文档首页 + 本表），无编译期检测手段（库 profile 对宿主不生效）；② panic 先经默认 hook 打印 stderr（库内不 set_hook 污染宿主，接受噪音）；③ 读路径残余（校验后文件被并发改写）**不在收敛范围**——C5「无并发写同一快照」语义的边界，读路径 panic 时快照已加载、不涉及落盘一致性 |
| **R20** | **距离类型路径被烧进图文件**（C7） | 重命名/移动 `DistDotClamped` 会让旧图全部失效 | 用 `load_hnsw`（**短名**比对）而非 `load_hnsw_with_dist`；manifest 记**自有** `dist_id`，与 Rust 类型路径解耦 | 类型**改名**仍会失效（走降级重建，不丢功能） |
| **R21** | **平台/端序绑定**（C4）：图文件是裸 f32 + native endian | 快照拷到别的平台 → 图不可用 | manifest 记 `platform` 并校验，不匹配即降级 | 图 sidecar **不可跨平台搬运**是既定事实，需在用户文档声明 |
| **R22** | **磁盘体积增加约 60%**（C8；`file_dump` 只支持 `DumpMode::Full`，向量必然被复制一份） | 大语料下显著。**12K 实测**：快照 52.3MB + graph 7.9~8.1MB + data 23.7MB ≈ **84MB**（**1.6×**；graph 体积随 HNSW 拓扑在跨进程间有小幅波动，与 R-P5-13 同源） | 先接受并实测；`--no-graph-persist` 逃生舱；**Step 4** compaction 重写图时一并优化（2026-09-07 重排前写作 Step 5） | 未解决，V2.0 接受 |
| **R23** | **`HnswIo` 必须比 `Hnsw` 活得长**（`load_hnsw*` 的 `'a: 'b`）⇒ 只能 `Box::leak` | 长生命周期服务反复加载会累积（每次约 200B + 路径串） | leak 后**丢弃句柄、不存字段**（P1-2），避免与 `Hnsw` 内部指向 Mmap 的共享借用形成别名 | 无回收路径；**依赖 `HnswIo: Send + Sync`** —— 已加编译期断言，`hnsw_rs` 升级时需复核 |
| **R24** | **加载后增量插入的建图参数不同**（C8：重载后 `extend_candidates = true`，`Hnsw::new` 是 `false`） | 长期增量写入后图质量与纯内存建库存在偏差，可能影响召回 | S2-T10 覆盖；实测 oracle 重合率 | **残余应对已修正**：`Hnsw::set_extend_candidates(&mut self, bool)` 是**公开 API**（`hnsw.rs:853`），可在加载后显式对齐回 `false`；真正拿不到 setter 的是 `datamap_opt`（仅 `pub(crate)` getter）。故本项**可修**，待实测偏差决定是否实施 |
| **R25** | **每次 `save` 全量重 dump 图** | 频繁 save 场景成本高。**12K 实测 43.5ms**（31.6MB 写入 + CRC 扫两遍）——远低于预估，当前**不是**瓶颈 | 先不优化；预留 dirty 标记（`dumped_len == len()` 可跳过）的位置 | 未解决；量级需在 100 万级复核 |

---

#### 14.2 V2 Step 4 引入的风险（R26 ~ R30）

> 实现语义见 **§7.5.3**；完整触发条件与残余风险见 `v2-step4-design.md` §9.1。

| # | 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- | --- |
| **R26** | **compaction 期间内存峰值 ≈2×**（旧 Index / 图 + 新 Index / 图并存） | 100K 级可能 OOM | 先按正确性实现；缓解手段（正排原地重排、分步替换）待实测后决定 | **10K 档实测未触发**（`eval-report.md` §8.8）；100K 扩展验证未跑 ⇒ 大语料下**仍可能 OOM**，V2.0 接受 |
| **R27** | **ID 重编号破坏外部持久引用**（D-S4-01 的必然代价） | 场景层持有的 `doc_id` / `chunk_id` 在 compact 后失效 | D-S4-09 **三处声明**：`CompactionReport.remapped` 字段 + `SearchIndex::compact` 的 rustdoc + `user-guide.md` §1.5；`source` / `content_hash` 作稳定键 | 已缓解；**集成测试已覆盖**：存活 `source` 的专属词跨 compact 一一对齐、被删 `source` 零命中 |
| **R28** | **compaction 耗时**（重建图 O(N)：12K 量级 ≈10~11.5s，100K 预估 60~100s） | 长时间独占 `SearchIndex` | 手动触发（D-S4-02，自动默认关）；**无墓碑早退**（`remapped=false`，不重建图）；CLI 与 `CompactionReport.total_ms` / `vector_rebuild_ms` 可观测 | ⏳ **未实测**：`churn_bench` 的 CSV 列只含 `coldstart_ms`、**不含 `compact_ms`**，故 10K 档只证明了体积/状态达标、未量出耗时。采集途径已就位（`helix compact --json` 的 `compact_ms`，或 `CompactionReport.total_ms`）；100K 档待 `SIZE=100000 ./scripts/eval_churn.sh` 扩展验证 |
| **R29** | **重建图引入拓扑抖动**（NFR-06 口径不破，但与「增量建库」的历史评测基线不再逐位可比） | 评测可比性 | 文档说明 + oracle 重合率 ≥0.99 断言（S4-T4） | 已接受 |
| **R30** | compaction 期间**无法服务**（单写者语义，无并发读者） | 需运维窗口 | V2.0 接受；**在线 compaction 归 Step 8**（并发读写） | 已接受 |

---

#### 14.3 V2 Step 5 引入的风险（R31 ~ R35）

> **已完成并合并**（2026-09-10：实现 PR **#36** 已并入 main `48c0ac7`；**S5-04 标定定稿 + S5-08 文档回写**
> 随设计文档 `v2-step5-design.md` **v0.5** 落地）。完整触发条件见设计文档 §9 —— **Q1~Q5 全部结案**
> （Q3/Q4 由评审结案；**Q1 / Q2 / Q5 由 S5-04 标定回填**）。

| # | 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- | --- |
| **R31** | **精确扫描的 `O(N)` 谓词判定项**（方案 A 的固有成本；设计文档 §4.2.4 第①段） | 该项随规模**线性增长**（而高选择度下路径 B 由 `ef` 封顶）⇒ 规模变大时交叉点**下移**，可能重新超过路径 B | 阈值可配 + `vector_route` 可观测；方案 B（枚举 `allowed` + `origin→PointId` 映射，纯 `O(allowed)`）留作逃生舱 | ✅ **已标定**（S5-04，10 万级）：**全档位达标**（最差 22.84ms，见 §14.3 上方与设计 §4.9）。⚠️ **规模上界未探明**——只在 10 万级（NFR-01 上限）标定，**100 万级未测**；若未来扩容须重标。方案 B **已判定不做**（Q5 结案） |
| **R32** | **兜底改变向量路输出**（近似 → 精确）：同一 `(query, 谓词, k)` 的结果不再与历史 ANN 基线逐位可比 | 与既有 ANN 评测基线不再逐位可比（性质同 Step 4 的 R29） | 文档声明 + `vector_route` 可见 + `--brute-fallback off` 可复现旧行为（A/B 对照） | 已接受。**范围受限**：只在 `Filtered` 且 `allowed ≤ 阈值` 的档位发生 |
| **R33** | **阈值是经验值**：选择度分布随字段基数 / 谓词组合剧烈变化，`allowed` 在阈值附近时性能台阶切换 | 阈值附近（实测 `allowed ∈ [5043, 9946]`）的档位收益不稳 | 设计文档 §4.9 标定曲线 + 可配（`Option<usize>`，`None` = 关闭）+ 指标暴露；文档写明「阈值是**性能**决策点，不是**正确性**边界」 | ✅ **已标定**（S5-04）：**定稿 8192**（原初值 1024 **过保守**，会漏掉 `allowed=5043` 这个实测 2.06× 的档位）。交叉点插值 ≈ 9700（**归一口径** = 9768；未归一插值 ≈7.2K~8.1K，对两轮漂移敏感），取 2¹³ 低于归一插值 ~15%、低于首个实测反转档位 9946。⚠️ 台阶仍在——区间内**未逐点扫描**，只测了两个端点 |
| **R34** | **精确扫描全程持 `points_by_layer` 读锁，且含潜在死锁**。① 持锁：`IterPoint::new` 构造即 `read()`（`hnsw.rs:631-641`），迭代期间不释放；② **递归读**：`IterPoint::next` 在**层切换**时**又取一次**（`hnsw.rs:661`）⇒ 同一把 `RwLock` 被**递归读**。`std::sync::RwLock` 官方不保证递归读锁可重入（其 futex 实现 `is_read_lockable` 还要求 `!has_writers_waiting`，写者优先） | 并发检索间**互相不阻塞**（读锁共享）；但（a）与**写端**（`insert` 取写锁）互斥 ⇒ 持锁时长随 `N` 线性增长，是写端的长时间阻塞源；（b）⚠️ **若恰在层切换那一刻有写者在等待，第二次读锁可能永久阻塞 ⇒ 挂死**（比"变慢"严重一个量级）。15 行 std-only 探针实测 **3/3 触发**（`exit=42`），时序敏感但可复现 | ① V2.0 单写者语义下**不可达**（Q-U1：`HnswRsIndex::add` 要求 `&mut self` ⇒ 读与写不可能重叠）；② **Step 8（读写并发）开工前**必须把遍历换成设计 §4.2.2 已写明的**逐层 `get_layer_iterator` 写法**——每层一个独立 `IterPointLayer`、层间 guard 不重叠 ⇒ **无递归读**，覆盖等价（层 `0..=entry_point_level`，一样每点恰一次）、成本同量级 | 已记录，**归 Step 8**；**未修**（措辞已于 2026-09-10 S5-08 由「与写端互斥」改写为「含**潜在死锁**」，来源：#36 评审 F3）。**⇒ 2026-09-16：已落为 `plan-v2.md` Step 8 的 T7-25（硬前置，必须先于 T7-06）** |
| **R35** | `SearchResponse` / `VectorIndex` 的**对外破坏性变更**（D-S5-05 加字段、D-S5-01 加**必选**方法） | 外部字面量构造 `SearchResponse`、外部 `VectorIndex` 实现会编译失败 | CHANGELOG `⚠️ 破坏性` 段 + rustdoc 迁移说明；0.x 阶段可接受 | 已接受（落在语义化版本 0.x 约定内） |

#### 14.4 V2 Step 6 引入的风险（R36 ~ R43）

> **状态：实现完成 + S6-10 收尾回写**（`v2-step6-design.md` **v0.4**，2026-09-13；D-S6-01~09 全部结案）。
> Step 6 的实现 PR 链：实现 PR **#38**（S6-01~03）→ **#40**（spike 判定）/ **#43** / **#44** 均已并入 `main`。
> ⚠️ **R36 / R37 是条件性风险**——只在 E2（多 session）/ E3（CoreML EP）经 spike 判定「投」之后才实际生效；
> **spike 已判定「不投」（T7-09，PR #40）⇒ 二者消解**。R38 / R39 已**关闭**（测试上锁），R40 **已上锁但残余仍在**。
> **S6-10 新增 R41 / R42 / R43**（见下表）。
> 完整触发条件与未决问题见 `v2-step6-design.md` §9；NFR-10 的实测读数见 `eval-report.md` §8.12~§8.13。

| # | 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- | --- |
| **R36** | **多 session 抬高内存峰值**（E2）：N 份 ort session 的 arena / 工作区，`intra_threads` 按 session 数切分后总占用可能高于单 session | NFR-05（峰值 RSS 372MB）被顶穿；小内存机器 OOM | spike 必录峰值 RSS（S6-01）；决策门含「峰值 RSS 增量 ≤ 20%」（D-S6-01），不达标即不投 | ✅ **已消解**（2026-09-12，PR #40）：spike 判「**不投**」（E2 收益远低于 +30% 门槛而 RSS **+96.4%**）⇒ 该路径不落地，风险不生效。⚠️ 该「不投」**只对建库侧**成立，**不可外推到查询侧**（见 R43） |
| **R37** | **E3（CoreML EP）改变向量数值**：EP 改变算子实现 ⇒ 浮点归约顺序变 | ① 所有既有效果指标（MRR / NDCG）与 P5 基线**不可横比**；② 快照的"语义"变了（同模型不同 EP ⇒ 向量不同） | ① D-S6-04：把 EP 编码进 `embedder_id` ⇒ CPU 建库 / CoreML 加载报 `ConfigMismatch` 而非静默错配；② 设计 §3.5 要求**显式声明不可横比**；③ E3 落地单独立决策（D-S6-01） | ✅ **已消解**（2026-09-12，PR #40）：E3（S6-05）随 spike「不投」**取消**，该降级路径不落地。⚠️ 若未来重开 EP，本条**原样复活**，须连带重跑 P5 效果基线 |
| **R38** | **`default_embedder()` 静默降级**（`search/config.rs:312-318` 的 `.ok()`）：`LocalEmbedder::new()` 失败 ⇒ `None` ⇒ 索引变成**纯 BM25**，**无错误、无警告** | 静默的功能缺失——最坏的一类（"要了向量却拿到纯文本"，且不知道坏了） | D-S6-05：`--vectors` 显式请求向量时**硬失败**（方案 A）；测试 S6-T7 断言"要么 `Err`、要么可观测标志为真，绝不静默" | ✅ **已关闭**（S6-09）：`--vectors` 硬失败已落地；S6-T7 上锁（`step6_incremental_build.rs`） |
| **R39** | **追加路径漏发图 manifest**：`build --index` 追加产生新点 ⇒ 图 sidecar 的 `nb_point` / `graph_crc` 全变；若 save 链路未重发，则新图永远匹配不上（**与 Step 4 的同一坑**） | 每次冷启动都走降级重建（≈10s），NFR-04 **静默失效** | S6-T13 显式断言：追加后冷启动 `GraphStatus == Loaded`（非 `Rebuilt`）且 manifest `nb_point` 与新增 chunk 数吻合 | ✅ **已关闭**：S6-T13 已落地并上锁（追加后冷启动 `GraphStatus == Loaded`） |
| **R40** | **ID 复用 ⇒ 幽灵向量**：`raw_vectors` / 图 sidecar 按 `chunk_id` 索引；若追加时新 ID 与历史 ID 重叠，旧向量会挂到新 chunk 上（正确性缺陷，与 Q-C1 同族但**成因相反**） | 静默的错误召回 | 设计 §4.5.2 给出"今天不冲突"的证明（`insert_*` 用槽位长度分配 + `export/import` 保留尾部空洞）；S6-T8 写成**不变式测试**锁住 | ⚠️ **已上锁，残余仍在**：S6-T8 不变式测试已落地；但证明仍依赖三条隐含前提（尾部空洞保留 / 导入不压缩 / compaction 同步重写 `raw_vectors`）⇒ **靠测试锁，不靠文档**。前提若被打破（如引入 compaction 压缩尾部）须重审 |
| **R41** | **建库路径峰值 RSS 远超检索路径预算**（S6-10 登记）：`build` 的编码 batch = 64 且按**长文本**切分 ⇒ 峰值 RSS 实测 **2.32 ~ 3.33 GB**；而 NFR-05 的 372MB 是**检索路径**（batch 1、单条查询）口径 | 用户按 NFR-05 的 372MB 预期选机器 ⇒ **建库时 OOM**（差一个数量级）；两处口径若混用会得出"已达标"的错误结论 | ① 需求侧 NFR-05 **补限定词**「口径 = 检索路径（batch 1）」（Q8 方案 C，已回写 `requirements-spec.md` v1.15）；② 本文档登记为独立风险；③ 预算按 `batch × 序列长度` 给，**只给形态不给数** | ⏳ **长期开放**：建库侧尚无独立 RSS 预算（NFR-05 明确**不并入** 372MB 基线）。⚠️ 未来若要给建库设预算，须先定 `batch` 与序列长度的取值口径 |
| **R42** | **依赖安全公告漂移**（S6-10 登记）：`bincode`（**RUSTSEC-2025-0141**）+ `paste`（**RUSTSEC-2024-0436**）均被 `cargo deny check advisories` 报出，且**均无升级路径** | 依赖链带未修复公告 ⇒ 若下游安全扫描严格，会阻断集成；供应链审计通不过 | ① ADR-005 §7.6 + `p4` R-P4-4 已记录 `bincode` 迁移待立项；② `paste` 为**传递依赖**，本 crate 无法消除（宏依赖，无替代版本）；③ CI 侧已按 RUSTSEC ID **精确豁免**（`deny.toml` 的 `[advisories] ignore`，两条各带**核实日期 2026-09-12**、依赖路径与**解除条件**）—— 豁免**显式留痕、非静默**（PR #41 `cb7371e` 落地）；`[licenses]` 白名单与公告无关，**不是**本条的缓解手段 | ⏳ **长期开放**：无升级路径 ⇒ 只能等上游或重构移除。⚠️ `bincode` 迁移**单独立项**（不属 Step 6） |
| **R43** | **查询侧编码把并发读吞吐封顶**（S6-10 登记，**NFR-10 ③**）：`crates/core/src/embed/local.rs:35` 的 `Mutex<TextEmbedding>` ⇒ `embed_query`（`:73-79`）**每次查询 `lock()`**，把多线程检索串行化；含查询侧编码的端到端路径（vector / hybrid）并发加速比**实测不达标** | NFR-10 ② 对**含查询侧编码**的路径判「未达标」：vector `QPS(4)/QPS(1)` = **1.88×**、hybrid = **1.63×**，均 < 判据 **2.5×**。⚠️ 纯 BM25（无查询侧编码）**2.98× 达标** ⇒ 瓶颈在编码而非检索内核 | **决定性对照**（S6-10，`eval-report.md` §8.13）：临时把 `embed_query` 移出被测路径 ⇒ vector t=4 **1.88× → 3.15×**、t=8 **1.87× → 4.35×** ⇒ **阈值可达，卡在实现**（不是内存带宽 / 分配器争用）。**NFR-10 保持「打开」不关闭**；复审触发条件 = **查询侧会话池（每 worker 独立 session）落地时**，届时须**先答挂起的 Q3**（不同 `sessions`/`intra_threads` 是否改变向量数值——若改变会破坏 S6-T11 的逐位一致前置条件） | ⚠️ **保持打开**：本轮**不动生产代码**（纯文档收窄 + 登记）。需求侧 NFR-10 已改「三部分」并**收窄 ②** 的适用范围为「不含查询侧编码的读路径」、写明 ③ 与复审触发条件（`requirements-spec.md` v1.15，P3-8 措辞更正）。**⇒ 2026-09-16：该触发条件已落为 `plan-v2.md` Step 8 的 T7-26（查询侧会话池 + 挂起的 Q3）；⚠️ 本条保持打开，不因「已排期」而关闭**<br>🔴 **2026-09-22 判后记录（`S8-09` / 第 1 轮评审 P3-1 补齐）**：**上述复审触发条件已触发并已回答** —— Q3′（实例数 / `intra_threads` 是否改变 `embed_query` 向量**数值**）的答案 = **都不改变**（设计 §4.11.4 判据①，含 `intra_threads` 的非生产路径对照）；而 **spike S8-S2 判「不投」**（判据② **2.10× < 2.5×**、判据③ **+148.4% ≫ 20%**）⇒ **本条保持「打开」**。🔑 两点必须与结论一起读：① 池化**确有增益**（未池化对照 1.23× → 池化 **2.10×**）⇒ FAIL **不得**读成「池化无效」；② 判据③ 的阈值在查询侧口径下**结构性不可达**（≈ 97 MiB/槽）⇒ 口径本身进复审。⚠️ **「已排期 / 已落地」都不是关闭理由**；将来若翻转（例如把 ③ 的阈值改成查询侧口径、或分核切分 `intra_threads`）⇒ **须重新打开本条**并附新读数。 |

---

#### 14.5 V2 Step 7 引入的风险（R44 ~ R48）

> **状态：实现完毕 + 收尾完成（S7-05，2026-09-16；设计 `v2-step7-design.md` v0.3）**。
> 📌 **锚点写法（本步定的规则，后续步骤沿用）**：**引用本仓源码用「符号指代」**（`文件` + 符号名，如「`bench.rs` 的 `--single-chunk` 处注释」），**不写行号** —— Step 7 的三次实现 PR **每次都让定义面的行号锚点漂移**（S7-03 一次 10 处、S7-04 一次 5 处），改成符号后从根上消掉这一类。**依赖源码**（`fastembed` / `hnsw_rs`）可**两者都写**：符号名 + 「v 版本下 `:行号`」的**带版本锚点**（依赖版本锁死 ⇒ 不会悄悄漂）。
> ⚠️ **规则的适用范围（2026-09-16 补记 —— 免得它被读成「已经全仓改完」）**：**新写的锚点必须符号化**；**存量裸行号在该行被改动时一并转换**（S7-05 + PR #57 评审响应已按此转 **11 处**：`plan-v2` 的 H3 行 / §4.1 两行 / §附-1、`architecture-design` §5.7、`requirements-spec` 的 NFR-12 行与头部注、`eval-report` 两处、`v2-step7-design` 的 H3 行）。⚠️ **同轮清点**：6 个活文档共 **121 处**行号锚点（指向本仓源码约 **60 处**），**其余未逐一改写** —— ① `v2-step7-design.md` 附录 D 是**刻意的锚点台账**（已就地加「设计期时点」标注 + 指向符号指代）；② 其余多属 Step 1~6 引入、**本轮未逐处复核实测漂移**（已登记：`architecture-design.md` §14.3 的 3 处、`plan-v2.md` 的 2 处、`v2-step7-design.md` 正文的 6 处）⇒ **全仓锚点符号化是一次独立的机械重构，建议单独立项**，不要在评审响应里顺手做（会把 diff 撑到无法评审）。
> ⚠️ **本步无 trait 破坏**——`Reranker` 只**新增 provided 方法** `candidate_window`（默认 `k`）⇒ 自定义实现**一行不改**；
> **`Explain` 加字段属对外破坏性变更**（R35 一族，D-S7-05）；`Metrics` 加两字段是**纯加法**、保持 `Copy`。
> ⚠️ **R44 与 R41 不是同一件事**（建库 vs 查询 / 峰值 vs 常驻）；**R45 包含「窗口被 `candidate_k` 静默夹取」这一空转陷阱**。
> 完整触发条件与未决问题见 `v2-step7-design.md` §9；NFR-12 的标定协议与决策门见其 §4.6。

| # | 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- | --- |
| **R44** | **精排器常驻内存远超检索路径预算**：`bge-reranker-v2-m3` FP32 权重 **≈2.19 GB**（`model.onnx.data` **2,271,088,656 B**，sha256 已核 `84b66c78…8945`）+ 激活 | NFR-05 的「峰值 RSS **372MB**（检索路径，batch 1）」**在精排开启时失效**（差一个数量级）⇒ 用户按 372MB 选机器会在开启精排后被 **OOM kill** | ① 需求侧 NFR-05 补限定词「**不含精排器**」（同 Step 6 的 Q8 方案 C 先例，已回写 `requirements-spec.md` **v1.16**）；② 精排**默认关**（D-S7-04）⇒ 不开就不付；③ **S7-04 实测峰值 RSS —— 已于 2026-09-16（S7-05 收尾）补测并落 `eval-report.md` §8.14.10**：检索路径 / 冻结图 / 含模型加载 / 3 次取中位 ⇒ 峰值 RSS 由「精排关」的 **403.7 MiB** 升到「精排开（`R = 20`）」的 **3110.5 MiB**（min 2959.1 / max 3488.9）⇒ **增量 2.64 GiB、7.71×**，且该增量是模型文件体积（2.12 GiB）的 **1.25×**（ONNX Runtime 运行时 + 单 batch 激活 + tokenizer） | ⚠️ **数值已补、条目保持开放**：**实测形态参考**（⚠️ **不写成「≥」型不变式**）=「精排开启后检索路径峰值抬升 ≈ **模型体积的 1.2~1.4 倍**（**中位口径 1.25×**；min 臂 **1.18×**、max 臂 **1.42×** —— 3 样本、开臂离散 ±9% ⇒ 单机读数不足以支撑「下界」）、相对**同一 harness** 的精排关臂（**403.7 MiB**）≈ **7.7×**」；⚠️ **NFR-05 的 372MB 属另一 harness、不可直接横比**（硬比 `3110.5/372` ≈ **8.4×**，仅作量级参考）；**仍不给精排侧的独立预算上限**（该上限应由用户的并发度 / 量化选型决定，与 R41 同族）。⚠️ 与 R41 的区别：R41 是**建库**路径峰值，R44 是**查询**路径**常驻**增量 |
| **R45** | **精排延迟主导 + 窗口静默封顶**：① 窗口放开后成本 ≈ `O(R)` 次 cross-encoder 前向，若默认开端到端 P99 从 ~3ms 涨到**百毫秒级** ⇒ NFR-02 **名义失效**；② ⚠️ **只改 `take(k)` 而不联动 `candidate_k`** ⇒ `R > 3k` 时窗口**被静默夹到 `3k`**（R=100 在 k=10 时退化成 30）⇒ **「窗口已放开」的结论建立在空转上** | 交互体验（NFR-02 / Agent 关键路径几十次调用） | ① 默认关（D-S7-04）；② **NFR-12 独立口径**（不并入 NFR-02）；③ 默认 `R` 由标定（设计 §4.6.4）决定；④ **候选池联动写进 rustdoc 不变式**（`candidate_k = max(3k, w, 10) ≥ take_n`）+ S7-T3 断言「间谍向量后端收到的 `k` ≥ `window`」；⑤ 精排器与查询侧编码共享 `Mutex` 串行瓶颈（同 R43）⇒ **明确不做池化** | ⚠️ **保持开放**：`R` 与延迟的取舍是**用户侧**决策；本 Step 只给「可配 + 有数据」 |
| **R46** | **`Hit.score` 的语义随开关而变**（融合分 → `σ(logit)`），且 `hits` 的**并列次序**规则随之改变 | 下游若用 `score` 设阈值 / 跨配置比较 / 缓存排序结果 ⇒ **静默失准**；与 R35 一族（对外可观测的破坏性变更） | ① `Explain.rerank_score` 让变换**可观测**（D-S7-05，NFR-07 精神）；② CHANGELOG 显式 `⚠️ 破坏性`；③ `Hit.score` 的 rustdoc 重写为「**当前排序依据**」并写明三种 mode × 精排开关的取值 | ⚠️ **识别成本仍在用户侧**：内核只能说清「这次 `score` 是什么」，无法阻止下游误用 |
| **R47** | **精排数值的可复现性未证实**：tokenizer 用 `PaddingStrategy::BatchLongest`（`fastembed` 的 `PaddingParams` —— `src/common.rs` 里 `.with_padding(PaddingStrategy::BatchLongest)` 处；v6.0.2 下 `:174-180`）⇒ **同一 `(query, doc)` 在不同批次组成下 padding 长度不同** ⇒ 浮点结果**可能不逐位相同**；ONNX 多线程归约亦然 | ① NFR-06（相同 query → 完全相同结果）**可能不成立**；② 标定实验的**分数轴不可横比**（只能比序数指标） | ① 设计 §4.7 三个层次探针 **P1/P2/P3**（S7-T8 / T9）；② 设计 §4.6.3 的「不可横比声明」；③ 若 P1/P2 出反例 ⇒ NFR-06 加限定词「**不含精排**」并**升需求版本** | ✅ **已全部实测（2026-09-16，S7-05 收尾；`eval-report.md` §8.14.11）**：**P1**（同进程 3 次）/ **P2**（跨进程 2 次）⇒ (`chunk_id`, `score`) **逐位一致**；**P3**（单条 vs 批内）⇒ 原始 logit **相同**（`2.2603965`）。⚠️ **P3 是单一样例、单一批组成** ⇒ **不构成**「跨 batch 组成恒逐位一致」的证明 ⇒ **NFR-06 判据仍只到「同一快照两次加载逐位一致」**（不得升级）。P3 的反例**不必修**（数学等价），但**改变报告怎么读** |
| **R48** | **512 token 截断使长段落的精排判据失真**：`HasMaxLength for RerankerModel = 512`（`fastembed` 的 `impl HasMaxLength for RerankerModel { const MAX_LENGTH: usize = 512 }` —— `src/reranking/init.rs`；v6.0.2 下 `:17-19`），而 T2Ranking 段落最长 **76,895 字符**（`crates/cli/src/bench.rs` 的 `--single-chunk` 处注释「200K 字符 > 语料最长段落（76,895）」）⇒ 精排**只看得到段落前段** | **可能把「方法不行」错判为「精排无提升」**（**结论错、而代码没错**）；反过来若调大 `max_length`，成本近似**平方**增长 | ① D-S7-08（保持 512 起步 + `1024` 对照档）；② 设计 §4.6.4 的判据②明写「拐点不可识别时要**如实记入局限**」；③ `max_length` 进精排器**身份字符串**（`id()`，D-S7-08 / 设计 §4.4.4）；④ **标定已完成**（读数见残余列） | ✅ **收窄后关闭（2026-09-16，S7-05 收尾）**：**在本评测集上**（T2Ranking 12K / `max_length = 512` 截断）**截断不是「精排无提升」的主因** —— `max_len 512→1024` 的增益 **≈0**（ΔNDCG **+0.0002**、ΔMRR +0.0057；逐条仅 **7/60** 变化、符号检验单侧 p = 0.5；读数 `eval-report.md` §8.14.7）。⚠️ **三条限定**：① **产品侧目标场景**（Agent query / 短 chunk）**未测、不外推**；② `max_length = 1024` 档**未测延迟**（「成本近似平方增长」是先验）；③ 若将来为评测集单独调大 `max_length`，**按本节判据重测** |

#### 14.6 V2 Step 8 引入的风险（R49 ~ R54）

> 来源：`v2-step8-design.md` **v0.1**（2026-09-16，待评审）。**编号由该设计文档起分配**（§14 导读钦定「V2.1 从 R49 起」）。
> ⚠️ 本步**不引入 trait 变更、不改任何既有 trait 签名**（五 trait 一字不改）。公开面 = **加法**（`SearchIndex::searcher(&self)` / `merge_pending()` / `merge_all()` / `MergeReport` / `Config::embed_sessions` / `Metrics.segments` / `Metrics.tombstoned`）
> + **两处待实现期核实的破坏性**（`VectorRoute` 新增变体是否需要 `#[non_exhaustive]`、`Config` 是否已有字面构造用法）—— ⚠️ **设计期不凭记忆下结论**（Step 7 曾凭空造出 `rerank_status` 字段的教训）。
> ⚠️ **R49 / R50 是「正确性」类，R51 ~ R54 是「性能 / 资源」类**：前者靠不变式 + 断言守（**没有「修完就关」的时点**），后者**都有 spike 出口**。

| # | 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- | --- |
| **R49** | **跨段 BM25 的「逐位一致」依赖三条前提**：① 全局统计量必须是**精确整数和**（`N` / `total_len` / `df(t)`）；② TAAT 累加必须**term 外层、段内层**；③ 段与段的 ID 空间**不重叠**（`base = 前序所有段长度之和`）。任一被破坏 ⇒ `score` 漂移，且**只在分段布局下出现** | 结果「看起来对」而分数已悄悄改变 ⇒ **后续所有质量对比失去可比性**（P5 锚点 `hybrid MRR@10(1) = 0.6922` / Step 7 的 `R = 20` 标定 / NFR-12 的 8.8s / 10.9s 全部建立在同一套打分上）；且**漂移只在分段布局下出现** ⇒ 极难归因 | ① 三条前提写进 rustdoc 不变式（设计 I8-5 / I8-6 / I8-7）；② **S8-T4** —— 同内容以「**单段**」与「**主段 + 2 个增量段**」两种布局建库，`bm25` 的 `(chunk_id, score)` **逐位相同**（**本设计最重要的一条测试**）；③ `Index::merge_from` 的 posting 顺序写成注释（**不得「顺便排序」**） | ⏳ **开放**（**结构性约束、无法用测试穷尽**；靠断言 + 评审红线守） |
| **R50** | **「合并保 ID」与 `compact()` 的「重编号」语义冲突**：`merge` **保** ID（设计 D-S8-04 的 FIFO 追加式基址）、`compact` **改** ID（FR-30 的既有行为） | 若合并实现偷懒复用 `Index::compacted()` ⇒ **用户侧 `chunk_id` 静默失效**（Agent 记忆里的 ID 指到别的 chunk，且不报错）；与 **R27**（ID 重编号破坏外部持久引用）同族，但**成因是内核自己搞混两套语义** | ① **D-S8-12** 把两者显式分离并写进文档（「**合并保 ID、回收改 ID**，两者不可混用」）；② `compact()` 前**强制 `merge_all()`**；③ `CompactionReport` 注记补「compact 后 ID 会变」 | ⏳ **开放**（**静默**正确性风险，靠文档 + 评审） |
| **R51** | **向量侧的合并成本与拓扑不确定**：`hnsw_rs` **无图合并 API** ⇒ 只能「dump 旧主图 → load → 增量 `insert` delta 点」（⚠️ 带出 **R23** 的 `Box::leak` + **R22** 的 1.6× 体积 + 临时 I/O）或「从 `raw_vectors` **全量重建**」（ADR-A 允许，≈ **38.7s/12K**） | 若「dump→load→insert」不可行而必须走全量重建 ⇒ **小 delta 也要付 O(N·logN)** ⇒ 合并频率被迫降到很低 ⇒ **delta 段堆积 ⇒ 读路径随段数变慢**（FR-17 的收益被吃掉） | ① spike **S8-S1** 的决策门（设计 §4.9.3）；② `MergeReport.vector_strategy` **如实记录走了哪条**（**不得静默降级**） | 🟡 **2026-09-22 回填（`S8-09`）**：spike S8-S1 **5 条判据全 PASS**（A/B = **0.134**）⇒ **方案 A 已落地**（`MergeReport.vector_strategy` 如实记录）。**残余收窄为**：① `HnswIo` 每次合并泄漏 **≈200B + 路径串**（10 次 ≈ 2 KB，见本列的 2026-09-17 两条）；② **图拓扑仍会漂移**（A 保住旧点拓扑，但 `insert` 新点后的 ANN 排名**不承诺逐位一致**）⇒ **保持开放** |
| **R52** | **热路径回归：`bm25_f` 从 `None` 变 `Some`**：单段模型下无过滤查询可传 `None` 走零谓词快路径；引入 delta 段后，跨段墓碑的存在使 postings **未被物理摘除** ⇒ 必须对**每条 posting** 求一次谓词（`dyn CandidateFilter::contains()`） | 无过滤查询的 BM25 lane **常数项变大** ⇒ 设计 §1.3 第 2 条「**零回归**」（`deltas` 恒空时逐位一致）之外的场景出现延迟退化 | ① 「**无 delta 且无墓碑**」时**显式短路回 `None`**（保住今天的热路径）；② **无墓碑但有 delta** 时用**逐段 `AliveOnly`**（零构建成本）；③ **S8-T12** 量化三档读数（无 delta / 有 delta 无墓碑 / 有跨段墓碑） | 🟡 **2026-09-22 已量化（`S8-09`）**：`Metrics.bm25_elapsed` 四档 p50 = 档1（主段）**44 us** → 档2（有 delta）**52 us** → 档3（+ 跨段墓碑）**53 us** → 档4（`merge_all()` 后）**50 us**；⇒ **逐 posting 谓词的成本仅 +1 us（+2%，档3−档2）**、**多段扫描 +8 us（+18%，档2−档1）**；档 4 回到 `segments = 1` / 墓碑 0（**可逆**）。⚠️ **档 4 ≠ 档 1**（其语料多 delta 那 400 篇）⇒ 「回到第 1 档量级」读作「回到 1 段的形态」。**保持开放**（是读数不是门槛） |
| **R53** | **视图指针锁与「写者优先」的交互**：`std::sync::RwLock` 在 macOS / futex 实现下，**写者等待会挡住新读者**（⚠️ **与 R34 是同一个机制**）⇒ 若写者持写锁做了重活，读端 P99 会出现尖刺 | **NFR-14 ②**（合并期的读延迟判据）失效 —— 「读不阻塞写」**名存实亡** | ① **I8-1**「**写锁只用于指针替换**」写成 rustdoc + **评审红线**；② **S8-T11** 直接测「**合并期 ÷ 静止期**」的 `took` P99 比值（判据 **②a 相对 ≤ 1.2（拟）** + **②b 绝对护栏**：仍在 NFR-02 预算内） | 🟡 **2026-09-22 已测（`S8-09`）**：`S8-T11` 三臂（A1 静止期前 / B 合并期 / A2 静止期后，**全程同一份内存图**）三次运行：②a = bm25 **1.107 / 1.049 / 1.063**、hybrid **0.992 / 0.927 / 0.993**（基线 = A1）⇒ **判据口径已定稿（去「拟」）**；⚠️ **但尾部扰动不可复现**（B 的 `max` = 8.5 / 40.8 / 23.5 ms vs A1 = 15.1 / 4.1 / 3.5 ms，**一次反向**；同深度第 4 大一次达 **2.74×**）⇒ **本轮不把尾部当判据** ⇒ **保持开放**（复审触发条件见设计 §4.13.1） |
| **R54** | **会话池的内存（R36 的查询侧背面）**：每 worker 一个 `TextEmbedding` 实例（⚠️ fastembed **无 `sessions` 参数** —— `InitOptions` 只有 `intra_threads`）⇒ N 份 `Session` + `Tokenizer` 各自**常驻** | 用户按 NFR-05 的 **372MB** 选机器 ⇒ 开启池化后 **OOM**（与 **R44** 同族，但 R44 是**精排**、本条是**查询侧编码**） | ① **默认 `embed_sessions = 1`**（零代价，**不开就不付**）；② spike **S8-S2** 按**查询侧口径**实测峰值 RSS（⚠️ **不得引用 E2 的建库口径读数** —— 那是 4000 段 / batch 64 / 长文本，见设计 §2.8）；③ 决策门含「**RSS 增量 ≤ 20%**」 | ✅ **2026-09-22 消解（`S8-09`）**：spike S8-S2 **判「不投」**（判据③ 峰值 RSS **+148.4%**、判据② `QPS(4)/QPS(1)` **2.10× < 2.5**）⇒ **池化不进生产默认路径**（`embed_sessions` 默认 1）⇒ **本风险的条件未被触发**。⚠️ 是「**因为判『不投』**而不发生」，**不是**「没人碰」——若将来结论翻转（或用户显式 `embed_sessions > 1`），本条须**重新打开**，并把「判据③ 的阈值在查询侧口径下对任何 N ≥ 2 结构性不可达」一并重估（设计 §4.11.4） |

⚠️ **`S8-09` 收尾已按实测回填（2026-09-22）**：**R51 / R52 / R53 保持开放**（分别因「图拓扑仍漂移」/「是读数不是门槛」/「尾部扰动不可复现」），**R54 记为「消解」**（因 spike S8-S2 **判「不投」** ⇒ 条件未触发 —— 不是「没人碰」，将来翻转须重新打开）。**R49 / R50 仍不标「已解决」**（正确性类、**没有「修完就关」的时点**）。
⚠️ **编号归属纪律（2026-09-17，#60 评审 P4-7②）**：**R49 = 跨段 BM25 的三条前提**、**R52 = `bm25_f` 热路径**；
「逐段判 `prefers_exact` ⇒ 小段更易命中低选择度兜底 ⇒ `vector_route` / `vector_shortfall` 语义变化」**不属于这两条**，
而是**路由 / 阈值的可观测语义** ⇒ 归 `v2-step8-design.md` 的 **Q5 + §4.13.3**（不新开风险编号）。
⚠️ **R52 的可逆性已由设计取形锁定（2026-09-17，#60 评审 P3-1 → 设计 §4.9.2）**：**跨段墓碑在合并时「物理化」**
（段携带的墓碑全部指向严格更早的段 ⇒ 合并时对目标 doc 走**既有的 `Index::remove` 语义**、再从 `View.tombstones` 移除）
⇒ **R52 的回归只覆盖「未合并窗口」**、`merge_all()` 之后热路径**回到 `None`**；⚠️ 若改取「墓碑永久留存」的另一种取形，
**R52 将不可逆**、且 §5 的「分段与单段逐位一致」在**带删除场景**不成立（全局 `N` / `total_len` / `df(t)` 会把已删 doc 仍计入）。
⇒ **R50 / R52 / R54 的残余判定，前面都要加一句「在『合并时物理化』的前提下」**。
⚠️ **R51 / R52 / R54 的读数落 `eval-report.md` §8.15**（S8-S1 / S8-T12 / S8-S2 的产物）；⚠️ 延迟类读数**不具跨机可引用性**，报数**必标运行范围**（核数 / 是否插电 / 是否在 CI）。

#### 14.7 V2 Step 9 引入的风险（R55 ~ R58）

> 来源：`v2-step9-design.md` **v0.2**（首版 v0.1，2026-09-23；**已过两轮评审** ⇒ v0.2，2026-09-24）。**编号由该设计文档起分配**（接 §14.6 的 R54）。
> ⚠️ 本步**不改任何既有 trait 签名**：`FusionStrategy` 的改动是**加法**（provided 方法 `weights_override`，默认 `None` ⇒ 既有实现零改动、默认**逐位一致**；先例 = `Reranker::candidate_window`，§5.7）。
> ⚠️ **R55 属「评测诚信」类**（不是产品缺陷，而是「结论可能不可信」）；**R56 ~ R58** 属「样本量 / 可复现 / 合规」类。

| # | 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- | --- |
| **R55** | **桶标签含 qrels ⇒ 标签泄漏**：`exact` / `paraphrase` 的划分用到 `qrels` 里 `grade ≥ 1` 的段落（`examples/t2_prep.rs:206-230`），而 `NFR-15` 的字面口径正是「在**分桶**上优于固定权重」 | 实现若「按桶标签」调权 ⇒ 等价于把答案喂进排序 ⇒ 评测集上**必然**达标，但结论**不可信、不可投产**（生产无 qrels ⇒ 该桶也算不出来） | ① **D-S9-02**：自适应信号**只允许**「检索自身可观测的量」；② 判据 **S9-3**（类型级白名单 + 仓内反扫 `fusion` 模块依赖 `qrels` / `relevance` / `bucket` = 0 命中）；③ **FR-35 的措辞更正**（『按桶』→『按 query 侧可观测特征、不得依赖相关性标注』）**已提出、待评审拍板**（设计 §6.3；**原措辞保留**） | 🟡 **未消解**（属**设计约束**类，靠 D-S9-02 + S9-3 守；⚠️ 与 **R49** 同族：**没有「修完就关」的时点**） |
| **R56** | **样本量约束**：paraphrase 桶仅 **80** 条 | 任何多参数 / 多阈值的「最优值搜索」都能把噪声拟合进去，且**无法用同一样本自证**（Step 7 已在 320 条上吃到「配对符号检验不显著、最小 p = 0.1553」） | ① **D-S9-08**：规则只取「**零参数 / 单阈值**」形态；② **决策门 G4**：判据必须在 θ 的**一段区间**内同时成立（**单点达标不算**）；③ 报数只报**幅度与同向**、**不声称显著**（D-S9-05） | 🟡 **未消解**（靠 G4 + 报数纪律守） |
| **R57** | **LLM 生成资产不可复现**：模型版本 / 端点 / 采样参数漂移 ⇒ 「同一份集」下次跑不出来 | 对照读数**不可复核**（同族：**R23** 的「拓扑不可复现」） | ① 资产**冻结入库**（以库内文件为准，不靠重跑）；② 记录 **model + 参数 + prompt**；③ 生成脚本**只读环境变量**、**CI 不发外部请求**（`S9-T12` 用桩） | 🟡 **未消解**（设计期定形，`PR9-3` 落地） |
| **R58** | **语料 / query 外发（数据出境与合规）**：T7-16 走**外部 LLM API**（用户 2026-09-23 拍板） | 把 T2Ranking 语料或 query 发给外部端点 ⇒ 合规风险（且 T2Ranking 的使用条款需核） | ① ⚠️ **开工前须用户确认**；② 若不允许 ⇒ **降级为「规则化生成」**并把**同构性声明降级**如实写进 `eval-report.md` §8.16；③ 密钥只走环境变量、不入库 | 🔴 **待确认**（阻塞 `PR9-3` 的「外部 API」分支） |

> ⚠️ **R55 / R56 与 R49 同族**：它们属**设计约束类** —— 靠「不变式 + 断言 + 报数纪律」守，**不靠一次修复关闭**。

---

## 15. 附录

### 15.1 参考资料

- Robertson & Zaragoza, *The Probabilistic Relevance Framework: BM25 and Beyond*, 2009 — BM25 理论基础
- Malkov & Yashunin, *Efficient and Robust Approximate Nearest Neighbor Search Using HNSW*, 2016 — HNSW 原论文
- Cormack, Clarke & Büttcher, *Reciprocal Rank Fusion Outperforms Condorcet and Individual Rank Learning Methods*, SIGIR 2009 — RRF 与 k=60 的出处
- BAAI/bge-small-zh-v1.5 模型卡 — 查询侧 instruction 前缀的权威说明

### 15.2 变更记录

| 版本   | 日期         | 变更                                                                     |
| ---- | ---------- | ---------------------------------------------------------------------- |
| v1.0 | 2026-09-02 | 由 `archive/requirements-and-design_v1.0.md` v1.0 拆分而来。承接第 5、6、7、8、9、10、11.1、11.2、12 章内容；新增「文档信息」「架构概述与关键决策」「质量属性设计」「ADR 决策记录」「实施计划与需求映射」五节 |
| v1.1 | 2026-09-02 | 依据 `thirdparty.md` 调研结论回写：① 新增 ADR-007（tantivy 作 dev 基线）、ADR-008（MSRV 1.90 + cargo-deny）；② 7.1 引入 `unicode-segmentation` / `unicode-normalization`，不再手写 Unicode 分段；③ 7.5 补充 `hnsw_rs` A/B 复核路径；④ 7.6 修正 bincode 选型理由（instant-distance 不含 bincode）并新增 `rkyv` A/B 取舍规则；⑤ 8.1 缓存由 `lru` 改为 `moka`；⑥ 9.1/9.2 版本与候选同步至实测值（fastembed 6.0.2、arroy 0.8.0、新增 hnsw_rs/usearch）；⑦ 12.1 新增 tantivy 对照测试；⑧ 13 更新 R1、新增 R9/R10 |
| v1.2 | 2026-09-02 | **P0 执行后回写**（详见 `p0-design.md` 第 12 章）：① 序列化定为 **`bincode 2.0.1`**（3.0.0 为玩笑发布）；② `moka` 需显式启用 `sync` feature；③ `fastembed` 必须 `default-features = false` 以移除 NCSA 依赖链；④ 模型实际来源为 `Xenova/bge-small-zh-v1.5`（非 Qdrant）；⑤ 枚举变体确认为 `BGESmallZHV15`（非 `BGESmallZH`） |
| v1.3 | 2026-09-03 | 依据 `p5-design.md` v1.2（评测数据源切换 T2Ranking）同步：① 4.x 目录树 data/ 注释更新；② 10.2 CLI 示例改 `data/t2-queries.jsonl`；③ 11 阶段门槛 P5 行更新（T2Ranking 装配 + 分级 NDCG）。评测数据集的**需求定义**见需求文档 9.4（v1.2），本文档不复制 |
| v1.4 | 2026-09-04 | 依据 `p6-design.md` v2.0（issue #1 接口重构，决策点 D-I1~D-I8 已拍板）：① 新增**第 10 章「对外接口设计（门面层）」**，原 10~14 章顺延为 11~15；② 新增 ADR-009（门面层 + 共用编排内核）；③ 5.1 `Document` 拆为输入 DTO + `DocRecord`；④ 4.2 补门面层边界；⑤ 11.3 错误类型新增 `ConfigMismatch`；⑥ 阶段表 P6 重定义为接口重构、原 v2 顺延 P7。详细设计见 `p6-design.md`，任务级见 `plan.md` |
| v1.5 | 2026-09-05 | 依据 `v2-step1-design.md` v0.3（V2 Step 1：向量软删除 + 过滤下推）回写：① 5.4 `VectorIndex` 新增 `search_filtered` 与 `None` 契约，新增 5.4.1 存活单一真源；② 5.5 `Retriever` 的过滤载体从 `Option<&Filter>` 改为 `Option<&dyn CandidateFilter>`，新增 5.5.1；③ **7.5 整节重写**（原 instant-distance + delta 方案已在 D8 移除）；④ 7.6 修正 bincode 版本坑，新增 7.6.1「新增结构不入快照」；⑤ 8.1 去掉失效的 `instant_distance::Search` 复用、新增过滤下推两行；⑥ 新增 8.3 的 `query::Metrics` 三项与其**当前不可观测**的限制、新增 8.4 空结果语义；⑦ 新增 **ADR-010**；⑧ 14 章 R1 标记已消除、新增 R11~R18；⑨ 9.3 决策表补 ADR-010 行 |
| v1.6 | 2026-09-06 | 依据 `v2-step2-design.md` v0.3（V2 Step 2：图持久化，**ADR-A 方案 C 已拍板**）回写：① 2.2 的 NFR-06 行删去过时论据「seed 固定」（`StdRng::from_os_rng()` 无 seed API，与 8.2 直接冲突），改为「图持久化冻结拓扑」；② 2.3 与 9.4 新增 **ADR-A**（图 = 快照的派生缓存，含实测：完整冷启动 ≈100ms、磁盘 1.6×、dump 43.5ms），并说明 §7.6.1 曾预引用的「ADR-011」已统一为 ADR-A；③ **5.4.3 新增**「图持久化 `VectorGraphPersist`」（basename 铁律 / `ef_search` 是入参 / `Box::leak` 三条结构性约束）；④ 7.6.2 新增「V2 Step 2：图 sidecar（ADR-A）」；⑤ 8.2 NFR-06 口径改为「同快照两次加载逐位一致」；⑥ 8.3 NFR-07 扩「降级可观测」（`GraphStatus`）；⑦ 10.3 补 `graph_status()` / `graph_dump_elapsed()`；⑧ **14.1 新增 R19~R25**（含 12K 实测：R22 体积 1.6×、R25 dump 43.5ms） |
| v1.7 | 2026-09-07 | **V2 计划复审 + 步骤重排同步**（复审结论已并入 `plan-v2.md` §附-3）：① §7.5「物理回收归 Step 5 compaction」→ **Step 4** 并补「compaction 后必须重发 manifest」；② §14.1 R22 对策同步为 Step 4；③ §14.1 R18 更新——低选择度兜底提前到 V2.0（D-J9：Step 5 / T7-22，新增 NFR-13）；④ §14.1 R19 残余归 Step 3（S3-e）；⑤ §7.6.2 补记事实——快照本体至今非原子（`storage/snapshot.rs:88` 直写无 fsync），崩溃即 `SnapshotCorrupted`；⑥ §12.2 注明矩阵仅反映 V1，V2 归属见 `plan-v2.md` §5 |
| v1.8 | 2026-09-07 | **V2 Step 3（原子快照）落地回写**（依据 `v2-step3-design.md` v0.3，D-S3-01~07 全部拍板）：① §7.6.2 前提段**销账**——快照本体原子性已落地（`storage/atomic.rs` 通用原语 `atomic_write`：tmp + flush + `sync_all` + rename + fsync 父目录，快照与 manifest 共用一份实现；格式零变化，`FORMAT_VERSION` 保持 2），Q-C3 正确性欠账清零；② §14.1 **R19 写路径残余收敛**——`dump_graph_caught`（`catch_unwind` → `Err(VectorGraph)`，汇入 P0-3 语义链），残余更新为「panic=abort 宿主约束（文档级）/ panic 噪音 / 读路径（C5 边界外）」三条；③ 12K fsync 代价实测入 `eval-report.md` §8.7（+0.027~0.035s，不触碰任何 NFR） |
| v1.9 | 2026-09-10 | **① V2 Step 4 回写补记**（`#32` / `9e2211e`，当时漏记版本行）：§7.5.3 墓碑物理回收（compaction，FR-30）+ **§14.2 新增 R26~R30**；10K churn 0.3×5 实测 graph+data 84.6→33.7MB。**② V2 Step 5 详细设计回写**（依据 `v2-step5-design.md` v0.3，D-S5-01~09 全部拍板）：**§5.4** `VectorIndex` 新增 `search_exact_filtered`（**必选**）+ `prefers_exact`（默认 `false`）两方法，并补 `vector_shortfall` 判读注记；**§5.8** `SearchResponse` 新增 `metrics` 字段（**破坏性**，R35）；**§8.3** 新增「Step 5 补外部可见」块（`Metrics` 进 `SearchResponse` / per-lane 耗时 / `vector_route` / 修两条早退 `took`）并把「当前限制」改写为「设计已承接、待实现」；**§14 章级主表 R18 补 ④**——「约 0.05ms」口径**修正**（只含「对候选算距离」，**不含 `O(N)` 遍历定位候选** 1~10ms）+ 三路径对策 + ⚠️ 「只覆盖低选择度子集，R11/R13/R17/R18 不得标已解决」；**§14.3 新增 R31~R35**（`O(N)` 谓词判定 / 兜底改变输出 / 阈值是经验值 / 精确扫描持 `points_by_layer` 读锁**归 Step 8** / `SearchResponse`+`VectorIndex` 对外破坏性变更）；§14 导读补 R31~R35 指引 |
| v1.10 | 2026-09-10 | **V2 Step 5 实现期口径精确化**（PR #36 评审 F1）：`vector_shortfall` 在精确路径下**不是「恒为 0」**——`allowed` 来自 `Index`、扫描枚举的是**图里的点**，两个独立来源，图滞后于索引时该值仍 > 0。修正 **§5.4** 注记与 **§8.3 `D-S5-07` 行**为**条件式**，并把该条件本身升级为信号：`Exact` + 缺口 `> 0` ⟺ **图未覆盖全部 allowed chunk**。同步精确化实现侧措辞（`query/metrics.rs` / `query/searcher.rs` / 集成测试 / CLI 图例 / `scripts/eval_filter.sh` / CHANGELOG） |
| v1.11 | 2026-09-10 | **V2 Step 5 收尾：S5-04 标定定稿 + S5-08 文档回写**（实现已合并 `48c0ac7`；设计文档升 **v0.5**）：① **§14.3 R31 / R33 由「未标定」改为「已标定」**——阈值定稿 **8192**（10 万级 A/B 两端点实测 + 归一插值交叉点 ≈ 9700，原初值 1024 过保守）；② ⚠️ **R34 措辞改写**——「与写端互斥」→「**含潜在死锁**」（`IterPoint::next` 层切换递归读同一把 `RwLock`），修法归 Step 8（设计 §4.2.2 逐层遍历）；③ **§14 章级主表 R18 补 ⑤**（标定后端到端 P99 + NFR-13 双口径）；④ **§14.3 表头**改为「已完成并合并」（Q1~Q5 全部结案） |
| v1.12 | 2026-09-11 | **V2 Step 6 详细设计回写**（`v2-step6-design.md` v0.1，D-S6-01~09 待评审）：**§14.4 新增 R36~R40**（多 session 内存峰值 / CoreML EP 改变向量数值 / `default_embedder()` 静默降级 / 追加漏发图 manifest / ID 复用致幽灵向量），其中 **R36、R37 标注为条件性**（取决于 embed 并行 spike 的「投 / 不投」判定）；§14 导读同步。**不触碰任何 trait / 结构 / FR / NFR**（NFR-10 数值目标缺失登记为设计文档 §9.2 Q4） |
| v1.13 | 2026-09-11 | **V2 Step 6 设计评审收口（版本面同步）**：设计文档升 **v0.2**——PR #38 的 7 条评审意见（F1~F7）全部采纳，**D-S6-05 / D-S6-08 拍板**、Q4~Q7 收口。**本文档无风险条目 / trait / 结构变更**：§14.4 R36~R40 保持原样（R38 的「应对」仍指向 D-S6-05 方案 A，本次仅是正式拍板）。NFR-10 的口径修订属**需求面**，落在 `requirements-spec.md` **v1.14** |
| v1.14 | 2026-09-13 | **V2 Step 6 收尾（S6-10）回写：§14.4 由「R36~R40」扩为「R36~R43」** —— ① **R36 / R37 消解**——T7-09 判「不投」（E2 +11.7%/+16.0% 而峰值 RSS **+96.4%/+289.2%**；E3 调优后 +1.2%）⇒ 多 session / CoreML EP 均不落地；② **R38 / R39 关闭**（S6-09 硬失败 + 内部测试接缝；S6-T13 断言追加后冷启动 `Loaded` 且 manifest `nb_point` 吻合）；③ **R40 上锁、残余仍在**（S6-T8 锁 ID 单调，但「不冲突」的证明仍依赖三条隐含前提）；④ **新增 R41**（**建库路径峰值 RSS 2.32~3.33GB 与 NFR-05 的 372MB 差一个数量级**；预算按 `batch × 序列长度` 给、**只给形态不给数**）/ **R42**（**依赖安全公告漂移**：`bincode` / `paste` 两条无升级路径的 unmaintained，**长期开放**）/ **R43**（**查询侧编码封顶并发**：`embed/local.rs:35` 的 `Mutex<TextEmbedding>`；实测 4 线程 vector **1.88×** / hybrid **1.63×**，临时移出编码后 vector **3.15×** ⇒ **阈值可达、卡在实现**；**保持打开**）；⑤ §14.4 表头状态改为「实现完成 + 收尾回写」。**无 trait / 结构变更**；NFR-10 / NFR-05 的口径修订属**需求面**，落在 `requirements-spec.md` **v1.15** |
| v1.15 | 2026-09-14 | **V2 Step 7（精排）详细设计回写**（`v2-step7-design.md` **v0.1**，D-S7-01~10 待评审）—— ① **§14.5 新增 R44~R48**（精排器常驻内存 ≈2.19GB / 精排延迟主导 + 窗口静默夹取 / `Hit.score` 语义随开关而变 / 精排数值可复现性未证实 / 512 token 截断使长段落判据失真）；② **§5.7 `Reranker` 新增 provided 方法 `candidate_window`（默认 `k`）** ⇒ → 既有实现一行不改、NoOp 逐位零回归；契约收紧「`rerank` 不得依赖 `hits[i].explain`」；新增 **`LocalReranker`**（gate `feature = "local-rerank"`，未启用时编译期不存在）；③ **§5.8** `Explain` 新增 **`rerank_score: Option<Score>`（⚠️ 破坏性）**、`Metrics` 新增 **`rerank_elapsed` / `rerank_window`（纯加法）**，`Hit.score` 精排生效时 **= `σ(logit)`**；④ **§14 导读**补 §14.5 指引与「R44 ≠ R41」「R47 反例不必修」两条纪律。⚠️ **本步不引入 trait 破坏**；**不进 `ConfigFingerprint`**（D-S7-10）。NFR-12 / NFR-05 / NFR-06 / NFR-07 与 FR-18 的修订属**需求面**，落在 `requirements-spec.md` **v1.16** |
| v1.16 | 2026-09-14 | **V2 Step 7 设计评审收口（纯版本面同步）**：设计文档升 **v0.2** —— 评审 **4 项拍板全部同意**、**F1~F6 全部采纳**。**本文档无风险条目 / trait / 结构变更**（**§14.5 的 R44~R48 一字未改**；评审已逐条核实 R46 = `Hit.score` 语义随开关而变、R48 = 512 token 截断使长段落判据失真，与登记一致）。评审 **F1** 指向的 `plan-v2` §8「`Config` 新增字段」与 **D-S7-02** 矛盾，已在 `plan-v2` 更正，**不涉本文档**。NFR-12 的措辞更正属**需求面**，落在 `requirements-spec.md` **v1.17** |
| v1.17 | 2026-09-16 | **V2 Step 7 收尾（S7-05，设计 `v2-step7-design.md` v0.3）**：**§14.5 按实测回填三项** —— **R44 数值已补**（精排内存增量 **2.64 GiB / 7.71×**，是模型文件体积的 1.25×；条目仍保持开放）、**R48 收窄后关闭**（本评测集上 `max_len 512→1024` 的 ΔNDCG **+0.0002** ⇒ 截断非「精排无提升」主因；限定「产品侧目标场景未测」）、**R47 已有全部实测**（P1 同进程 3 次、P2 跨进程 2 次逐位一致；P3 单条 vs 批内 logit 相同）；**另定一条锚点规则**（§14.5 导读）：本仓源码锚点一律符号指代、依赖锚点带版本，并据此改写 R44/R47/R48 的相关锚点。⚠️ 无 trait / 结构 / 预算变更；需求侧随之 **v1.18** |
| v1.18 | 2026-09-16 | **V2.1 计划修订（架构侧，纯补「承接关系」）**：**§14.3 R34** 的残余列补落点（`plan-v2.md` Step 8 的 **T7-25**，硬前置）、**§14.4 R43** 的残余列补落点（Step 8 的 **T7-26**）、**§14 主表 R11 / R13** 的应对列补 prefilter 落点（Step 10 的 **T7-27** + 新 **FR-34**）、**§14 导读补「V2.1 承接」段**并钉死「**V2.1 从 R49 起**编号」的纪律。⚠️ **不新增风险条目、不改任何既有条目内容、无 trait / 结构 / 预算变更**；R34 / R43 / R11 / R13 / R17 / R18 **一律不标「已解决」**；需求侧随之 **v1.19** |
| v1.19 | 2026-09-16 | **V2 Step 8（读写并发）详细设计回写**（依据 `v2-step8-design.md` **v0.1**，D-S8-01 ~ 12 待评审、**4 条需拍板**：D-S8-01 / D-S8-06 / D-S8-09 / D-S8-11）—— **① §14.6 新增 R49 ~ R54**（跨段 BM25 三条前提 / 「合并保 ID」vs `compact()` 重编号 / 向量合并成本与拓扑 / `bm25_f` 热路径回归 / 视图指针锁的写者优先 / 会话池内存）；**② §14 导读补 §14.6 指引** + 「R49/R50 无关闭时点」「R51/R54 等 spike、**不预先承诺**」两条纪律；**③ ADR-A 补扩展一行**（设计 §4.10）：「合并是 `save` 的前置步骤」⇒ 落盘仍是单段 ⇒ `FORMAT_VERSION` 保持 **2**、manifest 仍绑单快照 CRC ⇒ **不开 ADR-B**（多段快照登记为条件项 Q2）。⚠️ **本次只新增风险条目**（既有 **R1 ~ R48 一字未改**）、**无 trait / 结构 / 预算变更**；**开工前置 H4 / H5 / H6 三条已结清**（**H4** = 不开 ADR-B；**H5** = 取 ① + ②、**③ 明确不做**；**H6** = 写延迟口径改**直接计时** + 拟值 `P50 ≤ 1.0s / P99 ≤ 2.0s`）；需求侧随之 **v1.20** |
| v1.20 | 2026-09-17 | **V2 Step 8 设计第 1 轮评审响应（`v2-step8-design.md` v0.1 → v0.2）—— 只动 §14.6 R51 的「残余」列**：① **累积性**（方案 A 每次合并 `load_graph` ⇒ 每次多一个 leaked `HnswIo`；异步合并是 FR-17 常态路径 ⇒ 长驻宿主累积）；② **量级**（⚠️ **评审 P3-5 的量级前提经独立复算后更正**：leaked 的只有 `HnswIo`、**≈200B + 路径串 / 次**（`vector/persist.rs:210-216`），**不是图规模** ⇒ 10 次合并 ≈ 2 KB）；S8-S1 决策门第 ⑤ 条据此定阈值 **< 1 MiB**。另补一条**编号归属纪律**：**R49 = 跨段 BM25 三条前提、R52 = `bm25_f` 热路径**，「逐段 `prefers_exact` ⇒ route 语义变化」归设计文档的 **Q5 + §4.13.3**。⚠️ **本批未新增 / 修改任何风险条目**（R49 ~ R54 正文一字未改）、**无 trait / 结构 / 预算变更**；需求侧随之 **v1.21**（两处笔误） |
| v1.21 | 2026-09-22 | **v1.21（2026-09-22）**：**V2 Step 8 收尾（`S8-09`，设计 `v2-step8-design.md` 升 v0.15）—— 只回填 §14.6 的 R51 ~ R54 残余列 + 版本面**：① **R51** 收窄（spike S8-S1 五条判据全 PASS ⇒ 方案 A 已落地；残余 = `HnswIo` 累积 + **图拓扑仍漂移**）；② **R52 已量化**（`bm25_elapsed` 四档 44 / 52 / 53 / 50 us ⇒ 谓词 **+1 us**、多段扫描 **+8 us**）；③ **R53 已测**（`S8-T11` 三次运行 ②a 皆 ≤ 1.2、**基线定义写死**；尾部扰动不可复现 ⇒ 保持开放）；④ **R54 消解**（spike S8-S2 判「不投」⇒ 条件未触发；**将来翻转须重新打开**）。⚠️ **不新增 / 不删除任何风险编号**（R1 ~ R50 一字未改）、**无 trait / 结构 / 预算变更**；需求侧随之 **v1.22**（NFR-14 ② 与 NFR-11 定稿去「拟」+ FR-17 状态漂移修正） |
| v1.22 | 2026-09-22 | **V2 Step 8 收尾第 1 轮评审响应**（`v2-step8-design.md` v0.15 → v0.16，PR7b）—— **§14.4 的 `R43` 补判后记录**：`NFR-10` ③ 的复审触发条件**已触发并已回答**（Q3′）⇒ **`R43` 保持「打开」**（spike S8-S2 判「不投」；⚠️ 池化确有增益 1.23× → 2.10× ⇒ FAIL 不得读成「池化无效」）；⚠️ **不新增 / 不删除任何风险编号**；需求侧随之 **v1.23** |
| v1.23 | 2026-09-23 | **V2 Step 9 设计（`v2-step9-design.md` v0.1，PR9-0）**：① **§5.6 的 `FusionStrategy` 伪码与实现漂移 ⇒ 按实现更正**（原写多出 `weights` 形参、类型名 `ScoredHit` 不存在、缺 `k` 形参）+ 补 **provided 方法** `weights_override`；② **新增 §14.7（R55 ~ R58）** —— R55 **桶标签含 qrels ⇒ 标签泄漏** / R56 80 条桶的样本量 / R57 LLM 生成资产不可复现 / R58 语料外发；③ ⚠️ **不新增其它风险编号、不改既有条目内容** |
| v1.24 | 2026-09-24 | **V2 Step 9 设计第 1~2 轮评审响应**（`v2-step9-design.md` v0.1 → **v0.2**）——① **§5.6 补第二个 provided 方法 `fuse_adaptive`**：设计第 2 轮评审 **P3-1** 指出`weights_override` 只定义了「决策」、没定义「应用」—— 编排层只持 `&dyn FusionStrategy`、而 `fuse` 读策略私有字段（`rrf.rs:62`）⇒ **没有任何通道**把 `w` 送进融合计算；四条替代路（改签名 / 每查询重建 / 内部可变性 / 编排层重算）**逐条堵死** ⇒ 取「再加一个 provided 方法」（默认转发 ⇒ 零改动）+ 写死「开关关 ⇒ 不调用 `fuse_adaptive`、不构造 ctx」；② §14.7 的来源行同步为 **v0.2**。⚠️ **风险编号（R1 ~ R58）、trait 签名、结构、预算一律未动**；需求侧**不变**（本轮不改需求内容） |
