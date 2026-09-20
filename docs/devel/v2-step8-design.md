# HelixIndex V2 · Step 8 详细设计（读写并发：不可变视图 + 追加段 + 原子发布）

| 项 | 内容 |
| --- | --- |
| 版本 | **v0.8（2026-09-20，PR #63 第 3 轮评审响应）** —— v0.7（2026-09-19，PR #63 第 2 轮评审响应）→ 本轮为 `S8-03` 实现 PR 的**第 3 轮**评审（**复审通过**：原文「PR4（`S8-03` / `S8-04` + 最小合并）范围内评审通过 —— **无阻塞项**」，且它**独立复现**了两条 P1 的变异自证、实跑 3 条新判据），唯一新意见 = **1×P4**（`commit_view` 的 epoch 拒绝消息被 `commit()` 与 `fold_deltas()` 共用 ⇒ 对 `fold` 侧三条声明全不成立）⇒ **已采纳并顺手修**：新增 **`PublishCaller`**（`Commit` / `Fold` + `loss_and_recovery()`）⇒ 消息按调用方在**同一个地方**分家；判据 `S8_03_epoch拒绝的恢复指引按调用方分家`（变异：两臂对调 ⇒ 唯一红）+ 端到端**接线判据**；落点 **3 处**（`search/view.rs` / `error.rs` 表 / **本节**，第 3 处评审没点到）。**不改任何决策（D-S8-01 ~ 12）与风险编号。** v0.6（PR #62 第 1 轮评审响应）→ 本轮为 `S8-03` 实现 PR 的**第 2 轮**评审响应，**2×P1 + 1×P3 + 1×P4**：① 🔴 **P1-2 更正了 `absorb_window` 的判据**（「长度前缀」**不够** —— 并发 `fold` 不改 `epoch` ⇒ 世代对账有盲区；判据升级为**前缀 `Arc::ptr_eq` 身份校验**，不符即拒绝发布）⇒ 新增 **§4.9.6** + **§4.3.2 的锁内拒绝说明**；② 🔴 **P1-1 更正 §4.8.3 的查重伪代码**（墓碑有**两个落点**，只查已发布那个会静默吞掉重加/替换）；③ **P3-1** 三条 `Busy` 消息的**恢复指引按语义分家**（`commit()` 丢内容 ⇒ 重做 `add`/`remove`；`fold` 只丢计算 ⇒ 重试 `save`/`compact`）；④ **P4-1** 墓碑-only 空段的次生成本登记给 `S8-06`。**v0.6（2026-09-18，PR #62 第 1 轮评审响应）** —— v0.5（`S8-02` / `T7-18` 实施结果）→ 本轮为**实现 PR 的评审响应**：① 新增 **§4.3.5 第 1 轮评审响应**（**6 条全部采纳**，含 **P2 阻塞级**的 `commit()` 原子化：**两套实测**量化 + `M7`/`M8` 变异 + 2 条覆盖边界）；② 🔴 **P2 修复改变了实现取形**：`Shared::publish` / `advance` **已删除**，发布逻辑内联进新增的 `Shared::commit_view()`（持 `ids` 锁完成四步）⇒ **§4.3.2 / §4.3.4 的落点表已同步**；③ `tombstone_stats()` 改为**真·跨段求和**（抽出 `View::sums()` 纯函数）。**v0.5（2026-09-18，`S8-02` / `T7-18` 实施结果 = PR3）** —— v0.4（PR #61 第 1 轮评审响应）→ 本轮**只回填实施结果、不改任何决策**：① 新增 **§4.3.4 实施结果**（落点 / **5 条与正文的偏差** / 13 条测试 / **变异验证 M5·M6′** / **2 条覆盖边界**）；② ⚠️ **`into_index()` 不再可能失败**（行为变更，§4.12 已指定取形、但后果需确认）；③ **过渡约束**：`with_main_mut` 要求「无并发读者」、**`I8-2` 在 PR3 尚未成立**（由 S8-03 解除）；④ 可见性语义**刻意保留**「未 commit 亦可见」。**v0.4（2026-09-18，PR #61 第 1 轮评审响应）** —— v0.3（S8-01 / T7-25 实施结果）→ 本轮按评审 **4×P4**（**无阻塞**）+ 行内 4 条逐条处置：① **P4-1 数值勘误**（`L ≈ 2.4` → **`E[L] ≈ 2.9`**，口径一并改正为**最大次序统计量**）；② **P4-2 删除冗余断言**（`assert!(l0 < n)` **永不报红**）；③ **P4-3 不立项**、改交**行为级**替代物（**新增空图断言**）；④ **P4-4** PR 正文 run id 笔误；⑤ 🔴 **覆盖边界②就地更正**（原写「改回 `IntoIterator` 时任何 CI 断言都不会红」**已不准确** —— 空图断言能抓住；**M4 变异实测**：该用例红、其余 10 条全绿）。逐条处置见 **§4.2.1 末节**。**v0.3（2026-09-18，S8-01 / T7-25 实施结果）** —— v0.2（2026-09-17，第 1 轮评审响应）→ 本轮**只回填实现结果、不改任何决策**：① **T7-25 已实现**（`search_exact_filtered` 的全量遍历改为**逐层**，与 §4.2 的写法逐字一致）；② **S8-T1 的实际形态**（std-only 复刻体 + **受闸写者**握手 + 两条臂）与**平台前提实测**（macOS + Linux 各 5/5）；③ 🔴 **实现期新发现两条「覆盖边界」**（见 §4.2.1）：覆盖类断言**必须经由生产函数**（在用例内复刻循环 ⇒ 变异下仍绿，已实测）、把生产循环**改回 `IntoIterator`** 时**任何 CI 断言都不会红**（⚠️ **该表述已被 v0.4 就地收窄**：空图断言能抓住，见 §4.2.1）；④ **变异表**（3 组注入 + 命中）；⑤ **行号漂移实测 = 0**（活文档 13 处引用全部 ≤ 改动区间）。**v0.2（2026-09-17，第 1 轮评审响应）** —— v0.1（2026-09-16，首版）→ 该轮按评审 **5×P3 + 7×P4**（**无阻塞**）逐条处置：**12 条意见全部采纳**；其中 **P3-1（跨段墓碑在合并时的处置）/ P3-4（双写端 `commit()` 的原子性）** 是本轮**最实质的两条**（补写 §4.4.3 / §4.7.3 / §4.9.2 / §4.9.5 的取形与用例）；**P3-5（`Box::leak` 累积）** 结论采纳、**量级前提经独立复算后更正**（不是「按图规模增长」，实测 ≈ **200B + 路径串 / 每次合并**）；**P4-1（「100+ 处调用点」）** 经独立复算改为 **41 处**。⚠️ **本轮不改任何决策取值（D-S8-01 ~ 12）与风险编号**；逐条处置见 **§10**。 |
| 日期 | 2026-09-16 |
| 状态 | **PR #63 第 3 轮评审已响应（v0.8，2026-09-20）**：**复审通过（无阻塞项）** + 唯一新意见 **1×P4 已采纳并顺手修**（`PublishCaller` 让 epoch 拒绝消息按调用方分家；判据 + 变异 + 端到端接线判据齐备）。详见 **§4.9.6**。**PR #63 第 2 轮评审已响应（v0.7，2026-09-19）**：评审落点 = `pulls/63/reviews` 1 条 + 行内 4 条 ⇒ **2×P1 + 1×P3 + 1×P4，全部采纳**（正文另逐条确认 §6 十二项 = **12 项全 ✅**）。**第 2 轮的 2 条 P1 都落在「多写端 / 未发布状态」的缝隙里**（查重不看未发布墓碑 / `absorb_window` 的长度前缀假设被并发 `fold` 破坏）⇒ **两条都已确定性复现 + 修复 + 变异自证**；逐条处置见 **§4.9.6**。**PR #61 第 1 轮评审已响应（v0.4，2026-09-18）**：**4×P4、无阻塞**（+ 行内 4 条）⇒ **3 条采纳 + 1 条不立项（改交行为级替代物）**；**覆盖边界②就地收窄 + 新增空图断言（M4 变异实测）**。逐条处置见 **§4.2.1 末节**。**第 1 轮评审已响应（v0.2，2026-09-17）**：评审 **5×P3 + 7×P4、无阻塞**；**4 条需拍板 + 需求面 1 条全部获同意**（其中 3 条附前置：**P4-7① → D-S8-01** / **P3-4 → D-S8-06** / **P3-5 → D-S8-09**）；**12 条意见全部采纳**（含 1 条结论采纳、量级前提更正）。逐条处置见 **§10**。**待评审**。本文回答 `plan-v2.md` §4.0 的 **H4 / H5 / H6** 三条开工前置，并把 §4 Step 8 的四个任务（**T7-25 / T7-06 / T7-26 / T7-18**）落成 `D-S8-01~12` 决策、`S8-01~09` 任务与 `S8-T1~T16` 测试计划。**风险编号 R49~R54 由本文起分配**（架构 §14 导读钦定：「V2.1 从 R49 起」）。⚠️ **本文纯设计 —— 不落任何生产代码**。 |
| 上游 | `plan-v2.md` **v0.19** §4 Step 8 / §4.0 H4~H6 / §6「V2.1 门槛」/ §附-4；issue **#58**（Step 8 跟踪 issue）；`requirements-spec.md` **v1.19**（**FR-17** §5.3.5 / **NFR-14** 草案 / **NFR-11** / **NFR-10** ③）；`architecture-design.md` **v1.18**（**§14.3 R34** / **§14.4 R43** / **ADR-A**）；`v2-step5-design.md` **v0.5** §3.1 / §4.2.2（逐层遍历的正确写法与「每点恰一次」的证明） |
| 范围 | ① **T7-25** R34 修法（精确扫描改逐层 `get_layer_iterator`，**硬前置**）；② **T7-06** delta 分段（FR-17 完整版：**读不阻塞写**）；③ **T7-26** 查询侧会话池（解 **R43** / NFR-10 ③）+ 挂起的 **Q3**；④ **T7-18** 旧 API `#[deprecated]`（**先评估、后按结论执行**）。交付形态 = `SearchIndex::searcher(&self)` + 不可变 `View` + 后台合并**原语**。 |
| 非范围 | **不引入库内后台线程**（库不 `spawn`，合并原语由宿主 / CLI 调用，见 D-S8-08）；**不做「读能看到未 commit 的写」**（H5 ③ 明确不做）；**不做多进程 / 分布式并发写同一快照**（C5 既有边界不变）；**不做多段快照格式**（`save` 前先合并 ⇒ `FORMAT_VERSION` 保持 **2**，**不开 ADR-B**，见 **D-S8-01** —— ⚠️ **2026-09-17 按评审 P4-5 更正**：原写 D-S8-09，那是「向量侧合并取形」，H4 持久化形态才是 D-S8-01）；**不做 prefilter / 排序键索引**（Step 10 / T7-27）；**不做自适应融合与评测集换代**（Step 9 / H7）；**不改 `bm25` 打分公式与 P5 定稿的 `k1=1.5 / b=0.75`**；**不改向量距离口径与 `Hit.score` 语义**；**不给建库路径设内存上限**（R41 保持开放）；**不做跨平台图搬运**（R21 不变）；**不做精排器池化**（架构 §14.5 R45 ⑤ 已明确不做，本 Step 不动） |
| 交付物 | ① `crates/core/src/vector/hnsw_rs_index.rs` 精确扫描改**逐层遍历**（T7-25）② **新增** `crates/core/src/search/view.rs`（`Segment` / `View` / `Shared` / 跨段谓词）③ `Index::merge_from` + 跨段 `content_hash` 查重（`index/mod.rs`）④ 跨段 BM25 / 向量 / 谓词（`retriever/bm25.rs`、`retriever/vector.rs`、`query/filter.rs`、`query/searcher.rs`）⑤ `SearchIndex::searcher(&self)` + `into_searcher()` / `into_index()` 转 `#[deprecated]` 薄封装 ⑥ 合并原语 `SearchIndex::merge_pending()` / `merge_all()` ⑦ **查询侧会话池**（`embed/local.rs` + `Config::embed_sessions`，默认 1 ⇒ 零行为变化）⑧ 脚本 `scripts/eval_rw_concurrency.sh` + `eval-report.md` **§8.15** ⑨ 四处定义面回写 + CHANGELOG |

---

## ⚠️ 「需评审拍板」清单（**4 条**）

| # | 需拍板的事 | 本文建议 | 不同意会怎样 |
| --- | --- | --- | --- |
| **D-S8-01** | **持久化形态**：`save` 前是否**必须先把所有 delta 合并进主段**（从而保住「快照 = 单段」「`FORMAT_VERSION` = 2」「manifest 仍绑单个快照 CRC」）？ | ✅ **必须合并**（H4 的结论：**沿用 ADR-A 并显式声明扩展边界，不开 ADR-B**） | 若坚持「多段快照」，则 `SnapshotSections` / `FORMAT_VERSION` / `GraphManifest.nb_point` 全要改，**ADR-B 成为必出项**，Step 8 的体量与风险再上一个量级 |
| **D-S8-06** | **`into_index()` 的处置**：新模型下「换回写端」的**动机已消失**（`searcher()` 不再消耗写端），但既有调用点还在。 | ✅ **保留 + `#[deprecated]`**：实现 = 「用同一 `Arc<Shared>` 造一个新写端句柄」，并**把 ID 发号移进 `Shared`**（写串行、读侧永不触碰） | 若直接**移除**，`crates/core/tests/integration.rs` 与 2 条单测要改（**公开面破坏性变更**，需 CHANGELOG 的 `Removed`）；若**保留旧语义**，则新模型下无法实现 |
| **D-S8-09** | **合并器向量侧的取形**：`hnsw_rs` **无图合并 API**。 | ✅ **默认 = 「dump 旧主图 → load → 增量 `insert` delta 点」**；spike 不过则回落 **从 `raw_vectors` 全量重建**（ADR-A 允许，代价 ≈ **38.7s/12K**）。决策门见 §4.9.3 | 若直接选「全量重建」，**每次后台合并都要付 O(N·logN)** ⇒ 小 delta 也付全量代价，合并频率被迫降到很低 |
| **D-S8-11** | **查询侧会话池是否投**（NFR-10 ③ 的唯一出口，解 R43） | 🔴 **不预先承诺**：先出 **spike S8-S2**（查询侧口径的峰值 RSS + Q3 数值一致性），决策门见 §4.11.3；**不达标就如实记「未达标」并保持 R43 打开** | 若现在就承诺「投」，会重演 Step 6 的 E2 —— 数据出来是「不投」时**结论无法合并**（A2 复审的教训） |

其余 8 条决策为工程取舍，本文按建议执行；评审若反对，改的是**一段代码 + 一段文档**，不动需求面。

---

## 决策速览：D-S8-01 ~ D-S8-12

| # | 决策点 | 建议 | 定值来源 |
| --- | --- | --- | --- |
| **D-S8-01** | **H4：持久化形态** | 🔴 **需拍板**：✅ `save()` **先 `merge_all()` 再落盘** ⇒ 快照仍是**单段**、`FORMAT_VERSION` 保持 **2**、manifest 仍绑单快照 CRC ⇒ **不开 ADR-B**，只把「合并是 `save` 的前置步骤」写成不变式 | §4.9 / §4.10 |
| **D-S8-02** | **视图模型** | ✅ `Shared { view: std::sync::RwLock<Arc<View>> }`；`View { main: Arc<Segment>, deltas: Arc<[Arc<Segment>]>, tombstones: Arc<...>, generation: u64 }`。**发布 = 只替换一个指针** | §4.3 |
| **D-S8-03** | **H5：「读不阻塞写」的语义** | ✅ 取 **① + ②**（① 读路径**不取写锁**；② 写不抬高读延迟，口径落 **NFR-14 ②**），**③ 明确不做**（不做「读能看到未 commit 的写」） | §4.8 / §4.13 |
| **D-S8-04** | **段的 ID 空间** | ✅ **全局发号 + 段内本地 ID**，`base_doc / base_chunk = 前序所有段长度之和`；**读侧对外一律暴露全局 ID**；**合并保 ID（FIFO 追加式）** | §4.4 |
| **D-S8-05** | **`SearchIndex` 的并发形态** | ✅ **保持 `!Sync`**（`add(&mut self)` 一字不改）；并发 = 「**写端线程独占** + 读端持 **owned `Searcher`**」⇒ **零所有权破坏** | §4.1 / §4.8 |
| **D-S8-06** | **旧 API 处置（T7-18）** | 🔴 **需拍板**：✅ `SearchIndex::searcher(&self) -> Searcher`（**不消耗 self、不隐含 flush**）；`into_searcher()` 与 `into_index()` 转 **`#[deprecated]`** 薄封装；**其余 `pub` 面评估为「不废弃」** | §4.12 |
| **D-S8-07** | **BM25 跨段** | ✅ **全局统计量**（`N` / `total_len` / `df(t)` 全用**精确整数和**）+ TAAT 累加**按 term 外层、跨段内层** ⇒ **与单段布局逐位一致**（可证伪断言，§4.5.2） | §4.5 / S8-T4 |
| **D-S8-08** | **合并的宿主** | ✅ **库不 `spawn` 线程**：只提供 `merge_pending()`（合并一个段，增量为单位）/ `merge_all()`（合并到只剩主段）两个**原语**；后台线程由宿主 / CLI 起 | §4.9.4 |
| **D-S8-09** | **合并器向量侧取形** | 🔴 **需拍板**：✅ 默认 **dump → load → 增量 `insert`**；spike S8-S1 不过则回落**全量重建**（≈38.7s/12K） | §4.9.3 / S8-S1 |
| **D-S8-10** | **H6：NFR-11 写延迟口径与拟值** | ✅ 口径 = **`commit()` 的端到端 wall time（直接计时，不再是「推算」）**；**拟值** `P50 ≤ 1.0s / P99 ≤ 2.0s`（`batch_size = 64`、CPU/FP32、不含首次模型下载与加载），**待 S8-08 标定后定稿** | §4.13.2 / S8-08 |
| **D-S8-11** | **T7-26 查询侧会话池** | 🔴 **需拍板**：✅ **形态 = `Vec<Mutex<TextEmbedding>>` 池**（⚠️ fastembed **无 `sessions` 参数**，见 §2.7）；**默认 `embed_sessions = 1`（零行为变化）**；**投 / 不投由 spike S8-S2 判定**，不预先承诺 | §4.11 / S8-S2 |
| **D-S8-12** | **`compact()` 与视图的关系** | ✅ `compact()` 语义不变（**仍需独占、仍重编号**）⇒ 文档必须写明「**合并（merge）保 ID、回收（compact）改 ID**，两者不可混用」；`compact` 前**必须先 `merge_all()`** | §4.9.5 / R50 |

---

## 0. 开工前置三条的结账单（H4 / H5 / H6）

| # | 前置 | 状态 | 结论落点 |
| --- | --- | --- | --- |
| **H4** | delta 分段后的 manifest / 原子发布形态（是否出 **ADR-B**） | ✅ **本文回答：不开 ADR-B** | 依据 = §2.6 的源码事实 + §4.10 的推论：只要 **`save()` 把 `merge_all()` 作为第一步**，落盘形态就仍是**单段 `Index`**，`SnapshotSections` / `FORMAT_VERSION`（= 2）/ `GraphManifest`（`snapshot_crc` + `snapshot_len` + `nb_point`）**一个字节都不用改**。ADR-A 的三条不变式（manifest 唯一发布点 / 图 = 快照派生缓存 / `FORMAT_VERSION` 保持 2）**全部继续成立**。⇒ 落 **D-S8-01**；**ADR-B 登记为 Q2 的条件项**（若将来要做多段快照，H4 的原始担忧才成立） |
| **H5** | 「读不阻塞写」的语义 | ✅ **本文回答：取 ① + ②，明确不做 ③** | **① 读路径不取写锁** = 读端 `view.read()` 拿 `Arc` 后全程无锁（§4.3.3 的 I8-1）；**② 写不抬高读延迟** = 落 **NFR-14 ②**（§4.13.1 给相对 + 绝对双判据）；**③ 读能看到未 commit 的写 ⇒ 不做**（与 NFR-11「`commit()` 后可见」一致）。⇒ 落 **D-S8-03** |
| **H6** | NFR-11 的写延迟目标值 | ⏳ **本文只定口径与拟值，数值待 S8-08 标定** | 现有旁证 `eval-report.md` §8.11 的 **1.14 s/批**是**从累计 embed 耗时推算**、**不是直接计时**。本文把口径改成**直接计时**（`commit()` 的 wall time），并给**拟值** `P50 ≤ 1.0s / P99 ≤ 2.0s`（§4.13.2 给了推导链条）⇒ **同 NFR-13 / NFR-10 先例：先落「拟」、标定后定稿** |
| 附加 | **口径冲突择一**：FR-17 §5.3.5「**写入后**立即可查」 vs NFR-11「**`commit()` 后**立即可查」 | ✅ **本文收回（取 NFR-11）** | 取形 = **可见性 = `commit()` 后**（`plan-v2.md` §4.0 H5 已预写）。⚠️ **不改语义，只改措辞** ⇒ 本文建议把 `requirements-spec.md` §5.3.5 的「写入后立即可查」改写为「**`commit()` 后立即可查**」并保留原措辞引文（「被推翻的结论也写下来」纪律）。**该改写属需求面 ⇒ 需要评审同意后才动**（见 §6 与 PR 正文） |

---

## 1. 目标与验收

### 1.1 要解决的问题

| ID | 问题 | 证据（源码级） | 严重度 |
| --- | --- | --- | --- |
| **Q-U1** | **写端独占**：`into_searcher()` 后无法再写（FR-17 的「读不阻塞写」完整版未做） | `SearchIndex.inner` 直接持有 `Inner`（**非** `Arc`）；`into_searcher(mut self)` **消耗自身**把 `Inner` 包进 `Arc` 交给读端；读端想写必须 `Searcher::into_index()`（`Arc::try_unwrap`，clone 残留即 `Err`）⇒ **类型层面同一时刻只可能有一端存在** | 高（Step 8 本身就是它） |
| **R34** | **精确扫描的遍历含递归读锁** | 全量遍历用 `&PointIndexation` 的 `IntoIterator`（`HnswRsIndex::search_exact_filtered`）⇒ `IterPoint::new` 构造即 `points_by_layer.read()`（`hnsw_rs v0.3.4` `hnsw.rs:633`），迭代期间**不释放**；`IterPoint::next` 在**层切换**时**再取一次**同一把锁（`hnsw.rs:661`）⇒ 同一把 `RwLock` 被**递归读** | **高（永久挂死）**：现「不可达」仅因 `HnswRsIndex::add` 要求 `&mut self`（单写者）。**并发一开该路径即活**，若恰在层切换那一刻有写者在等待 ⇒ `std::sync::RwLock` 的读锁不可重入 ⇒ **永久阻塞**（架构 §14.3 R34 记录 std-only 探针 **3/3** 复现） |
| **R43** | **查询侧编码把并发吞吐封顶** | `LocalEmbedder { inner: Mutex<TextEmbedding> }`；`embed_query` **每次查询 `lock()`** ⇒ vector / hybrid 的端到端路径被串行化（实测 `QPS(4)/QPS(1)`：vector **1.88×** / hybrid **1.63×**，判据 **2.5×**） | 中（NFR-10 ③ 未达标） |
| **口径冲突** | FR-17 §5.3.5「写入后立即可查」vs NFR-11「`commit()` 后立即可查」 | 两处定义面互相矛盾（`requirements-spec.md` §5.3.5 表格 vs NFR-11 行；`plan-v2.md` §附-4 第 7 条） | 中（**设计期不选定 ⇒ 验收无法写**） |

