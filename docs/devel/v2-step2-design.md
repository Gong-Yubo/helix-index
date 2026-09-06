# HelixIndex V2 · Step 2 详细设计（图持久化 + 并行建图：冷启动与确定性）

> 面向 Agent 场景的通用检索引擎内核——V2 Step 2 的详细设计（**已实施，等合并**）。
> 本文件回答：HNSW 图怎么落盘、落在哪、和快照怎么对齐、坏了怎么办、怎么验证。
> **本文同时承载 ADR-A**（plan-v2 §4.0 H1：图持久化格式 + 多文件原子性），ADR-A 是 Step 2 与 Step 3 的共享前置。

| 项 | 内容 |
| --- | --- |
| 版本 | **v0.3（终审拍板，开工版）** |
| 日期 | 2026-09-06 |
| 状态 | **ADR-A 已拍板（D-S2-01~07 全部定稿）**；v0.2 的 7 决策已获外部评审同意；v0.3 补录开工前源码复核的两项新事实（见「开工前复核记录」）。<br>**2026-09-06 实施完成（S2-01~S2-12，守门全绿，等用户合并）** —— 实测见文末「实施结果（S2-10 实测）」 |
| 上游 | `plan-v2.md`（Step 2 / H1 / Q-P2 / Q-P3 / D7）、`requirements-spec.md` v1.5（FR-29 / FR-16 / NFR-04 / NFR-06）、`architecture-design.md` v1.5（§5.4 / §7.5 / §7.6 / R1~R18） |
| 范围 | T7-05 图持久化 + 并行建图；**ADR-A 定稿** |
| 非范围 | 原子快照的实施（Step 3，本文只定协议）、墓碑物理回收（Step 5）、embed 并行与增量构建（Step 4）、精排（Step 6） |

---

## 评审速读：7 个待拍板决策

> 评审者时间有限时，先看这张表。每条在 §6 有完整取舍与证据。
> **v0.2 更新**：外部评审（GLM）已对 7 项**全部表示同意**，同时提出 7 项 P0 修订——**全部已落实进本文**，处置明细见「评审修订记录」。

| # | 决策 | 我的建议 | 阻塞谁 |
| --- | --- | --- | --- |
| **D-S2-01** | **ADR-A 主体**：图落盘的布局与「多文件原子性」怎么解 | **方案 C**：sidecar 三件套 + **独立 manifest 作唯一原子发布点** + 校验失败降级重建；**`FORMAT_VERSION` 保持 2**（快照格式一个字节不动） | 阻塞 Step 2 全部开工；Step 3 直接沿用 |
| **D-S2-02** | 图 dump 会把向量**原样复制一份**（12K 约 +25MB），体积增加约 60%，接受吗 | **接受**（hnsw_rs 只暴露 `DumpMode::Full`，省不掉；内存不变、只涨磁盘） | 影响 S2-04 / S2-10 |
| **D-S2-03** | 持久化能力在类型系统上怎么表达（改 `VectorIndex` trait / 新增 trait / 门面层按 backend 分派） | **新增独立 trait `VectorGraphPersist`**，门面层按 `VectorBackend` 分派 | 影响 S2-03 / S2-04 |
| **D-S2-04** | 图出问题时的默认行为：**静默降级重建** / 警告后降级 / 直接报错 | **默认「警告后降级」**，提供 `strict` 选项；降级必须可观测（NFR-07） | 影响 S2-06 / 多个测试口径 |
| **D-S2-05** | 并行建图 `parallel_insert` 是否纳入本 Step | **纳入为可选开关**，默认**关**（保住与 P5 基线的可比性），先出实测数据再定默认值 | 影响 S2-11 |
| **D-S2-06** | 底层 `storage::load` 路径（bench）是否也接图持久化 | **接**（否则 10 万级评测仍要吃 11.57s 与 R-P5-13 抖动） | 影响 S2-05 / S2-09 工作量 |
| **D-S2-07** | 配置指纹是否扩展（`vector_backend` / `dist_id` / 平台标记） | **不扩**（扩了会打断「Brute 逃生舱加载 Hnsw 建库快照」这一现有能力），改为写进 manifest 做交叉校验 | 影响 S2-04 / S2-07 |

---

## 评审修订记录（v0.1 → v0.2）

外部评审（GLM）对 v0.1 提出 **7 项 P0 / 6 项 P1 / 若干 P2**。全部**逐条复核源码后**修订如下，**架构与 ADR-A 主体未动**：

| # | 评审意见 | 处置 | 落地位置 |
| --- | --- | --- | --- |
| **P0-1** | basename 双后缀 bug：`dump`/`load` 传 `"{name}.hnsw"` 会去找 `foo.idx.hnsw.hnsw.graph`。**属实且致命**——降级路径完全正常，所有「能加载」断言都会绿，NFR-04 静默失效 | **已修**：basename 一律传**快照文件名全名** `"foo.idx"`；新增断言 `GraphStatus::Loaded` 的前置 | §4.6、§5.3、附录 A、S2-T2 |
| **P0-2** | `graph_format` 实际是 **3** 不是 4（`hnswio.rs:1369`）。**属实** | **已修**：不硬编码，dump 后用公开 `load_description` 读回实际值 | §4.4、§5.3、§4.6 步骤 4.5 |
| **P0-3** | `save` 路径图失败语义未定义 | **已修**：写路径也按「图是缓存」处理——警告 + 不发布 manifest，strict 才 Err；R19 补 save 侧 panic 面 | §4.5、R19 |
| **P0-4** | `Box<dyn VectorIndex>` 触达不到 `dump_graph` | **已修**：`VectorIndex` 加默认方法 `as_graph_persist()`，对象安全、零破坏 | §5.2、附录 A |
| **P0-5** | `load_graph` 缺 `ef_search` 入参；且 `HnswRsIndex{..}` 字段对兄弟模块不可见 | **已修**：`load_graph(base, m, ef_search)` + `from_loaded` 构造器。已核实全 crate **无 `set_ef*`** | §5.3、附录 A |
| **P0-6** | `storage::save` 要带出 body_crc | **已修**：`save_with_crc` 返回 `Result<u32>`，保留 `save` | §4.5、S2-05、附录 A |
| **P0-7** | S2-T6 / 验收 5 的「逐位一致」在 R-P5-13 下写不死 | **已修**：改为「加载成功 + oracle Top-10 重合率 ≥ 0.95」；S2-T7（Brute）保留逐位 | §1.3 验收 5、S2-T6 |
| **P1-1** | 加 `Description` 预校验（步骤 4.5）+ C3 panic 清单不全 | **已修**：新增步骤 4.5；C3 证据补 `821/639/`read_exact().unwrap()`/from_utf8().unwrap()` | §4.6、C3、附录 B |
| **P1-2** | 去掉 `_io: Option<&'static mut HnswIo>` 字段（别名隐患） | **已修**：`Box::leak` 后丢弃句柄；R23 补「依赖 `HnswIo: Send+Sync`，升级需复核」 | §5.3、R23 |
| **P1-3** | 建图参数（M / ef_construction）变更检测不到 | **已修**：预校验时比对 `Description.max_nb_connection` / `ef`，无需新增 manifest 字段 | §4.6 步骤 4.5 |
| **P1-4** | manifest rename 后需 fsync 父目录 | **已修** | §4.5、S2-02 |
| **P1-5** | 测试计划缺 3 条 | **已修**：新增 S2-T15~T17；S2-T2 固定 ef_search | §8 |
| **P1-6** | S2-09（bench 接线）描述过粗 | **已修**：明确「图绑定的快照从哪来」两种路径的处置 | S2-09 |
| **P2** | 引用错位、行号偏差、`parallel_insert_slice`、日期 | 逐条处置见 §10.1 | §10.1 |

**未采纳 / 有分歧的点**（2 项，已在文中标注）：

- **C3「reload 侧文件打不开返回 Err」**：评审指出 panic 面只在「文件可打开但内容坏」。**接受并已修正措辞**，但结论不变——CRC 先验仍是唯一防线，因为「可打开但内容坏」正是崩溃残留的形态。
- **「文档头部日期 2026-09-06 疑似笔误」**：经核对**不是笔误**，本文确于 2026-09-06 撰写与修订。

---

## 开工前复核记录（v0.2 → v0.3，2026-09-06 实施前源码复核）

> 开工前对本机 `hnsw_rs-0.3.4` 源码做了最后一轮逐行复核，发现两项 v0.2 未覆盖的事实。
> 均不动摇 ADR-A 主体，但直接影响 S2-03 的实现细节，已同步进 §5.3 与 §3.2。

| # | 新事实 | 证据 | 对策（已落进实现任务） |
| --- | --- | --- | --- |
| **N1** | **`Description::dump` 无条件写 `MAGICDESCR_4`**（0x002a6779），尽管构造 `Description` 时 `format_version: 3`。⇒ `load_description` 读回的实际是 **4，不是 3**（v0.2/P0-2 引 `hnswio.rs:1369` 说「实测 3」也不准确——那是结构体字面量，落盘 magic 是 4） | `hnswio.rs:880`（`MAGICDESCR_4.to_ne_bytes()`）vs `hnswio.rs:1369`（字面量 3） | 方案本就是「dump 后 `load_description` 读回实际值、不硬编码」（P0-2），故无需改协议；此处仅修正文档记载。校验侧同样用读回值比对 |
| **N2** | **`load_hnsw` 构造的 `Hnsw` 无条件设 `datamap_opt: true`**（与是否真用 mmap 无关）。而 `file_dump` 在 `datamap_opt = true` 时 `overwrite = false` → **拒绝覆盖已存在的 `.hnsw.data`**，改为生成 `foo.idx-<rand>.hnsw.*` 随机后缀文件——「加载 → 增量 add → save」会**静默把图写到错误文件名**，manifest 记的还是旧名 ⇒ 下次加载 CRC 对不上（能降级但脏） | `hnswio.rs:601`（`datamap_opt: true`）、`api.rs:74`（`overwrite = !get_datamap_opt()`）、`hnswio.rs:165-186`（`DumpInit` unique_basename 循环） | **dump 前先删除旧的两个 sidecar 文件**（`foo.idx.hnsw.graph` / `.data`）。图是缓存、删除最坏降级重建，与 ADR-A 地基自洽；同时把「重 dump 必删旧文件」写进 `dump_graph` 实现契约。附带修正 v0.2 的一个不准确表述：0.3.4 默认 `ReloadOptions` **不开 mmap**（`hnswio.rs:96-104`），`load_hnsw` 把向量**读进内存**（`PointData::V`），P1-2 所述「S 指向 Mmap」只在显式 `set_mmap` 时成立——「leak 后丢弃句柄」的做法不变且更安全 |

---

## 1. 目标与验收

