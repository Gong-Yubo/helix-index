# 更新日志

本项目所有值得注意的变更都记录在此。
格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

> P0~P5 的条目为**逆向补写**（2026-09-03），依据各阶段设计文档与 git 历史整理；
> 此后每次变更即时追加。

## [Unreleased]

### V2 Step 2 · 实测与文档回写（S2-10 / S2-12，2026-09-06）

#### 12K 实测（`data/t2-corpus.jsonl`，release，macOS aarch64）

| 项 | 改造前（图不持久化） | 改造后 |
| --- | --- | --- |
| 快照加载（含位图/字段索引重建） | 53.4ms | 76.9ms |
| 图加载 | ——（无此路径） | **23.3ms** |
| **完整冷启动** | **≈11.6s** ❌ | **≈100ms** ✅（约 116×） |
| 图重建（降级路径） | 11.57s | 9.96s |
| 图 sidecar dump | —— | 43.5ms |
| 磁盘占用 | 52.3MB | 84.1MB（**1.6×**，graph 7.9 + data 23.7） |

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
- **`scripts/eval_perf.sh`**：NFR-04 改为三口径分别计时并**自动求和判定**
  （快照加载 + 图 sidecar 加载 = 完整冷启动 vs 2000ms），图重建单列为降级路径

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
  - ⚠️ T3 的意图（图路径与重建路径质量等价）已由 **T6 的 oracle 重合率断言**（≥0.95）
    与 `persist.rs` roundtrip 的逐位一致断言先行覆盖
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
| 磁盘占用 +60% | 图 sidecar 使 52.3MB → 84.1MB（1.6×），`hnsw_rs` 只支持全量 dump，省不掉 | V2 Step 5 compaction 时一并优化；当前用 `--no-graph-persist` 逃生舱 |
| paraphrase 桶融合 | hybrid 被弱路 BM25 稀释 | P6：自适应融合 / reranker 兜底 |
| 假负例敏感度 | 未做对照 | P6：仅 qrels 语料重建对照 |

## 下一版本（P6，v2）范围

Rerank / MMR 多样性 / 自研索引，以及上表四项残留。
任务清单见 `docs/devel/plan.md`。