### 1.2 对应需求

| 编号 | 口径（原文见需求文档） | 本 Step 的落点 |
| --- | --- | --- |
| **FR-17** | 增量写入：新增文档进入 delta 区，立即对检索可见；delta 在后台异步合并进主索引；**合并期间不得阻塞读请求**；不得以「重建索引中」为由拒绝查询 | **本文的主线**（§4.3~§4.10）。⚠️ 「写入后立即可查」按 H5 取形读作「**`commit()` 后**」（§0 附加行） |
| **NFR-14** | **（草案）** 读写并发下的读延迟与可见性：① 可见性 = `commit()` 后立即可查；② 读延迟 = 合并期间持续检索无失败无超时、P99 仍在 NFR-02 预算内；③ 不得以「重建索引中」为由拒绝查询 | §4.13.1 给**可判定**的测量口径与拟值（**数值与口径由本文定稿**） |
| **NFR-11** | 增量可见性与写延迟：`commit()` 后立即可查；写延迟有界 = flush 一批的耗时 | §4.13.2 **把口径从「推算」改为「直接计时」** + 拟值（D-S8-10） |
| **NFR-10** | 并发读吞吐：`--threads 4` QPS ≥ `--threads 1` × 2.5（**② 的适用范围已收窄为「不含查询侧编码的读路径」**）；**③ 编码侧并发保持「打开」**，复审触发条件 = 查询侧会话池落地时 | §4.11（T7-26）+ spike S8-S2 ⇒ **NFR-10 ③ 的唯一出口** |
| **NFR-02** | 混合 P99 < 20ms | §4.13.1 的 **②b 绝对判据**护栏；⚠️ 若分段本身抬高基线，须**显式声明 NFR-02 的适用范围**（不擅改数值） |
| **NFR-06** | 相同 query + 相同索引 → 完全相同结果（tie-break by `chunk_id`） | §4.14 的不变式 I8-4：**同一已提交内容、不同段布局 ⇒ BM25 结果逐位一致**（这是本文最强的一条设计属性）；⚠️ ANN 向量路**不在**该承诺内（§4.6.2） |
| **NFR-07** | 每次检索输出 `took` / 各 lane 耗时 / 候选数；**降级不得静默** | §4.13.3：新增 `Metrics.segments`（本次检索扫了几个段）与 `Metrics.tombstoned`（墓碑命中数）；合并的可观测由 `MergeReport` 提供 |
| **FR-30** | 墓碑物理回收（compaction） | §4.9.5：`compact()` 语义不变，但**必须先 `merge_all()`**；与合并的 ID 语义分离（R50） |

### 1.3 验收标准（Step 8 完成的定义，**可证伪**）

1. **R34 已消除（空转防护）**：`search_exact_filtered` 的全量遍历改为 **`for l in 0..=pi.get_max_level_observed() { pi.get_layer_iterator(l) }`**；由 **S8-T1**（std-only 探针复跑，**必须有 1 次「反转后为红」的反向自证**）与 **S8-T2**（覆盖等价：每点恰 `yield` 一次）双证。
2. **零回归（`deltas` 恒空时逐位一致）**：视图骨架落地后、delta 分段未启用时，**全部既有测试与 `make eval-quality` 的读数与改动前逐位一致**（`bm25` 三项 Δ = 0；hybrid 同图逐位一致）。**这条单列**，是「大重构不夹带行为变化」的护栏。
3. **跨段 BM25 逐位一致（核心断言）**：**同一内容**分别以「单段」与「主段 + N 个追加段」两种布局建库 ⇒ `bm25` 模式的 `hits`（`chunk_id` 序列 + `score` 逐位）**完全相同**；`Explain` 亦同。⚠️ **`vector` / `hybrid` 模式不在承诺内**（ANN 拓扑不同），但 **Brute 后端的 `vector` 模式必须也逐位一致**（精确路径确定性）。
4. **合并保 ID**：一次 `merge_pending()` 前后，**同一 chunk 的全局 `chunk_id` 不变**，且 `save` → `load` 往返后仍不变（S8-T8）。
5. **读不阻塞写（FR-17 的禁止项）**：后台合并进行中，读端持续发起检索 **100% 成功、无超时**，且**结果只反映已提交前缀**（不出现「合并完成后的内容」也不出现「丢段」）—— 由 S8-T10 的**并发探针**（写线程 + 读线程 + 合并线程三者同时跑）钉住。库内**不存在**「重建索引中」类错误码（S8-T10 用 `grep` 断言 `Error` 枚举无该变体）。
6. **NFR-14 有实测**：`eval-report.md` **§8.15** 收录合并期 / 静止期的 `took` P50/P99（同机同图同 query 集），给出 **②a 相对判据**与 **②b 绝对判据**的读数与**结论**；并给出 **NFR-11 写延迟**的直接计时 P50/P99。
7. **可见性语义**：`add()` 之后**未** `commit()` ⇒ 新 doc **查不到**；`commit()` 之后**立即可查**（S8-T7 双向断言，含反向自证）。
8. **确定性（NFR-06 不破）**：同一 view 连续 N 次检索逐位一致；`View.generation` 不变期间读端拿到的 `Arc<View>` 内容不变（S8-T9）。
9. **T7-26 有结论（投 / 不投均可合并）**：spike S8-S2 产出「查询侧口径的峰值 RSS 增量」与「Q3 数值一致性」两条读数；**若判「不投」，R43 保持打开、NFR-10 ③ 如实记为未达标**，并把该结论写进 §9 与 `eval-report.md`。
10. **守门全绿**：`make fmt && make lint && make test && make deny && make shell`；CI 全绿；新代码无 `unsafe`；**无新增第三方依赖**（`deny.toml` 不变）。

---

## 2. 现状与问题定位（源码级）

> ⚠️ 锚点规则（架构 §14.5 导读）：**本仓源码用「符号指代」**；**依赖源码**用「符号 + `v 版本` 下 `:行号`」。本文一律遵守。

### 2.1 今天读与写在**类型层面**就不可能共存

| 事实 | 位置 |
| --- | --- |
| `SearchIndex` 直接持有 `Inner`（**不是** `Arc`），且 `Inner` 注释写明「`Arc` 共享：`into_searcher` 零拷贝移交给读端」 | `crates/core/src/search/index.rs` 的 `Inner` 定义与 `SearchIndex.inner` 字段 |
| `into_searcher(mut self)` **消耗自身**，把 `Inner` 包成 `Arc` 交给 `Searcher` | `SearchIndex::into_searcher` |
| `Searcher` 持 `Arc<Inner>`，是 `'static + Clone + Send + Sync` | `crates/core/src/search/searcher.rs` 的 `Searcher` 定义 |
| `Searcher::into_index()` 用 `Arc::try_unwrap` 取回写端；**clone 残留即 `Err`** | `Searcher::into_index` |
| `SearchIndex` 文档明说它是 **`!Sync`**（含可变 `pending`，`add(&mut self)`） | `SearchIndex` 的结构注释 |

⇒ **今天的模型是「停世界交接（stop-the-world handoff）」**：写端交出所有权 → 读端检索 → 读端交回所有权。**没有共享可变状态，因此读路径一把锁都不取**——这正是 NFR-10 ① 的「逐位一致」在 Step 6 能轻松成立的结构性原因，也是 FR-17 完整版做不了的根因。

⚠️ **这条事实决定了 Step 8 的性质**：它不是「给现有结构加把锁」，而是**引入第一步共享状态**。所有风险都从这里长出来。

### 2.2 R34：递归读的**精确位置**与修法的可行性

**问题位置**（依赖源码，`hnsw_rs v0.3.4`）：

| 符号 | 位置 | 行为 |
| --- | --- | --- |
| `IterPoint::new` | `hnsw.rs:633` | 取 `points_by_layer.read()` 存入 `pi_guard`，**迭代全程持有** |
| `IterPoint::next`（层切换分支） | `hnsw.rs:656-677`，其中 `:661` | **再取一次** `self.point_indexation.points_by_layer.read()` ⇒ **递归读同一把 `RwLock`**（`:660` 取的是**另一把** `entry_point` 锁，不构成递归） |
| `IterPointLayer::new` | `hnsw.rs:701` | 取**一次** `points_by_layer.read()` |
| `IterPointLayer::next` | `hnsw.rs:715-723` | **只索引** `pi_guard[self.layer]`，**不再取锁** ⇒ **无递归** |
| `generate_new_point` | `hnsw.rs:498-526`，push 在 `:511` | `let mut p_id = PointId(level as u8, -1)`（**`:505`**；`level` 来自 **`:500`** 的 `layer_g.generate()` —— ⚠️ **2026-09-17 按评审 P4-2 更正**：原写「`p_id.0 = level`（`:500`）」，`:500` 实为随机抽层那一行）⇒ 每个点**只被推入它自己那一层**，**无回填低层** ⇒ **每点恰在一层** |
| `get_max_level_observed` | `PointIndexation`：`hnsw.rs:469-475`；`Hnsw`：`:814` | 返回 `entry_point.p_id.0`；`check_entry_point`（`:529-552`）保证 entry point 是**最大层**的点 ⇒ 它 = 全局最大层 |

**修法**（本仓，`HnswRsIndex::search_exact_filtered`）：

```rust
// 现在：递归读（IterPoint::next 层切换时再取锁）
for point in self.hnsw.get_point_indexation() { /* … */ }

// 改为：逐层遍历（每层一个独立 IterPointLayer，层间 guard 不重叠 ⇒ 无递归读）
let pi = self.hnsw.get_point_indexation();
for l in 0..=pi.get_max_level_observed() as usize {
    for point in pi.get_layer_iterator(l) { /* 循环体逐字不变 */ }
}
```

**覆盖等价性**（`v2-step5-design.md` §3.1 v0.2 的关键纠正，本文复核后确认成立）：

- 每点只在自己那一层 ⇒ `0..=max_level_observed` 的**并集 = 全部点、每点恰一次**；
- 单层 `get_layer_iterator(0)` **不是**全量（实测 N=5000 漏 **3.74%**）——**所以「逐层」的层号必须从 0 走到 `get_max_level_observed()`，不能只写 0**；
- **写法必须钉死**：`for l in 0..=pi.get_max_level_observed()`（⚠️ 不是 `pi.get_max_level()`——后者返回构造期授权的 `max_layer`（`:809-812`），**大于**实际观测层，会白跑空层；两者仅在「已建满」时相等）；
- ⚠️ **空图边界**：`get_max_level_observed()` 在 `entry_point == None` 时返回 **0**（`:470-474`），而 `points_by_layer` 由 `PointIndexation::new` 按 `max_layer` 预分配（`:448-456`）⇒ `get_layer_iterator(0)` 落在合法下标上、返回空迭代器 ⇒ **不 panic** ✅。库自身的 `debug_dump`（`:485-490`）用的就是同一个 `0..=max_level_observed` 模式，可作先例。

⚠️ **成本**：逐层写法把 1 次「长持锁」换成 `L+1` 次「短持锁」（`L` = 最大层，12K 语料下 `L` 很小，M=32 ⇒ `E[L] ≈ (ln N + γ)/ln 32 ≈ 2.9`；⚠️ **2026-09-18 按第 1 轮评审 P4-1 就地更正**：原写 `L ≈ log₁/₃₂(12000) ≈ 2.4` 为**误算**（该式实为 `2.71`），且 `L` 的口径是**最大次序统计量**）。**总量同量级，但每次持锁时长从 O(N) 降到 O(该层点数)** ⇒ 对写端的阻塞窗口从「整次扫描」降到「单层」。**这是本修法顺带的第二个收益**（R34 的原文只说了「消除死锁」，没说这个）。

### 2.3 字段索引 / 谓词 / 过滤**只认单个 `Index`**

| 事实 | 位置 |
| --- | --- |
| `try_build_predicate(index: &Index, filter) -> Option<Box<dyn CandidateFilter>>` —— **入参是一个 `&Index`** | `crates/core/src/query/filter.rs` 的 `try_build_predicate` |
| `ChunkFilter { doc_bits: DocBits, index: &'a Index }`；`contains(chunk_id)` 走 `index.doc_of(chunk_id)` | 同文件的 `ChunkFilter` / `impl CandidateFilter for ChunkFilter` |
| `allowed_count()` 是**硬契约必须精确**（不得返回估算值，否则 `prefers_exact` 会**静默失效**） | `crates/core/src/predicate.rs` 的 `CandidateFilter::allowed_count` 文档 |
| `AliveOnly` 借用 `&ChunkBits`（**零构建成本**的热路径谓词） | 同文件的 `AliveOnly` |
| `doc_bits(filter, index)` 从 `index.field_index()` 出发；`allowed_chunk_count(bits, index)` 再由 doc 位图换算 chunk 数 | `crates/core/src/query/filter.rs` 的 `doc_bits` / `allowed_chunk_count` |
| 编排层的热路径优化：**BM25 路在无用户过滤时传 `None`**（因为 `Index::remove` 已物理摘除 postings） | `crates/core/src/query/searcher.rs` 的 `bm25_f` 计算处 |

⇒ 跨段后 `ChunkFilter` 必须变成「多段谓词」，且 `allowed_count()` 必须是**各段之和**（精确性硬契约不变）。**并且**：一旦有墓碑落在**既往段**上，`bm25_f` 就不能再传 `None`（postings 没被物理摘除）⇒ **热路径回归**（登记 R52）。

### 2.4 ID 分配是**每 `Index` 内自增** ⇒ 段的 ID 空间必然重叠

| 事实 | 位置 |
| --- | --- |
| `ForwardStore::insert_doc`：`let id = self.docs.len()`，`docs.push(Some(doc))` | `crates/core/src/index/forward.rs` |
| `ForwardStore::insert_chunk`：`let id = self.chunks.len()`，`chunks.push(Some(chunk))`、`alive.set(id)` | 同文件 |
| `Index::add` 直接调上面两个并返回 `(DocId, Vec<ChunkId>)` | `crates/core/src/index/mod.rs` 的 `Index::add` |
| `Index::add` 是**幂等去重**的去重点：`content_hashes.get(&content_hash)` + `forward.doc(existing).is_some()` | 同处 |
| `ForwardStore::tombstone_chunk/doc` 只置 `None`，**不缩短 `Vec`** | `crates/core/src/index/forward.rs` |
| `Index::compacted(&IdRemap)` **会重编号**（单调保留相对顺序） | `crates/core/src/index/mod.rs` 的 `compacted` |
| `SnapshotSections` 里 `docs` / `chunks` 是 `Vec<Option<..>>`，**ID 由 DTO 显式携带** ⇒ save/load 往返**保 ID** | `crates/core/src/index/mod.rs` 的 `SnapshotSections`、`crates/core/src/storage/snapshot.rs` |

⇒ **两个独立 `Index` 的 ID 空间从 0 起、必然重叠** ⇒ 段必须带**基址**，且**合并只能追加、不能重编号**（否则用户手里的 `chunk_id` 会失效）。**但**：`tombstone` 不缩短 `Vec` ⇒ **「前序所有段长度之和」恰好等于下一段的基址**，且**合并（FIFO 追加）后仍然成立**（§4.4.2 给出证明骨架）。

### 2.5 【设计期新发现 A】BM25 的打分是**可加的** ⇒ 跨段逐位一致**结构性成立**

`Bm25Retriever::search_filtered`（`crates/core/src/retriever/bm25.rs`）的完整依赖面：

| 量 | 来源 | 类型 | 跨段是否可精确合成 |
| --- | --- | --- | --- |
| `idf(df)` | `n = index.num_chunks()`（u32）、`df`（u32） | **整数** | ✅ 精确可加（`ΣN`、`Σdf` 都是整数和） |
| `avgdl` | `index.avgdl()` = `total_len(u64) / num_chunks(u32)` | 整数除法 → f32 | ✅ 用**全局** `total_len` / `num_chunks` 算，与单段**同一个表达式、同一批数值** ⇒ 逐位相同 |
| `dl` | `index.chunk_len(posting.chunk_id)` | 每 chunk | ✅ 段内 |
| `tf` | `posting.tf` | 每 posting | ✅ 段内 |
| 累加 | `acc: HashMap<ChunkId, f32>`，**外层 `for token in tokens`** | f32 | ✅ **只要按「term 外层、跨段内层」遍历**，每个 chunk 在每 term 上**恰累加一次**、且**顺序与单段完全一致** ⇒ 逐位相同 |
| 排序 | `(score 降序, chunk_id 升序)` 全序 | — | ✅ chunk_id 用**全局** ID 即可 |

⇒ **结论（本文的核心设计性质）**：**只要把 `df` / `N` / `total_len` 换成全局精确整数和，并保持「term 外层」的累加顺序，分段布局的 BM25 结果与单段布局逐位一致。** 这**不是**近似、也不是巧合，是**结构**。

⚠️ 三条前提必须同时成立，任一被破坏都会让「只出现在分段布局下」的分数漂移（**极难归因**）⇒ 登记 **R49**，并把三条写进 rustdoc 不变式（§4.5.3）。

⚠️ **反面记录**：本发现推翻了一个直觉上很自然的设计——「每段各算各的 idf，再比分数」。那会让同一个 chunk 在 `[单段]` 与 `[主段+增量段]` 两种布局下**得到不同的分数**，且**只有分段时**才出现。本文明确**不采用**该取形。

### 2.6 快照 / manifest 都是「单段」形态（H4 的源码依据）

| 事实 | 位置 |
| --- | --- |
| `save()` 第一步就是 `commit()`；随后 `save_with_crc(path, &self.inner.index, &vectors, &fingerprint)` **只序列化一个 `Index`** | `SearchIndex::save` |
| 图 sidecar 的发布点是 **manifest**：`snapshot_crc` + `snapshot_len` 作「版本锚点」，`nb_point: u64` 记点数 | `crates/core/src/storage/graph.rs` 的 `GraphManifest` |
| manifest 原子发布 = `write_manifest_atomic` → `atomic_write`（tmp → flush → `sync_all` → rename → fsync 父目录） | `crates/core/src/storage/graph.rs` / `crates/core/src/storage/atomic.rs` |
| ADR-A 的三条不变式：**① `FORMAT_VERSION` 保持 2**；**② manifest 是唯一原子发布点**；**③ 图 = 快照的派生缓存（可随时丢弃）** | `architecture-design.md` 的 ADR-A |
| `load_with` 只 `load_with_crc(path)` 出一个 `Index` + `raw_vectors` | `SearchIndex::load_with` |

⇒ **H4 的担忧（「manifest 要挂多段图」）只在「落盘也要多段」时才成立**。**若 `save` 前先把 delta 全部合并**（D-S8-01），则落盘形态**一个字节都不变** ⇒ **ADR-B 不开**，ADR-A 的三条不变式全部续存。

### 2.7 【设计期新发现 B】「查询侧会话池」的真实形态：**fastembed 没有 `sessions` 参数**

| 事实 | 位置 |
| --- | --- |
| `LocalEmbedder { inner: Mutex<TextEmbedding>, dim }`；`embed_query` **每次查询 `lock()`** | `crates/core/src/embed/local.rs` |
| `TextEmbedding` 的字段 = `tokenizer` + `session` + `pooling` + `need_token_type_ids` + `quantization` + `output_key` | `fastembed v6.0.2` `src/text_embedding/init.rs:163-170` |
| `embed(&mut self, texts, batch_size)` —— **`&mut self`**（`transform` 需要可变） | `fastembed v6.0.2` `src/text_embedding/impl.rs:453-456` |
| `InitOptions` 的线程参数**只有 `intra_threads`**（`with_intra_threads`），**没有 `sessions`** | `fastembed v6.0.2` `src/text_embedding/init.rs:33`（字段）/ `:67-69`（builder） |
| `init_session_builder(execution_providers, intra_threads)`：`None` ⇒ `std::thread::available_parallelism()`；然后 `Session::builder().with_intra_threads(threads)` | `fastembed v6.0.2` `src/common.rs:262-290` |
| 「多个 session」在既有 spike 里是**临时代码**：`bench_embed_session.rs` 自己定义 `sessions: usize` 档位，用 `new_session(...)` **建多个 `TextEmbedding` 实例** | `crates/core/examples/bench_embed_session.rs`（E1/E2/E3 档位表与 `new_session`） |

⇒ **两处口径澄清**（进入 §6 影响面与 PR 正文）：

1. **「每 worker 独立 session」= 每 worker 一个 `TextEmbedding` 实例**（各含一份 `Session` + `Tokenizer`）；
2. ⚠️ **`plan-v2.md` §4.0 H5 / Step 8 与 issue #58 写的「不同 `sessions` / `intra_threads`」里的 `sessions` 不是 fastembed 的参数** —— 它是 spike harness 自己的档位概念。⇒ **Q3 应改述为「不同 `intra_threads`（以及不同实例数）是否改变 `embed_query` 的向量数值」**。**建议在后续计划修订中把该措辞改掉**（本文不改计划，只在 §6 登记）。

### 2.8 【设计期新发现 C】R36 的「不投」数据**不能外推到查询侧**

| 事实 | 位置 / 读数 |
| --- | --- |
| R36：多 session 抬高内存峰值。E2 实测 **2 session 峰值 RSS +96.4%**、**4 session +289.2%** | `architecture-design.md` §14.4 R36；`eval-report.md` §8.10 |
| **但 E2 的口径是「建库路径」**：`data/t2-corpus.jsonl` 前 4000 段、**按 64 切片**（复刻 `flush` 的真实切片）、长文本 | `bench_embed_session.rs` 的口径表 |
| R36 的残余列已写明：**「该『不投』只对建库侧成立，不可外推到查询侧」** | `architecture-design.md` §14.4 R36 残余列 |
| 查询侧是 **batch 1 × 短 query**；ORT 的 per-run 激活 arena 与 batch / 序列长度强相关 ⇒ **每 session 的常驻开销在查询侧应当小得多（待实测）** | 推理（**本行为推断，非实测**） |

⇒ **T7-26 的决策门必须按查询侧口径重测**，**不能引用 E2 的 +96.4%/+289.2% 作判据**。登记为 §4.11.3 的决策门与 **R54**。

---

## 3. 设计约束（既有事实，本文不重新论证）

### 3.1 「逐位一致」是本仓的硬通货

Step 5 的 I3/I4（BM25 与精确向量路径的逐位一致）、Step 2/3 的 NFR-06（同快照两次加载逐位一致）、Step 6 的「逐位一致是并发吞吐判据的前置条件」——**本仓的质量门主要靠「逐位一致」而不是「统计上更好」**。⇒ 本文把 §2.5 的可加性**升格为设计约束**，并据此写 S8-T4。⚠️ **同时必须诚实地划出它不覆盖的地方**（ANN 向量路），见 §4.6.2。