### 1.1 要解决的问题（plan-v2 §3 编号）

| ID | 问题 | 证据 | 严重度 |
| --- | --- | --- | --- |
| **Q-P2** | NFR-04 冷启动图重建 **11.57s**（12,000 条），完整冷启动 ≈ 11.6s 不满足秒级 | `eval-report.md` 8.3 | 中 |
| **Q-P3** | 图重建抖动 **R-P5-13**（NDCG 极差 0.0024/0.0023）：hnsw_rs 用 OS 熵建图，同一份向量两次建图拓扑不同 | `eval-report.md` 3.4 | 低（但影响评测可比性） |
| **Q-C3（部分）** | 图进入 `save()` 后，「快照」从单文件变成多文件，多文件之间无原子性 ⇒ 图新/快照旧的版本错配 | plan-v2 §4.0 H1 | 高（正确性） |

### 1.2 对应需求

| 编号 | 需求 | 本文落点 |
| --- | --- | --- |
| **FR-29** | 图持久化：HNSW 图落盘/加载，冷启动达秒级 | §5 全文 |
| **FR-16** | 索引二进制快照（V2 补图持久化与原子性） | §4 ADR-A |
| **NFR-04** | 冷启动：快照 + 图加载 < 2s | §1.3 验收 1、§5.8 口径 |
| **NFR-06** | 确定性：相同 query + 相同索引 → 完全相同结果 | §1.3 验收 2（口径**必须**改成「同快照两次加载」） |
| **D7** | p4-design/p5-design 的图持久化遗留项 | 本 Step 结案 |

### 1.3 验收标准（Step 2 完成的定义，可证伪）

| # | 验收 | 口径 / 判据 |
| --- | --- | --- |
| **1** | **NFR-04 达标** | 12K 语料（T2Ranking，`--single-chunk`）**完整冷启动** < 2s：快照加载 + 图加载 + 位图/字段索引重建，一并计时 |
| **2** | **消解 R-P5-13** | **同一快照连续加载两次**，固定 query 集（≥200 条）的 Top-10 结果**逐位一致**（chunk_id 与 score 全等）。<br>⚠️ 口径**不是**「两次建库结果一致」——`parallel_insert` 下该命题不成立（§3.2 C8） |
| **3** | **图是真源的派生缓存，可随时丢弃** | 删掉 manifest / 删掉图文件 / 篡改图文件任一字节 / 旧快照（无图）四种情况下，索引**仍可加载且检索结果可用**（降级重建） |
| **4** | **损坏的输入不 panic** | 图文件被截断到 1%、50%、99% 三种，加载**返回 Err 或降级**，**不得** panic（`hnsw_rs` 的 reload 路径有 12 处 `assert_eq!`/`unwrap()`，见 §3.2 C3） |
| **5** | **旧快照可加载** | `data/t2-index.snapshot`（`FORMAT_VERSION=2`、无图 sidecar）加载成功，`GraphStatus::Rebuilt`，且检索结果与暴力 oracle 的 Top-10 重合率 ≥ 0.95（S2-T3 口径）。<br>⚠️ **不能用「与改造前逐位一致」做判据**——旧快照无图 ⇒ 走重建 ⇒ 重建本身跨进程不确定（§2.2），逐位一致在物理上不成立（P0-7） |
| **6** | **不破坏现有逃生舱与正确性** | ① Brute 后端加载含图快照 → 忽略图，结果不变；② 纯 BM25 快照不写图；③ Step 1 的「删除后跨快照不复活」（T2）在图持久化后**仍绿** |
| **7** | **体积与耗时有实测记录** | 记录 12K 下的：快照体积增量、图 dump 耗时、图加载耗时（写入 `eval-report.md` 与 `eval_perf.sh`） |
| **8** | 守门全绿 | `make fmt && make lint && make test && make deny` + CI 全绿 |

---

## 2. 现状与问题定位

### 2.1 冷启动的 11.57s 花在哪

`SearchIndex::load_with`（`search/index.rs:330-349`）当前对每条 `raw_vectors` 逐条 `vi.add(id, NormalizedVector::new(v))`，即**全量重建 HNSW 图**：

```rust
VectorBackend::Hnsw => {
    let mut vi = HnswRsIndex::with_capacity(raw_vectors.len().max(1024));
    for (id, v) in &raw_vectors {          // ← 12,000 次 HNSW insert（含 ef_construction=300 的邻域搜索）
        vi.add(*id, NormalizedVector::new(v.clone()))?;
    }
    Box::new(vi) as Box<dyn VectorIndex>
}
```

 bench 的底层路径同样如此（`cli/src/bench.rs:631-647`），并已明确标注「跨进程不确定 R-P5-13」。

### 2.2 R-P5-13 的机理，以及为什么验收口径必须改

`Hnsw::insert` 内部用 OS 熵做层采样（`hnsw_rs` 的 `level_scale` 随机数），且邻域更新与**插入顺序**相关。⇒ 同一份向量、同一份代码，两次建图拓扑不同 ⇒ 近似检索的 Top-K 有微小差异 ⇒ 评测 NDCG 极差 0.0024。

图持久化后：

- 「同快照两次**加载**」→ 同一张图 → **逐位一致**（命题成立，这就是验收 2）
- 「两次**建库**」→ 若用 `parallel_insert`，插入顺序不确定 ⇒ 拓扑不确定（**命题不成立**）

> 这条是 plan-v2 已经点明的（§4 Step 2 验收栏），本文把它落成硬口径，并在 §5.7 说明 `parallel_insert` 与 NFR-06 的关系。

### 2.3 现有快照边界（D1）

`storage/mod.rs` 的模块文档写明：**「不序列化 HNSW 图」**。快照正文 = `SnapshotSections` + `vectors` + `fingerprint`，文件 = 12B header（magic `IDX1` / version / crc32）+ 正文（`storage/codec.rs:5-9`）。

`FORMAT_VERSION = 2`，`positions` feature 下 `effective_version() = 3`（`codec.rs:24-43`）。V2 Step 1 刻意**不升版**（架构 §7.6.1），代价是新增结构不入快照。**本文推荐方案延续这个选择**（见 D-S2-01）。

---

## 3. 关键调研：`hnsw_rs-0.3.4` 的 dump / reload 真实语义

> 全部结论来自本机 crate 源码（`~/.cargo/registry/src/index.crates.io-*/hnsw_rs-0.3.4/`），行号见附录 B。
> **这一章是本设计的事实基础**——其中 C3/C4/C6 三条是 plan-v2 §4.0 H1 尚未覆盖的新发现，直接决定了 ADR-A 的形态。

### 3.1 对外可用接口清单

| 能力 | 签名 | 位置 |
| --- | --- | --- |
| 落盘（**唯一公开入口**） | `AnnT::file_dump(&self, path: &Path, file_basename: &str) -> anyhow::Result<String>` | `api.rs:37,70` |
| 读回 | `HnswIo::new(dir, basename) -> HnswIo` / `load_hnsw::<T,D>() -> anyhow::Result<Hnsw<'b,T,D>>` | `hnswio.rs:319,433` |
| 读回（显式给距离） | `HnswIo::load_hnsw_with_dist(&self, f: D)` | `hnswio.rs:533` |
| 只读描述头 | `load_description(io_in: &mut dyn Read) -> Result<Description>` | `hnswio.rs:939` |
| 并行插入 | `Hnsw::parallel_insert(&self, datas: &Vec<(&Vec<T>, usize)>)` | `hnsw.rs:1212` |
| 并行插入（切片版，**推荐**） | `Hnsw::parallel_insert_slice(&self, datas: &Vec<(&[T], usize)>)` | `hnsw.rs:1224` |
| 串行插入 | `Hnsw::insert(&self, (&[T], usize))` | `hnsw.rs:1060` |

`prelude.rs` 把 `api::*` / `hnsw::*` / `hnswio::*` 全部重导出，故 `use hnsw_rs::prelude::*` 即可。

### 3.2 八条硬结论（写进设计约束）

| # | 结论 | 证据 | 对设计的影响 |
| --- | --- | --- | --- |
| **C1** | 落盘是**双文件**：`{basename}.hnsw.graph`（拓扑）+ `{basename}.hnsw.data`（向量）。且 `DumpInit::new` 以 `create(true).truncate(true)` 打开——**dump 过程非原子**，中途崩溃必留残缺文件 | `hnswio.rs:194-224`、`api.rs:70-93` | 不能把「dump 成功」当作「文件完整」；**必须**有独立的完整性校验与发布点（→ ADR-A） |
| **C2** | **只能 Full dump**。`HnswIoT::dump`（可选 `DumpMode::Light`）是 `pub(crate)`，外部 crate **无法调用**；公开的 `file_dump` 内部硬编码 `DumpMode::Full` | `hnswio.rs:77`、`api.rs:81` | 「只落拓扑、不落向量以省空间」**在 0.3.4 上不可行**；体积增加是硬成本（→ D-S2-02） |
| **C3** | **reload 路径大量 `assert_eq!` / `unwrap()` / `process::exit(1)`**，损坏输入会 **panic 或强杀进程**，而不是返回 `Err`。<br>⚠️ **精确边界（评审修正）**：**文件打不开 → 干净 `Err`**（`hnswio.rs:377-385,394-402`）；panic 面落在「**文件能打开但内容坏**」 | reload 侧：`418`（`load_description().unwrap()`）、`456,464,555,563`（assert_eq）、`639`（`panic!`）、`705,710`、`1137,1146`（assert）、`764`（越界索引）、`821`（`exit(1)`）、`1172`（`exit(1)`）、`1228-1276`（9 处 `read_exact().unwrap()`）、`1024,1038`（`from_utf8().unwrap()`）；<br>dump 侧：`208,226`（`panic_any`，**写路径同样暴露**） | 「可打开但内容坏」正是崩溃残留的形态 ⇒ **绝不能把未校验的文件交给 `hnsw_rs`**。CRC 先验是**正确性底线**（→ §4.5、§4.6 步骤 4.5，验收 4）。写路径的 panic 面见 P0-3 / R19 |
| **C4** | 数据文件是**裸 `f32` 原生字节 + 原生端序**（`from_raw_parts` 直写直读，无编码无对齐保证），所有标量用 `to_ne_bytes` | `hnswio.rs:1104-1114`（写）、`1160-1167`（读） | 图 sidecar **不可跨平台/跨端序搬运**；必须在 manifest 里记平台指纹并校验（→ §4.4） |
| **C5** | 图自带**自描述头** `Description`：`format_version / dumpmode / max_nb_connection / level_scale / nb_layer / ef(=ef_construction) / nb_point / dimension / distname / t_name` | `hnswio.rs:848-869` | 拿到了**免费的第二道校验**（dim、nb_point、distname）；但 **`ef_search` 不在图里**，加载后必须回填（→ §5.4） |
| **C6** | `load_hnsw*` 的生命周期是 `fn load_hnsw<'b,'a>(&'a mut self) -> Result<Hnsw<'b,T,D>> where 'a: 'b` ⇒ **`HnswIo` 必须活得比 `Hnsw` 长** | `hnswio.rs:433-438,533-538` | 而我们存的是 `Hnsw<'static, f32, DistDotClamped>`（`vector/hnsw_rs_index.rs:53`）⇒ 只有 `Box::leak` 一条路（→ §5.4，风险 R23） |
| **C7** | **距离被写进图**：`distname = type_name::<D>()`（完整 Rust 类型路径）。`load_hnsw_with_dist` 比对**完整路径**，`load_hnsw` 只比对**最后一段**（短名） | `hnswio.rs:580`（全路径）、`482`（短名）、`1378`+`hnsw.rs:834-836`（写入） | 我们的私有类型路径 `helix_core::vector::hnsw_rs_index::DistDotClamped` **被烧进文件**；改名即破坏兼容。⇒ 优先 `load_hnsw`（短名比对，容忍模块移动）+ manifest 里记**自有的** `dist_id`（→ §4.4，风险 R20） |
| **C8** | `parallel_insert` 走 `rayon::par_iter` ⇒ 插入顺序不确定 ⇒ **拓扑不可复现**；且重载后的 `Hnsw` 是 `extend_candidates: true`，而 `Hnsw::new` 是 `false` | `hnsw.rs:1212-1218`、`hnswio.rs:512,601` vs `hnsw.rs:776` | ① 验收口径改「同快照两次加载」（§2.2）；② 加载后继续增量插入，建图参数与纯内存建库**不同**（→ 风险 R24） |