### 3.2 `hnsw_rs` **没有图合并 API**

- 无 `merge` / `union`；无 `remove`（Q-C1 的既定事实）；`Hnsw` 无 `Clone`。
- 唯一的「结构复用」通道是**持久化往返**：`file_dump`（`VectorGraphPersist::dump_graph`）→ `load_graph` / `load_hnsw*`。
- ⚠️ 注意 `graph_basename()` 与 `basename 铁律`（`file_dump(dir, "foo.idx")` 会**自行追加** `.hnsw.graph` / `.hnsw.data`；传错会**静默永远降级**）。
- ⚠️ `load_hnsw*` 的 `'a: 'b` ⇒ `Hnsw<'static>` 只能 `Box::leak`（R23，**无回收**）。

### 3.3 后台合并**不能改 `chunk_id`**

`compact()` 的重编号（`IdRemap`）是**显式、用户触发**的既定行为（FR-30），文档已写明「ID remap 单调 ⇒ BM25 顺序不变」。但**后台合并是自动发生的** ⇒ 若它也重编号，用户（尤其 Agent 的记忆记录）手里的 `chunk_id` 会**静默失效**。⇒ 本文把「**合并保 ID**」写成**强约束**，并把它与 `compact()` 的语义**显式分离**（R50 / D-S8-12）。

### 3.4 守门与依赖预算

无 `unsafe`、无新增第三方依赖（`deny.toml` 不变）、MSRV 1.90、`--no-default-features` 与各 feature 组合都要编译、`make shell` 要过。
⇒ **本设计的并发原语只能用 `std`**：`std::sync::RwLock`（视图指针）+ `std::sync::Mutex`（ID 发号 / delta 构建）+ `std::thread`（由**宿主/CLI** 起，不进库）。⚠️ **不引入 `arc-swap` / `crossbeam`**。

### 3.5 「文档与代码冲突时以代码为准」

本轮已核实并**修正了三处计划/文档措辞**（见 §2.7 与 §2.8）：`sessions` 参数不存在、R36 读数不可外推。⇒ 本文与四处定义面的回写**一律以源码与实测为准**。

---

## 4. 详细设计

### 4.1 总览：四件事的落点与耦合度

| 任务 | 落点 | 与「delta 分段」的耦合 | 可独立合并？ |
| --- | --- | --- | --- |
| **T7-25**（R34 修法） | `crates/core/src/vector/hnsw_rs_index.rs`，**一个函数体的循环改写** | **零耦合** | ✅ **建议第一个合**（风险最低、收益明确、可独立评审） |
| **T7-06**（delta 分段） | 新增 `search/view.rs` + 跨段 BM25 / 向量 / 谓词 + 合并器 | 本体 | 拆成 PR 3~5（见 §8） |
| **T7-26**（会话池） | `embed/local.rs` + `Config` | **零耦合**（只在编码侧） | ✅ 可并行于 PR 4/5 |
| **T7-18**（旧 API） | `search/index.rs` / `search/searcher.rs` | **依赖 T7-06 的 API 形态**（`searcher()` 必须先存在） | ❌ 必须与 PR 3 同批 |

⚠️ **这张表本身是一条结论**：**四条任务里有两条与最重的那条完全正交** ⇒ Step 8 可以「先摘低垂果实、再啃硬骨头」，不必等设计全部落地才能有产出。

**总览图（文字版）**：

```
写端线程（独占一个 SearchIndex，!Sync，add(&mut self) 不变）
  └─ SegmentBuilder { delta: Index, pending: Vec<PendingChunk>, raw: Vec<..>, vector: Option<VectorIndex> }
        │  commit()
        ▼
  ┌───────────────────────────  Shared { view: RwLock<Arc<View>> }  ──────────────────────────┐
  │  View { main: Arc<Segment>, deltas: Arc<[Arc<Segment>]>, tombstones: Arc<..>, generation } │
  └──────────────────────────────────────────┬────────────────────────────────────────────────┘
        ▲ searcher(&self) → owned Searcher（持 Arc<Shared>）       │ 后台合并（原语 merge_pending()）
        │                                                          ▼
读端线程（N 个，持 Arc<View>，全程无写锁）           新 View（main 增长、deltas 缩短）⇒ 一次原子指针替换
```

### 4.2 T7-25：R34 修法（`S8-01`）

见 §2.2 的完整定位与证明。**落点**：`HnswRsIndex::search_exact_filtered` 的遍历循环。

**必须同时落地的四条回归锁**（`plan-v2.md` §4 Step 8 的评审补强建议，本文照收）：

| # | 锁 | 落点 |
| --- | --- | --- |
| ① | **std-only 探针复跑**（证「递归读已消除」） | S8-T1（探针现成、成本极低；**必须有一次「改写前为红」的反向自证**，否则无法证明探针有鉴别力） |
| ② | **覆盖等价断言「每点恰 `yield` 一次」** | S8-T2（基数 == `get_nb_point()` + 去重后仍相等 + 与入库 ID 全集相等） |
| ③ | **逐层写法钉死为 `for l in 0..=pi.get_max_level_observed()`** | 代码注释 + S8-T2 的一条「换成 `get_layer_iterator(0)` 立刻红」的断言 |
| ④ | **定向用例**：取一个 `level ≥ 1` 的点，用它自己的向量查、断言排第一 | **已存在**（`HnswRsIndex` 的 `精确扫描覆盖高层点_定向用例`）⇒ 本 PR 只需确认它仍绿，不必新建 |

⚠️ **③ 的负向断言要小心**：`assert!(覆盖数 < get_nb_point())` 这类断言在「层级抖动」下是**概率性**的（`LayerGenerator` 用 `StdRng::from_os_rng()` 无 seed）⇒ 沿用既有用例的做法：**先断言 victim 不在 layer 0**（证明鉴别力），再断言结果正确。

#### 4.2.1 实施结果（`S8-01` / T7-25，2026-09-18）

**落点与形态**：`crates/core/src/vector/hnsw_rs_index.rs` 的 `search_exact_filtered`，
写法与 §4.2 逐字一致：

```text
let pi = self.hnsw.get_point_indexation();
for layer in 0..=pi.get_max_level_observed() as usize {
    for point in pi.get_layer_iterator(layer) { /* 循环体逐字不变 */ }
}
```

原 rustdoc 的 `# ⚠️ 全量遍历必须用 `&PointIndexation` 的 `IntoIterator`` **整段重写**为
「必须**逐层**（不可用 `IntoIterator` —— R34 递归读）」，含 5 段：① 递归读的机制与出处、
② 层号范围（含 `1/M` 漏点率）、③ **不可用 `get_max_level()`**、④ 空图边界、
⑤ 成本（单次持锁由 `O(N)` 降到 `O(该层点数)`）。**不改任何决策**。

🔴 **实现期新发现 ①（覆盖边界，已实测）**：**覆盖类断言必须经由生产函数**。
`全量遍历基数等于点数且零重复` 原写法是**在用例内复刻**那个逐层循环 ⇒ 把**生产**循环的层号上界
改成 `0..=0` 时它**仍然绿**（**变异 M1 实测**）⇒ 已改为经 `search_exact_filtered(q, n, None)`
断言覆盖等价（同一次变异下**变红**）。用例内保留的逐层并集断言只作 `get_layer_iterator`
的 **API 契约**证据（②），不承担「锁生产代码」的职责。

🔴 **实现期新发现 ②（覆盖边界，原文 + 2026-09-18 第 1 轮评审后的就地更正）**：
**原写**：把生产循环**改回 `IntoIterator`** 时，**本仓没有任何 CI 断言会红** ——
因为那条路径**覆盖仍然正确**（`IntoIterator` 的语义就是全量）。
⇒ 🔴 **更正（原表述已不准确）**：**加了空图断言之后**，该回退**在空图这一条输入上会被抓住** ——
旧写法在空图上 panic 于 `hnsw.rs:662` 的 `entry_point_ref.as_ref().unwrap()`，
新增的 `空图精确扫描返回空且不panic` 因此**变红**（**M4 变异实测**：该用例红、
**其余 10 条全绿** ⇒ 空图这一条是本仓**唯一**能抓住该回退的断言）。
⚠️ **仍抓不住的情形**：改回 `IntoIterator` **且同时**补空图早退 —— 那种写法仍带 R34 的递归读，
只能靠 **std-only 探针（`tests/step8_rw_concurrency.rs`）作为可执行证据 + 评审红线**
（同族的「不变式靠评审守」见架构 §14.6）。⇒ **不声称「该回退已被完全覆盖」。**

**S8-T1 的实际形态**（`crates/core/tests/step8_rw_concurrency.rs`，std-only、无 `hnsw_rs` 依赖）：

| 臂 | 内容 | 断言 |
| --- | --- | --- |
| **正向** | 「取锁 → 迭代 → **释放** → 下一层」（= 新形状），全程有一个**已排队的写者** | 必须**完成**（活性）；把层间 `drop` 去掉即**超时变红**（变异 M2） |
| **反向自证** | 「取读锁 → **持锁期间再取一次**」（= 旧形状，对应 `hnsw.rs:633` + `:661`） | 第二次读**必须被拒**：`try_read` 臂（精确、不挂线程）+ 阻塞臂（R34 本体，有界 300ms） |

🔑 **「写者已排队」是被观测的事实，不是计时假设**：`try_read()` 在读锁**共享**语义下
「无写者时必然成功」⇒ 它在持读锁期间失败**只能**由「有写者等待」造成 ⇒
`等到写者排队()` 以**自旋直到观测到**替代 `sleep`。
🔑 **写者必须受闸**：直接 `spawn(|| lock.write())` 时，写者可能在读线程拿锁**之前**就取到并释放
⇒ 前提永不成立（**实测**：无闸时 `等到写者排队` 自旋 10s 超时；加闸后立即通过）。
**平台前提实测**（各 5 轮全数命中）：macOS（`pthread_rwlock_tryrdlock`）与
**Linux `rust:1-slim`（= CI 的 `ubuntu-latest`）** —— 后者用 Docker 实跑，
因为「写者优先」是 R34 成立的**环境前提**，不能靠推断。

**变异表**（`变异手法 | 命中门 | 失败输出`）：

| # | 注入 | 结果 |
| --- | --- | --- |
| **M1** | 生产循环层号上界 → `0..=0` | ✅ **4 条红**：`精确扫描条数为min_k与allowed` / `精确扫描与暴力逐位一致`（I4）/ **`全量遍历基数等于点数且零重复`** / `精确扫描覆盖高层点_定向用例`。⚠️ **首轮注入时第 3 条仍是绿的**（因为当时它在用例内复刻循环）⇒ 据此改了 ① 的写法 |
| **M2** | 正向臂 `drop(g0)` → `std::mem::forget(g0)`（= 退化成旧形状） | ✅ **正向臂红**（10.01s 超时，消息指认「疑似退化成持读锁期间再取锁」）；反向臂**仍绿**（它测的是另一件事） |
| **M3′** | `受闸写者` 改取**读**锁（⇒ 永不产生排队的写者） | ✅ **两臂皆红**，且失败消息正是**预期的那条**（「10s 内未观测到写者排队 ⇒ 新读者被拒」，10.00s） ⇒ **前提观测有牙齿** |
| **M4** | 生产循环**改回 `IntoIterator`**（`for point in self.hnsw.get_point_indexation()`） | ✅ **1 条红**：`空图精确扫描返回空且不panic`（panic at `hnsw_rs.rs-0.3.4/src/hnsw.rs:662:62`）；**其余 10 条全绿**（含 I4 逐位一致与经生产路径的覆盖等价）⇒ 本仓其余断言对该回退**零鉴别力**、本用例是**唯一**绊线 |
| M3（废弃） | `受闸写者` 不等闸（保留通道） | ⚠️ **不作为证据**：该注入使接收端被丢弃 ⇒ `go_tx.send` 失败 ⇒ 红是**握手断裂**、不是「写者抢跑」⇒ 如实标注，换成 M3′ |

**行号漂移（实测，非目测）**：用 `git diff -U0` 的 hunk 累计位移逐个换算 ——
本文件的改动全部落在 **`≥ 302` 行**，而**活文档里 13 处**对该文件的行号引用
（`v2-step1/2/4/5-design.md`）**全部 ≤ 257** ⇒ **本 PR 零漂移**。
（`v2-step5-design.md` 等**历史设计文档**里的行号**按纪律不改写**；若要治本，走已在
架构 §14.5 登记的「全仓锚点符号化」立项。）

**🔧 第 1 轮评审响应（2026-09-18，PR #61）**：评审落点 = `pulls/61/reviews` **1 条**
（**4×P4、无阻塞**）+ **行内 4 条**；`issues/61/comments` = **0**。落点 SHA = `19635d7`（= 当时 head）。

| # | 评审意见 | 处置 | 落点 |
| --- | --- | --- | --- |
| **P4-1** | rustdoc ⑤ 与 CHANGELOG 的 `L ≈ log₁/₃₂(12000) ≈ 2.4` 实算 ≈ 2.71 | ✅ 采纳（数值勘误）——🔑 **按「一类缺陷全扫」又抓出 1 处评审未点到的**（**§4.2 正文的成本段**）⇒ **本 PR 内共 3 处** | rustdoc ⑤ / **设计 §4.2 正文** / CHANGELOG 一并改为 **`E[L] ≈ (ln N + γ)/ln 32 ≈ 2.9`**（`L` 的口径是**最大次序统计量**，不是 `log₃₂ N`；`log₃₂ 12000 = 2.71` 作粗式参考，并在文中标注原值 `2.4` 为误算） |
| **P4-2** | `全量遍历基数等于点数且零重复` 的 `assert!(l0 < n)` 被前两条蕴含 | ✅ 采纳 → **删除** | 同用例 ③ 处（改为一行注释说明「**永不报红**、零鉴别力，留着会让人高估覆盖度」） |
| **P4-3** | 为「改回 `IntoIterator` 无 CI 断言会红」提供**源码级**绊线（`include_str!` + `contains`） | ❌ **不立项**，改交**行为级**替代物 | 新增单测 `空图精确扫描返回空且不panic`（见下） |
| **P4-4** | PR 正文 §6 的 run id `35310301302` 笔误 | ✅ 采纳 | PR 正文（改 `35310501302`） |
| **顺带收益** | 旧 `IntoIterator` 在**空图**上 panic（`hnsw.rs:662`） | ✅ 采纳（**并升级为测试**） | rustdoc ④ 末尾补半句 + 新增空图断言（不只写在注释里） |

🔑 **P4-3 不立项的理由**（评审明确「是否立项由你拍板」）：源码级文本断言属「**作者意图的近似**」
而非**可观测结果**（对格式/写法敏感、改写形式即可绕过），与本仓既有教训（判据要选可观测结果）相抵；
更关键的是**已有行为级替代物**：空图断言能抓住**同一条回退**，且可做变异验证（M4）⇒ 采用替代物即**超额**满足 P4-3 的目的。⚠️ 边界如实写明：两者都**不是证明**，且都抓不住「改回 + 补空图早退」。

**留给收尾（S8-09）的清单**：`plan-v2.md` §7 进度表的 T7-25 行改 ✅（定义面本 PR 不碰）。

### 4.3 视图模型（`S8-02` / D-S8-02）

#### 4.3.1 三个类型

```rust
// crates/core/src/search/view.rs（新）

/// 一个**不可变**段：段内 `Index` 的本地 ID 从 0 起，对外用 `base_*` 折算成全局 ID。
pub(crate) struct Segment {
    pub index: Index,                                  // 冻结后不再修改
    pub vector: Option<Box<dyn VectorIndex>>,          // 该段的向量侧（可为空：纯 BM25 段）
    pub raw_vectors: Vec<(ChunkId, Vec<f32>)>,         // 段内本地 chunk_id
    pub base_doc: DocId,
    pub base_chunk: ChunkId,
    pub generation: u64,                               // 段的创建序号（诊断 / 归并排序）
}

/// 读端看到的一致快照。**不可变**：任何变化都产出新的 `Arc<View>`。
pub(crate) struct View {
    pub main: Arc<Segment>,
    pub deltas: Arc<[Arc<Segment>]>,                   // FIFO；合并按序消费
    pub tombstones: Arc<Tombstones>,                   // 针对**既往段**的全局 doc 墓碑
    pub generation: u64,
}

/// 写端与读端共享的唯一对象。
pub(crate) struct Shared {
    view: std::sync::RwLock<Arc<View>>,                // ⚠️ 写锁**只**用于替换指针
    ids: std::sync::Mutex<IdAllocator>,                // 写侧串行；读侧永不触碰
    cfg: Arc<Config>,
    graph: GraphOpts,
    backend: VectorBackend,
}
```

⚠️ **为什么不引入 `arc-swap`**：守门约束「无新增依赖」（§3.4）。`RwLock<Arc<View>>` 的**写锁持有时长 = 一次指针替换**（纳秒级），足以满足 I8-1。

#### 4.3.2 发布协议（唯一写点）

```
fn publish(shared: &Shared, next: View) {
    // ⚠️ I8-1：进入写锁**之前**，`next` 必须已经完整构造好。
    //          写锁内只允许 `*guard = Arc::new(next)` 这一句。
    *shared.view.write().expect("视图锁中毒") = Arc::new(next);
}
```

🔴 **实现已收敛（`S8-02` 评审 P2 的结构性对策，2026-09-18）**：本片段描述的是**不变式**（写锁内
只做一次指针替换），而不是要求有一个叫 `publish` 的方法。实现把发布**内联进
`Shared::commit_view()`**，**不再单独暴露 `publish`** —— 因为只要有独立的 `publish`，
「取快照 → 算 base → 推进序号 → 发布」就**可以**被写成四段分离（那正是 P2 指出的缺陷形状：
双写端并发 `commit()` 时 `generation` 可回退、且带着同一形状进 `S8-03` 会让两个 delta 段
拿到相同 `base_*`）。⇒ **消除入口，而不是加一个检查**。详见 **§4.3.5**。

🔴 **`commit_view` 的 `build_next` 可返回 `Err`（2026-09-19 第 2 轮评审 P1-2）**：发布前的
**锁内**判据不止「世代」（见 §4.3.5 的 `expected_epoch`），还包括**快照前缀身份**（`fold` 的
窗口吸收，见 §4.9.6）。这类判据**只能**在锁内做（锁外做等于没做）⇒ 闭包必须能把失败带出来，
且失败时**不做任何记账**（序号不消耗、`base` 不推进、视图不动 —— 有判据：`S8_03_发布失败时不消耗任何记账`）。
⚠️ 这不违反 I8-1：写锁内**仍然只有一次指针替换**，检查发生在 `ids` 锁内。

#### 4.3.3 不变式（写进 rustdoc 的三条）

| ID | 不变式 |
| --- | --- |
| **I8-1** | **写锁只用于指针替换**：`Shared::view` 的写锁内**不得**做任何计算、I/O 或分配（除 `Arc::new`）。任何「先取写锁再干活」的写法都必须在评审里被拒。 |
| **I8-2** | **段一旦进入过任何 `View`，永不再被修改**（`Segment` 全程 `&` 访问；要改就造新段）。 |
| **I8-3** | **读端只在取快照那一刻取一次读锁**：`let v: Arc<View> = shared.view.read()?.clone();` 之后全程无锁；一次检索**只用一个 `View`**（不得在一次检索中途重新取）。 |

⚠️ **I8-3 的最后半句是正确性的关键**：如果一次检索中途重新取 view，可能出现「BM25 用了 v1 的段、向量用了 v2 的段」⇒ 跨 lane 的 chunk 语义不一致。**必须一次取定**。

#### 4.3.4 实施结果（`S8-02` / `T7-18`，2026-09-18，**PR3**）

**落点**（新增 1 文件 + 改 3 文件 + 迁移 2 处调用点）：

| 文件 | 变更 |
| --- | --- |
| `crates/core/src/search/view.rs`（**新**） | `Segment` / `View` / `Shared` / `Tombstones` / `IdAllocator` / `SegmentSums` + `snapshot` / `with_main_mut` / **`commit_view`**（发布内联在此；~~`publish`~~ / ~~`advance`~~ 已在 PR #62 第 1 轮评审响应中**删除**） |
| `crates/core/src/search/index.rs` | 删 `Inner`（内容容器上移为 `Segment`）；`SearchIndex` 改持 `Arc<Shared>`；新增 `seg()` / `searcher(&self)`；`into_searcher()` 转 `#[deprecated]` 薄封装；`commit()` = `flush()` + **发布** |
| `crates/core/src/search/searcher.rs` | `Searcher` 改持 `Arc<Shared>`；`parts()` / `has_vector()` / `infer_mode()` / `run()` 接 `&View`；`SearchRequest::exec` 取一次快照（`I8-3`）；`into_index()` 转 `#[deprecated]` |
| `crates/cli/src/main.rs`（3 处）/ `examples/search_basic.rs`（1 处） | **迁移**到 `commit()` + `searcher()`（示范新用法） |
| `tests/*.rs`（7 个文件） | 加 `#![allow(deprecated)]`（**刻意**沿用旧 API 以锁住其行为不变） |

⚠️ **`deltas` 恒空 + 不动 `FORMAT_VERSION`**：`commit()` **不移动任何内容**（内容只有一段），
只把视图指针重新发布一次 ⇒ **`save` / `load` / `snapshot` 形态一字未改**（与 §4.10 一致）。

##### 🔴 与设计正文的**偏差**（5 条，需评审确认）

1. **`into_index()` 不再可能失败**（**行为变更**）。§4.12 已指定「用同一 `Arc<Shared>` 造新写端」，
   但即时后果是：**「`Searcher` clone 残留」与「写端存活」在 `Arc<Shared>` 视角下不可区分**，
   而后者在 `S8-02` 是**合法状态** ⇒ `Arc::try_unwrap` 的「refcount > 1 则报错」语义**不可实现**。
   ⇒ 取形 = **总是成功**（返回类型 `Result` 保留不变，为将来留空间）。
   ⚠️ **这正是「双写端」成为可能的入口** ⇒ `commit()` 全程持 `Shared::ids` 锁**从本条起就是必需的**
   （设计 §4.4.3 / 评审 P3-4 的前置条件在 PR3 即已成立）。既有单测 `clone残留时into_index报错`
   已按新语义改口径为 `S8_02_into_index总是成功且与旧clone共享视图`。
2. **过渡约束：写操作要求「无并发读者」**。`deltas` 恒空 ⇒ 内容唯一 ⇒ 写端经
   [`Shared::with_main_mut`] 用 `Arc::get_mut` **就地改 `main`**。有读者持有当前 `Arc<View>` 时
   返回 `Err`（消息含「需要独占」）。**该约束由 `S8-03`（delta 写入）解除**，届时本入口整体删除。
   也因此 **`I8-2`（段不可变）在 PR3 尚未成立** —— 已在 `view.rs` 的模块文档里如实标注。
3. **可见性语义**：`add` 之后**未** `commit` 也能查到倒排 —— **刻意保留**（§1.3 第 2 条要求
   「行为与重构前逐位一致」）。§4.8.3 终态的「未 `commit` 不可见」由 `S8-03` 收紧，
   届时用例 `S8_02_可见性语义与重构前一致_add未commit亦可查到倒排` 的断言**必须反转**。
4. **`ViewFilter` 未引入**（属 `S8-04`）；`IdAllocator` 本阶段只承载「段的 `base_*` + 视图序号」，
   **未接管 chunk 发号**（`base_*` 恒 0 ⇒ 全局 ID == 本地 ID，与重构前逐位一致）。
5. **`tombstone_stats()` 已改成「跨段求和」的形状**（`main` + `deltas` + 阶段不变式断言），
   与 §4.9.5 的表一致；`S8-03` 起无需再改该函数。

##### 测试（**本 PR 新增 13 条**）

| 落点 | 用例 | 说明 |
| --- | --- | --- |
| `view.rs`（5） | `S8_02_初始视图的deltas恒空且序号为零` / `S8_02_提交只递增序号且不产生新段` / **`S8_02_有读者时写入必须失败`** / `S8_02_已取出的快照不受后续提交影响` / **`S8_02_跨段求和真的遍历deltas`**（PR #62 响应新增） | 骨架不变式；第 3 条在 `S8-03` 时应**删除**（不是改成永远绿）。⚠️ 前两条的旧名分别是 `…发布只递增序号…` / `…不受后续发布影响`（`publish` 已删，见 §4.3.5） |
| `index.rs`（2） | `S8_02_commit递增视图序号` / **`S8_02_并发commit不丢失视图序号`**（PR #62 响应新增：4 写端 × 150 次 + 观察线程） | 🔴 **必须是单测**（见下方覆盖边界 ①）；后者是 P2 的**回归锁**（修复前实测 70 次回退） |
| `tests/step8_segments.rs`（8） | `S8_T9_同视图内检索逐位一致`（50 次）/ `S8_02_searcher不消耗写端且读端能看到后续提交` / `S8_02_searcher不隐含flush` / `S8_02_旧API与新API结果逐位一致` / `S8_02_into_index换回写端后继续add` / `S8_02_searcher仍满足static_clone_send_sync` / `S8_02_可见性语义与重构前一致_add未commit亦可查到倒排` / `S8_02_commit后立即可查且幂等` | 外部可观测行为 |

##### 变异验证（`M5` / `M6′`，2 组）

| # | 注入 | 实测结果 |
| --- | --- | --- |
| **M5** | `commit()` **不发布**（当时的取形是调用 `publish` 方法） | ⚠️ **首轮零命中**（`tests/step8_segments.rs` 8 条**全绿**）⇒ 补 `index.rs` 的单测后 **1 条红**（`S8_02_commit递增视图序号`），其余 10 条绿。⚠️ **时点**：`publish` 方法已在 PR #62 第 1 轮评审响应中删除（见 §4.3.5）；该变异在今天的代码上等价于「`commit_view` 不发指针」 |
| **M6′** | `into_searcher()` 去掉隐含 `commit()` | ⚠️ **首轮零命中**（全仓测试**全绿**）⇒ 把「旧 API vs 新 API 逐位一致」改用**带向量 lane** 的装配后 **1 条红**，其余 7 条绿 |

##### 🔴 两条覆盖边界（**如实登记，不声称已覆盖**）

① **「`commit()` 不发布」在外部 API 层面不可观测**：`S8-02` 期内容与视图**同源**
   （写端就地改 `main`）⇒ 读端读的还是那一个段、照样看得到新内容 ⇒ **任何集成测试都抓不住它**
   （**M5 实证**）。能抓住的只有 `pub(crate)` 可见的单测（`generation` 递增）。
   `S8-03` 起内容落进 `deltas` 后，§1.3 第 7 条的可见性用例才会从外部覆盖它。
② **「`into_searcher` 的隐含 flush」只在带向量 lane 的装配下可测**：纯 BM25 下 `pending` 恒空
   ⇒ `flush` 是 no-op ⇒ 删掉隐含 flush **全仓测试全绿**（**M6′ 实证**）。
   ⇒ 该用例已改用带假 embedder 的装配，并在注释里写明「为什么必须是这个装配」。

⚠️ **另有一条由类型系统保证、无断言覆盖**（不是缺陷，但要知道）：
`searcher(&self)` **不可能**隐式 `flush` —— `flush` 需要 `&mut self`，而本方法是 `&self`。

#### 4.3.5 第 1 轮评审响应（2026-09-18，**PR #62**）

**评审落点**：`pulls/62/reviews`（**1 条：1×P2（阻塞）+ 2×P3 + 3×P4**）+ **行内 5 条**；
`issues/62/comments` = **0**；落点 SHA = `1ad692b`。评审自陈为**静态评审**（未复跑守门链，以 CI 10/10 为准）。
**结论：6 条全部采纳**（P2 本 PR 内修复；2×P3 本 PR 内完成；3×P4 顺手完成）。

##### 逐条处置

| # | 级别 | 问题 | 处置 | 落地位置 |
| --- | --- | --- | --- | --- |
| **1** | 🔴 **P2** | `commit()` 的 `snapshot → advance → publish` **三段不在同一临界区** ⇒ 双写端并发时 **`generation` 可回退**；且旧「单调性告警」比较的是**发布前的旧 `cur`** ⇒ 永不触发 | ✅ **本 PR 修**（评审建议 ①）：新增 `Shared::commit_view()` 持 `ids` 锁完成四步；**删除 `publish` / `advance`** ⇒ 唯一发布路径（结构性对策）；删除形同虚设的告警 | `view.rs` → `Shared::commit_view`；`index.rs` → `commit()`；新增用例 `S8_02_并发commit不丢失视图序号` |
| **2** | 🟡 P3 | `tombstone_stats()` 声称「跨段求和」，实际**无任何求和**（只有阶段断言 + 只读 `main`）⇒ `S8-03` 起 release 下**静默少算** | ✅ **本 PR 修**（评审建议 ①）：真写成逐段求和；抽成 `View::sums()` **纯函数**（否则 `deltas` 恒空时与只读 `main` 无差别 ⇒ **永远测不到**）；顺带把跨段墓碑从存活文档数扣掉 | `view.rs` → `SegmentSums` / `View::sums`；`index.rs`；新增用例 `S8_02_跨段求和真的遍历deltas` |
| **3** | 🟡 P3 | PR3 条目插入时把 PR2 响应条目的**标题行整行替换** ⇒ 该条目**失去标题** + 标题尾巴残成孤行 + 正文悬空 | ✅ 补回完整标题；`## [Unreleased]` 重复（**既有问题**）**顺手合并** | `CHANGELOG.md` |
| **4** | 🔵 P4 | probe 断言 `contains("0.")` **几乎恒真**、零鉴别力 | ✅ 改用 `Explain` 的**结构化字段**（`vector_score` / `vector_rank` 至少一个 `Some`） | `tests/step8_segments.rs` |
| **5a** | 🔵 P4 | 过渡期并发冲突复用 `Error::InvalidInput`（= 参数错）⇒ 调用方无法在「重试」与「报错退出」间决策 | ✅ 新增 **`Error::Busy`**（⚠️ **破坏性**：`Error` 无 `#[non_exhaustive]`） | `error.rs`；`view.rs` → `write_needs_exclusive` |
| **5b** | 🔵 P4 | `add()` 的「`Err` 但倒排已写入」未写文档 | ✅ 写进 `add()` 的 doc（含「重试为什么安全」） | `index.rs` |
| **6a** | 🔵 P4 | `compact()` 末步失败会丢弃整套重建结果（旧模型类型上不可能） | ✅ 写进 `compact()` 的 doc：`S8-02` 期须在**无活动读者**时调用 | `index.rs` |
| **6b** | 🔵 P4 | `searcher()` 方法文档与模块文档的可见性口径拧 | ✅ 方法文档补「口径要连读模块文档」并说清 `S8-02` 期倒排**边写边可见** | `index.rs` |

##### 独立复核：**两套实测**量化 P2（不是照抄结论）

| 手法 | 结果 |
| --- | --- |
| **确定性手工交错**（直接编排三个 `pub(crate)` API） | 终值 `generation = 1`（应为 2）⇒ **回退成立**；告警判据 `1 <= 0` = **false ⇒ 不触发**（形同虚设**证实**） |
| **真实并发**（4 写端 × 150 次 `commit()` + 观察线程） | **70 次回退**，样本 `(65,64)` / `(82,81)` / `(134,127)` |

⚠️ **判据选择本身修正过一次**：最初用「终值 == 总数」判定 ⇒ **跑 3 轮全绿**（最后 `publish` 的
恰好是最后 `advance` 的线程时终值仍正确）⇒ 改用**观察线程**判「过程」（NFR-07 承诺的是
「序号只增不减」，不是「终值正确」）。该经验已写进回归用例的注释。

##### 变异验证（`M7` / `M8`）

| # | 注入 | 实测结果 |
| --- | --- | --- |
| **M7** | `Shared::commit_view` 里把 `drop(ids)` 提前（⇒ `publish` 脱离 `ids` 临界区，即原缺陷形状） | ✅ **命中**：`S8_02_并发commit不丢失视图序号` **FAILED**，观察到 **1591 次回退** |
| **M8** | `View::sums` 丢掉 `deltas`（只遍历 `main`，即原缺陷形状） | ✅ **命中**：`S8_02_跨段求和真的遍历deltas` **FAILED**，`left: 1, right: 3` |

两条变异均**已还原并逐字节核对**（md5 一致后复跑全绿）。

##### ⚠️ 覆盖边界与诚实标注

- 两条新用例都是**回归锁**：修复后其判据**结构性成立**（`publish` 顺序 == `advance` 顺序）
  ⇒ 牙齿来自**修复前的复现**（70 次 / 1591 次），**不是**当前代码里的某个可变点。
- `S8-02` 期这两条判据在**外部 API 层面不可观测**（内容与视图同源）⇒ 只有 `pub(crate)` 单测能抓。
- **本轮的判据失效 1 次（自记）**：核「`## [Unreleased]` 是否本 PR 引入」时误用了**过期的本地
  `main`**（`e3540d0`）⇒ 一度得出「是 PR3 引入」的**错结论**；改用 `origin/main`（`5548998`）后
  确认**评审的判断正确**（`origin/main` 上即 2 处），并用 `git log -S "## [Unreleased]"`
  进一步定位到 **PR #61 的合并提交**引入。

### 4.4 段的 ID 空间（`S8-02` / D-S8-04）


#### 4.4.1 基址的定义

```
base_doc(段)   = 前序所有段的 docs.len() 之和
base_chunk(段) = 前序所有段的 chunks.len() 之和
全局 ID = base + 本地 ID
```

段内 `Index::add` 返回的本地 ID ⇒ `SearchIndex::add` 的 `AddOutcome` **对外填全局 ID**（`AddOutcome` 的公开契约不变）。

#### 4.4.2 为什么「前序长度之和」在 FIFO 合并下恒成立

1. `ForwardStore::tombstone_*` **只置 `None`、不缩短 `Vec`**（§2.4）⇒ 段的 `chunks.len()` **只增不减**（除 `compacted()`）。
2. 合并 = **把 `deltas[0]` 的条目按本地顺序 append 到 `main`** ⇒ 新 `main.chunks.len()` = `old_main.chunks.len() + deltas[0].chunks.len()`，**恰好等于「旧 main + deltas[0]」两段长度之和** ⇒ **`deltas[1]` 的 `base_chunk` 不变** ✅。
3. 归纳可得：**只要合并严格按 FIFO 顺序从一个前缀消费，所有现存段的 `base_*` 保持有效**。

⚠️ **推论（必须写进文档）**：合并**不得**跳段、不得交换顺序、不得重编号；`compact()` 会重编号 ⇒ **必须 `merge_all()` 之后再 `compact()`**（D-S8-12）。

#### 4.4.3 ID 发号（`Shared::ids`）

```rust
struct IdAllocator { next_doc: DocId, next_chunk: ChunkId }
```

- **写端串行**（`SearchIndex` 是 `!Sync`；且 `into_index()` 造出的新写端也遵守同一约定）⇒ 短期持锁、无争用；
- 🔴 **`commit()` 的「seal + publish」必须与发号同处一个临界区**（**2026-09-17 按评审 P3-4 补**）：
  D-S8-06 保留 `into_index()` ⇒ 同一 `Arc<Shared>` 上**可以同时存在两个写端句柄**（各自独占线程、各自持 `builder`）。
  若「算 `base_*`（= 前序所有段长度之和）」与「把新段追加进 `View.deltas`」**不在同一临界区内**，两个写端会算出**相同 base**
  ⇒ **段 ID 空间重叠 ⇒ 直接破坏 I8-7**。⇒ 取形 = **`commit()` 的「flush → 算 base → 构造段 → 追加 `deltas` → 发布 `View`」全程持 `Shared::ids` 锁**
  （该锁同时保护「ID 发号」与「段基址计算 + 段追加」）。⚠️ 这把锁**只被写端取**（**I8-3**：读端永不触碰）⇒ 不违反 I8-1。
  ⚠️ 今天**结构上不可能**出现双写端（`into_index()` 的 `Arc::try_unwrap` 要求 refcount == 1）——**「双写端」是保留 `into_index()` 新引入的可能性**，故 **D-S8-06 的前置条件就是本条**。
- **读端永不触碰**（I8-3）⇒ 不进读路径；
- 归零时机 = **`save` 之后**（单段、`base = 0`）。⚠️ **`load` 之后不归零**：主段已有 N 个槽位，而新段基址 = 「前序所有段长度之和」（§4.4.1）⇒ 从 0 起步会让新段的本地 ID 被折算成**同一个全局 ID**、与主段**直接重叠** ⇒ 破坏 `I8-7`。⇒ 取形 = 初始化时取 `main.index.total_docs()` / `total_chunks()`（槽位数）。

⚠️ **为什么不用 `AtomicU64`**：`DocId` / `ChunkId` 是 **`u32`**（`types.rs`）⇒ 需要 `AtomicU32` × 2 或一把 `Mutex`；而写端本就串行 ⇒ `Mutex` 语义更清楚、也便于把「一次分配一对」做成不可分割的操作。**这不是性能路径。**

### 4.5 BM25 跨段（`S8-03` / D-S8-07）

#### 4.5.1 全局统计量的来源

```
N        = Σ_seg seg.index.num_chunks()          （u32，精确和）
total_len= Σ_seg seg.index.total_len()           （u64，精确和）
df(t)    = Σ_seg seg.index.doc_freq(t)           （u32，精确和；seg 内无该词则 0）
avgdl    = total_len as f32 / N as f32           （⚠️ 用全局量算，**不得**各段各算）
```

`doc_freq(t)` 走**字符串**查段内词典（`InvertedIndex::doc_freq(term)`），**不跨段共享 `TermId`**；段内取 postings 用该段自己的 `term_id(term)`。

⚠️ **`total_len` 必须是「所有段之和」而不是「各段 `avgdl` 的加权平均」** —— 后者会引入两次舍入 ⇒ 破坏逐位一致（R49 的第 ① 条前提）。

#### 4.5.2 遍历顺序（逐位一致的**充分条件**）

```rust
for token in tokens {                     // ⚠️ 外层 = query term（顺序不变）
    let term = token.term;
    let df = Σ_seg doc_freq(term);
    if df == 0 { continue }
    let idf = idf(df, N);
    for seg in view.segments_in_order() { // ⚠️ 内层 = 段（FIFO：main, deltas[0], deltas[1], …）
        let Some(tid) = seg.index.term_id(term) else { continue };
        for posting in seg.index.postings_by_id(tid) {
            /* 谓词判定；dl = seg.index.chunk_len(本地 id)；tf = posting.tf */
            *acc.entry(seg.global_chunk(posting.chunk_id)).or_insert(0.0) += partial;
        }
    }
}
```

⇒ 每个 chunk 在**每个 term** 上**恰累加一次**，`acc[chunk]` 的累加顺序 = **term 顺序**，与单段布局**完全一致** ⇒ f32 结果逐位相同。

#### 4.5.3 三条前提（写进 rustdoc 不变式）

| ID | 前提 | 破坏后的症状 |
| --- | --- | --- |
| **I8-5** | `N` / `total_len` / `df` **必须用全局精确整数和** | idf / avgdl 漂移 ⇒ **只在分段布局下出现的分数变化** |
| **I8-6** | TAAT 累加**必须 term 外层、段内层** | 同一 chunk 的 f32 累加顺序变化 ⇒ **末位 bit 漂移** |
| **I8-7** | 段的 `chunk_id` 空间**不得重叠**（§4.4 的基址不变式） | 不同 chunk 的分数被合并 ⇒ **结果错**（不是漂移） |

### 4.6 向量跨段（`S8-05`）

#### 4.6.1 取形

- **每段一个向量索引**（`Box<dyn VectorIndex>`），段自己的本地 `chunk_id`；
- 向量路**对每段各查一次**（`k = candidate_k`，或按 §4.6.3 的过采样规则），然后**全局归并**：按 `(distance asc, 全局 chunk_id asc)` 取前 `candidate_k`；
- **谓词逐段下推**：给第 `i` 段的是「第 `i` 段的本地谓词」（§4.7）；
- `Metrics.vector_route`：⚠️ **各段可能给出不同的 route**（一段走 ANN、另一段走精确）⇒ **唯一取形 = 各段 route 的「并集分类」**（全 `Ann` ⇒ `Ann`；全 `Exact` ⇒ `Exact`；混合 ⇒ **新值 `VectorRoute::Mixed`**）。⚠️ **2026-09-17 按评审 P3-3 收敛**：本行原并存三个取形（「取最保守者」/「新增独立计数 `vector_route_mixed`」/「并集分类」）⇒ **已删掉前两个草稿**，只留并集分类一条。`Mixed` 的破坏性判定见 **Q5**（评审代核 + 本文独立复核，见附录 C）。

#### 4.6.2 ⚠️ ANN 路径**不承诺**与单段逐位一致

`hnsw_rs` 的图拓扑依赖**插入顺序与随机层高**（`StdRng::from_os_rng()` 无 seed）。`[单段 12K 点建图]` 与 `[主段 8K + 增量段 4K 各自建图]` 是**两张不同的图** ⇒ 近似检索的候选集不同 ⇒ 结果**可以不同**（这是 ANN 的定义域，不是缺陷）。

⇒ **本文的承诺边界**（必须写进用户文档与 NFR-06 的注记）：

| 后端 / 模式 | 分段 vs 单段 逐位一致？ |
| --- | --- |
| `bm25` | ✅ **承诺**（§4.5.2 的构造性证明） |
| `vector` + **Brute** 后端 | ✅ **承诺**（每段精确 + 全局归并 = 全局精确；`(距离, chunk_id)` 全序） |
| `vector` / `hybrid` + **Hnsw** 后端 | ❌ **不承诺**（图不同）。⚠️ **但同一 view 内**的确定性（NFR-06 的原文口径）**仍然成立** |

#### 4.6.3 过采样与 `prefers_exact`

- 路径 A（热路径）的 `knbn` 过采样公式里的 `alive_ratio = allowed / len` —— 分段后 `allowed` 与 `len` 都是**该段的** ⇒ **逐段算**（这是正确的：过采样是为该段的图服务的）；
- 路径 C（精确）的触发由 `prefers_exact(filter)` 决定 ⇒ **逐段判定**；
- ⚠️ `BRUTE_FALLBACK_MAX_ALLOWED = 8192` 的阈值语义 = 「**该段的 `allowed_count`**」⇒ 段越小越容易走精确路径。**这是分段的一个副作用**（小段更容易命中低选择度兜底）⇒ **默认不改阈值**，但**必须改上报口径**：`Metrics.vector_route` / `vector_shortfall` 从「一次检索一个值」变成**逐段判定 + 并集上报**（§4.13.3）。⚠️ **2026-09-17 按评审 P4-7② 更正**：原写「登记进 R49 的观测项」—— R49 的登记内容是跨段 BM25 的**三条前提**、R52 是 `bm25_f` 热路径，**两者都不含本条**；本条的归属是 **Q5（`VectorRoute` 混合取形）+ §4.13.3**。

### 4.7 过滤谓词跨段（`S8-04`）

#### 4.7.1 类型

```rust
// crates/core/src/search/view.rs
pub(crate) struct ViewFilter<'a> {
    /// (段, 该段的 DocBits + 段内 &Index)；顺序与 `View::segments_in_order()` 一致
    per_seg: Vec<SegmentFilter<'a>>,
    /// 针对既往段的全局 doc 墓碑（命中即挡）
    tombstones: &'a Tombstones,
    allowed: usize,          // Σ 各段 allowed（**精确**，硬契约不变）
    kind: FilterKind,
}

impl CandidateFilter for ViewFilter<'_> {
    fn contains(&self, global_chunk_id: ChunkId) -> bool {
        let Some((i, local)) = self.locate(global_chunk_id) else { return false };
        if self.tombstones.blocks_doc(self.per_seg[i].global_doc_of(local)) { return false }
        self.per_seg[i].chunk_allowed(local)
    }
    fn allowed_count(&self) -> usize { self.allowed }
    fn kind(&self) -> FilterKind { self.kind }
}
```

⚠️ **`allowed_count()` 的精确性契约不放松**（`predicate.rs` 的硬契约）：`allowed = Σ_seg allowed_seg - 墓碑挡掉的 chunk 数`。⚠️ 后半项**必须精确计算**（墓碑命中的 doc 在该段的存活 chunk 数），**不得估算**，否则 `prefers_exact` 静默失效（那正是这条契约存在的理由）。

#### 4.7.2 `locate(global) -> (seg_index, local)` 的实现

段数极少（主段 + 少量 delta）⇒ **线性扫描 `base` 区间**即可（每段一次 `u32` 比较）。⚠️ **不要**为此建 `HashMap`（段数 ≤ 8 时更慢，且增加一致性维护面）。

#### 4.7.3 ⚠️ 热路径回归（R52）

| 场景 | 今天 | 新模型（**未合并窗口**） | 新模型（**`merge_all()` 之后**） |
| --- | --- | --- | --- |
| 无用户过滤、**无墓碑** | `bm25_f = None`（零谓词） | ✅ **保持 `None`**（显式短路：`view.deltas.is_empty() \|\| view.tombstones.is_empty()`） | ✅ 同左 |
| 无用户过滤、**有墓碑** | 不可能（写端独占 ⇒ `remove` 直接摘 postings） | ❌ `bm25_f = Some(ViewAlive)`，每 posting 一次 `dyn contains()` | ✅ **回到 `None`**（墓碑已**物理化**并从 `View.tombstones` 移除 ⇒ §4.9.2） |

⚠️ **「合并后」这一列是 2026-09-17 按评审 P3-1 补的**，它依赖一个本轮才写明的取形：**跨段墓碑在合并时物理化**（§4.9.2）⇒ R52 的回归**只覆盖「未合并窗口」**、且**可逆**。若取「墓碑永久留存」的另一种取形，R52 将**不可逆**、且 §4.5 的「逐位一致」在带删除场景**不成立**（本轮明确不选该取形）。