> **C3 值得单独强调**：`load_hnsw` 的第一句就是 `let init = self.init()`，而 `init()` 里 `load_description(&mut graph_in).unwrap()`（`hnswio.rs:418`）。一个被截断的 `.hnsw.graph` 会让整个进程 panic。
> 项目现有原则「**不得静默读错**」（`crates/core/src/storage/mod.rs:3-5`，模块文档原文）在 `hnsw_rs` 这里**不由它自己保证**，只能由我们在调用前保证。
> 补充一个好消息（评审指出、已复核）：**文件不存在/打不开时 `HnswIo::init` 返回的是干净的 `Err`**（`hnswio.rs:377-385`），不是 panic——所以「图文件缺失」这一常见场景无需 CRC 兜底，CRC 专门防「文件在但内容坏」。

### 3.3 与 D-J3（原子 rename）的关系

plan-v2 §2.2 D-J3 定的是「崩溃恢复方案 = **原子 rename**」。引入图之后，「rename」要 rename 什么就成了一个真问题：

- 若 rename 三个文件（graph/data/snapshot），三者之间仍有时间窗；
- 若引入目录 + 代际指针，则 `save(path)` 的对外语义从「一个文件」变成「一个目录」——**破坏性变更**，CLI、bench、`data/` 现有快照全受影响。

ADR-A 要回答的正是这个。见 §4。

---

## 4. ADR-A：图持久化格式与多文件原子性（plan-v2 H1 的回答）

### 4.1 决策背景

H1 原文给了两个候选：① 图并入快照 blob（`FORMAT_VERSION` 升版）；② sidecar + manifest/generation 目录级原子替换。
源码调研后，我认为**两个都有硬伤**，并提出第三个方案：

| 方案 | 做法 | 硬伤 |
| --- | --- | --- |
| **A. 并入快照 blob** | 把两个图文件塞进 `Snapshot` 新字段，`FORMAT_VERSION: 2 → 3` | ① `HnswIo` 只能**从文件路径**加载（`load_hnsw` 内部 `File::open`，无 "from reader" 公开 API）⇒ 加载时得**先写回临时文件**再读，引入临时目录生命周期与清理问题；② `FORMAT_VERSION` 升版 ⇒ **现有快照全部失效**（架构 §7.6.1 的「旧快照可加载」承诺作废，`data/t2-index.snapshot` 需重建 ~226s）；③ save 时内存峰值多一份 ~30MB |
| **B. sidecar + 代际目录** | `foo.idx/` 目录下 `gen-N/{index.idx, *.hnsw.*}`，`CURRENT` 指针原子切换 | ① `save(path)` 语义从文件变目录，**对外破坏性变更**；② 磁盘占用随代际数增长，需要 GC 策略（Step 2 阶段无谓地引入运维语义） |
| **C. sidecar + 单原子 manifest（推荐）** | 图文件与快照同目录平铺；**唯一原子发布点是 `*.hnsw.manifest`**（小文件，`tmp` + `fsync` + `rename`）；manifest 记录父快照 CRC 与两个图文件的 CRC | ① 磁盘上多 3 个文件（可接受的观感代价）；② 崩溃后若 manifest 未发布，图被判为「过期」→ **降级重建**（慢但正确） |

### 4.2 方案 C 的核心论点：**图是快照的派生缓存，不是真源**

这是 ADR-A 的结论，也是整份设计的地基：

```
快照 foo.idx  ──CRC32──►  图 sidecar 的「父版本标识」
                              │
                              ├─ 一致 → 图可用（快路径）
                              └─ 不一致 / 缺失 / 损坏 → 丢弃图，从 raw_vectors 重建（慢路径，功能不丢）
```

由此得到三条推论：

1. **多文件原子性问题被消解**：不需要「三个文件同时 rename」。只要 manifest 是原子发布的，且 manifest 里带着三个文件的 CRC，**「要么全对、要么全不算」**这个语义就成立了。
2. **图可以随时删**：运维/回滚/磁盘紧张时删掉三个 sidecar 文件不会损坏索引，只是下一次冷启动慢。
3. **Step 2 与 Step 3 解耦**：Step 3（T7-13 原子快照）只需让 `foo.idx` 自己 tmp+rename；图 sidecar 沿用本机制自动跟随，无需再动协议。

> 这条论点同时把 Q-C3 的风险从「正确性事故」降级为「性能退化」——这正是「缓存」该有的失败模式。

### 4.3 文件布局与命名

```
foo.idx                      快照（真源，格式一个字节不变，FORMAT_VERSION 仍为 2）
foo.idx.hnsw.graph     图拓扑   （hnsw_rs 原生格式，派生）
foo.idx.hnsw.data      图数据   （hnsw_rs 原生格式，含向量副本，派生）
foo.idx.hnsw.manifest  清单     （自有格式，**唯一原子发布点**）
```

命名由 `hnsw_rs` 的规则决定：`file_dump(dir, basename)` 产出 `dir/basename.hnsw.graph` 与 `dir/basename.hnsw.data`（`hnswio.rs:194-206`；reload 侧同规则 `373-375`）。我们取 `dir = 快照所在目录`、`basename = 快照文件名全名`（即 `"foo.idx"`，**不再追加任何后缀**），于是 `foo.idx` 自然得到上面这套名字——**无需任何路径转换规则，也不会与 `.idx` 后缀冲突**。

> ⚠️ **这是全文最容易写错的一处**（P0-1）：后缀 `.hnsw.graph` 由 `hnsw_rs` 追加，**我们再拼一次就会得到 `foo.idx.hnsw.hnsw.graph`**。这个 bug 的阴险之处是——降级路径完全正常，所有「能加载」的断言都会绿，只有 NFR-04 静默失效。⇒ 统一走 `graph_basename()` 一个函数，禁止各处手拼。

**体积估算（12K / 512 维，供评审判断量级，实际以 S2-T14 实测为准）**：

| 文件 | 估算公式 | 估算值 |
| --- | --- | --- |
| `.hnsw.data` | `nb_point × (4B magic + 8B origin_id + 8B len + dim×4B)` | 12,000 × 2,068B ≈ **24.8 MB** |
| `.hnsw.graph` | `nb_point × (17B 点头 + 16 层 × 8B 层头 + ~32 邻居 × 17B)` | 12,000 × ~690B ≈ **8.3 MB** |
| `.hnsw.manifest` | 定长 + 若干短字符串 | **< 1 KB** |

即快照 52.3MB → 约 85MB（**+63%**）。内存侧不受影响：现在本来就是 `raw_vectors`（24.6MB）+ 图内向量（24.6MB）两份，图持久化后仍是两份。

### 4.4 manifest 结构（`storage/graph.rs`，新增）

```rust
/// 图 sidecar 清单。**唯一原子发布点**：只有它落地的那一刻，图才算"发布"。
pub struct GraphManifest {
    // ── 自我描述 ──
    pub manifest_version: u32,   // 清单格式版本，初始 1
    pub producer: String,        // 生产者标识，如 "helix-core-0.3.0"（诊断用，不参与校验）

    // ── 与内核实现的绑定（校验失败即降级）──
    // P0-2：dump 实际写的是 3（hnswio.rs:1369），**不要硬编码**——
    //       由 dump 结束后用公开 load_description 读回真实值填入（§5.3）
    pub graph_format: u32,       // = Description.format_version（读回值，非字面量）
    pub dist_id: String,         // **自有**距离标识 "dot-clamped-v1"，与 Rust 类型路径解耦（见 C7）
    pub platform: u32,           // 指针宽度 + 端序指纹（见 C4）：0x01 = LE/64bit

    // ── 建图参数（P1-3：不在指纹里，改常量后旧图会静默错配）──
    pub max_nb_connection: u32,  // = Description.max_nb_connection（内核常量 M=32）
    pub ef_construction: u32,    // = Description.ef（内核常量 EF_CONSTRUCTION=300）

    // ── 与快照的绑定（核心）──
    pub snapshot_crc: u32,       // 父快照正文的 CRC32 —— 图的"版本锚点"
    pub snapshot_len: u64,       // 父快照正文长度（防同 CRC 不同长度的极端碰撞）
    pub dim: u32,                // 必须等于 ConfigFingerprint.dim
    pub nb_point: u64,           // 图中点数（**含墓碑**，故 >= 快照 vectors 条数，见 §4.6）

    // ── 两个图文件的完整性 ──
    pub graph_crc: u32, pub graph_len: u64,
    pub data_crc:  u32, pub data_len:  u64,
}
```

文件编码复用现有约定：12B header（`MAGIC_GMAN` + `manifest_version` + crc32）+ bincode 正文，与 `storage/codec.rs` 同构，写临时文件 → `flush` → `sync_all` → `rename` → **fsync 父目录**（P1-4）。

**为什么 `snapshot_crc` 是版本锚点而不是「generation 号」**：CRC 天然覆盖「内容」，generation 只覆盖「次数」。`save()` 写出新快照后旧图立即失效（因为 CRC 变了），不需要维护计数器，也不需要 GC。