⇒ 设计取形（⚠️ **2026-09-19 按 PR4 评审 P3-4.5 与实现同步**）：**判据只看「有没有跨段墓碑」** —— 无墓碑（**不论有没有 delta**）⇒ **零谓词**（`bm25_f = None`）；有跨段墓碑 ⇒ 传谓词。
🔑 实现比原文本更省：原写「有 delta 但无墓碑时逐段借 `AliveOnly`」，但**段内删除总是物理摘除 postings**（§4.8.3 分支 ①）⇒ 活 postings **必然是干净的**，多传一层谓词只是白付「每 posting 一次 `dyn contains()`」（已实测：`query/searcher.rs` 的 `bm25_needs_filter` 就是这条判据的唯一落点）。
⇒ **回归只发生在「确实有跨段墓碑」时**，且由 S8-T12 的 bench 对照量化。

### 4.8 写侧：delta 缓冲与 `commit()` 的可见性（`S8-02` / `S8-03` / D-S8-03 / D-S8-05）

#### 4.8.1 写端结构

```rust
pub struct SearchIndex {
    shared: Arc<Shared>,
    builder: SegmentBuilder,        // 私有、可变
    embed_elapsed: Duration,        // 既有计数口径不变
    embed_count: usize,
    graph: GraphOpts,
    graph_status: GraphStatus,
    graph_dump_elapsed: Option<Duration>,
    backend: VectorBackend,
}

struct SegmentBuilder {
    index: Index,                   // 段内本地 ID，从 0 起
    pending: Vec<PendingChunk>,     // 既有
    raw_vectors: Vec<(ChunkId, Vec<f32>)>,
    vector: Option<Box<dyn VectorIndex>>,
    tombstones: Tombstones,         // 针对既往段的全局 doc 墓碑
}
```

⚠️ **`SearchIndex` 保持 `!Sync`**（D-S8-05）——`add(&mut self)` 的签名一字不改，**这是「零所有权破坏」的关键**。并发来自：「写端线程独占 `SearchIndex`」+「读端持 **owned `Searcher`**（不借用写端）」。

#### 4.8.2 `searcher(&self) -> Searcher`（D-S8-06）

| 项 | 旧 `into_searcher(mut self)` | 新 `searcher(&self)` |
| --- | --- | --- |
| 接收者 | `self`（消耗） | `&self`（不消耗） |
| 隐含 `flush()` | **是** | **否**（⚠️ 调用方须自己 `commit()`） |
| 返回 | `Searcher` | `Searcher`（持 `Arc<Shared>` 的克隆） |
| 与写端共存 | ❌ | ✅ |

**兼容策略**：`into_searcher()` 保留为 `#[deprecated]` 薄封装 = `{ self.commit()?; self.searcher() }` ⇒ **既有 41 处调用点零改动，行为逐位不变**（⚠️ **2026-09-17 按评审 P4-1 更正**：原写「100+ 处」，高估 ≈2.4~2.6×；精确口径 = **生产调用 4 处（`cli/src/main.rs` 3 + `examples/search_basic.rs` 1；`core/src` 生产调用 = 0）+ 测试 37 处**，另有 2 处**定义**与 11 处**注释** —— 复算方法与「与评审 38 处的口径差异」见 **§10.3**）。⚠️ 新 `searcher()` 的「不隐含 flush」是**有意的语义收紧**：它让「可见性 = `commit()` 后」在 API 层面显式化（对齐 NFR-11），而不是靠一个隐式副作用。

#### 4.8.3 `add` / `remove` / `commit` 的语义

| 方法 | 行为 |
| --- | --- |
| `add(&mut self, doc)` | ① 跨段查重（见下）；② 分块 → **delta 的 `Index::add`**（本地 ID）；③ 有向量侧则进 `pending`；④ `pending.len() >= batch_size` 时**自动 `flush()`**（既有行为不变）。⚠️ **此时新 doc 对读端不可见**（它还在 delta 里、未发布） |
| `flush(&mut self)` | 既有语义不变（整批 embed + liveness 过滤 + `add_batch` 进 delta 的向量索引） |
| `commit(&mut self)` | ① `flush()`（既有）；② **封段**：把 `builder` 冻结成 `Segment`（算 `base_*`、`generation`）；③ 若段为**空**（无 chunk、无墓碑）⇒ 不发布（避免空段堆积）；④ `publish(...)`；⑤ 起一个新的空 `builder`。⚠️ **`commit()` 是唯一的可见性边界** |
| `remove(&mut self, doc_id)` | ⚠️ **全局 doc_id**。分三种：① doc 在**当前 delta** ⇒ 直接本地 `Index::remove`（**物理摘 postings**，与今天一致）；② doc 在**既往段** ⇒ 记进 `builder.tombstones`（**不动既往段**）；③ 不存在 ⇒ 既有行为（报错或 no-op，按现有实现） |

**跨段 `content_hash` 查重**（FR-15 幂等 upsert 的必要条件）：

```
fn doc_id_by_hash_global(view: &View, builder: &SegmentBuilder, hash: u64) -> Option<DocId> {
    // 顺序：当前 delta（未发布、最新）→ deltas 逆序 → main
    // ⚠️ 命中后还要判「该 doc 是否已被墓碑挡住」——被挡住的话**不算命中**（可重新 upsert）
    // 🔴 且**两处墓碑都要查**：`View.tombstones`（已发布）**+** `builder.tombstones`（未发布）
    //    —— 只查前者时「`remove(X)` 后不 `commit()` 就重加同 hash」会命中刚被删的 X
    //    ⇒ 重加/替换**静默丢失**（2026-09-19 第 2 轮评审 P1-1 落地，见 §4.9.6）
}
```

⚠️ **这里有一个必须显式处理的语义**：今天 `remove` 会 `content_hashes.remove(&hash)`（`Index::remove`），所以「删除后可重新 upsert」成立（既有测试 `删除后可重新upsert`）。跨段后，主段的 hash 表**删不掉** ⇒ 必须靠**墓碑挡一下**来实现同样的语义 ⇒ 「命中但被墓碑挡住 ⇒ 视为未命中」这一条**必须有定向用例**（S8-T6）。
🔴 **2026-09-19 第 2 轮评审 P1-1 补充**：墓碑有**两个落点**（`builder` 未发布 / `View` 已发布），且 `remove` 的**分支②**（目标在既往段）落在**前者**。「查重只查已发布墓碑」会让「`remove` 后不 `commit()` 直接重加」这条路径**静默丢失替换内容** —— 而 FR-15 的替换文档流程**总是**走分支②。修法与回归见 **§4.9.6**。

### 4.9 后台合并（`S8-07` / D-S8-08 / D-S8-09）

#### 4.9.1 原语

```rust
impl SearchIndex {
    /// 合并**一个**未合并段（FIFO 队首）进主段。返回本次合并报告。
    pub fn merge_pending(&mut self) -> Result<Option<MergeReport>>;
    /// 合并**全部**未合并段（`save()` / `compact()` 的前置步骤）。
    pub fn merge_all(&mut self) -> Result<MergeReport>;
}

pub struct MergeReport {
    pub segments_merged: usize,
    pub chunks_merged: usize,
    /// 本次合并中被**物理化**（真删）的跨段墓碑条数（取形见 §4.9.2）。
    /// 合并完成后这些条目**从 `View.tombstones` 移除**（⇒ 热路径可回到 `None`，见 §4.7.3）。
    pub tombstones_applied: usize,
    pub vector_strategy: VectorMergeStrategy,   // Incremental | Rebuild
    pub vector_merge_ms: u128,
    pub total_ms: u128,
    pub generation: u64,
}
```

⚠️ **库内不 `spawn` 线程**（D-S8-08）：合并是**一次调用完成一件事**的原语。后台线程由 **CLI / 宿主**负责（`helix serve` 或 `build --watch` 形态由 CLI 决定，不属本 Step）。

**理由**：① 库不该替宿主决定线程与生命周期；② 库内起线程会让「无 `unsafe`、可 `Send`/`Sync` 推导、测试可控」三条全部变复杂；③ 本仓已有先例（`moka` 缓存是库，但**线程归属**一直是宿主的）。

#### 4.9.2 文本侧的合并：`Index::merge_from`

```rust
// crates/core/src/index/mod.rs（新增，pub(crate)）
impl Index {
    /// 把 `other` 的条目**按本地顺序追加**到 `self`（**不重编号**，D-S8-04）。
    /// 前提：`other` 的本地 ID 在 `self` 之后连续（由 §4.4.2 的基址不变式保证）。
    pub(crate) fn merge_from(&mut self, other: Index) -> Result<usize>;
}
```

要点：

| 部件 | 处理 |
| --- | --- |
| `forward.docs` / `chunks` / `doc_chunk_count` | **append**（`other` 的本地 ID 已经等于「`self.len() + local」⇒ **位移为 0**，无需改写） |
| `forward.alive` 位图 | 按偏移 `set` 每位 |
| `inverted` | 遍历 `other.export()` 的 `(term_dict, postings)`，对每个 posting 用**位移后的 chunk_id** 调 `add` |
| `stats` | `total_len += other.total_len`、`num_chunks += other.num_chunks` |
| `chunk_lens` | `extend` |
| `content_hashes` | `extend`；⚠️ **碰撞必须处理**（理论上不应发生——写入时已跨段查重；但仍要 `debug_assert` + 确定性策略） |
| `field_index` | ⚠️ **不能简单 extend** ⇒ 用 **`FieldIndex::rebuild(&self.forward.docs)`**（既有函数，全量重建）。12K 文档下重建成本可接受；**并且**重建后「基数保护」的降级判定可能与增量维护的结论不同 ⇒ **必须有一条「合并后字段索引 == 全量重建」的等价用例**（S8-T13） |
| **跨段墓碑** 🔴 | **合并时物理化**（**2026-09-17 按评审 P3-1 补，本轮最实质的一条**）：段携带的墓碑**全部**指向**严格更早**的段（§4.8.3 的分支 ① 已把「同一 delta 内的删除」就地物理删除，**不会变成墓碑**）⇒ 在 append 完成后，对该 doc 走**既有的 `Index::remove` 语义**（物理摘 postings + `content_hashes.remove` + `stats.total_len / num_chunks` 回滚 + `field_index` 随之重建），然后**从 `View.tombstones` 移除**该条 ⇒ `MergeReport.tombstones_applied` 记的就是这个条数 |
| ⚠️ 物理化的边界 | **不改 `forward.docs` / `forward.chunks` 的 `Vec` 长度**（`Vec<Option<..>>` 只置 `None`）⇒ **基址不变式（§4.4.2）不受影响**；FIFO 合并保证「墓碑的目标必已在主段」（若目标还在更晚的段里、则该段还没轮到合并） |

⚠️ **为什么取「物理化」而不取「墓碑永久留存」**（P3-1 的两条路）：后者会让 ① `View.tombstones` 永久非空 ⇒ `bm25_f` **永远** `Some`（R52 **不可逆**）；② 全局 `N` / `total_len` / `df(t)` 把已删 doc **仍计入** ⇒ 与「单段建库 + `remove`」的统计量**发散** ⇒ **§4.5 的逐位一致在带删除场景不成立**（S8-T4 原来只测无删除场景，测不出这条）；③ `save` 落盘快照也**不再等价于今天 `remove` 后的形态** ⇒ 与 **D-S8-01**「落盘形态一个字节都不变」的表述冲突。⇒ **取物理化**，并在 S8-T4 加「带跨段删除」变体把这条钉死。

⚠️ **顺序性**：`inverted.add` 会把 posting `push` 到该 term 的链尾 ⇒ 合并后链内顺序 = 「主段原序 + 增量段序」= **全局 `chunk_id` 升序**（因为基址连续）⇒ **与单段建库的 postings 顺序一致** ✅。这条是 S8-T4「逐位一致」的结构性依据之一，**必须写成注释**（否则后人一次「顺便排序」就会破坏 I8-6 之外的又一个前提）。

#### 4.9.3 向量侧的合并（D-S8-09，**需拍板**）

`hnsw_rs` 无图合并 API（§3.2）⇒ 两个候选：

| 方案 | 做法 | 成本 | 风险 |
| --- | --- | --- | --- |
| **A（建议默认）** **dump → load → 增量 `insert`** | 把主段的图 `file_dump` 到临时目录 → `load_graph` 载回 → 把 delta 的向量 `insert` 进去 → 得到新图 | `dump`（12K 实测 **43.5ms**）+ `load`（**23.3ms**）+ `insert(delta)`（逐点，12K 全量约 **38.7s** ⇒ 小 delta 时与 delta 规模成正比） | R23 的 `Box::leak`（无回收）；R22 的体积 1.6×；**临时文件 I/O**；⚠️ 需要主段图**可 dump**（纯内存建库、从未 `save` 过 ⇒ **没有主段图文件**） |
| **B（回落）** **从 `raw_vectors` 全量重建** | 把主段 + delta 的 `raw_vectors` 合起来重建一张图 | **≈38.7s / 12K**（与 delta 大小无关） | 已有代码路径（`rebuild_vector_index`）；无 `Box::leak` 新增；无临时 I/O |

**建议默认 A + 回落 B**（`MergeReport.vector_strategy` 如实记录走了哪条）。

**决策门（spike S8-S1，可证伪）**：

1. **可行性**：A 路径能跑通（dump→load→insert 后可检索、`GraphStatus::Loaded`、`nb_point` 吻合）；
2. **收益**：A 在「delta ≈ 主段的 1/10」时的耗时 **≤ B 的 1/5**；
3. **正确性**：A 与 B 产出的图在**同一 query 集**上 top-10 **重合率 ≥ 0.99**（⚠️ **不要求逐位**——图不同、ANN 结果本来就允许不同，见 §4.6.2）；
4. 任一不满足 ⇒ **回落 B**，并把「合并 = 全量重建」的代价写进 NFR-14 与用户文档。
5. 🔴 **累积（2026-09-17 按评审 P3-5 新增）**：同一进程**连续合并 10 次**，峰值 RSS 增量 **< 1 MiB**。
   ⚠️ 这个阈值的意义**不是「不许有泄漏」**（`Box::leak` 是 R23 的既定事实），而是**把两种量级区分开**：
   「只泄漏 `HnswIo`（≈ **200B + 路径串 / 次**）」 vs 「误泄漏整张图（≈ 50MB / 次）」——
   前者 10 次 ≈ 2 KB（淹没在 RSS 噪声里、必然通过），后者 10 次 ≈ 500 MB（必然失败）。见 §9.1 R51。

⚠️ **非对称（评审 P3-5 的另一半）**：**方案 A 每次合并都 `load_graph` ⇒ 每次都多一个 leaked `HnswIo`；方案 B 是内存重建、不 `load` ⇒ 不新增泄漏**。
⚠️ 但**「B 更省内存」只在「图本身」这一项上成立**：B 的新图内存构建、旧图随 `Segment` 释放 ⇒ 可回收；
而 A 的 leaked 对象**也不可回收**（只是量级极小）。**首篇 R51 已按此写明非对称**（架构 §14.6）。

⚠️ 方案 A 有一个**必须先解决的边界**：**主段图可能不存在**（未落盘 / 被降级重建 / `--no-graph-persist`）⇒ 取形 = **先 `dump` 主段图到临时目录再 `load`**（对内存图也能做，`VectorGraphPersist::dump_graph` 是 `&self`）✅。但这也意味着**每次合并都产生一次全量 dump I/O**（⏳ 50MB 级临时文件）。

#### 4.9.4 合并的触发与顺序

| 项 | 取形 |
| --- | --- |
| 触发 | **由宿主调用**：`merge_pending()`（一次一段）。CLI 可在后台线程循环调用；库不决定策略 |
| 顺序 | **严格 FIFO**（§4.4.2 的基址不变式要求） |
| 并发 | 同一 `SearchIndex` 上**串行**（`&mut self` 天然保证）；⚠️ **不允许两个写端同时合并** |
| 与读端 | **完全无关**（读端继续吃旧 `Arc<View>`）⇒ FR-17 的「合并期间不得阻塞读请求」在**结构层面**成立 |
| 段数上限 | 不设硬上限；⚠️ 读路径成本随**段数**线性增长 ⇒ 建议宿主在 `deltas.len() > K`（默认 **4**，可由 CLI 传参）时优先合并 ⇒ 登记 Q6 |

#### 4.9.5 与 `compact()` / `save()` 的关系

| 操作 | 前置 | 说明 |
| --- | --- | --- |
| `save(&mut self, path)` | **`merge_all()`**（D-S8-01） | 落盘仍是**单段 `Index`** ⇒ `FORMAT_VERSION` 保持 2、manifest 不变、ID 不变 |
| `compact(&mut self)` / `compact_and_save` | **`merge_all()`**（D-S8-12） | 既有语义不变（**重编号**、回收墓碑）。⚠️ 文档必须写明：**`compact` 之后 `chunk_id` 会变**（既有行为，但新模型下读者可能持有旧 view ⇒ 必须写清「旧 view 的 ID 与 compact 后的 ID 不可混用」） |
| `tombstone_stats()` | 无 | 改为**跨段求和**；⚠️ **合并后归零**（墓碑已物理化、条目已从 `View.tombstones` 移除 —— §4.9.2） |
| 跨段墓碑 | 随段合并 | **物理化**（§4.9.2）：走既有 `Index::remove` 语义 + 从 `View.tombstones` 移除 ⇒ `MergeReport.tombstones_applied` 计数；**这是 `save` 落盘形态与今天 `remove` 后形态等价的前提** |

#### 4.9.6 第 2 轮评审响应（2026-09-19，**PR #63**）

> 落点 = `pulls/63/reviews` **1 条**（id `5255969412`，SHA `f69fb8e`）+ **行内 4 条**；
> `issues/63/comments` 无新增。**2×P1 + 1×P3 + 1×P4**（正文另逐条确认了 §6 十二项，**12 项全 ✅**）。

| # | 级别 | 落点 | 结论 | 落地 |
| --- | --- | --- | --- | --- |
| 1 | 🔴 P1 | `doc_id_by_hash_global` 不认 builder 上的**未发布墓碑** | ✅ **属实，已复现** | §4.8.3 伪代码 + `doc_id_by_hash_global` 判据改为「两处都查」；回归 = `S8_T6_remove未commit即重加不得被去重吞掉`；变异 `M-P1-1` |
| 2 | 🔴 P1 | `absorb_window` 的**长度前缀**假设被**并发 fold** 破坏 | ✅ **属实，已复现** | 判据从「长度」升级为**前缀身份**（`Arc::ptr_eq`）；`Options` → 拒绝发布（`Err(Busy)`）；回归 = `S8_03_窗口吸收必须校验前缀身份而非只看长度`；变异 `M-P1-2a` |
| 3 | 🟡 P3 | 两条 `Busy` 消息的恢复指引漏了「跨段墓碑也一并丢弃」 | ✅ 采纳 | 两条消息改为「重新执行未成功的 `add` **与 `remove`**」 |
| 4 | 🔵 P4 | 墓碑-only 空段的次生成本（`metrics.segments` 膨胀 + fold 期白付 O(N)） | ✅ 登记 | `SegmentBuilder::is_empty()` 文档 + `commit()` 注释；**处置归 `S8-06`** |

**P1-2 的机制（为什么世代对账拦不住）**：`fold` **不改 `epoch`**（`epoch` 只在 `publish_reset` 即 `compact` 重编号时 `+1`）⇒ 两条 `fold` 的交错**完全落在世代对账的盲区**里。原判据只约束**长度**（`cur_deltas.len() >= snapshot_deltas`），而该交错恰好让长度**相等**：

```text
① 视图 main M（已用 U）+ deltas [D1]，epoch 0
② A：fold 取快照（[D1]）→ O(N) 克隆 + O(N·logN) 向量重建（**秒级窗口**）
③ B：fold 把 D1 吸进 main 并发布（deltas = []，epoch 仍 0）
④ B：add(D2) → commit()（deltas = [D2]，ids.next = U + d2）
⑤ A：发布 —— 长度 1 == 1 ⇒ 旧判据放行；`skip(1)` 跳掉的却是 **D2**
   ⇒ D2 从视图静默消失，而 ID 空间已记账（**永久孤儿区间**）；A 随后的 save 把该状态落盘