### 4.5 写入序列（`SearchIndex::save`）

```
1. commit()/flush()                       ← 现状
2. 写快照正文 → foo.idx，取回 body_crc     ← 现状（Step 3 才改成 tmp+rename；Step 2 不动）
   body_crc 由 storage::save_with_crc 返回（P0-6，见 S2-05 / 附录 A）
3. 若无向量（纯 BM25）或 backend != Hnsw  → 删掉可能存在的旧 sidecar 三件套，结束
4. AnnT::file_dump(dir, "foo.idx")        → 覆盖写 foo.idx.hnsw.graph / foo.idx.hnsw.data
                                            （C1：非原子；C3 dump 侧 panic_any 在 208/226）
   ── 失败处理（P0-3）：见下方「写路径失败语义」──
5. 读回两个文件算 CRC32 + 长度（流式，不整读进内存）
6. 用 load_description 读回 graph_format / max_nb_connection / ef_construction（P0-2 / P1-3）
7. 组装 manifest → 写 foo.idx.hnsw.manifest.tmp → flush → sync_all → rename → **fsync 父目录**
                                            ← 原子发布点（P1-4 补目录 fsync）
```

**写路径失败语义（P0-3，评审指出的缺口）**：图是**缓存**，所以 **`save` 不得因图失败而失败**。步骤 4 的 `file_dump` 失败（磁盘满 / 目录不可写 / `panic_any`）时：

| 模式 | 行为 |
| --- | --- |
| 默认 | 输出警告（含原因）→ **不发布 manifest**（若已发布则删除之）→ `save` **正常返回 Ok**。快照本身是完好的，下一次冷启动降级重建 |
| `strict` | 返回 `Err(Error::VectorGraph(..))` |

> ⚠️ 前置检查（目录存在且可写）**只能缩小不能归零** panic 面：C3 的 `panic_any`（`hnswio.rs:208/226`）是 TOCTOU 窗口内的真实行为，且 `panic_any` 不是 `Err`，无法在调用侧 catch。⇒ R19 覆盖写路径。

**为什么图文件本身不 fsync**：30MB 的 `sync_all` 在冷建库路径上不便宜，且**不 fsync 也不会产生错误行为**——最坏情况是崩溃后 CRC 不匹配 → 降级重建。manifest 是唯一必须 fsync 的文件（它很小，且它是发布点）。

**为什么 manifest rename 后还要 fsync 父目录（P1-4）**：只 fsync 文件不 fsync 目录，掉电后 rename 本身可能不持久。最坏结果仍是降级重建（与缓存哲学一致），但成本近乎为零，且 **S2-02 的这段原子写机制正是 Step 3 原子快照要复用的**，在这里做对比后面补更容易。

**为什么图文件不做 tmp+rename**：`DumpInit` 的文件名由 basename 拼接而成（会产生 `foo.idx.hnsw.tmp.hnsw.graph` 这种双后缀），rename 两步反而增加中间状态；而 manifest 已经承担了「发布」语义，图文件是否原地覆盖无所谓。

### 4.6 读取序列（`SearchIndex::load_with`）

```
1. (index, vectors, fingerprint, body_crc) = storage::load(path)
2. 跳过条件（任一成立即不碰图）：backend == Brute | vectors 为空 | cfg.embedder 为 None
   （后两者当前已被指纹校验挡住，纯防御性；P2）
3. 读 manifest：文件不存在 / header 错 / CRC 错 / 解码错 / manifest_version 未知  → 降级①
4. 逐项校验，任一不符 → 降级①：
   manifest_version 已知 | graph_format 已知 | dist_id 匹配 | platform 匹配
   dim == fingerprint.dim | snapshot_crc == body_crc | snapshot_len 匹配
   两个图文件的 len 匹配 且 CRC 匹配
4.5 【P1-1】Description 预校验（在把文件交给满是 unwrap 的 load_hnsw 之前）：
    用公开 load_description 读 .hnsw.graph 的头，与 manifest / 内核常量逐项比对：
      format_version == manifest.graph_format
      dimension      == fingerprint.dim
      nb_point       == manifest.nb_point
      distname 短名  == 预期（"DistDotClamped"）
      t_name         == "f32"
      max_nb_connection == M(32)   ← P1-3：改常量后旧图不会静默错配
      ef             == EF_CONSTRUCTION(300)
    任一不符 → 降级①
    （load_description 对截断文件安全：read_exact 全部 `?` 传播；
      仅 distname/t_name 的 from_utf8().unwrap() 在 1024/1038，且 len>256 已先行拒绝）
5. 校验通过 → HnswIo::new(dir, "foo.idx")       ← ⚠️ P0-1：basename 是**快照文件名全名**，
                                                    hnsw_rs 自己追加 ".hnsw.graph"/".hnsw.data"。
                                                    传 "foo.idx.hnsw" 会去找
                                                    foo.idx.hnsw.hnsw.graph ⇒ 必然降级，
                                                    且所有「能加载」断言照样绿（NFR-04 静默失效）
              .load_hnsw::<f32, DistDotClamped>()
6. 加载后补校验：nb_point >= vectors.len()（含墓碑，见下），否则降级①
7. 回填 ef_search（C5：图里没有它；由 load_graph 的入参带入，见 §5.3）
降级①：从 raw_vectors 全量重建（现状路径）+ 按 D-S2-04 的策略报告
```

> **basename 规则的唯一真相**：`file_dump(dir, basename)` 与 `HnswIo::new(dir, basename)` **同规则**拼接 —— `basename + ".hnsw.graph"` / `basename + ".hnsw.data"`（dump 侧 `hnswio.rs:194-206,211-222`，reload 侧 `hnswio.rs:373-375,394-395`）。要得到 `foo.idx.hnsw.graph`，**传入的 basename 必须是 `"foo.idx"`**。

> **`nb_point >= vectors.len()` 而不是 `==`**：Step 1 之后 `remove()` 会摘掉 `raw_vectors` 里的条目，但 `hnsw_rs` 无 remove API，图里的墓碑向量摘不掉（`architecture-design.md` §7.5.1）。所以「图中点数」恒 ≥ 「原始向量条数」。
> 已核实**不会出现 chunk_id 复用**：`ForwardIndex::insert_chunk` 是 `id = chunks.len()` 的 append-only 分配（`index/forward.rs:49-61`），墓碑位不被复用 ⇒ 图里的墓碑 id 与新插入 id **不可能相撞**。这是本方案能成立的一个关键前提，值得在评审时确认。

### 4.7 兼容性与降级矩阵

| 场景 | 期望行为 | 由谁拦下 |
| --- | --- | --- |
| 旧快照（`FORMAT_VERSION=2`，无 sidecar） | 加载成功，重建图，结果与改造前一致 | manifest 缺失 → 降级① |
| 图被删 / manifest 被删 | 同上 | 降级① |
| 图文件被截断（崩溃残留） | 不 panic，降级重建 | 步骤 4 的 len+CRC 校验（**C3 的关键防线**） |
| 快照更新了但图没更新（save 中途崩） | 降级重建 | `snapshot_crc` 不匹配 |
| 图更新了但快照没更新 | 降级重建 | 同上（对称） |
| `dim` 与指纹不符（换模型后残留旧图） | 降级重建 | `dim` 校验 |
| 图换平台搬运（LE→BE / 32→64bit） | 降级重建 | `platform` 校验 |
| `hnsw_rs` 升级导致 `graph_format` 变化 | 降级重建（**不影响可用性**） | `graph_format` 校验 |
| **建图常量变更后残留旧图**（M / ef_construction，P1-3） | 降级重建（否则「加载图 + 增量插入」与「全新建库」参数分叉） | 步骤 4.5 比对 `max_nb_connection` / `ef` |
| manifest 格式未知（未来版本写的） | 降级重建 | `manifest_version` 校验（S2-T15） |
| 距离类型改名 / 语义变更 | 降级重建（`dist_id` 由我们显式升版） | `dist_id` 校验 |

**这张矩阵就是「图是缓存」这一论点的兑现**：没有任何一行是「索引不可用」。

---

## 5. 详细设计

### 5.1 模块落点

| 新增/改动 | 职责 | 备注 |
| --- | --- | --- |
| **`storage/graph.rs`（新增）** | `GraphManifest` 定义 + bincode 编解码 + CRC + 原子写（tmp/fsync/rename）+ 读与校验 | 依赖 `crc32fast`（已在用）、`bincode`；**不依赖** `hnsw_rs`，便于单测 |
| **`vector/persist.rs`（新增）** | `VectorGraphPersist` trait（D-S2-03）+ `HnswRsIndex` 的 dump/load 实现（含 `Box::leak`、`ef_search` 回填） | 唯一 import `hnsw_rs` 持久化 API 的地方 |
| `vector/mod.rs` | 重导出 `VectorGraphPersist`；`VectorIndex` 新增 `add_batch`（D-S2-05）与 **`as_graph_persist`**（P0-4） | 两者都有默认实现，trait 主线**不改**（见 D-S2-03） |
| `storage/snapshot.rs` | 新增 `load_with_crc`（读路径带出 CRC）+ **`save_with_crc` 返回正文 CRC**（P0-6，写路径） | 供 manifest 的 `snapshot_crc` 填写与校验 |
| `search/index.rs` | `save` 写图 + 写 manifest；`load_with` 校验 + 降级 | 门面层唯一接线点 |
| `error.rs` | 新增 `VectorGraph(String)` / `GraphStale`（降级原因，可观测） | 见 §5.9 |
| `cli/src/main.rs` | build/search 打印图体积 / dump 耗时 / 加载耗时；新增 `--no-graph-persist` 逃生舱 | |
| `cli/src/bench.rs` | 底层路径接图持久化（D-S2-06） | |

### 5.2 `VectorGraphPersist`：持久化能力怎么表达（D-S2-03）

`VectorIndex` 是对象安全 trait（`Box<dyn VectorIndex>`，`search/index.rs:23`）。持久化两边不对称：

- dump 是 `&self` 方法 → **对象安全**
- load 是关联函数（无 `self`）→ **不对象安全**

三个候选：

| 候选 | 做法 | 评价 |
| --- | --- | --- |
| (a) 给 `VectorIndex` 加 `dump`/`load` | load 加 `where Self: Sized` 会让 trait 整体不可对象安全 | ❌ 直接否（`Box<dyn VectorIndex>` 是 `Inner` 的字段类型） |
| (b) 新增独立 trait（**推荐**） | `trait VectorGraphPersist: VectorIndex { fn dump_graph(...)->Result<GraphStats>; fn load_graph(...) -> Result<Self> where Self: Sized; }` | ✅ 主 trait 零改动；`BruteForceIndex` 不实现它（无图可持久）⇒ 类型系统就表达了「Brute 无图」，比运行时 backend 判断更直白 |
| (c) 不加 trait，门面层 match backend | 与 `load_with` 现有的 `match backend` 一致，改动最小 | ⚠️ 简单但把「哪些后端可持久化」这个知识放在门面层，新增后端时易漏 |

推荐 **(b)**：多一个 trait 换来「能力在类型上，不在 if 里」。门面层仍用 `match backend` 做**分派**（因为要构造具体类型），但**能力声明**归 trait。

**P0-4：门面层怎么触达它（`Box<dyn VectorIndex>` 的下转问题）**

`Inner.vector_index` 是 `Option<Box<dyn VectorIndex>>`（`search/index.rs:23`），trait 对象**不能跨 trait 转换**，`match backend` 也拿不到 `Box` 里的具体类型。评审指出这个分派路径在 v0.1 里缺失——属实。解法是给 `VectorIndex` 加一个**默认方法做显式下转**：

```rust
pub trait VectorIndex: Send + Sync {
    // …现有方法不变…

    /// 若该后端支持图持久化，返回其 `VectorGraphPersist` 视图；否则 `None`。
    /// 默认 `None` ⇒ 「Brute 无图」仍是**类型事实**，不是运行时 if。
    fn as_graph_persist(&self) -> Option<&dyn VectorGraphPersist> { None }
}

impl VectorGraphPersist for HnswRsIndex {
    fn as_graph_persist(&self) -> Option<&dyn VectorGraphPersist> { Some(self) }
}
```

- **对象安全**（`&self`、返回具体 sized 类型 `Option<&dyn …>`）⇒ 不破坏 `Box<dyn VectorIndex>`；
- **零破坏**：默认实现返回 `None`，现有两个实现都不用改；
- 与 (b) 的立意完全自洽：能力声明仍在 trait 上，门面层只是「问一句」。

### 5.3 `HnswRsIndex` 的 dump/load 实现要点

**basename 的唯一正确写法（P0-1，评审抓到的双后缀 bug）—— dump 与 load 共用**

```rust
let dir      = base.parent().unwrap_or(Path::new("."));
let basename = base.file_name()...to_string_lossy().to_string();   // "foo.idx"
// ⚠️ 不要再拼 ".hnsw"！hnsw_rs 自己会追加：
//    file_dump(dir, "foo.idx") → foo.idx.hnsw.graph / foo.idx.hnsw.data
//    传 "foo.idx.hnsw" → foo.idx.hnsw.hnsw.graph（找不到 ⇒ 永远降级，且测试不易察觉）
```

**dump（`S2-03`）**

```rust
fn dump_graph(&self, base: &Path) -> Result<GraphStats> {
    let dir      = base.parent().unwrap_or(Path::new("."));
    let basename = base.file_name()...to_string_lossy();        // "foo.idx"（不追加后缀）
    // N2（开工前复核）：load_hnsw 构造的 Hnsw 无条件 datamap_opt=true，
    // file_dump 因此拒绝覆盖旧 .hnsw.data、改写随机后缀文件名 ——
    // 「加载→增量 add→save」会静默把图写错位置。对策：dump 前先删旧 sidecar 两文件
    //（图是缓存，删除最坏降级重建，与 ADR-A 地基自洽）。
    remove_old_sidecars(dir, &basename);
    // 前置：目录存在且可写（DumpInit::new 打不开文件会 panic_any，C3 → R19 写路径）
    hnsw.file_dump(dir, &basename).map_err(|e| Error::VectorGraph(e.to_string()))?;
    // P0-2 / N1：graph_format **不硬编码** —— dump 后用公开 load_description 读回真实值
    //（落盘 magic 是 MAGICDESCR_4，读回 4；不要信文档里任何字面量）
    let d = load_description(&mut File::open(graph_path(dir, &basename))?)
        .map_err(|e| Error::VectorGraph(e.to_string()))?;
    Ok(GraphStats {
        nb_point: self.len() as u64,
        dim: self.dim() as u32,
        graph_format: d.format_version as u32,              // 读回值（实测 4，N1）
        max_nb_connection: d.max_nb_connection as u32,      // P1-3
        ef_construction: d.ef as u32,                       // P1-3
    })
}
```

**load（`S2-03`）**

```rust
fn load_graph(base: &Path, m: &GraphManifest, ef_search: usize) -> Result<Self> {
    let dir      = base.parent().unwrap_or(Path::new("."));
    let basename = base.file_name()...to_string_lossy();     // 同上，**不加 .hnsw**
    // 1) 生命周期（C6）：HnswIo 必须活得比 Hnsw 长，而我们要 Hnsw<'static>
    //    P1-2：leak 后**直接丢弃句柄**，不存进结构体（见下方说明）
    let io: &'static mut HnswIo = Box::leak(Box::new(HnswIo::new(dir, &basename)));
    // 2) 用 load_hnsw 而非 load_hnsw_with_dist：前者只比对**短名**（C7），
    //    模块移动不会让旧图失效；DistDotClamped 已 #[derive(Default)]，满足约束
    let hnsw: Hnsw<'static, f32, DistDotClamped> = io.load_hnsw()...;
    // 3) ef_search 是**入参**（P0-5）：全 crate 无 set_ef*，只能由 HnswRsIndex 字段承载
    Ok(HnswRsIndex::from_loaded(hnsw, ef_search))            // 见下
}
```

四个必须写进注释的坑：

1. **`Box::leak` 是有意为之**，不是疏忽：每次加载泄漏一个 `HnswIo`（约 200B + 路径串）。CLI/库典型用法是「一进程一加载」，量级可忽略；备选方案（给 `HnswRsIndex` 加生命周期参数）会顺着 `Inner` → `Box<dyn VectorIndex>` → `Searcher` 全链路扩散，且 `'static` 约束下最终仍要 leak。
   **P1-2：leak 后丢弃句柄，不在结构体里存 `_io: Option<&'static mut HnswIo>`**。理由：重载后的 `Hnsw<'static>` 内部 `PointData::S(&[T])` **已经共享指向 `HnswIo` 内部的 Mmap**（`hnswio.rs:828-829`），再同时持有一个「永不解引用」的 `&'static mut` 是裸露的别名隐患（`&mut` 的唯一性与只读借用并存）。内存占用完全一样——`Hnsw<'static>` 的借用已保证其存活；将来真要回收，重建 `HnswIo` 即可。
   ⚠️ 附带前提：**`HnswIo: Send + Sync`**（字段为 `PathBuf/String/ReloadOptions/Option<DataMap>/Arc<AtomicUsize>/bool`，`DataMap` 内含 `mmap_rs::Mmap`，后者有 `unsafe impl Send/Sync`）。**S2-03 必须加编译期断言**：
   ```rust
   const _: () = { fn assert_send_sync<T: Send + Sync>() {} fn _a() { assert_send_sync::<HnswIo>(); } };
   ```
   `hnsw_rs` 升级时需复核此条（否则 `VectorIndex: Send + Sync` 会编译失败）。
2. **绝不跳过 CRC 先验**——`load_hnsw` 的第一行就会 `unwrap()`（C3）。
3. **加载后必须回填 `ef_search`**（P0-5）：已核实全 crate **没有 `set_ef*`**（只有 `get_ef_construction`），ef 只能作为 `search` 参数逐次传（`hnsw.rs:1587-1589`），所以 `HnswRsIndex.ef_search` 字段是**唯一载体**，必须由 `load_graph` 的入参带入。值来自 `SearchIndexBuilder::with_ef_search` 或默认 `EF_SEARCH=200`；**bench 的 `--ef-search`（`bench.rs:635-637`）必须能流进来**，否则 S2-02 的逐位一致断言会因 ef 不同而产生假差异。
4. **`HnswRsIndex { hnsw, ef_search }` 这种字面量构造在 `persist.rs` 里编译不过**——字段对兄弟模块不可见。需要 `pub(super) fn from_loaded(hnsw: Hnsw<'static, f32, DistDotClamped>, ef_search: usize) -> Self`（或 `pub(crate)` 字段）。

### 5.4 门面层接线（`search/index.rs`）

`save`：见 §4.5 序列。新增分支：**无向量 / backend != Hnsw 时，主动删除可能存在的旧 sidecar**（否则「关掉向量重建库」会留下永远匹配不上的僵尸图文件）。取后端持久化能力用 P0-4 的下转：

```rust
let stats = match inner.vector_index.as_ref().and_then(|vi| vi.as_graph_persist()) {
    Some(g) => match g.dump_graph(path) {
        Ok(s)  => Some(s),
        Err(e) => { warn!("图落盘失败，已跳过（缓存，不影响快照）: {e}"); return finish_without_graph(); }  // P0-3
    },
    None => { cleanup_stale_sidecars(path)?; None }   // Brute / 纯 BM25
};
```

`load_with`：见 §4.6 序列。降级路径复用现有重建代码，只是多一次警告输出。

**不变的部分（重要边界）**：

- `Snapshot` 结构、`FORMAT_VERSION`、`ConfigFingerprint` **四个字段全部不动**（D-S2-07）⇒ 旧快照可加载，配置指纹校验逻辑零改动。
- Brute 逃生舱行为不变：仍从 `raw_vectors` 建 `BruteForceIndex`，**忽略图**。

### 5.5 并行建图（`parallel_insert`，D-S2-05）

T7-05 原文包含「并行建图」。设计要点：

- **接口**：`VectorIndex` 新增 `fn add_batch(&mut self, items: &[(ChunkId, NormalizedVector)]) -> Result<()>`，默认实现为循环 `add`；`HnswRsIndex` 覆盖为 **`parallel_insert_slice`**（`hnsw.rs:1224`，签名 `&Vec<(&[f32], usize)>`）。
  **实现提示（P2）**：用 `parallel_insert_slice` 而非 `parallel_insert`——`NormalizedVector::as_slice()` 直接组装 `(&[f32], usize)`，**不必**给 `NormalizedVector` 加 `as_vec()`（其内部 `Vec<f32>` 字段对兄弟模块不可见）。对象安全，无破坏性。
- **落点**：只用在**批量建库**（`SearchIndex::flush` 的一次性大批量灌入，需 >= 某阈值，如 1000 条）；**增量 add 保持串行**（单条语义不变）。
- **确定性的代价**：并行灌入 ⇒ 拓扑不可复现 ⇒ 「同批向量两次建库结果不同」。但**图一旦落盘即被冻结**，所以只要验收口径是「同快照两次加载」（§1.3 验收 2），NFR-06 不受影响。
- **默认值的取舍**：建议默认**关**。理由：P5/P6 的全部基线（NDCG/MRR/延迟）都在串行建图下测出，Step 6 精排的对比实验需要可比基线；先出实测（S2-11）再决定是否翻默认值。