```

**修法**：段一旦发布就**不可变**（`I8-2`）⇒ `Arc::ptr_eq` 是合法的**身份**判据 ⇒ 快照把 `Vec<Arc<Segment>>`（clone = O(1)，只加引用计数）带进发布闭包，闭包内校验「`cur_deltas` 的前 N 项与快照**逐个 `Arc::ptr_eq`**」；不符 ⇒ `Err(Busy)`。为此 **`commit_view` 的 `build_next` 改为可返回 `Result`**（检查必须在**锁内**做，否则等于没做），并明确「`Err` ⇒ **不做任何记账**」（序号不消耗、`base` 不推进、视图不动）—— 该承诺有判据：`S8_03_发布失败时不消耗任何记账`（变异 `M-P1-2b`）。

⚠️ **`absorb_window` 的 `debug_assert` 已删除**：它被定位为「防**旁路**守卫」，但**合法路径**（并发 fold）就能踩到 ⇒ 必须 dev/release **双端显式拒绝**，不能只在 debug 下 panic。同一条判据同时覆盖「并发 `compact` 造成的长度回退」。

⚠️ **三条 `Busy` 的产生点现在语义有别**（`error.rs` 已同步成表）：
- **基址对账**（`commit()`）：丢的是**本写端 builder 里未提交的内容 + 跨段墓碑** ⇒ 恢复 = **重做 `add` 与 `remove`**；
- **世代对账**（`commit()` **或** `fold_deltas()`）：由 `commit_view` 的 `expected_epoch` 对账产生。
  ⚠️ **它被两条路径共用**，而「丢了什么 / 怎么恢复」**按调用方而不同** ⇒ 由 **`PublishCaller`**
  （`search/view.rs`）声明身份、消息在**同一个地方**分家：`commit()` ⇒ 与「基址对账」同；
  `fold_deltas()` ⇒ **只有本次合并的计算**（内容仍在视图里），恢复 = **重试本次 `save()` / `compact()`**；
- **快照前缀身份**（`fold_deltas()`）：`fold` 是**维护性**操作，被判拒时增量段**仍在视图里、内容没丢**，丢的只是本次合并的**计算** ⇒ 恢复 = **重试本次 `save()` / `compact()`**。
  🔴 第 2 轮评审 P3-1 正是要求把这条**差别**写进消息（NFR-07：恢复路径也要如实）。
  🔴 **第 3 轮评审 P4** 补上了当时遗漏的一半：P3-1 只做了「前缀身份那条有自己准确的 fold 版文案」，
  而**世代对账那条仍在复用 `commit()` 的文案** ⇒ 对 `fold` 侧三条声明全不成立（见 `error.rs` 的
  产生点表）。现统一由 `PublishCaller` 分家；判据 = `S8_03_epoch拒绝的恢复指引按调用方分家`
  （**变异**：把 `Self::Fold` 臂改成复用 `Self::Commit` 的文案 ⇒ 本条**唯一红**）。
  🔑 落点有 **3 处**（评审点名 2 处，第 3 处按「一类缺陷全扫」找到）：`search/view.rs` 的消息 /
  `error.rs` 的产生点表与段落 / **本节**（原文把「基址 + 世代」都归给 `commit()`，
  完全没提 epoch 检查也会被 `fold_deltas` 走到）。

**附加（本轮守门链抓到、非评审项）**：`compaction_cli_semantics::CLI4` 的 `dst_total < src_tomb`
是 **`D-S8-01` 系统性影响的第 4 条**（上一轮记的「3 条均已收口」漏了它）—— 该断言由 PR #31 引入、
`main` 上稳健，`D-S8-01` 之后源与结果**都是 4 点图** ⇒ 只剩 `OsRng` 抖动（实测 **1/8 假红**）；
处置同 `CLI3`（换**8 点基线**参照系 + 变异自证）。详见 `CHANGELOG` 同条目。

### 4.10 持久化：H4 的结论（D-S8-01）——**不开 ADR-B**

| 项 | 结论 |
| --- | --- |
| `SnapshotSections` | **一字不改**（仍是单段） |
| `FORMAT_VERSION` | **保持 2**（ADR-A 的不变式 ① 续存） |
| `GraphManifest` | **一字不改**（`snapshot_crc` / `snapshot_len` / `nb_point` 仍绑单个快照） |
| 跨段墓碑 | **合并时已物理化**（§4.9.2）⇒ 落盘的 `forward` / `inverted` / `content_hashes` 与「**今天 `remove` 之后**」的形态**等价**（postings 已摘、hash 已摘、`alive` 已清）| 
| 图 sidecar | **单份**（合并后的主段图）；`save` 的 dump 计时口径不变 |
| `load` | 产出**单段 view**（`deltas = []`、`base_* = 0`）；⚠️ **`ids` 不归零** —— `next_doc` / `next_chunk` 取**主段已用的槽位数**（见下方更正） |
| ADR-B | **不开**；但**登记为 Q2 的条件项**：若将来要求「不合并就落盘」（例如 delta 大到 merge 成本不可接受），H4 的原始担忧（manifest 挂多段图）才会成立 |
| ADR-A | **沿用**，并显式补一条扩展边界：**「`save` 必须先 `merge_all()`」**（这是 ADR-A 之外的新增约束，但**不改变** ADR-A 的任何既有条款） |

> ⚠️ **更正（`S8-03` 实施期实测，2026-09-19）**：本表原写「`load` … **`ids` 归零**」——**该措辞是错的**。
> 主段已有 N 个槽位，而新段基址按 §4.4.1 是「前序所有段长度之和」⇒ 从 0 起步会让新段的本地 ID 被
> 折算成**同一个全局 ID**、与主段**直接重叠**（不同 chunk 的分数被合并 = 结果错），**破坏 `I8-7`**。
> 实测：`step6_incremental_build` 的 `S6_T8_追加分配的id严格大于历史最大id` 与
> `S6_T10_追加后往返检索结果一致` 两条用例**当场变红**，改为「从主段槽位数起步」后双双转绿。
> ⇒ 已按上表更正为「**`ids` 不归零**」。

⚠️ **本文对 H4 的回答是「用前置合并把问题消解掉」，不是「把问题解决了」** —— 这一点必须在 PR 正文里说清，避免评审误读为「多段持久化已经做了」。

### 4.11 T7-26：查询侧会话池（`S8-08` / D-S8-11）

#### 4.11.1 形态

```rust
pub struct LocalEmbedder {
    pool: Vec<Mutex<TextEmbedding>>,   // 长度 = Config::embed_sessions（默认 1）
    next: AtomicUsize,                 // 轮转取槽（避免总撞同一把锁）
    dim: usize,
}
```

- `embed_query` / `embed_documents`：**轮转取一个槽**，只锁那一个 ⇒ 不同线程可并行；
- `Config::embed_sessions: usize`，**默认 1** ⇒ **零行为变化**（仍是今天的单 `Mutex<TextEmbedding>` 语义，只是包了一层 `Vec`）；
- ⚠️ **`dim` / `id()` / `is_normalized()` 不变**；`ConfigFingerprint` **不扩**（会话数不影响索引内容）；
- ⚠️ **每个实例要重新加载模型**（`TextEmbedding::try_new`）⇒ 构造耗时为 `N ×`，且**每个实例各自持有一份 `Session` + `Tokenizer`**。`default_cache_dir()` 复用 ⇒ **不重复下载**。

#### 4.11.2 ⚠️ Q3 的改述与必答性

计划里的 Q3 写作「不同 `sessions` / `intra_threads` 是否改变向量数值」。按 §2.7，`sessions` 不是 fastembed 参数 ⇒ **改述为**：

> **Q3′：不同 `intra_threads`（以及不同实例数）是否改变 `embed_query` 的向量数值？**

**若改变** ⇒ 「每 worker 独立 session」会让同一 query 在不同并发度下得到**不同的向量** ⇒ 破坏 NFR-10 ① 的「逐位一致」前置条件 ⇒ **judgement 必须是「不投」**（或退化为「池内所有实例统一 `intra_threads`，且只用于 `embed_documents`」）。

⚠️ **Q3′ 必须由 spike 实测回答**，不允许按「ONNX 是确定性的」推断（R47 的教训：不同 batch 组成下的 logit 已实测**相同**，但那是精排；embed 侧没测过）。

#### 4.11.3 决策门（spike S8-S2，可证伪）

| # | 判据 | 阈值 | 依据 |
| --- | --- | --- | --- |
| ① | **数值一致性（Q3′ 的前置）** | 同 query、单线程 vs 池化，**向量逐位相同**（`to_bits()` 相等） | 不满足 ⇒ **直接判「不投」**，后两条不必测 |
| ② | **吞吐增益** | `QPS(4)/QPS(1)` 的 vector 路 **≥ 2.5×**（NFR-10 的既有判据） | 沿用 Step 6 的判据，不新造 |
| ③ | **峰值 RSS 增量** | **≤ 20%**（相对「`embed_sessions = 1`」臂，**查询侧口径：batch 1 × 短 query**，逐档位独立进程 + 外置 `/usr/bin/time -l`） | 借用 D-S6-01 的门槛值；⚠️ **口径必须重测**（§2.8） |

⚠️ **三条全过才投**（合取，同 D-S6-01 的先例）。**任一条不过就如实记「未达标」**，`NFR-10` ③ 保持「打开」、R43 保持「打开」——**这是完全可合并的结论**（Step 6 的 E2 已经是一次先例）。

⚠️ **不要引用 E2 的 +96.4% / +289.2% 作判据**（口径不同，§2.8）；但**可以在报告里并列展示**，说明「同一手段在建库口径与查询口径下的代价不同」。

### 4.12 T7-18：旧 API `#[deprecated]` 的评估（`S8-02` 内落地）

⚠️ issue **#5** 的原始定位是「**评估**」⇒ **输出允许是「不废弃」**。本文先做全量评估，再给结论。

**评估口径**：`pub` 面里凡是「因为旧所有权模型而存在」的 API，看它在新模型下是否**有替代 / 语义是否仍成立**。

| API | 新模型下 | 处置 |
| --- | --- | --- |
| `SearchIndex::into_searcher(mut self)` | 有替代（`searcher(&self)`）；语义仍成立但**隐含 flush** 与新语义不符 | ✅ **`#[deprecated(note = "改用 searcher()；并显式 commit()")]`**，实现 = `commit()?; self.searcher()` |
| `Searcher::into_index()` | **动机消失**（不再是「换回」的唯一通道）；但保留可让既有调用点零改动 | ✅ **`#[deprecated]`**，实现 = 用同一 `Arc<Shared>` 造新写端（D-S8-06） |
| `SearchIndex::add(&mut self)` / `commit` / `flush` / `save` / `remove` / `compact*` | 语义不变（`compact` 多了 `merge_all()` 前置） | ❌ **不废弃** |
| `Searcher::search` / `search_with` / `SearchRequest` | 不变 | ❌ **不废弃** |
| `Index` 的 `pub` 面（`add` / `remove` / `export` / `import` / `compacted` …） | 属「内核直用」逃生舱，**没有替代** | ❌ **不废弃**（⚠️ 但要写清「绕过门面使用 ≥ 遵守 `view` 的不变式」不在承诺内） |
| `VectorIndex` / `Retriever` / `FusionStrategy` / `Reranker` / `Embedder` 五 trait | 不变 | ❌ **不废弃** |
| `HnswRsIndex::with_parallel_build` / `parallel_inserts` 等 | 不变 | ❌ **不废弃** |

⇒ **结论 = 「只废弃 2 个所有权模型遗留 API，其余一律保留」**。⚠️ 该结论**不改变任何 trait 契约**，`CLAUDE`/用户文档里的「六 trait 替换矩阵」**不受影响**。

### 4.13 NFR-14 / NFR-11 的口径（`S8-09` 定稿）

#### 4.13.1 NFR-14（读延迟与可见性）

| 部分 | 口径（**本文定稿**） | 判据 |
| --- | --- | --- |
| **① 可见性** | **`commit()` 后立即可查**；`add()` 之后未 `commit()` ⇒ **不可见** | S8-T7 双向断言；见 §0 附加行的口径冲突收回 |
| **② 读延迟** | **②a 相对判据（主判据）**：同机、同 query 集、**同一已提交内容**下，「**合并进行中**」的 `took` P99 ≤ 「**静止期**」的 `took` P99 × **1.2（拟）**；**②b 绝对判据（护栏）**：合并期 `took` P99 **仍在 NFR-02 的 20ms 预算内** | S8-T11；数值标「拟」、由 S8-08 标定后定稿 |
| **③ 不拒绝查询** | 合并期 N 次查询 **100% `Ok`**；库内**不存在**「重建索引中」类错误码（`Error` 枚举用 `grep` 断言） | S8-T10 |
| ⚠️ 附加 | **A/B 纪律**（承 Step 7）：所有「合并期 vs 静止期」的比较必须在**同一张冻结图 + 同一段布局的内容**上做（`--index` 读快照 + `--runs 1`），否则比的是两个变量 | 协议照 `perf-ab-calibration` 的四条抗噪声规则 |

⚠️ **为什么用相对判据做主判据**：②b 依赖 NFR-02 的 20ms，而**分段本身会抬高基线**（多段扫描 + 谓词）⇒ 若用绝对判据做主判据，会把「分段带来的基线变化」和「合并带来的干扰」混在一起（同 Step 7 的「图漂移 ~0.6%」教训：**必须先把控制组钉住**）。

#### 4.13.2 NFR-11（写延迟）—— 口径从「推算」改为「直接计时」

| 项 | 内容 |
| --- | --- |
| **测量口径** | `commit()` 调用的 **wall time**，直接计时（不再是 `embed_elapsed / 批数` 的推算）。**含**：`flush`（embed + 归一化 + 灌向量）+ 封段 + 发布；**不含**：首次模型下载 / `try_new` 加载 |
| **报告口径** | P50 / P99 + `n` + 运行范围（`batch_size` / 语料 / 机器），并按 `pending` 条数分档（默认 64） |
| **拟值** | **P50 ≤ 1.0 s / P99 ≤ 2.0 s**（`batch_size = 64`、CPU / FP32、T2Ranking 12K） |
| **拟值的推导（只有一条旁证，必须标清）** | `eval-report.md` §8.11：增量构建 `21.412 s / 18.75 批 ≈ **1.14 s/批**`（⚠️ 推算值）。`commit()` 的成本主体就是这一批的 embed（构建耗时里 **embed 占 93%**，`v2-step6-design.md`）。拟值 = 1.14 s 上界 + ~75% 余量取整 ⇒ P99 ≤ 2.0 s（**该余量是工程判断，不是实测**） |
| **定稿** | 由 **S8-08** 标定后回填（同 NFR-13 / NFR-10 / NFR-12 的先例：先落「拟」、标定定稿） |

#### 4.13.3 可观测（NFR-07）

新增（**纯加法**，不改既有字段语义）：

| 字段 | 位置 | 含义 |
| --- | --- | --- |
| `Metrics.segments` | `crates/core/src/query/metrics.rs` | 本次检索实际扫了几个段（`1` = 只有主段 ⇒ 零回归） |
| `Metrics.vector_segments` | 同上 | 向量路本次**实际覆盖**了几个段（`0` = 本次没走向量路） |
| `Metrics.tombstoned` | 同上 | 本次检索所依据的视图里的**跨段墓碑条数**（0 = 无墓碑） |
| `Metrics.vector_route` | 既有 | ⚠️ 混合时新增 `VectorRoute::Mixed`（Q5） |
| `MergeReport` | `crates/core/src/search/index.rs` | 合并的可观测（§4.9.1） |

⚠️ **`Metrics.segments` 必须进 `SearchResponse.metrics`**（既有链路已在，见 `query/metrics.rs` 的四处断链修复史）。

> **2026-09-19 三处更正与补充**（`S8-03` 评审批次 P2-2 落地，与实现同步）：
>
> 1. **新增 `Metrics.vector_segments`**（评审 P2-2 的建议取形）：`Metrics.segments` 单独**不是**缺陷信号，
>    「**向量路只覆盖了一部分段**」才是。默认 `mode` 是 `Hybrid` ⇒ `S8-05` 之前，
>    只要有向量能力，**每次 `commit()` 之后、`save`/`compact` 之前**都处于半盲态
>    （`helix serve` 这种「写端持续 commit、无人调 fold」的形态下**长期**如此）。
>    ⇒ 原表只列 `segments` 会漏掉这条真正要看的信号；PR5 的验收项 = `vector_segments == segments` 恒成立。
> 2. **`tombstoned` 的口径更正**（原文：「本次检索**被跨段墓碑挡掉的候选数**」）：该口径在**召回层不可良定义** ——
>    同一个 chunk 会被「BM25 路 / 向量路」各取一次、甚至在同一路的多个 posting 上重复出现
>    ⇒ 计数随 lane 数与 term 命中数**虚增**，读出来无法解释（也无法与「候选数」口径对齐）。
>    改为「**视图里的跨段墓碑条数**」：精确、O(1)，且正是调用方真正要的那个量
>    （`0` ⇒ 热路径已回到零谓词，`R52` 的回归可逆）。
> 3. **三者都必须在 `search_parts` 的入口填值**（三条早退路径也带真实值）：`Metrics::default()` 是
>    草稿缓冲区，只在返回路径上被覆盖 —— 早退路径若漏填，调用方读到的 `0` 是**假值**（`S5-T8` 的同族教训）。

### 4.14 不变式清单（写进 rustdoc 的三条以内 + 其余进设计文档）

**写进 rustdoc 的三条**（用户/实现者最容易被绊倒的）：

1. **`SearchIndex::searcher()` 不隐含 `flush`**：可见性 = `commit()` 后（对齐 NFR-11）。要「写完马上查」请先 `commit()`。
2. **合并（`merge_*`）保 ID；`compact()` 改 ID**。两者不可混用；`compact` 前必须先 `merge_all()`。
3. **分段布局下 `bm25` 与单段布局逐位一致；`vector`/`hybrid` 的 Hnsw 路径不承诺**（图拓扑不同）。Brute 后端承诺。

**其余（设计文档内的编号不变式）**：I8-1（写锁只用于指针替换）/ I8-2（段不可变）/ I8-3（一次检索只用一个 `View`）/ I8-4（同 view 内确定性）/ I8-5（全局精确整数统计量）/ I8-6（term 外层累加）/ I8-7（段 ID 空间不重叠）。

---

## 5. 决策记录（D-S8-01 ~ D-S8-12）

> 4 条需拍板的已在开头单列；本节给出全部 12 条的**理由**与**不同意会怎样**。

### D-S8-01 H4：持久化形态 ✅ 建议：`save` 前先 `merge_all()`，**不开 ADR-B**

- **理由**：`SnapshotSections` / `FORMAT_VERSION` / `GraphManifest` 三个面**一个字节都不用改**（§2.6 的源码依据）；ADR-A 的三条不变式全部续存；ID 在 save/load 往返后不变（既有行为，§2.4 的 DTO 显式携带 ID）。**用一条前置步骤消解一个架构级问题，是最省的取形。**
- **代价**：`save` 的成本多了「合并所有未合并段」。⚠️ **限定（2026-09-17 按评审 P4-7①）**：`save` 本来就 `commit()` + 全量序列化（O(N)），**文本侧**的合并在同一量级；**向量侧视 S8-S1 结论** —— 若回落方案 B（全量重建），`save` 会多付 ≈ **38.7s / 12K**（§4.9.3）。原写「合并在同一量级」**只对文本侧成立**。
- **不同意会怎样**：多段快照 ⇒ `FORMAT_VERSION` 升 3 + `GraphManifest` 改多段 + 向后兼容分支 ⇒ **ADR-B 必出**，Step 8 体量与风险再上一档。

### D-S8-02 视图模型 ✅ 建议：`RwLock<Arc<View>>` + 不可变 `Segment`

- **理由**：零新增依赖（守门约束）；写锁只做指针替换 ⇒ 读端不被写者拖住（I8-1）；`View` 不可变 ⇒ 读端拿到的快照**天然一致**（不需要 epoch / 引用计数 / 版本回退）。
- **替代方案（不采纳）**：`arc-swap`（新依赖）；细粒度锁（读会被写阻塞，**直接违反 FR-17**）；`crossbeam-epoch`（复杂度不匹配）。

### D-S8-03 H5：「读不阻塞写」的语义 ✅ 建议：① + ②，不做 ③

- **理由**：③（读能看到未 commit 的写）与 NFR-11 的「`commit()` 后可见」直接冲突（`plan-v2` §4.0 H5 已预写）；且它会让「一次检索的可见性边界」变得不确定（读端可能看到半批数据）。

### D-S8-04 段的 ID 空间 ✅ 建议：全局发号 + 段内本地 + 基址 = 前序长度之和，**合并保 ID**

- **理由**：§4.4.2 证明基址在 FIFO 合并下**恒有效**；不改 `Index` 的 ID 语义；`AddOutcome` 的公开契约不变。
- **替代方案（不采纳）**：① 段内用**全局稀疏 ID**（`Vec` 前置空洞 ⇒ 内存爆炸）；② 合并时**重编号**（用户侧的 `chunk_id` 静默失效）。

### D-S8-05 `SearchIndex` 的并发形态 ✅ 建议：**保持 `!Sync`**，`add(&mut self)` 不动

- **理由**：并发来自「写端线程独占 + 读端持 owned `Searcher`」，**不需要** `SearchIndex: Sync`；`add(&mut self)` 一字不改 ⇒ 既有调用点零波及；写端串行本来就是正确语义（多写者需要外部串行，文档写明）。
- **不同意会怎样**：改 `add(&self)` + 内部 `Mutex` ⇒ **公开签名变更**（`&mut self` → `&self` 虽源码兼容，但 `SearchIndex: Sync` 会让「两个线程同时写同一个 Index」编译通过 ⇒ 语义陷阱）。

### D-S8-06 旧 API 处置（T7-18）🔴 需拍板 ✅ 建议：`searcher(&self)` 新增 + 2 个 API `#[deprecated]`

- 见 §4.12 的全量评估表。**其余一律不废弃**（含 `Index` 的内核直用面与五 trait）。

### D-S8-07 BM25 跨段 ✅ 建议：全局精确整数统计量 + term 外层累加

- **理由**：§2.5 的可加性 ⇒ **逐位一致是结构性结论**，不是调参结果。这是本文唯一一条「零成本拿到强正确性」的设计。
- **反例（明确不采纳）**：各段各算 idf（分数只在分段布局下漂移，极难归因——R49）。

### D-S8-08 合并的宿主 ✅ 建议：库只给原语，**不 `spawn` 线程**

- 见 §4.9.1 的三条理由。⚠️ **CLI 是否提供后台合并线程**属 CLI 设计，本文只承诺原语。

### D-S8-09 合并器向量侧取形 🔴 需拍板 ✅ 建议：dump→load→insert，回落全量重建

- 见 §4.9.3 的决策门。

### D-S8-10 H6：NFR-11 口径与拟值 ✅ 建议：直接计时 + `P50 ≤ 1.0s / P99 ≤ 2.0s（拟）`

- 见 §4.13.2。⚠️ **拟值只有一条旁证**（1.14 s/批，且是推算）——必须在需求面标「拟」，禁止被当作实测引用。

### D-S8-11 T7-26 查询侧会话池 🔴 需拍板 ✅ 建议：形态定为「池」，**投不投由 S8-S2 判定**

- 见 §4.11。⚠️ **不预先承诺**（Step 6 的 A2 复审教训）。

### D-S8-12 `compact()` 与视图的关系 ✅ 建议：语义不变，但**必须 `merge_all()` 前置** + 文档写清 ID 语义分离

- **理由**：`compact()` 的重编号是**既有、显式、用户触发**的行为（FR-30）；新模型下它多了一个陷阱——**旧 view 的 ID 与 compact 后的 ID 不可混用** ⇒ 必须写进文档与 `CompactionReport` 的注记。

---

## 6. 影响面与兼容性

| 面 | 变更 | 破坏性？ |
| --- | --- | --- |
| `SearchIndex::searcher(&self)` | **新增** | 无（纯加法） |
| `SearchIndex::into_searcher(mut self)` | 转 `#[deprecated]` 薄封装，**行为不变** | ⚠️ 编译**警告**（`#[deprecated]` 是警告不是错误）；CI 若开 `-D warnings` 需给既有调用点加 `#[allow(deprecated)]` 或直接迁移 ⚠️ **这是一个必须处理的工程细节**（见 §8 的 PR3） |
| `Searcher::into_index()` | 转 `#[deprecated]`，返回类型**不变**（`Result<SearchIndex>`） | ⚠️ 同上 |
| `SearchIndex::add/commit/flush/save/remove/compact*` | 语义不变（`save` / `compact` 多一步 `merge_all()` 前置） | 无（签名不变） |
| `SearchIndex::merge_pending()` / `merge_all()` | **新增** | 无 |
| **新增** `MergeReport` | 公开类型 | 无（新增） |
| `Metrics.segments` / `Metrics.vector_segments` / `Metrics.tombstoned` | **纯加法**（但见下方 ⚠️） | 无 |
| `VectorRoute::Mixed` | **新增枚举变体** | ⚠️ **破坏性**：`VectorRoute` 若非 `#[non_exhaustive]`，外部 `match` 会**编译失败** ⇒ **必须核实**（若是，则要么加 `#[non_exhaustive]`，要么改为「新增一个独立字段 `vector_route_mixed: bool`」）。**取形待实现期核实后定**（登记 Q5） |
| `Config::embed_sessions` | **新增字段**，默认 **1** | ⚠️ `Config` 是 `pub struct`（字段公开）⇒ 直接构造 `Config { .. }` 的代码会编译失败。**核实是否已有此用法**；若有 ⇒ 走 `ConfigBuilder` 或补 `Default`。登记为实现期检查项 |
| `ConfigFingerprint` | **不扩**（会话数不影响索引内容） | 无 |
| `Error` 枚举 | **无新增变体** | 无（§4.13.1 ③ 还要求「不得存在『重建索引中』类变体」） |
| 快照格式 | **不变**（`FORMAT_VERSION` = 2） | 无 |
| 图 manifest | **不变** | 无 |
| 五 trait（`VectorIndex` / `Retriever` / `FusionStrategy` / `Reranker` / `Embedder`） | **不变** | 无 |
| `Index` 的 `pub` 面 | **不变**（新增 `pub(crate) merge_from`） | 无 |
| CLI | `save` / `compact` 行为不变；**新增** `--merge-segments` 之类的后台合并开关（取形待 CLI 设计） | 待定 |
| 需求面 | **NFR-14 定口径 + NFR-11 补口径与拟值**（§4.13）；⚠️ **FR-17 §5.3.5 的措辞**由「写入后」改为「`commit()` 后」（**不改语义**） | ⚠️ **属需求面，需评审同意**（§0 附加行） |
| 架构面 | **§14.6 新增 R49~R54**；ADR-A 补一条扩展边界（§4.10） | 无 |
| `.rs` 变更规模（预估） | **新增 ≈ 600~900 行**（`view.rs` 为主体），**改动 ≈ 300~500 行**（跨段 BM25/向量/谓词 + 合并器） | ⚠️ **L 级**，与 `plan-v2` 的标注一致 |

⚠️ **两处「破坏性」必须实现期核实**（`VectorRoute` 的 `#[non_exhaustive]`、`Config` 的字面构造用法）——**本文不凭记忆下结论**（Step 7 曾凭空造出 `rerank_status` 字段的教训）。

⚠️ **表外新增面（2026-09-19，请评审复核）**：`Metrics.vector_segments` **不在**本节原表中，是
`S8-03` 评审批次 P2-2 落地时按评审建议新增的（理由见 §4.13.3 的更正块）。
另需登记一条**既有事实**：`Metrics` 是 `pub struct` 且**无 `#[non_exhaustive]`** ⇒ 三个新字段对
「字面构造 `Metrics { .. }` 的下游」是**破坏性**（与 Step 7 新增 `rerank_window` / `rerank_elapsed` 同一形态）。
本仓内唯一字面构造点是 `query/metrics.rs` 自己的单测（已同步）；**是否给 `Metrics` 补
`#[non_exhaustive]`** 属公开面决策，本 PR 不做（补它同样会让下游字面构造编译失败 ⇒ 没有「零破坏」选项，同 Q5）。

---

## 7. 测试计划（S8-T1 ~ S8-T16）

> 标注：**CI** = 进 `cargo test --workspace`；**本地** = 需真实模型 / 长耗时，`#[ignore]` 或走脚本；**脚本** = 走 `scripts/`。

| # | 用例 | 类别 | 落点 | 鉴别力（这条红意味着什么） |
| --- | --- | --- | --- | --- |
| **S8-T1** | **R34 探针复跑**：std-only 探针（写线程等待 + 读线程在层切换时递归读）**必须不再死锁** | CI | `crates/core/tests/step8_rw_concurrency.rs` | ⚠️ **必须含反向自证**：把遍历改回 `IntoIterator` 后该用例**变红**（证明探针有鉴别力，否则「绿」没有意义） |
| **S8-T2** | **覆盖等价「每点恰一次」**：**经生产路径** `search_exact_filtered` 的 ID 集 == 入库 ID 全集（2026-09-18 第 1 轮评审后：`get_layer_iterator(0)` 的基数 `<` `get_nb_point()` 一条**已删** —— 由 `upper > 0` + 并集等式**蕴含**、**永不报红**） | CI | `crates/core/src/vector/hnsw_rs_index.rs` 单测（**扩展既有用例**） | 红 = 遍历写回单层（3.7% 静默错答） |
| **S8-T3** | **定向用例**：`level ≥ 1` 的点自查询排第一 | CI | 既有用例（确认仍绿） | 红 = 高层点被漏 |
| **S8-T4** | **跨段 BM25 逐位一致**：同内容 `[单段]` vs `[主段 + 2 个增量段]` ⇒ `hits` 的 `(chunk_id, score)` **逐位相同**；`Explain` 亦同。🔴 **＋「带跨段删除」变体（2026-09-17 按评审 P3-1 新增；⚠️ 「合并前」参照系于 2026-09-19 按 PR4 评审 P2-5 更正）**：先建主段 → `remove` 一条（落进墓碑）→ `commit` 出新 delta → **在合并前后各比一次**，但**两条断言的参照系不同**：<br>① **合并前（未合并窗口）** ⇒ 必须与「**单段建库、但保留被删 doc**（`N` / `total_len` / `df(t)` 一律**含它**）+ **命中集剔除该 doc**」逐位一致 —— 墓碑态在段内**仍然存活**，`View::bm25_totals()` 与 `df` 都**不扣墓碑**（实测：`view.rs` 的 `bm25_totals` 只做 Σ、`bm25.rs` 的 `df` 只做 Σ）⇒ 拿「单段建库 + `remove`」（统计量已**不含**它）当参照物，`idf`/`avgdl` 必然不同 ⇒ **逐位一致结构上不可能成立**（照原写法写测试会永远红并误导成实现错误）。<br>② **合并后（墓碑已物理化）** ⇒ 与「**单段建库 + 同序 `remove`**」逐位一致（这条同时锁住 §4.9.2 的物理化）| CI | `crates/core/tests/step8_segments.rs` | 红 = I8-5 / I8-6 / I8-7 之一被破坏（**本设计最重要的用例**） |
| **S8-T5** | **Brute 后端的 `vector` 模式跨段逐位一致** | CI | 同上 | 红 = 精确路径的全局归并写错（`(距离, chunk_id)` 全序） |
| **S8-T6** | **跨段 `content_hash` 幂等 + 删除后可重新 upsert**：主段有 doc ⇒ `add` 同 hash 报 `deduped`；`remove` 后 `add` 同 hash **不再 deduped**。🔴 **2026-09-19 第 2 轮评审 P1-1 补一个变体**：`remove` 后**不 `commit()`** 直接重加 ⇒ 必须 `!deduped` 且 `doc_id != X`（**这一条才是唯一绊线** —— 变异 `M-P1-1` 实测：本变体红、上面那条绿） | CI | 同上 | 红 = 「命中但被墓碑挡住」被当成命中（§4.8.3 的语义）；**或**查重漏看 `builder` 上未发布的墓碑 |
| **S8-T7** | **可见性双向**：`add` 后未 `commit` ⇒ 查不到；`commit` 后 ⇒ 立即查到 | CI | 同上 | 红 = 可见性边界错（NFR-14 ①） |
| **S8-T8** | **合并保 ID + save/load 往返保 ID**：`merge_pending()` 前后同一 chunk 的全局 `chunk_id` 不变；`save`→`load` 后仍不变 | CI | 同上 | 红 = D-S8-04 的基址不变式被破坏 |
| **S8-T9** | **同 view 内的确定性**：同 query 连续 100 次逐位一致；`generation` 不变期间 `Arc<View>` 内容不变 | CI | 同上 | 红 = I8-2（段被就地修改） |
| **S8-T10** | **三线程并发探针**（🔴 **2026-09-17 按评审 P3-4 加「双写端」臂**：由 `into_index()` 再造一个写端、两写端各自持续 `add`/`commit`，断言**段 ID 空间无重叠**（`base` 严格递增、全局 `chunk_id` 无重复）—— 这是 §4.4.3 那条锁的鉴别力用例）：写线程持续 `add`/`commit`、合并线程持续 `merge_pending`、读线程持续检索 ⇒ **零 `Err`、零超时、结果只反映已提交前缀**；并断言 `Error` 枚举**无**「重建索引中」类变体 | CI（规模小） | 同上 | 红 = 读端被阻塞 / 看到未提交内容 / 出现新错误码 |
| **S8-T11** | **合并期读延迟**（同机、同**图**、同 query 集 —— ⚠️ **2026-09-17 按评审 P4-3 更正**：原写「同 fig」）：②a 比值 ≤ 1.2（拟）与 ②b 绝对护栏 | 脚本 | `scripts/eval_rw_concurrency.sh` | 红 = 写锁被用在重活上（I8-1 被违反） |
| **S8-T12** | **热路径回归对照**：无 delta / 有 delta 无墓碑 / 有跨段墓碑 / **有跨段墓碑但已 `merge_all()`** **四档**的 `bm25` lane 耗时（第 4 档由 P3-1 的「合并后回到 `None`」推出 —— 2026-09-17 新增） | 脚本 | 同上 | 量化 R52（不是门槛，是读数）；第 4 档必须回到第 1 档量级 |
| **S8-T13** | **合并器等价的字段索引**：合并后的 `field_index` 求值 == 全量重建的求值（过滤 battery） | CI | `crates/core/tests/step8_segments.rs` | 红 = `FieldIndex::rebuild` 与增量维护不等价（§4.9.2） |
| **S8-T14** | **合并器向量侧等价**：合并后的图在固定 query 集上与全量重建图的 top-10 重合率 ≥ 0.99 | CI（小规模、Brute/Hnsw 各一） | 同上 | 红 = 向量合并丢点 |
| **S8-T15** | **NFR-11 写延迟直接计时** | 脚本 | `scripts/eval_rw_concurrency.sh` | 产出 P50/P99 读数（D-S8-10 的定稿依据） |
| **S8-T16** | **Q3′ 数值一致性 + 会话池内存**（spike S8-S2 的产物） | 本地 | `crates/core/examples/bench_embed_session.rs` 扩展 + 脚本 | 红 = 池化改变向量数值 ⇒ **判「不投」** |

⚠️ **测试计数口径**：报告时**取全部 `^test result` 行求和**，并标注**运行范围**（哪些 feature / 哪些被 `#[ignore]`）。

---

## 8. 实施任务拆分（S8-01 ~ S8-09）与 PR 切分

| 任务 | 内容 | 量级 | 依赖 |
| --- | --- | --- | --- |
| **S8-01** | **T7-25**：R34 修法（逐层遍历 + S8-T1/T2/T3） | **S** | 无（**硬前置**） |
| **S8-02** | **视图骨架**：`view.rs`（`Segment`/`View`/`Shared`/`ViewFilter`）+ `searcher(&self)` + `into_searcher`/`into_index` deprecated + ID 发号 + **`deltas` 恒空**（行为与今天逐位一致） | **M** | S8-01 |
| **S8-03** | **delta 写入闭环**：`SegmentBuilder` + `commit()` 封段发布 + 跨段 `content_hash` 查重 + `remove` 的三种分支 | **M** | S8-02 |
| **S8-04** | **跨段 BM25 + 谓词**：全局统计量 + term 外层累加 + `ViewFilter`（含 `allowed_count` 精确性） | **M** | S8-03 |
| **S8-05** | **跨段向量**：逐段检索 + 全局归并 + `VectorRoute` 混合取形 + `Metrics.segments`/`tombstoned` | **M** | S8-03 |
| **S8-06** | **spike S8-S1**（合并器向量侧）+ **合并器**：`Index::merge_from` + `merge_pending`/`merge_all` + `MergeReport` + `save`/`compact` 前置 | **L** | S8-04 + S8-05 |
| **S8-07** | **T7-18**：旧 API 废弃（**并入 S8-02 的 PR**，见下） | **S** | S8-02 |
| **S8-08** | **T7-26 + spike S8-S2**：查询侧会话池 + Q3′ + 决策门结论 | **M** | 无（**可并行**） |
| **S8-09** | **标定 + 收尾**：NFR-14 / NFR-11 定稿 + `eval-report.md` **§8.15** + 四处定义面回写 + `v2-step8-design.md` 升版 | **M** | 全部 |

### PR 切分建议（照 V2.0 惯例，7 段）

| PR | 标题 | 内容 | 备注 |
| --- | --- | --- | --- |
| **PR1** | `docs(step8): Step 8 详细设计` | **本文** + 四处定义面回写 + CHANGELOG | **= 本 PR**，纯文档（`.rs` 零改动） |
| **PR2** | `fix(vector): 精确扫描改逐层遍历，消除层切换的递归读锁（T7-25）` | S8-01 | ⚠️ **建议先合**：风险最低、收益明确、与其他 PR 零耦合 |
| **PR3** | `feat(core): 不可变视图骨架 + searcher(&self) + 所有权遗留 API 废弃（S8-02 / T7-18）` | S8-02 + S8-07 | ⚠️ **必须守住「`deltas` 恒空 ⇒ 与今天逐位一致」**（§1.3 第 2 条） |
| **PR4** | `feat(core): delta 分段写入 + 跨段 BM25 / 谓词（S8-03 / S8-04）` | S8-03 + S8-04 | 核心 PR；S8-T4 是它的门 |
| **PR5** | `feat(core): 跨段向量检索（S8-05）` | S8-05 | 可与 PR4 合并评审、但**分开提交便于二分** |
| **PR6** | `feat(core): 分段合并器 + save/compact 前置（S8-06）` | S8-06 + spike S8-S1 的结论 | 含 `MergeReport` |
| **PR7** | `feat(embed)+docs: 查询侧会话池（T7-26）+ NFR-14/NFR-11 定稿（S8-08 / S8-09）` | S8-08 + S8-09 | ⚠️ **S8-08 可提前到 PR3 之前**（与 PR4~6 完全正交） |

⚠️ **每个 PR 必带 CHANGELOG 条目**；分支 → PR（base `main`）→ 评审 → **用户手动合并**（agent 不 `gh pr merge`）。

---

## 9. 风险与未决问题

### 9.1 新增风险（**建议写入架构 §14.6**，编号 **R49 ~ R54**）

| # | 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- | --- |
| **R49** | **跨段 BM25 的「逐位一致」依赖三条前提**（I8-5 全局精确整数统计量 / I8-6 term 外层累加 / I8-7 段 ID 空间不重叠）。任一被破坏 ⇒ 分数漂移且**只在分段布局下出现** | 结果「看起来对」但分数悄悄变了 ⇒ 后续所有质量对比失去可比性 | ① 三条前提写进 rustdoc 不变式 + 合并器注释；② **S8-T4 逐位一致断言**（本设计最重要的一条测试）；③ `Index::merge_from` 的 posting 顺序写成注释（**不得「顺便排序」**） | ⏳ 开放（结构性约束，无法用测试穷尽；靠断言 + 评审守） |
| **R50** | **「合并保 ID」与 `compact()` 的「重编号」语义冲突** | 若合并实现偷懒复用 `compacted()` ⇒ **用户侧的 `chunk_id` 静默失效**（Agent 记忆里的 ID 指到别的 chunk） | ① D-S8-12 把两者显式分离并写进文档；② `compact()` 前强制 `merge_all()`；③ `CompactionReport` 的注记补「compact 后 ID 会变」 | ⏳ 开放（静默正确性风险，靠文档 + 评审） |
| **R51** | **向量侧的合并成本与拓扑不确定**：`hnsw_rs` 无图合并 API ⇒ 只能 dump→load→insert（R23 的 `Box::leak` + R22 的 1.6× 体积 + 临时 I/O）或全量重建（≈38.7s/12K） | 若 A 不可行且必须走 B ⇒ **小 delta 也要付 O(N) 重建**，合并频率被迫降到很低 ⇒ delta 段堆积 ⇒ 读路径随段数变慢 | spike **S8-S1** 的决策门（§4.9.3）；`MergeReport.vector_strategy` 如实记录走了哪条 | ⏳ 开放（等 spike） |
| **R52** | **热路径回归：`bm25_f` 从 `None` 变 `Some`**（一旦有跨段墓碑，postings 没有被物理摘除 ⇒ 必须每 posting 判谓词） | 无过滤查询的 BM25 lane 常数变大（每 posting 一次 `dyn contains()`） | ① 「无 delta 且无墓碑」时**显式短路回 `None`**；② 无墓碑但有 delta 时用逐段 `AliveOnly`（零构建成本）；③ **S8-T12** 量化三档读数 | ⏳ 开放（读数，不是门槛） |
| **R53** | **视图指针锁与「写者优先」的交互**：`std::sync::RwLock` 在 macOS / futex 实现下，写者等待会挡住新读者（**与 R34 是同一个机制**）⇒ 若写者持写锁做了重活，读端 P99 会尖刺 | 合并期的读延迟判据（NFR-14 ②）失效 | ① **I8-1**（写锁只用于指针替换）写成 rustdoc + 评审红线；② **S8-T11** 直接测「合并期 vs 静止期」的 P99 比值 | ⏳ 开放（靠不变式守） |
| **R54** | **会话池的内存（R36 的查询侧背面）**：N 个 `TextEmbedding` 实例各持一份 `Session` + `Tokenizer` ⇒ 可能顶穿 NFR-05 的 372MB | 用户按 NFR-05 选机器 ⇒ 开启池化后 OOM | ① **默认 `embed_sessions = 1`**（零代价，不开就不付）；② spike **S8-S2** 按**查询侧口径**实测峰值 RSS（⚠️ **不得引用 E2 的建库口径读数**）；③ 决策门含「RSS 增量 ≤ 20%」 | ⏳ 开放（等 spike） |

### 9.2 未决问题（Q1 ~ Q8）

| # | 问题 | 影响 | 建议取形 |
| --- | --- | --- | --- |
| **Q1** | **后台合并的宿主**：库内不起线程（D-S8-08）⇒ 谁调 `merge_pending()`？ | 决定 FR-17 的「**异步**合并」在**产品层面**是否真的成立（库层面只给原语） | **CLI 侧**（`helix serve` / `build --watch`）起后台线程；**本 Step 只交付原语**，CLI 线程留 Step 8 之后或横切任务 |
| **Q2** | **是否永远不做多段快照？** | D-S8-01 把多段快照推给了「将来」 | 登记为**条件项**：若出现「delta 大到 `save` 时合并成本不可接受」的真实场景，再开 **ADR-B** |
| **Q3′** | （**改述自 Q3**）不同 `intra_threads`（及不同实例数）是否改变 `embed_query` 的向量**数值**？ | 若改变 ⇒ 池化破坏 NFR-10 ① 的「逐位一致」前置 ⇒ **必须判「不投」** | **spike S8-S2 必答**；⚠️ 不允许推断（R47 的教训） |
| **Q4** | **默认 `embed_sessions` 取值** | 默认 1 = 零收益零风险；默认 > 1 = 在**没有实测支撑**的情况下付内存 | 默认 **1**；若 S8-S2 通过，再在 CLI / 用户文档给推荐值（**库侧默认不动**） |
| **Q5** | ✅ **已结案（2026-09-17，评审代核 + 本文独立复核）**：**`VectorRoute` 定义于 `vector/mod.rs:37`，未标 `#[non_exhaustive]`**，三变体 `None` / `Ann` / `Exact`，经 `query/mod.rs:24` 再导出、在公开的 `Metrics.vector_route` 上暴露 ⇒ **新增 `Mixed` 变体对下游 exhaustive `match` 是破坏性变更**；**给枚举补 `#[non_exhaustive]` 同样会让下游 exhaustive `match` 编译失败**（须加通配臂）⇒ **两条路都属 breaking，没有「零破坏」选项**。⇒ 取形 = **新增 `Mixed` 变体 + PR5 的 CHANGELOG 记 `Breaking`**。~~**`VectorRoute` 混合取形**~~：新增 `Mixed` 变体（破坏性，取决于是否 `#[non_exhaustive]`）还是新增独立布尔字段？ | 影响公开面 | **实现期核实** `VectorRoute` 的定义后定；倾向**新增 `Mixed` 变体 + 给枚举补 `#[non_exhaustive]`** |
| **Q6** | **段数上限与「何时合并」的策略**（`deltas.len() > K` 才合并？） | 读路径成本随段数线性增长 | 库**不设**策略；CLI / 宿主按 `K = 4`（拟）触发；**D-S8-08 的「库只给原语」在此延续** |
| **Q7** | **NFR-14 ② 的判据取形**：相对（比值）还是绝对（ms）？ | 决定报告怎么读 | **相对为主 + 绝对为护栏**（§4.13.1 已给理由：避免把「分段的基线变化」与「合并的干扰」混在一起） |
| **Q8** | ✅ **已结案（2026-09-17，评审代核 + 本文独立复核）**：**`Config` 无 `impl Default` / 无 `Default` derive**（`search/config.rs:35` 的 `pub struct Config {` 上方只有文档注释），全仓**唯一**字面构造点是 `config.rs:301` 的 `build_config`（crate 内部）⇒ **仓内无下游破坏**。⇒ 取形 = **加在 `Config` 上（字段公开）+ 同步 `build_config`**；`ConfigBuilder` 不必需。~~`Config::embed_sessions` 加在 `Config` 还是 `ConfigBuilder`？~~ | 公开面（§6 的破坏性核实项） | **实现期核实** `Config` 是否有字面构造用法后定；若走 `ConfigBuilder` 则风险最小 |

---

## 10. 第 1 轮评审响应（v0.2，2026-09-17）

> 评审：`pulls/60/reviews` **1 条**（**5×P3 + 7×P4，无阻塞**）+ 行内 **0** 条 + `issues/60/comments` **0** 条 ⇒
> **本轮评审只在 `reviews` 一处**（本轮 head `75aa724`）。评审的**独立复核表**（18 条仓库侧 + 16 条依赖侧）
> 与**4 条拍板 + 需求面 1 条**的回应见该评审正文；本节只记**处置**。

### 10.1 逐条处置