### 5.6 CLI / bench 接线与 NFR-04 口径

| 位置 | 改动 |
| --- | --- |
| `helix build` | 打印「图落盘 X 条 / Y MB / 耗时 Z」；`--no-graph-persist` 逃生舱（写库但不落图） |
| `helix search --index` | 现有的 `[快照加载 … 耗时]` 之后补一行 `[向量图加载 …（持久化图 / 降级重建）]`，**降级必须显式可见**（D-S2-04） |
| `helix bench --index` | 底层路径接图持久化（D-S2-06），保留 `[HNSW 图重建 N 条 耗时]` 这行的语义为「降级重建」 |
| `scripts/eval_perf.sh` | NFR-04 判定改为**完整冷启动**（快照 + 图 + 索引重建）< 2s；新增「图体积增量 / dump 耗时」两行记录 |

### 5.7 错误类型新增

```rust
/// 向量图 sidecar 读写失败（hnsw_rs 落盘/加载、manifest 编解码）
#[error("向量图读写失败: {0}")]
VectorGraph(String),

/// 图 sidecar 与快照不匹配（缺失/损坏/版本不符），已降级重建
#[error("向量图不可用（{reason}），已降级为加载后重建")]
GraphStale { reason: String },
```

`GraphStale` 只在 **strict 模式**（D-S2-04）下作为 `Err` 返回；默认模式下作为**警告**输出并继续。

### 5.8 可观测性（NFR-07）

降级不能静默。默认行为：

```
[警告] 向量图 sidecar 不可用（原因：图文件 CRC 不匹配），已降级为加载后重建（冷启动会变慢）
```

同时在 `search/index.rs` 暴露 `SearchIndex::graph_status() -> GraphStatus { Loaded, Rebuilt(reason), NotApplicable }`，便于上层与测试断言。

---

## 6. 决策记录（D-S2-xx）

| # | 决策 | 选项 | 建议 | 理由 / 代价 |
| --- | --- | --- | --- | --- |
| **D-S2-01** | **ADR-A 主体**：图落盘布局与多文件原子性 | A 并入 blob / B 代际目录 / **C sidecar + 单原子 manifest** | **C** | A 需升 `FORMAT_VERSION`（旧快照全废）且加载时要回写临时文件（`HnswIo` 只能从文件读）；B 把 `save(path)` 变成目录，破坏性变更。C 的代价只是磁盘多 3 个文件，换来「图可随时丢弃 + 与 Step 3 解耦」 |
| **D-S2-02** | 图 dump 复制一份向量（12K 约 +25MB，快照 52.3MB → 约 +60%） | 接受 / 图持久化时不落 `vectors` section / 配置开关 | **接受** | `DumpMode::Light` 是 `pub(crate)`，外部拿不到（C2）；靠 `DataMap::from_hnswdump` 反读又要依赖 hnsw_rs 内部格式，脆弱。**注意：内存不变**（现在本来就是 raw_vectors + 图各一份 24.6MB），只涨磁盘 |
| **D-S2-03** | 持久化能力的类型表达 | 改 `VectorIndex` / **新 trait `VectorGraphPersist`** / 门面层 if | **新 trait** | 主 trait 必须保持对象安全；新 trait 让「Brute 无图」成为类型事实 |
| **D-S2-04** | 图不可用时的默认行为 | 静默降级 / **警告后降级** / 报错 | **警告后降级 + strict 选项** | 静默降级违反 NFR-07（会掩盖「图其实一直没生效」这类问题）；直接报错则把「可丢弃的缓存」变成「强依赖」，违背 ADR-A 的地基 |
| **D-S2-05** | 并行建图是否纳入本 Step | 纳入并默认开 / **纳入但默认关** / 推迟 | **纳入但默认关** | 保住与 P5/P6 基线的可比性；先出实测再定默认值。若评审认为本 Step 应聚焦冷启动，也可整体推迟到 Step 4（与 embed 并行一起做） |
| **D-S2-06** | bench 底层路径是否接图持久化 | 接 / 不接 | **接** | 不接则 10 万级评测仍吃 11.57s 与 R-P5-13 抖动，Step 4/6 的性能与相关性基线都不稳。代价：多改一处底层调用点 |
| **D-S2-07** | 配置指纹是否扩展 | 扩展 / **不扩展** | **不扩展** | 把 `vector_backend` 写进指纹会打断「Brute 逃生舱加载 Hnsw 建库快照」这一现有能力；`dim` 已覆盖模型维度风险，其余（dist/平台/图格式）写进 manifest 做交叉校验即可 |

**外部评审（GLM）对七个决策的意见：全部同意**，无分歧：

| # | 评审意见 |
| --- | --- |
| D-S2-01 | **同意方案 C**。A 的两条硬伤均经源码证实（无内存 reader 入口，`hnswio.rs:377/398`）；B 的破坏性成立。**按 P0 清单修订后定稿** |
| D-S2-02 | **同意接受**。C2 属实；「内存不变」与 `eval-report` §8.4 的两份向量口径吻合 |
| D-S2-03 | **同意独立 trait**，但**必须补 P0-4 的分派方法**，否则门面层触达不了它 |
| D-S2-04 | **同意警告后降级 + strict**，并**扩展到写路径**（P0-3） |
| D-S2-05 | **同意纳入默认关**（T7-05 原文确含并行建图，`plan-v2:166`）。若推迟到 Step 4 也可接受，但 D7 结案口径要改 |
| D-S2-06 | **同意接**；先回答 P1-6（图绑定的快照从哪来）再开工 |
| D-S2-07 | **同意不扩**。已核实不扩指纹与逃生舱/校验逻辑兼容 |

---

## 7. 影响面与兼容性

| 面 | 影响 | 是否破坏 |
| --- | --- | --- |
| **快照格式** | 一个字节不变（`FORMAT_VERSION` 仍 2，`Snapshot` 三字段不变） | ✅ 无破坏，旧快照可加载 |
| **磁盘** | 每个含向量快照多 ~3 个文件，+ 约 1.6× 体积（12K：52.3MB → 约 85MB，待实测） | ⚠️ 观感与体积成本 |
| **内存** | 不变（raw_vectors 与图各持一份向量的现状未变） | ✅ |
| **`VectorIndex` trait** | 主线不变；新增 `add_batch` 与 `as_graph_persist`（**均有默认实现**） | ✅ |
| **`storage::load` / `save`** | 新增 `load_with_crc` / `save_with_crc`；保留 `load` / `save` 原签名（内部转发） | ✅ |
| **公开 API** | 新增 `VectorGraphPersist`、`GraphManifest`、`GraphStatus`、`Error::VectorGraph/GraphStale` | ✅ 纯新增 |
| **CLI** | build/search 输出多两行；新增 `--no-graph-persist` | ✅ |
| **确定性语义** | NFR-06 口径细化：「同快照两次加载逐位一致」；「两次建库一致」在并行建图下**不再保证** | ⚠️ 需回写需求文档 |

---

## 8. 测试计划

> 编号 S2-Tn。带 ⚠️ 的是**必测**（直接对应验收或 C3 风险）。

| # | 测试 | 断言 |
| --- | --- | --- |
| **S2-T1** ⚠️ | 图 roundtrip | `build → save` 后三个 sidecar 文件存在；manifest 各字段与快照实际 CRC / 长度 / dim / nb_point 一致 |
| **S2-T2** ⚠️ | **消解 R-P5-13**（验收 2） | ① **前置断言：两次加载的 `GraphStatus` 都必须是 `Loaded`**（P0-1：basename 拼错时降级路径一切正常，只断言「能加载」会漏掉 NFR-04 静默失效）；② 两次均固定同一 `ef_search`；③ 200 条 query 的 Top-10 `(chunk_id, score)` 序列**逐位相等** |
| **S2-T3** | 图加载 vs 重建的质量等价性 | 持久化图与重建图对同一 query 集，与暴力 oracle 的 Top-10 重合率均 ≥ 0.95（沿用 `hnsw_rs_index.rs` 现有口径） |
| **S2-T4** ⚠️ | **图可丢弃**（验收 3） | 四种场景（删 manifest / 删图 / 改图一字节 / 旧快照无图）均可加载且检索结果可用 |
| **S2-T5** ⚠️ | **损坏输入不 panic**（验收 4，对应 C3） | 图文件截断到 1% / 50% / 99%，以及 magic 被篡改：加载**不 panic**，返回可用索引（降级）或 Err |
| **S2-T6** | 旧快照兼容（验收 5） | `FORMAT_VERSION=2`、无 sidecar 的快照加载成功，`GraphStatus::Rebuilt`，oracle 重合率 ≥ 0.95（**不做逐位断言**，理由见验收 5） |
| **S2-T7** | 逃生舱不破 | Brute 后端加载含图快照 → 忽略图，`GraphStatus::NotApplicable`，结果与改造前**逐位一致**（精确扫描是确定性的，此处逐位断言成立） |
| **S2-T8** | 纯 BM25 不写图 | 无 embedder 时 `save` 不产出任何 sidecar；且**会删掉**已存在的旧 sidecar |
| **S2-T9** ⚠️ | 删除 + 图持久化的跨快照正确性 | `add → remove → save → load`：结果不含已删 doc；且 `manifest.nb_point >= vectors.len()`（墓碑留在图里）；Step 1 的 T2 在图持久化后仍绿 |
| **S2-T10** | 加载后继续写入 | `load → add → save → load`：新 chunk 可检索，第二次加载的图包含新点（`nb_point` 增长） |
| **S2-T11** | 维度/距离/平台不匹配 | 手工构造 `dim` 或 `dist_id` 或 `platform` 不匹配的 manifest → 降级重建，不 panic |
| **S2-T12** | 多次加载不泄漏到崩 | 同进程连续 `load` 50 次（小语料），不 panic、结果一致（R23 的回归护栏） |
| **S2-T13** | 并行建图（若 D-S2-05 纳入） | `add_batch` 并行路径与串行路径的 oracle 重合率差 < 1 个百分点；阈值以下的小批量自动回落串行 |
| **S2-T14** | NFR-04 实测（验收 1/7） | 12K：`eval_perf.sh` 报告完整冷启动 < 2s；记录图体积增量、dump 耗时、加载耗时 |
| **S2-T15** | manifest 版本未知（P1-5） | 手工构造 `manifest_version = 999` → 降级重建，`GraphStatus::Rebuilt`，不 panic |
| **S2-T16** | 快照已更新、图未更新（P1-5） | 用旧 manifest 覆盖新 manifest（或改 `snapshot_crc`）→ 降级重建；对应 §4.7 矩阵行 |
| **S2-T17** | 建图参数漂移（P1-3） | 手工把 `Description.max_nb_connection` / `ef` 与内核常量改成不一致 → 步骤 4.5 预校验拦下，降级重建 |

---

## 9. 实施任务拆分（S2-01 ~ S2-12）

> 一个 PR 一个主题；顺序即依赖顺序。

| # | 任务 | 依赖 | 量级 |
| --- | --- | --- | --- |
| **S2-01** | 本文评审 + ADR-A 拍板（D-S2-01~07），回写 `plan-v2.md` §4.0 H1 状态 | — | S |
| **S2-02** | `storage/graph.rs`：`GraphManifest` + 编解码 + CRC + 原子写（tmp → fsync → rename → **fsync 父目录**，P1-4）+ `graph_basename()` + 单测 | S2-01 | S |
| **S2-03** | `vector/persist.rs`：`VectorGraphPersist` + `HnswRsIndex::dump_graph/load_graph`（basename 不追加后缀、`Box::leak` 后**丢弃句柄**、短名比对、`ef_search` **入参**、`from_loaded` 构造器）+ **`HnswIo: Send + Sync` 编译期断言** | S2-02 | M |
| **S2-04** | 门面层接线：`save` 写图 + 写 manifest（含 P0-3 写路径失败语义）；`load_with` 校验（含 §4.6 步骤 4.5 的 Description 预校验）+ 降级；无图场景清理旧 sidecar；`VectorIndex::as_graph_persist` 下转（P0-4） | S2-03 | M |
| **S2-05** | `storage::load_with_crc` 暴露正文 CRC + **`save_with_crc` 返回正文 CRC**（P0-6）；保留 `load` / `save` 兼容签名 | — | S |
| **S2-06** | 错误类型 + `GraphStatus` + 警告输出 + strict 选项 | S2-04 | S |
| **S2-07** | 单测 S2-T1~T17（含损坏输入不 panic 的四种截断；**T2 必须先断言 `GraphStatus::Loaded`**） | S2-04 | M |
| **S2-08** | CLI 接线（build/search 输出、`--no-graph-persist`） | S2-04 | S |
| **S2-09** | bench 底层路径接图持久化（D-S2-06）。**先答「图绑定的快照从哪来」（P1-6）**：<br>· `--index` 加载路径 → 自然成立，图随快照同目录；<br>· **内存直建路径**（`build_backend`，`bench.rs:631-647`）→ 建库后用 `save_with_crc` 落到**临时目录下的快照文件**再走正常 load 路径；或（更简单）**要求 bench 持久化场景必须经 `--index`**，内存直建路径维持现状并打印提示。倾向后者：bench 关注检索延迟，冷启动已在 S2-T14 单独测 | S2-05 | S |
| **S2-10** | NFR-04 实测 + `eval_perf.sh` 口径刷新 + 体积/dump 耗时记录 | S2-08, S2-09 | S |
| **S2-11** | 并行建图 `parallel_insert`（`add_batch` + 阈值 + 开关 + 实测，D-S2-05） | S2-04 | M |
| **S2-12** | 文档回写：架构 v1.6（**ADR-A** + §5.4.3 图持久化 + §7.6.2 + R19~R25）、需求（FR-29 落定、NFR-04 刷新、NFR-06 口径细化）、`plan-v2` 进度与 D7 结案、`CHANGELOG`、`docs/README.md` 索引。**另含 6 项上游回写，见 §10.1** | S2-10 | M |

---

## 10. 风险与未决问题

> 编号沿用架构文档第 14 章的 R 系列（现有 R1~R18，本文新增 R19~R25，评审通过后回写架构文档）。

| # | 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- | --- |
| **R19** | **`hnsw_rs` panic/exit(1)**（C3），**读路径与写路径都暴露**：<br>· 读：损坏图文件 → panic 或 `exit(1)`（文件打不开则幸免，返回 Err）<br>· 写：`DumpInit` 打不开文件 → `panic_any`（`hnswio.rs:208/226`） | 崩溃恢复场景下进程直接死，违反「不得静默读错」之上的更强要求「不得崩」 | 读：manifest CRC + 长度 + 维度 + 平台 + **Description 预校验**（§4.6 步骤 4.5）五道先验，全过才调 `hnsw_rs`；S2-T5 覆盖四种损坏形态。<br>写：前置检查目录可写 + 失败按 P0-3 语义处理（警告/strict） | ① 校验通过**之后**文件被并发改写仍可能 panic（无「并发写同一快照」语义，不处理）；② 写路径的 `panic_any` 是 TOCTOU 窗口，**只能缩小不能归零**——`panic_any` 不是 `Err`，调用侧无法 catch |
| **R20** | **距离类型路径被烧进图文件**（C7） | 重命名/移动 `DistDotClamped` 会让旧图全部失效 | ① 用 `load_hnsw`（**短名**比对）而非 `load_hnsw_with_dist`；② manifest 记**自有** `dist_id`，与 Rust 类型路径解耦；③ 语义变更时显式升 `dist_id` | 类型**改名**仍会失效（走降级重建，不丢功能） |
| **R21** | **平台/端序绑定**（C4）：裸 f32 + native endian | 快照拷到别的平台 → 图不可用 | manifest 记 `platform` 并校验，不匹配即降级 | 图 sidecar **不可跨平台搬运**是既定事实，需在用户文档声明 |
| **R22** | **磁盘体积增加约 60%**（12K 估算：快照 52.3MB → +~25MB 数据文件 + ~8MB 图文件 ≈ 85MB，待 S2-T14 实测） | 大语料（100 万级）下显著 | 先接受并实测；Step 5 compaction 重写图时可一并优化；`--no-graph-persist` 逃生舱 | 未解决，V2.0 接受 |
| **R23** | **`Box::leak` 的 `HnswIo`**（C6） | 每次加载泄漏约 200B + 路径串；长生命周期服务反复加载会累积 | 量级极小且可控；**leak 后丢弃句柄、不存字段**（P1-2，避免与 `Hnsw` 内部指向 Mmap 的共享借用形成别名）；S2-T12 护栏 | 目前无回收路径；**依赖 `HnswIo: Send + Sync`**，S2-03 加编译期断言，`hnsw_rs` 升级时需复核 |
| **R24** | **加载后增量插入的建图参数不同**（C8：`extend_candidates` 重载后为 `true`，`Hnsw::new` 为 `false`） | 长期增量写入后，图质量与纯内存建库存在偏差，可能影响召回 | S2-T10 覆盖；实测 oracle 重合率；文档记录 | 无法在外部改私有字段；若实测偏差显著，需评估「加载后强制重建」策略 |
| **R25** | **每次 `save` 全量重 dump 图** | 频繁 save 场景成本高（30MB 级写入 + CRC 扫两遍） | 先不做优化；预留 dirty 标记（`dumped_len == len()` 可跳过）的位置，实测后决定 | 未解决 |

**未决问题（需评审补充意见）**

1. `parallel_insert` 的实测收益未知（hnsw insert 有锁竞争，可能只有 2~3× 而非线性加速）。是否值得为它引入「建图拓扑不可复现」这一永久性代价？建议 S2-11 先出数据再决策默认值。
2. 是否需要**同时保留最近一代图**（崩溃后回滚到上一代而不是降级重建）？本文推荐不做（增加 GC 语义），**评审补充了一个更硬的理由**：`DumpInit` 是原地 `create(true).truncate(true)` 打开（`hnswio.rs:199-202`）——**dump 一开始，上一代图就没了**。要保留上一代必须先 rename 旧图，等于把被否掉的方案 B 的一半请回来，不值。⇒ 本条收敛为「不做」。
3. 图 sidecar 是否应该进 `.gitignore` / 是否需要 `helix inspect` 之类的诊断子命令？倾向**不在本 Step 做**（评审同意）。

### 10.1 P2 项与上游回写清单（归入 S2-12）

**已在本版直接处置**：

| 项 | 处置 |
| --- | --- |
| 「不得静默读错」引用错位（原写「架构 §4.2」） | 该原文实际在 `crates/core/src/storage/mod.rs:3-5`（模块文档），已改引此处 |
| 附录 B 行号偏差（`extend_candidates`、data open） | 已按复核结果更正（512/601、220、776） |
| `parallel_insert` → `parallel_insert_slice` | 已采纳（§5.5），并核对其真实签名是 `&Vec<(&[T], usize)>` |
| `HnswIo::new` 的 dir 参数类型 | **已核实为 `&Path`**（`hnswio.rs:319`），非 UTF-8 路径无额外风险，无需处理 |
| §4.6 步骤 2 跳过条件补 `cfg.embedder 为 None` | 已补（纯防御性） |
| 头部日期 2026-09-06 疑似笔误 | **经核对不是笔误**，本文确于该日撰写与修订 |

**需在 S2-12 回写上游的 6 项**：

1. **需求 NFR-06 的备注「HNSW seed 固定」是过时论据**——`StdRng::from_os_rng()` 无 seed API（`p5-design.md:493` 已证），与本文 §2.2 直接冲突，**必须改**。
2. **ADR 编号统一**：架构 §7.6.1（`architecture-design.md:723`）预引用了「ADR-011」，与本文「ADR-A」需统一为同一个编号。
3. **D7 归属**写 `p5-design` 更准（D7 编号仅定义于 `p5-design.md:562`；`p4-design` 只有 R-P4-4）。
4. **NFR-07 口径延伸**：现有口径只有检索可观测字段，「**降级可观测**」属延伸——回写时要么扩需求口径，要么换锚点（建议直接扩，降级静默是真实风险）。
5. **验收 1 口径对齐**：本文「位图/字段索引重建一并计时」比 `plan-v2` / 需求文档的「快照 + 图加载」更宽，回写时须对齐（建议采用本文的**完整冷启动**口径，更贴近 NFR-04 本意）。
6. **Q-C3 重新定级**：本文将其从「中」改为「高」（属正确性风险），回写时须注明是本文重新定级。

---

## 附录 A：新增 / 变更 API 一览