| # | 位置 | 处置 | 落地位置（本文内） |
| --- | --- | --- | --- |
| **P3-1** | 跨段墓碑在**合并时**的处置未写明 | ✅ **采纳，取「A. 物理化」** | §4.9.2（新增「跨段墓碑」行 + 物理化边界行 + 选 A 的三条理由）、§4.9.5（表 + `tombstone_stats()` 改为「合并后归零」）、§4.7.3（表加「合并后」列）、§4.9.1（`tombstones_applied` 语义）、§4.10（落盘等价精确化）、S8-T4（**加「带跨段删除」变体，合并前后各一条断言**）、S8-T12（第 4 档）、附录 A.3 不变式 3 |
| **P3-2** | 附录 B `cargo run -p helix-cli` 包不存在（Step 6 评审 F2 的**复发**） | ✅ 采纳 | 附录 B 命令 ⑥ 改为 `-p helix` + 注明「包名见 `crates/cli/Cargo.toml:2`、F2 同类复发」；并**逐条实跑核对了附录 B 其余命令**（`-p helix-core` ✅ / `eval_quality.sh --runs` ✅ / `--example bench_embed_session` ✅ / `data/t2-corpus.jsonl` ✅） |
| **P3-3** | §4.6.1 三个取形并存；顺带把 Q5 / Q8 的核实做掉 | ✅ 采纳 | §4.6.1（**删两个草稿**、只留「并集分类 ⇒ 新值 `Mixed`」）、Q5（**结案**：无 `#[non_exhaustive]` ⇒ 两条路都 breaking）、Q8（**结案**：无 `impl Default`、唯一字面构造在 `config.rs:301`）、附录 C |
| **P3-4** | 双写端并存时 `commit()` 的「算 base + 发布」原子性未写明 | ✅ 采纳（**D-S8-06 的前置条件**） | §4.4.3（新增「seal + publish 必须与发号同处一个临界区」的取形与理由）、S8-T10（**加「双写端」臂**：断言段 ID 空间无重叠） |
| **P3-5** | `Box::leak` 是「每次合并 +1」的累积效应 | ⚠️ **结论采纳、量级前提更正**（见 §10.2） | S8-S1 决策门**新增第 ⑤ 条**（连续 10 次合并 RSS 增量 < 1 MiB，**阈值的意义是把「只泄漏 `HnswIo`」与「误泄漏整张图」区分开**）、§4.9.3（补 A / B 的**非对称**）、架构 §14.6 R51 的残余列 |
| **P4-1** | 「既有 **100+ 处调用点**」高估 ~2.6× | ⚠️ **采纳、但数字经独立复算后与评审不同**（见 §10.3） | §4.8.2 改为精确口径：**41 处调用点**（+2 处定义 +11 处注释） |
| **P4-2** | `p_id.0 = level`（`:500`）→ `:505` | ✅ 采纳 | §2.2 表 + 附录 C，并**把代码片段改为逐字原文** `let mut p_id = PointId(level as u8, -1)`（原「`p_id.0 = level`」是转述、文中并不存在该行） |
| **P4-3** | S8-T11「同 **fig**」→「同**图**」 | ✅ 采纳（笔误） | §7 S8-T11 |
| **P4-4** | 需求 v1.20 行「**复发**触发条件」→「**复审**」；同段加粗嵌套断开 | ✅ 采纳（**两处位置都在 `requirements-spec.md`**；加粗断裂实为 **`:9` 头部状态行**，非 `:595`） | `requirements-spec.md:595`（措辞）+ `:9`（**多余 `**` 已删**，见 §10.4） |
| **P4-5** | 非范围「不开 ADR-B，见 **D-S8-09**」→ **D-S8-01** | ✅ 采纳 | 头部「非范围」行 |
| **P4-6** | §4.9.2 field_index 行的「重复插入会双计」与代码不符 | ✅ 采纳（**机理写错了**） | §4.9.2 `field_index` 行——真实机理是**同一数值同时登记 terms + numbers 两池、合计计入同一限额**（`field_index.rs:51-52`），而 `field_index.rs:188` 有 `!contains_key` 守卫 ⇒ 重复插入同一 value 键**不会**重复 +1；「必须 rebuild」的结论仍成立（另有依据：degraded 粘滞 + 降级字段缺键） |
| **P4-7** | ① D-S8-01 代价「同一量级」只对文本侧成立；② 「登记进 **R49** 的观测项」挂错 | ✅ 两条都采纳 | ① D-S8-01 代价行加限定（向量侧视 S8-S1）；② §4.6.3 改为**改挂 Q5 + §4.13.3**（⚠️ **与评审建议的 R52 不同**，理由见 §10.2） |

### 10.2 与评审结论**不同**的地方（两类）

**① P3-5 的量级前提：结论采纳，但「内存随合并次数线性增长」的量级被高估。**

评审的表述是「长驻进程的内存随合并次数**线性增长**」（隐含按图规模计），并据此要求 S8-S1 加「重复合并不增长内存」的门。
**独立复算**（`crates/core/src/vector/persist.rs:210-216` 的 P1-2 注释 + 架构 §14.6 R23 的影响列）：

| 项 | 实测/代码事实 |
| --- | --- |
| leaked 的对象 | **只有 `HnswIo`**（`Box::leak(Box::new(HnswIo::new(dir, &basename)))`，`persist.rs:216`） |
| 量级 | 「**每次加载约 200B + 路径串**」（`persist.rs:214` 逐字），R23 影响列同 |
| **不是**什么 | **不是**图 / 向量数据 —— `HnswIo` leak 后句柄被**直接丢弃**（P1-2），向量被读进内存（`PointData::V`）、由 `Hnsw<'static>` **自持**；旧 `Segment` 释放时**那部分是可回收的** |

⇒ 「线性」在**数学上成立**（每次合并 +1 个 `HnswIo`），但**速率是 ≈200B/次**而不是图规模；
10 次合并 ≈ 2 KB（被 RSS 噪声淹没）。⇒ 处置：**保留该门，但把阈值定成「< 1 MiB」并在注释里写明它的鉴别力**
（把「只泄漏 `HnswIo`」与「误泄漏整张图」区分开）——否则一个「按 ≤20% 相对增量」写的门会**必然假通过**（2KB 相对 373MB 是 0.0005%）。

**② P4-7② 的归属：改成 Q5 + §4.13.3，而不是 R52。**

评审建议「改挂 **R52**（或补进 R51 的段堆积链条）」。**未采纳该归属**，理由是三条各自的范围：
- **R49** = 跨段 BM25 的**三条前提**（I8-5/6/7）；
- **R52** = **`bm25_f` 热路径**（谓词从 `None` 变 `Some`）；
- 本条（逐段判 `prefers_exact` ⇒ 小段更易命中低选择度兜底 ⇒ `vector_route` / `vector_shortfall` 语义变化）**既不是打分前提、也不是 bm25 热路径**，而是**路由/阈值的可观测语义** ⇒ 归 **Q5（`VectorRoute` 混合取形）+ §4.13.3 可观测节**更贴。
（若评审坚持挂 R52，改一行即可；**本轮不改风险编号**。）

### 10.3 P4-1 的独立复算：**41 处**（≠ 评审的 38 处）

**复核方法**：`git ls-files crates` ⇒ 对每个 `.rs` 逐行匹配 `into_searcher|into_index`，按「行首是否为 `//`」「是否 `fn` 定义」「是否 `.into_x(` 调用」「是否落在 `#[cfg(test)]`/`tests/`」分类（脚本一次性跑，非目测）。

| 分类 | 计数 |
| --- | --- |
| 原始提及（含注释） | **56** |
| 其中注释行 | **11**（`core/src` 7 + 集成测试 4） |
| **代码提及（非注释）** | **45** |
| 　├ **定义**（`index.rs` 的 `into_searcher` + `searcher.rs` 的 `into_index`） | 2 |
| 　├ **生产调用点**（`cli/src/main.rs` 3 + `examples/search_basic.rs` 1；**`core/src` 生产调用 = 0**） | **4** |
| 　└ **测试里的调用**（集成测试 33 + `searcher.rs` 的单测 4；另有 2 个**测试函数名**含该词） | **37** |
| ⇒ **调用点合计（生产 4 + 测试 37）** | **41** |

⚠️ **与评审的 38 不同**：评审给的是「**38** = 44 raw − 6 注释；tests 32 / src 2 / cli 3 / examples 1」。逐项比对：**cli 3 ✅ / examples 1 ✅ / src 2 ✅**（3 项完全一致），差额全在 **tests（32 vs 37）与 raw（44 vs 56）** ⇒ **两者的分类口径不同**（我这版把 `searcher.rs` 的 `#[cfg(test)]` 块 6 处与 4 处行内注释计入 tests 侧；是否计入「测试函数名」也影响 2 处）。**不宣称评审算错** —— 两套口径都自洽，本文采用**自己这版**并把口径写在上面（可复核）。
⚠️ **实质结论不变**：无论 38 还是 41，「**100+**」都是高估（≈2.4~2.6×），且**生产调用点只有 4 处**——`#[deprecated]` 薄封装的「零改动」结论仍然成立，但**理由比原文更强**：不是因为「调用点太多不能改」，而是**这 4 处本来就是「交出读端」的合法用法**（本次只是给它加一层 deprecated 提示）。

### 10.4 顺手修的**既有**缺陷（非本次引入，评审未点名）

按「一类缺陷要全仓扫」的纪律，用**非级联判据**（逐行 `**` 奇偶 × 与 `main` 内容对照 + 合法嵌套对照样本自证）扫了 6 个文件：

| 文件 | 本次引入 | 既有（`main` 上就有） | 处置 |
| --- | --- | --- | --- |
| `requirements-spec.md` | **1**（`:9`，本 PR 新写的 v1.20 段） | 0 | ✅ **已修**（删多余 `**`） |
| `architecture-design.md` | 0 | **1**（`:9` 状态行的孤儿 `**`） | ✅ **顺手修**（该行本 PR 已在改） |
| `plan-v2.md` | 0 | **1**（`:10` 状态行的孤儿 `**`） | ✅ **顺手修**（同上） |
| `v2-step8-design.md` / `docs/README.md` / `CHANGELOG.md` | 0 | 0 | — |
| `v2-step8-design.md` | 0 | — | ⚠️ **块级配对曾误报 1 处**：块内**跨行**加粗（表格相邻行之间）会被块级配对级联错位 ⇒ 用**逐行奇偶**复核后确认为**假报**（该文件奇数行 = 0）。**判据要选「可复核的单一事实」，不要选「级联的推导结果」。** |

⚠️ **`CHANGELOG.md` 有 4 行「逐行奇偶为奇」是合法的跨行加粗**（`**A…
…B**`）⇒ 逐行口径对它是**假报**，已按块级确认。两次假报都说明：**加粗类检查器必须先跑「合法嵌套 / 跨行加粗」两个好样本自证**（本轮探针的 `--selftest` 含 4 个好样本 + 1 个坏样本）。

### 10.5 本轮**不适用**变异测试（纯文档 PR）

本 PR **没有任何断言可变异**（`.rs` 零改动、无新增测试）。替代验证 = **§10.4 的探针（含 4 个好样本 + 1 个坏样本的对照自证）** + 评审自身的独立复核表。
⚠️ 唯一「新门」是 **S8-S1 的第 ⑤ 条**，它是**将来实现期的 spike 判据**，本 PR 不落代码 ⇒ 无门可变异。

### 10.6 未闭环 / 盲区（如实登记）

1. **P3-1 / P3-4 的取形是「设计期决定」，尚无实现**：影响面 = S8-03 / S8-06 / S8-07 三个任务（PR4 / PR6）。评审建议「在 PR4/PR6 开工前以 v0.2 补明」—— **本轮已补明**，实现按本节取形走。
2. **P3-2 是已修错误的复发**：本轮只修了附录 B 的那一条。⚠️ **更彻底的处置（把「包名 / 命令实存性」做成守门）本轮未做** —— 建议与 §10.4 的加粗探针**合并立为一个工程卫生项**（见 `plan-v2.md` §4 的「V2.1 横切任务」）。
3. **评审的 38 vs 本文的 41 未收敛到单一口径**：已在 §10.3 写明两套口径与差额来源；**若评审要求统一，我按评审口径改**（改的是 4 处文档数字）。
4. **`LayerGenerator` 的层高生成实现细节仍未核实**（附录 C ①）—— 与本 Step 无关，保持登记。

**结论**：12 条意见**全部处置完毕**（11 条采纳 + 1 条结论采纳/量级更正 + 1 条归属不同）；`.rs` 零改动；设计文档 v0.1 → **v0.2**。

---

## 附录 A：新增 / 变更 API 一览 + 不变式

### A.1 新增公开面

| 项 | 位置 | 签名 / 形态 | 破坏性 |
| --- | --- | --- | --- |
| `SearchIndex::searcher` | `crates/core/src/search/index.rs` | `pub fn searcher(&self) -> Searcher`（**不消耗、不隐含 flush**） | 无 |
| `SearchIndex::merge_pending` | 同上 | `pub fn merge_pending(&mut self) -> Result<Option<MergeReport>>` | 无 |
| `SearchIndex::merge_all` | 同上 | `pub fn merge_all(&mut self) -> Result<MergeReport>` | 无 |
| `MergeReport` | 同上 | `pub struct MergeReport { segments_merged, chunks_merged, tombstones_applied, vector_strategy, vector_merge_ms, total_ms, generation }` | 无 |
| `VectorMergeStrategy` | 同上 | `pub enum { Incremental, Rebuild }` | 无 |
| `Config::embed_sessions` | `crates/core/src/search/config.rs` | `pub embed_sessions: usize`（默认 **1**） | ⚠️ 待核实（Q8） |
| `Metrics.segments` / `Metrics.tombstoned` | `crates/core/src/query/metrics.rs` | `pub usize` | 无 |
| `VectorRoute::Mixed` | `crates/core/src/retriever/vector.rs`（或 `query`） | 枚举变体 | ⚠️ 待核实（Q5） |

### A.2 变更公开面

| 项 | 变更 | 兼容性 |
| --- | --- | --- |
| `SearchIndex::into_searcher` | → `#[deprecated]` 薄封装（行为**不变**：仍 `commit()` + 交出 `Searcher`） | 源码兼容；编译警告 |
| `Searcher::into_index` | → `#[deprecated]`（返回类型不变） | 源码兼容；编译警告 |
| `SearchIndex::save` | 内部多一步 `merge_all()` | 签名不变；**耗时**增加（同量级） |
| `SearchIndex::compact` / `compact_and_save` | 内部多一步 `merge_all()` | 同上 |

### A.3 写进 rustdoc 的三条不变式（附录 A 的收口）

1. `searcher()` 不隐含 `flush`：**可见性 = `commit()` 后**。
2. **合并保 ID；`compact()` 改 ID**；`compact` 前必须先 `merge_all()`。
3. **分段布局下 `bm25` 与单段逐位一致**（**含「带跨段删除」场景**：墓碑在合并时物理化，全局统计量与「单段建库 + 同序 `remove`」一致）；**`vector`/`hybrid` 的 Hnsw 路径不承诺**（Brute 承诺）。
4. **`commit()` 的「seal + publish」与 ID 发号同处一个临界区**（`Shared::ids` 锁）⇒ 双写端下段 ID 空间**不可能重叠**（I8-7）。

---

## 附录 B：执行命令（**可复制**）

```bash
# ── 0) 守门（分段跑，⚠️ 整条链塞一次前台调用会被 SIGKILL）──
make fmt && make lint
cargo test --workspace
make shell
make deny

# ── 1) R34 探针（T7-25；⚠️ 必须含「改回旧写法即红」的反向自证）──
cargo test -p helix-core --test step8_rw_concurrency -- --nocapture

# ── 2) 跨段逐位一致（本设计最重要的断言）──
cargo test -p helix-core --test step8_segments -- --nocapture

# ── 3) 读写并发：三线程探针 + 合并期读延迟 + 写延迟（本地 release）──
#    ⚠️ 延迟数字在 CI 共享 runner 上不具可引用性
#    ⚠️ `scripts/eval_rw_concurrency.sh` **尚未存在** —— 它是 S8 的交付物（§8 交付物 ⑧），
#       S8-09 落地后本条命令才可执行（2026-09-17 补注）
./scripts/eval_rw_concurrency.sh --index /tmp/frozen.idx --runs 1

# ── 4) 零回归对照（视图骨架落地后、delta 未启用时）──
./scripts/eval_quality.sh --runs 3          # bm25 三项必须 Δ = 0

# ── 5) 会话池 spike（S8-S2；逐档位独立进程，RSS 用外置 time）──
/usr/bin/time -l cargo run -p helix-core --release --example bench_embed_session -- \
    --texts 4000 --configs e1 --rounds 3 --warmup 0 --json /tmp/s8-e1.json
# ⚠️ 查询侧口径必须另给（batch 1 × 短 query），不得沿用 E2 的 4000 段长文本口径

# ── 6) 冻结图（⚠️ 只做一次；A/B 必须在同一张图上做）──
cargo run -p helix --release -- build --input data/t2-corpus.jsonl --index /tmp/frozen.idx
# ⚠️ 2026-09-17 按评审 P3-2 更正：包名是 `helix`（`crates/cli/Cargo.toml:2`），**不是** `helix-cli`
#    —— 这是 Step 6 评审 F2 已修过的同类错误复发（`cargo pkgid -p helix-cli` → did not match any packages）。
#    附录 B 其余命令已逐条实跑核对：`-p helix-core` ✅ / `scripts/eval_quality.sh --runs` ✅ /
#    `--example bench_embed_session` ✅ / `data/t2-corpus.jsonl` ✅。
```

---

## 附录 C：依赖源码核实记录

| 位置 | 事实 | 影响本文哪一条 |
| --- | --- | --- |
| `hnsw_rs v0.3.4` `hnsw.rs:633`（`IterPoint::new`） | 构造即 `points_by_layer.read()`，迭代全程持有 | §2.2 R34 定位 |
| `hnsw_rs v0.3.4` `hnsw.rs:656-677`（其中 `:661`） | 层切换时**再取一次**同一把 `points_by_layer` 读锁 ⇒ **递归读** | §2.2 / R34 / D-S8 的 T7-25 |
| `hnsw_rs v0.3.4` `hnsw.rs:660` | 同行取的是**另一把** `entry_point` 锁 ⇒ **不构成递归**（避免误判成「两把锁都递归」） | §2.2 |
| `hnsw_rs v0.3.4` `hnsw.rs:701` / `:715-723`（`IterPointLayer`） | 构造取一次锁；`next` 只索引 `pi_guard[self.layer]`，**不再取锁** | §2.2 修法可行性 |
| `hnsw_rs v0.3.4` `hnsw.rs:498-526`（`generate_new_point`，push 在 `:511`） | `let mut p_id = PointId(level as u8, -1)`（**`:505`**；`level` = `:500` 的 `layer_g.generate()`）；**只推入自己那一层、无回填** ⇒ 每点恰在一层 | §2.2 覆盖等价证明 |
| `hnsw_rs v0.3.4` `hnsw.rs:469-475`（`PointIndexation::get_max_level_observed`）/ `:814`（`Hnsw` 同名方法） | 返回 `entry_point.p_id.0`；空图为 **0** | §2.2 写法钉死 + 空图边界 |
| `hnsw_rs v0.3.4` `hnsw.rs:529-552`（`check_entry_point`） | entry point 恒为最大层的点 ⇒ `get_max_level_observed()` = 全局最大层 | §2.2 |
| `hnsw_rs v0.3.4` `hnsw.rs:809-812`（`get_max_level`） | 返回构造期授权的 `max_layer`（**≠** 实际观测最大层）⇒ **不可**用作循环上界 | §2.2 的「写法必须钉死」 |
| `hnsw_rs v0.3.4` `hnsw.rs:448-456`（`PointIndexation::new`） | `points_by_layer` 预分配 `max_layer` 个槽 ⇒ `get_layer_iterator(0)` 恒在合法下标 | §2.2 空图不 panic |
| `hnsw_rs v0.3.4` `hnsw.rs:485-490`（`debug_dump`） | 库自身用的就是 `for l in 0..=max_level_observed` | §2.2 先例 |
| `hnsw_rs v0.3.4` `src/layergenerator.rs` **不存在**（`LayerGenerator` 在别处） | ⚠️ 本文**未**核实 `LayerGenerator::generate` 的具体实现；`StdRng::from_os_rng()` 无 seed 是**既有结论**（架构 §8.2 / `p5-design.md`），本文沿用 | §4.6.2（ANN 不承诺逐位一致） |
| `fastembed v6.0.2` `src/text_embedding/init.rs:33` / `:67-69` | `InitOptions` 的线程参数**只有 `intra_threads`**（`with_intra_threads`）；**无 `sessions`** | §2.7 / D-S8-11 / Q3′ |
| `fastembed v6.0.2` `src/text_embedding/init.rs:163-170` | `TextEmbedding { tokenizer, pooling, session, need_token_type_ids, quantization, output_key }` ⇒ **每实例一份 `Session` + `Tokenizer`** | §2.7 / §4.11.1 |
| `fastembed v6.0.2` `src/text_embedding/impl.rs:453-456` | `embed(&mut self, …)` | §2.7 / §4.11.1（池化是唯一形态） |
| `fastembed v6.0.2` `src/common.rs:262-290` | `init_session_builder(eps, intra_threads)`：`None` ⇒ `available_parallelism()`；`Session::builder().with_intra_threads(threads)` | §2.7 / §4.11 |

⚠️ **本文未核实的项**（诚实标注，2026-09-17 更新）：① `LayerGenerator` 的层高生成实现细节 —— **仍未核实**（本文只依赖「每点恰在一层」这一条，已由 `generate_new_point` 的 push 语义直接给出）；② ~~`VectorRoute` 是否 `#[non_exhaustive]`（Q5）~~ ⇒ ✅ **已核实并结案**（`vector/mod.rs:37`，**无** `#[non_exhaustive]`；评审代核、本文独立复核，见 Q5）；③ ~~`Config` 是否有字面构造用法（Q8）~~ ⇒ ✅ **已核实并结案**（**无** `impl Default`；全仓唯一字面构造在 `config.rs:301` 的 `build_config`，属 crate 内部；评审代核、本文独立复核，见 Q8）。

---

## 附录 D：本文引用的项目内证据

| 主张 | 位置 |
| --- | --- |
| 今天读与写在类型层面不可能共存（stop-the-world 交接） | `crates/core/src/search/index.rs`（`SearchIndex.inner` 注释 / `into_searcher`）、`crates/core/src/search/searcher.rs`（`Searcher` / `into_index`） |
| 每点恰在一层、`get_layer_iterator(0)` 不是全量（实测 3.74%） | `docs/devel/v2-step5-design.md` §3.1（v0.2 的关键纠正）与附录 C 的探针读数 |
| 逐层遍历的正确写法 | `docs/devel/v2-step5-design.md` §4.2.2（本文的写法定稿依据） |
| R34 = 递归读同一把 `RwLock`，探针 3/3 复现 | `docs/devel/architecture-design.md` §14.3 R34 |
| R43 = `Mutex<TextEmbedding>` 把查询侧并发封顶；实测 1.88× / 1.63×，移出编码后 3.15× | 同上 §14.4 R43；`docs/devel/eval-report.md` §8.12 / §8.13 |
| R36：多 session 峰值 RSS **+96.4% / +289.2%**（**建库口径**，不可外推查询侧） | 同上 §14.4 R36；`docs/devel/eval-report.md` §8.10 |
| ADR-A 三条不变式（`FORMAT_VERSION` 保持 2 / manifest 唯一发布点 / 图 = 派生缓存） | 同上 ADR-A |
| manifest 绑单快照（`snapshot_crc` / `snapshot_len` / `nb_point`） | `crates/core/src/storage/graph.rs` 的 `GraphManifest` |
| `Index::remove` 物理摘 postings ⇒ 无过滤时 BM25 可传 `None` | `crates/core/src/index/mod.rs` 的 `Index::remove`；`crates/core/src/query/searcher.rs` 的 `bm25_f` |
| `allowed_count()` 必须是精确计数（硬契约） | `crates/core/src/predicate.rs` |
| `FieldIndex::rebuild(&docs)` 存在 | `crates/core/src/index/field_index.rs` |
| `Index::compacted(&IdRemap)` 重编号 + BM25 顺序不变的证明骨架 | `crates/core/src/index/mod.rs`；`docs/devel/v2-step4-design.md` |
| NFR-11 的 1.14 s/批是**推算**、不是直接计时 | `docs/devel/requirements-spec.md` NFR-11 行（S6-10 结案段）；`docs/devel/eval-report.md` §8.11 |
| 构建耗时里 **embed 占 93%** | `docs/devel/v2-step6-design.md` §1.1 |
| 「冻结图 + `--runs 1`」的 A/B 纪律（P5 锚点自带 ~0.6% 图漂移） | `docs/devel/v2-step7-design.md` §3.1；`docs/devel/plan-v2.md` §4「V2.1」区 |
| 计划要求 R49+ 由 `v2-step8-design.md` 起分配 | `docs/devel/architecture-design.md` §14 导读「V2.1 承接」段 |
| Step 8 的四个任务与三条开工前置 | `docs/devel/plan-v2.md` §4 Step 8 / §4.0 H4~H6；issue **#58** |