```rust
// ── storage/graph.rs（新增）──
// ⚠️ P0-1：传给 hnsw_rs 的 basename 是**快照文件名全名**；下面两个常量只是
//          hnsw_rs 自行追加的产物后缀，用于拼路径，**不要**再拼进 basename。
pub const GRAPH_SUFFIX: &str = "hnsw.graph";         // → foo.idx.hnsw.graph
pub const DATA_SUFFIX:  &str = "hnsw.data";          // → foo.idx.hnsw.data
pub const MANIFEST_SUFFIX: &str = "hnsw.manifest";   // → foo.idx.hnsw.manifest
pub struct GraphManifest { /* 见 §4.4 */ }
pub struct GraphPaths { pub graph: PathBuf, pub data: PathBuf, pub manifest: PathBuf }
pub fn graph_paths(snapshot: &Path) -> GraphPaths;
/// 传给 `file_dump` / `HnswIo::new` 的 basename —— **快照文件名全名**，不追加任何后缀
pub fn graph_basename(snapshot: &Path) -> String;    // "foo.idx"
pub fn read_manifest(path: &Path) -> Result<Option<GraphManifest>>;   // 缺失 → Ok(None)
pub fn write_manifest_atomic(path: &Path, m: &GraphManifest) -> Result<()>;  // tmp+fsync+rename+fsync_dir
pub fn file_crc32_len(path: &Path) -> Result<(u32, u64)>;             // 流式，不整读

// ── vector/persist.rs（新增）──
pub struct GraphStats {
    pub nb_point: u64, pub dim: u32,
    pub graph_format: u32,          // P0-2：dump 后由 load_description **读回**，非字面量
    pub max_nb_connection: u32,     // P1-3
    pub ef_construction: u32,       // P1-3
}
pub trait VectorGraphPersist: VectorIndex {
    fn dump_graph(&self, base: &Path) -> Result<GraphStats>;
    // P0-5：ef_search 是**入参**（全 crate 无 set_ef*，只能由字段承载）
    fn load_graph(base: &Path, m: &GraphManifest, ef_search: usize) -> Result<Self> where Self: Sized;
}

// ── vector/mod.rs（变更）──
pub trait VectorIndex {
    // 新增（有默认实现，不破坏现有实现）
    fn add_batch(&mut self, items: &[(ChunkId, NormalizedVector)]) -> Result<()>;
    // P0-4：门面层从 Box<dyn VectorIndex> 触达持久化能力的**唯一**通道
    fn as_graph_persist(&self) -> Option<&dyn VectorGraphPersist> { None }
}

// ── storage/snapshot.rs（变更）──
pub fn load_with_crc(path: &Path) -> Result<(Index, Vec<(ChunkId, Vec<f32>)>, ConfigFingerprint, u32)>;
pub fn load(path: &Path) -> Result<LoadedSnapshot>;   // 保留，内部转发
// P0-6：写路径同样要带出正文 CRC（供 manifest 的 snapshot_crc 填写）
pub fn save_with_crc(path: &Path, snap: &Snapshot) -> Result<u32>;
pub fn save(path: &Path, snap: &Snapshot) -> Result<()>;   // 保留，内部转发并丢弃 crc

// ── search/index.rs（变更）──
pub enum GraphStatus { Loaded, Rebuilt(String), NotApplicable }
impl SearchIndex { pub fn graph_status(&self) -> GraphStatus; }

// ── error.rs（变更）──
Error::VectorGraph(String);
Error::GraphStale { reason: String };
```

## 附录 B：`hnsw_rs-0.3.4` 源码证据索引

> 路径：`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/hnsw_rs-0.3.4/`

| 结论 | 证据 |
| --- | --- |
| 落盘唯一公开入口是 `AnnT::file_dump` | `src/api.rs:37`（trait 声明）、`src/api.rs:70-93`（实现） |
| 双文件 + basename 拼接规则 | `src/hnswio.rs:194-206`（graph）、`src/hnswio.rs:211-222`（data） |
| `create(true).truncate(true)` ⇒ 非原子 | `src/hnswio.rs:199-202`、`src/hnswio.rs:216-219` |
| `DumpInit::new` 打不开文件会 `panic_any` | `src/hnswio.rs:208`、`src/hnswio.rs:226` |
| `file_dump` 硬编码 `DumpMode::Full` | `src/api.rs:81` |
| `HnswIoT`（可选 Light 模式）是 `pub(crate)` | `src/hnswio.rs:77` |
| 数据文件 = 裸字节 + 原生端序 | `src/hnswio.rs:1104-1114`（写）、`src/hnswio.rs:1160-1167`（读） |
| `Description` 自描述字段 | `src/hnswio.rs:848-869`；写入侧 `src/hnswio.rs:880-917` |
| `distname = type_name::<D>()` | `src/hnswio.rs:1378` + `src/hnsw.rs:834-836` |
| `load_hnsw_with_dist` 比对**完整路径** | `src/hnswio.rs:574-580` |
| `load_hnsw` 只比对**最后一段** | `src/hnswio.rs:475-482` |
| **文件打不开 → 干净的 Err**（不是 panic） | `src/hnswio.rs:377-385`（graph）、`394-402`（data） |
| reload 路径的 panic 点（**文件能打开但内容坏**） | `418`（`load_description().unwrap()`）、`456/464/555/563`（assert_eq）、`639`（`panic!` incoherent size of T）、`705/710`（assert）、`764`（`points_by_layer[layer][rank]` 越界）、`821`（`exit(1)`）、`1024/1038`（`from_utf8().unwrap()`）、`1137/1146`（assert）、`1172`（`exit(1)` unknown format_version）、`1228-1276`（9 处 `read_exact().unwrap()`） |
| dump 侧 `panic_any`（**写路径**） | `src/hnswio.rs:208`（graph）、`226`（data） |
| `load_description` 对截断文件安全 | `src/hnswio.rs:939-1042`（`read_exact` 全部 `?`；`len > 256` 先行拒绝） |
| dump 写入 `format_version: 3` | `src/hnswio.rs:1369`（**不是 4**；`load_description` 支持 2/3/4 三档 magic） |
| 生命周期 `'a: 'b` | `src/hnswio.rs:433-438`、`src/hnswio.rs:533-538` |
| 重载后 `extend_candidates: true` vs `new` 的 `false` | `src/hnswio.rs:512`、`601` vs `src/hnsw.rs:776` |
| `parallel_insert` / `parallel_insert_slice` 用 rayon | `src/hnsw.rs:1212-1218`、`1224-1226` |
| 无 `set_ef*`（ef 只能逐次作为 search 参数传） | `src/hnsw.rs:1587-1589`（仅 `get_ef_construction` 在 `805`） |
| `prelude` 重导出 io 相关类型 | `src/prelude.rs:1-11` |

## 附录 C：本文引用的项目内证据

| 结论 | 证据 |
| --- | --- |
| 加载后全量重建图 | `crates/core/src/search/index.rs:330-349` |
| bench 底层重建图 | `crates/cli/src/bench.rs:631-647` |
| 快照格式 / header 布局 / `FORMAT_VERSION` | `crates/core/src/storage/codec.rs:5-43` |
| `Snapshot` 三字段与 CRC 校验 | `crates/core/src/storage/snapshot.rs:19-28`、`97-123` |
| `HnswRsIndex` 存 `Hnsw<'static, …>`、`ef_search` 字段 | `crates/core/src/vector/hnsw_rs_index.rs:52-56` |
| `DistDotClamped` 已 `#[derive(Default)]` | `crates/core/src/vector/hnsw_rs_index.rs:41-49` |
| chunk_id append-only（墓碑位不复用） | `crates/core/src/index/forward.rs:49-61` |
| 软删除后图里留墓碑（无 remove API） | `architecture-design.md` §7.5.1 |
| NFR-04 / 体积 / 内存的实测基线 | `docs/devel/eval-report.md` §8.3、§8.4 |

---

## 实施结果（S2-10 实测，2026-09-06）

S2-01~S2-12 全部落地，守门全绿（fmt / clippy `--workspace --all-targets -D warnings` /
208 测试 / MSRV 1.90 / rustdoc `-D warnings` / `--no-default-features` /
`--features charabia` / cargo-deny）。分三个 PR：**#12**（core 图持久化）、
**#13**（CLI/bench 接线 + 并行建图）、文档回写。

环境：macOS aarch64 / release / `data/t2-corpus.jsonl` 12,000 chunk / bge-small-zh-v1.5。

### 验收对照

| # | 验收 | 结果 |
| --- | --- | --- |
| 1 | 完整冷启动 < 2s | ✅ **≈100ms**（快照 76.9ms + 图 23.3ms），余量 20× |
| 2 | 消解 R-P5-13（同快照两次加载逐位一致） | ✅ S2-T1/T2 覆盖，前置断言 `GraphStatus::Loaded` |
| 3 | 图可丢弃（四种场景降级仍可用） | ✅ S2-T4/T5/T6 覆盖 |
| 4 | 损坏输入不 panic | ✅ S2-T5 覆盖（截断 1%/50%/99% + magic 篡改） |
| 5 | 旧快照可加载 | ✅ S2-T6 覆盖（oracle Top-10 重合率 ≥ 0.95） |
| 6 | 不破坏逃生舱与正确性 | ✅ S2-T13/T15/T16 覆盖 |
| 7 | 体积与耗时有实测记录 | ✅ 见下表 |
| 8 | 守门全绿 | ✅ |

### 体积与耗时（12K）

| 项 | 实测 |
| --- | --- |
| 快照 `foo.idx` | 52.3 MB |
| 图 `foo.idx.hnsw.graph` | 7.9 MB |
| 数据 `foo.idx.hnsw.data` | 23.7 MB |
| manifest | 91 B |
| **磁盘增量** | **84.1 MB（1.6×）** |
| 图 sidecar dump 耗时 | **43.5ms**（含 CRC 扫两遍 + manifest 原子发布） |
| 图 sidecar 加载耗时 | **23.3ms**（含五道先验校验） |
| 快照加载耗时 | 76.9ms（含位图/字段索引重建） |
| **完整冷启动** | **≈100ms**（改造前 ≈11.6s，**约 116×**） |
| 降级路径（图缺失） | 53.3ms + 图重建 **9.96s** |

### 实施中修正的两处实测预期

- **R25「每 save 全量重 dump」不是瓶颈**：预估「30MB 级写入 + CRC 扫两遍」会贵，
  实测仅 **43.5ms**。dirty 标记优化暂不需要（100 万级需复核）
- **`--no-graph-persist` 的价值被重新定位**：它省的是磁盘（1.6× → 1×），
  不是构建时间（dump 只占全链路 203s 中的 43ms）

### 上游回写已落位（S2-12）

1. 需求 `requirements-spec.md` **v1.6**：NFR-06 删去过时论据「HNSW seed 固定」、
   NFR-04 口径改为「完整冷启动」并补实测、NFR-07 扩「降级不得静默」、FR-29 落定
2. 架构 `architecture-design.md` **v1.6**：**ADR-A** 入 2.3 与 9.4、新增 5.4.3 与 7.6.2、
   8.2/8.3 口径修正、10.3 补观测方法、新增 14.1（R19~R25）
3. `plan-v2.md`：Step 2 状态与实测表、Q-P2/Q-P3 标记已解决、D7 结案
4. `eval-report.md` 8.3：冷启动小节重写（P5 vs V2 Step 2 对照）
5. `docs/README.md`：v2-step2-design 入索引、eval_perf.sh 口径说明
6. `CHANGELOG.md`、`user-guide.md`（图 sidecar 用户可见行为）
