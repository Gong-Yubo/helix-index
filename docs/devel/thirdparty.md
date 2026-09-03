# HelixIndex 第三方库调研与引入建议

| 项目      | 内容                                                                     |
| ------- | ---------------------------------------------------------------------- |
| 版本      | v1.1                                                                   |
| 日期      | 2026-09-02（v1.0）/ 2026-09-03（v1.1）                                            |
| 状态      | **已采纳并回写**；**P0 已执行验证**（2026-09-02），执行中发现的 5 处修正见第 4 章各节与 `p0-design.md` 第 12 章 |
| v1.1 变更 | 2026-09-03：5.2 新增非 crate 资产 **T2Ranking 数据集**（Apache-2.0，P5 评测数据源，选型见 `p5-design.md` 第 3 章） |

**P0 执行后的 5 处修正**

| # | 修正                                                            | 影响文档                                  |
| - | ------------------------------------------------------------- | ------------------------------------- |
| 1 | `bincode 3.0.0` 为玩笑发布 → 改用 **2.0.1**                          | 本文档 4.6、架构文档 9.1/7.6/ADR-005、plan.md |
| 2 | `moka` 必须启用 `sync` feature                                    | 本文档 4.9、架构文档 9.1、plan.md 附录 B        |
| 3 | `fastembed` 必须 `default-features = false`（移除 NCSA 依赖链）        | 本文档 4.3、架构文档 9.1、plan.md 附录 B        |
| 4 | 模型实际来源为 `Xenova` 而非 `Qdrant`，且**未声明 License**                 | 本文档 4.3/5.2、需求文档 8.3（新增 P5）、p0-design.md |
| 5 | 依赖白名单需补充 `CDLA-Permissive-2.0` / `MPL-2.0` / `Unicode-3.0`   | `deny.toml`、p0-design.md 12.6         |
| 上游文档    | `docs/devel/requirements-spec.md`、`docs/devel/architecture-design.md`、`docs/devel/plan.md`    |
| 数据来源    | crates.io API、docs.rs、HuggingFace API 实测拉取（非记忆），见附录 A                |
| 核查时点    | Rust stable 1.98.0（2026-08-20 发布）                                      |

---

## 1. 本文档要解决的问题

架构文档第 9 章已确定技术选型，但存在三个未回答的问题：

1. 已选定的库之外，**还有哪些环节其实有成熟可用、License 友好的库可以省掉自研工作量？**
2. 现有选型中**有没有已经过时或事实错误的地方？**（核查后发现 3 处，见第 7 章）
3. 这些依赖**合起来会不会带来 License 或 MSRV 冲突？**（发现 1 处严重冲突，见第 6 章）

本文档按「能力域」逐项给出候选对比、结论与理由，最终产出一张可分发的依赖清单。

---

## 2. 判定标准

对每个候选库，按以下 6 条打分，结论分三档：

| 档位           | 含义                          |
| ------------ | --------------------------- |
| ✅ **引入**     | 直接用，不自己写                    |
| ⏳ **暂缓 / 评估** | 当前阶段不需要，但记下来，到 P4/P5 用数据决定是否切换 |
| ❌ **不引入**    | 自研，或该能力本身不在范围内              |

**6 条判据**

| # | 判据        | 说明                                                             |
| - | --------- | -------------------------------------------------------------- |
| 1 | **是否差异化所在** | 本项目的立论是"手写倒排 + BM25 + 可控融合"。**核心算法自研是目的而非成本**，这类即使有成熟库也不引入    |
| 2 | **正确性风险**   | 自己写容易错且错了难发现（Unicode 边界、数值稳定），优先用库                             |
| 3 | **依赖成本**    | 传递依赖数量、是否引入 C/C++ 工具链、编译耗时、二进制体积                               |
| 4 | **License**  | 是否宽松许可（MIT / Apache-2.0 / BSD / BSL / CC0）；**任何 GPL/LGPL/AGPL 一票否决** |
| 5 | **维护活跃度**   | 发布时间、下载量、是否有 pre-release 陷阱                                    |
| 6 | **可替换成本**   | 本项目已把核心能力抽象成 trait（`Analyzer` / `Embedder` / `VectorIndex` / `Retriever` / `FusionStrategy`），**凡是被 trait 隔离的部分，切换实现只改一个文件，因此"先决策、后验证"的代价很低** —— 这是本文档敢于给出"暂缓/评估"结论的前提 |

> 第 6 条是整个调研的方法论基础：**不要在 P0 把非关键选型钉死，但必须把替换成本降到最低。**

---

## 3. 结论速览

| 能力域         | 候选                                    | 结论            | 一句话理由                                    |
| ----------- | ------------------------------------- | ------------- | ---------------------------------------- |
| 全文检索内核      | `tantivy` 0.26.1                      | ❌ 主依赖 / ✅ 测试基线 | 手写是项目目的；但**用工业实现当 oracle 验证手写 BM25 正确性**，成本极低价值极高 |
| 中文分词        | `jieba-rs` 0.10.3                     | ✅             | MIT，生态成熟；只做分词，其余自研                       |
| 一体化分词       | `charabia` 0.10.0                     | ⏳ P5 对照       | Meilisearch 出品，MIT，但默认 feature 很重；P5 做分词效果对照 |
| 词典分词        | `lindera` 6.0.0                       | ❌             | 词典数据数十 MB，MVP 不需要                        |
| Unicode 分段   | `unicode-segmentation` 1.13.3         | ✅             | 手写 Unicode 边界判断坑多，20 行依赖换正确性             |
| 停用词表        | `stop-words` 0.10.0                   | ❌             | 中文表需自维护，英文表抄一份即可，不值得一个依赖                  |
| 英文词干化       | `rust-stemmers` 1.2.0                 | ⏳ v2 可选       | 架构 7.1 已定 v1 不做                          |
| Embedding   | `fastembed` 6.0.2                     | ✅             | Apache-2.0；⚠️ 依赖 `ort =2.0.0-rc.13`（预发布），见 4.3 |
| 推理兜底        | `ort` 1.16.3 + `tokenizers`           | ⏳ 兜底路径        | 若 RC 依赖不可接受，可自研 ~150 行 BGE 推理              |
| 向量索引（v1）    | `instant-distance` 0.6.1              | ✅             | 纯 Rust、MIT/Apache-2.0；缺点是无增量插入            |
| 向量索引（升级项）   | **`hnsw_rs` 0.3.4**                   | ⏳ **P4 A/B**  | **纯 Rust + 支持增量 insert + 内建 DistDot（要求归一化，正好匹配我们的方案）+ 支持过滤。可能直接消掉 R1 风险** |
| 向量索引（备选）    | `usearch` 2.26.2                      | ⏳ P6          | 功能最全（add/remove/filtered_search），但要 C++（cxx） |
| 向量索引（备选）    | `arroy` 0.8.0                         | ❌             | 增量 + 过滤都好，但耦合 LMDB，与"内存 + 快照"路线冲突          |
| 距离计算加速      | `simsimd` 6.5.16                      | ⏳ 若保留 delta 区  | Apache-2.0，SIMD 版余弦/内积；MVP 用 rayon 暴力足够   |
| 序列化         | **`bincode` 2.0.1**                   | ✅             | MIT；⚠️ **3.0.0 是玩笑发布不可用**（P0 实测，见 4.6）      |
| 零拷贝加载       | `rkyv` 0.8.18                         | ⏳ P4 A/B      | MIT，接近 O(1) 加载，对 NFR-04（<2s）有吸引力；代价是数据结构要 derive |
| 快照压缩        | `zstd` 0.13.3                         | ⏳ 可选 feature  | MIT，体积换加载时间，P5 实测后决定                     |
| 内存映射        | `memmap2` 0.9.11                      | ⏳ 与 rkyv 绑定    | 只有走上 rkyv 零拷贝路线才需要                         |
| 过滤位图        | `roaring` 0.11.5                      | ⏳ P4          | MIT/Apache-2.0；MVP 用 `Vec<u32>`/bitset 足够，注意 MSRV 1.90 |
| 嵌入式 KV      | `redb` 4.2.0                          | ❌             | 持久化路线是快照，不引入 KV                           |
| 文本分块        | `text-splitter` 0.32.0                | ⏳ v2          | 自研只要 ~80 行，且要精确维护 char 偏移（溯源需求）            |
| 缓存          | `moka` 0.12.16 / `quick_cache` 0.7.0  | ✅ 二选一         | 推荐 `moka`：并发安全且带 TTL；`lru` 需自己套锁          |
| Token 计数    | `tiktoken-rs` 0.12.0                  | ⏳ v2（FR-25）   | MIT；配合自研 `TokenCounter` trait              |
| 工程基础        | serde / thiserror / anyhow / clap / tracing / rayon / crc32fast / smol_str | ✅ | 全部宽松许可，无争议                                |
| 测试/基准       | `criterion` / `proptest` / `tempfile` | ✅ dev-dep     | 宽松许可                                      |
| 合规工具        | `cargo-deny` 0.20.2                   | ✅ 推荐          | MIT/Apache-2.0，自动检查依赖 License 与来源          |
| 服务层         | axum / tokio                          | ❌             | 明确不做（需求文档第 7 章）                           |
| 并发容器        | `dashmap` 6.2.1                       | ❌ v1          | 7.x 仍是 rc；v1 单写者 + `RwLock` 足够              |

---

## 4. 分项分析

### 4.1 全文检索内核（BM25 / 倒排索引）

| 候选                | License | 传递依赖          | 结论           |
| ----------------- | ------- | ------------- | ------------ |
| `tantivy` 0.26.1  | MIT     | **50+ 个**（含 tantivy-columnar / sstable / fst / bitpacker 等） | ❌ 主依赖 / ✅ 测试基线 |

**不引入为主依赖的理由**（ADR-001 已定，此处补充新论据）：

- 引入 tantivy 等于把本项目的核心（倒排构建、BM25 打分、统计量维护）整个外包，项目不复存在
- BM25 参数、OR 语义、增量统计量的可调试性会全部落入黑盒
- 50+ 传递依赖对一个 MVP 是过重的负担

**但建议作为 dev-dependency 引入，用作正确性 oracle**：

```toml
[dev-dependencies]
tantivy = "0.26"
```

理由与用法：

1. **P1 阶段**：写一组对照测试，用 tantivy 建同一份语料的索引，逐条比对 BM25 分值（允许 `k1`/`b` 与平均长度口径差异，但排序应高度一致）。这比"手算 5 篇文档"覆盖率高一个量级，且能发现手算样本覆盖不到的边界（空 query、全停用词 query、超长 query）
2. **P5 阶段**：作为第三路基线加入对照实验（`BM25自研` / `tantivy` / `向量` / `混合`）。**如果自研 BM25 与 tantivy 的 NDCG@10 差距超过 5%，说明自研实现有问题，必须查清再发布结论**
3. MIT 许可，仅 dev 依赖，不影响分发

> 这是本次调研里我最推荐的一个"看起来矛盾"的动作：**不用它实现功能，但用它证明自己写得对。** 自研实现最大的风险从来不是写不出来，而是"写错了但看不出来"。

---

### 4.2 分词

| 候选                          | License | 结论      | 理由                                                                        |
| --------------------------- | ------- | ------- | ------------------------------------------------------------------------- |
| `jieba-rs` 0.10.3           | MIT     | ✅       | 只借分词能力，过滤链/停用词/归一化自研，可控                                                   |
| `charabia` 0.10.0           | MIT     | ⏳ P5 对照 | Meilisearch 出品，语言检测+归一化+分词+停用词一体化。默认 feature 会拉 `lindera` 与多语词典；可用 `default-features = false, features = ["chinese"]` 精确裁剪（此时只多拉 jieba-rs / aho-corasick / csv / fst / whatlang） |
| `lindera` 6.0.0             | MIT     | ❌       | 词典分词，数据数十 MB，与 MVP 规模不匹配                                                  |
| `unicode-segmentation` 1.13.3 | MIT OR Apache-2.0 | ✅ | **中英分段用它，别手写**。Unicode 边界（全角、符号、emoji、假名）自己判断极易出错                     |
| `unicode-normalization` 0.1.25 | MIT OR Apache-2.0 | ✅ 建议 | NFC 归一化，避免"é"与"e+´"被当成两个 term                                          |
| `stop-words` 0.10.0         | MIT OR Apache-2.0 | ❌    | 一个词表不值得一个依赖；且中文停用词必须自己维护                                                  |
| `rust-stemmers` 1.2.0       | MIT/BSD-3-Clause | ⏳ v2 | 架构 7.1 已定 v1 不做（损害专有名词精确匹配）；BSD-3 属宽松许可，v2 引入无障碍                       |

**建议落地**

```toml
jieba-rs = "0.10"
unicode-segmentation = "1.13"
unicode-normalization = "0.1"
```

**P5 对照实验（建议写进 plan.md T5）**：用裁剪版 `charabia` 实现第二个 `Analyzer`，在同一评测集上对比 `jieba-rs + 自研过滤链` 与 `charabia` 的 NDCG@10。由于 `Analyzer` 已是 trait，切换成本 = 一个实现文件 + 一个 feature flag。

---

### 4.3 Embedding

| 项                       | 事实（实测）                                                                                        |
| ----------------------- | --------------------------------------------------------------------------------------------- |
| `fastembed` 最新          | **6.0.2**（Apache-2.0）；架构文档写的 5.17.4 是上一个大版本                                                  |
| 关键传递依赖                  | `ort =2.0.0-rc.13`（**预发布**）、`tokenizers ^0.22.2`、`ndarray ^0.17.2`、`safetensors`、`serde_json` |
| 5.17.4 与 6.0.2 的依赖差异    | **无差异**（两者都 pin `ort =2.0.0-rc.13`），换版本不能规避 RC 风险                                            |
| `ort` 稳定线               | 1.16.3（1.x 最后版本）；2.0.0 至今未发稳定版，最新仍为 `2.0.0-rc.13`                                           |
| ONNX Runtime 二进制        | MIT（Microsoft），`ort/download-binaries` 会自动下载                                                    |
| BGE 模型 License          | `BAAI/bge-small-zh-v1.5` = **MIT**；`BAAI/bge-m3` = **MIT**；`BAAI/bge-reranker-v2-m3` = **Apache-2.0** |

**结论：✅ 引入 fastembed 6.0.2**，理由不变（架构 ADR-003）。

**P0 实测补充（2026-09-02）**

1. **必须 `default-features = false`**。默认 feature 含 `image-models`，会引入 `image → ravif → rav1e → libfuzzer-sys`（NCSA 许可）。只需文本模型时改为：

   ```toml
   fastembed = { version = "6.0.2", default-features = false, features = [
       "ort-download-binaries-native-tls",
       "hf-hub-native-tls",
   ] }
   ```

   这样既移除 NCSA 许可项，又保留 `TextEmbedding::try_new` 所需的 `hf-hub` feature，且依赖树明显变小。

2. **实际下载的模型仓是 `Xenova/bge-small-zh-v1.5`，不是 `Qdrant/...`**（本节表格此前写的是 Qdrant）。实测缓存内容：`onnx/model.onnx`（90 MB）+ tokenizer 等，共 96 MB。**该仓在 HuggingFace 上 `license` 字段为空**——上游 `BAAI` 为 MIT，但转换仓未声明。

**已决策（2026-09-02）：采用选项 A，接受。** 理由：① 上游原始模型为 MIT；② Xenova 是公开的模型格式转换仓，非再训练产物；③ 替换成本高而收益不确定。残留风险已记入需求文档 8.3 P5，P2 实现 `Embedder` 时复核（选项 B/C 为回落路径）。

3. **默认缓存目录在项目根**：fastembed 默认把模型下到 `./.fastembed_cache/`（96 MB）。务必加 `.gitignore`；实现 `Embedder` 时应显式 `InitOptions::with_cache_dir()` 指到仓库外。

4. 编译与下载实测：`ort` 走**预编译二进制**（未触发源码编译，因此**不缺 cmake**），fastembed + ort 编译 42.5s，模型下载 49.0s。

**但要知悉 RC 依赖风险**：`ort =2.0.0-rc.13` 被**精确锁定**（`=` 而非 `^`），意味着：

- 无法随 ort 2.0 的后续 rc 自动升级，需等 fastembed 发版
- 若 ort 2.0 正式版 API 有变，fastembed 需要跟进发布
- `Cargo.lock` **必须入库**，否则不同时间构建会拉到不同的传递依赖树

**兜底路径**（若 RC 依赖在评审中被否决）：直接依赖 `ort 1.16.3`（MIT OR Apache-2.0）+ `tokenizers 0.23.1`（Apache-2.0）自研 BGE 推理，核心工作量约 150 行：加载 ONNX 模型 → tokenizer 编码 → 前向 → mean pooling → L2 归一化。**不建议 MVP 走这条路**，但它是可控的退路。

**其他不直接依赖**：`ort` / `tokenizers` / `ndarray` / `candle-core` / `hf-hub` 都通过 fastembed 传递引入即可，不在我们的 `Cargo.toml` 里直接声明（避免版本冲突与重复编译）。

---

### 4.4 向量索引（本次调研最重要的发现）

| crate                       | License         | 增量 insert | 删除    | 查询时过滤            | 纯 Rust | 备注                                        |
| --------------------------- | --------------- | --------- | ----- | ---------------- | ------ | ----------------------------------------- |
| `instant-distance` 0.6.1    | MIT OR Apache-2.0 | ❌       | ❌     | ❌                | ✅      | **现选**；`build()` 一次性消费 points             |
| **`hnsw_rs` 0.3.4**         | **MIT/Apache-2.0** | **✅**  | ❌     | ✅ `search_filter` | **✅**  | **纯 Rust；`DistDot` 明确要求"入库前 L2 归一化"，与我们的方案天然契合；甚至自带 `l2_normalize` 辅助函数** |
| `usearch` 2.26.2            | Apache-2.0      | ✅         | ✅     | ✅ `filtered_search` | ❌ C++（cxx） | 功能最全，支持 `exact_search` 做暴力对照，支持 Cos 度量 |
| `arroy` 0.8.0               | MIT             | ✅         | ✅     | ✅                | ❌ LMDB（heed） | 与"内存 + 快照"路线冲突                        |

**关键发现：`hnsw_rs` 能在"纯 Rust"前提下解决 R1 风险。**

架构文档 7.5 为绕开「instant-distance 无增量插入」设计了一整套「主索引 + delta 暴力区 + 后台重建」方案（对应 plan.md T4-06）。而 `hnsw_rs` 0.3.4：

- `pub fn insert(&self, datav_with_id: (&[T], usize))` —— 构造后可随时单条插入，**不需要 delta 区，不需要重建**
- `pub fn search_filter(&self, data: &[T], knbn, ef, filter: Option<&dyn FilterT>)` —— 内建查询时过滤，正好服务于 FR-14
- `DistDot` 的文档原文：**"essentially the Cosine distance but we suppose all vectors have been l2 normalized to unity BEFORE INSERTING in HNSW"** —— 这正是架构 7.3 定下的归一化方案，语义完全对齐，无需改造
- 纯 Rust（依赖 rayon / crossbeam-epoch / anyhow），不引入 C++ 工具链
- MIT/Apache-2.0 双许可

**代价与不确定项**

- **无删除接口**（文档与 API 中均未发现 remove/delete）—— 但我们的删除是墓碑 + 过滤，本来就不依赖物理删除，**这个缺点在我们场景下不构成问题**
- 序列化支持存在（`file_dump` / `hnswio`），但能否直接满足我们的"快照 + CRC + 版本"格式需实测
- 生态热度低于 usearch，最后发布 0.3.4（0.x 阶段），需评估维护活跃度

**建议**

1. **v1 保持 `instant-distance`**（P2 已定，且它更轻）
2. **在 P4（T4-06 之前）做一次 A/B 原型**：用 `hnsw_rs` 实现第二个 `VectorIndex`，对比「instant-distance + delta」与「hnsw_rs 原生增量」在增量写入延迟、召回一致性、内存占用上的表现
3. **若 hnsw_rs 达标，直接删掉 delta 区设计**（T4-06 及相关风险应对），代码量与复杂度都会显著下降。`VectorIndex` 已是 trait，替换成本 = 一个实现文件
4. `usearch` 作为 P6 备选（当**物理删除**成为硬需求时——此时 hnsw_rs 不满足，需 C++ 依赖换功能）
5. `arroy` 排除：LMDB 后端与"内存 + 二进制快照"的持久化路线正面冲突

> 这条建议的价值在于：**R1 目前被当成"必须绕开的约束"，但它其实是"选型可以消除的约束"。** 值得在 P4 花半天时间验证。

---

### 4.5 距离计算与暴力扫描

| 候选                | License    | 结论         | 理由                                                          |
| ----------------- | ---------- | ---------- | ----------------------------------------------------------- |
| 手写 + `rayon`      | —          | ✅ v1       | delta 区限制在总量 10% 内（≤1000 条 × 512 维），暴力扫描约 0.5M 次乘加，在毫秒级，无需优化 |
| `simsimd` 6.5.16  | Apache-2.0 | ⏳ 若 delta 保留且成为瓶颈 | SIMD 加速的余弦/内积/欧式；若 P4 实测 delta 扫描占比过高再引入         |

建议：**先测量再优化**。plan.md T4-06 完成后用 `criterion` 测 delta 扫描耗时占比，超过总延迟 20% 再考虑 simsimd。

---

### 4.6 持久化与序列化

| 候选                   | License         | MSRV  | 结论         | 理由                                                                                        |
| -------------------- | --------------- | ----- | ---------- | ----------------------------------------------------------------------------------------- |
| **`bincode` 2.0.1**  | MIT             | —     | ✅ **采用**   | 当前真实稳定线                                                    |
| ~~`bincode` 3.0.0~~  | MIT             | 1.85  | ❌ **不可用**  | **P0 实测：源码只有一行 `compile_error!("https://xkcd.com/2347/")`**，是玩笑/占位发布。教训见下方 |
| `bincode` 1.3        | MIT             | —     | ❌          | 架构文档原选型，其理由（"与 instant-distance 内部对齐"）本身是**事实错误**，见 7.1      |

> **教训（值得写进团队规范）**：crates.io 的 `max_version` **不等于**"可用的最新版本"。`bincode 3.0.0` 在 crates.io 上正常显示版本、License、依赖关系，看起来与真发布无异，实际是 XKCD 2347（Dependency）的玩笑。调研依赖时必须至少确认一点：该版本能真正编译（P0 的编译验证就是在这一步兜住的）。
| `rkyv` 0.8.18        | MIT             | 1.81  | ⏳ P4 A/B   | **零反序列化**：mmap 后直接访问，加载接近 O(1)，对 NFR-04（冷启动 <2s）是最强手段。代价：所有落盘结构需 `derive(Archive)`，且格式升级要处理版本兼容 |
| `zstd` 0.13.3        | MIT             | 1.64  | ⏳ 可选 feature | 快照体积可降 3~5 倍，代价是加载时的解压耗时；属于"体积换时间"，P5 实测后决定                                          |
| `memmap2` 0.9.11     | MIT OR Apache-2.0 | 1.65 | ⏳ 与 rkyv 绑定 | 只有走上零拷贝路线才需要                                                                          |
| `crc32fast` 1.5.1    | MIT OR Apache-2.0 | 1.63 | ✅          | 快照 CRC，已在计划中                                                                              |
| `redb` 4.2.0         | MIT OR Apache-2.0 | 1.90 | ❌          | 持久化路线是快照，不引入嵌入式 KV                                                                        |

**建议**：`storage/codec.rs` 已抽象，先上 bincode；**P4 结束时用 1 万 chunk 的快照实测 bincode vs rkyv 的加载耗时**，若 bincode 已满足 NFR-04（<2s）就不折腾 rkyv——零拷贝很酷，但 derive 侵入所有数据结构的代价是实打实的。

---

### 4.7 过滤位图

| 候选                 | License         | MSRV  | 结论    | 理由                                              |
| ------------------ | --------------- | ----- | ----- | ----------------------------------------------- |
| `Vec<u32>` / bitset | —              | —     | ✅ v1  | 1 万 chunk 的过滤，线性扫描完全够用                          |
| `roaring` 0.11.5   | MIT OR Apache-2.0 | **1.90** | ⏳ P4 | 纯 Rust，交并差高效；⚠️ MSRV 1.90，与当前 NFR-08 的 1.80 冲突 |
| `croaring` 2.7.0   | Apache-2.0      | **1.95** | ❌  | C 绑定版，MSRV 更高，无必要                                |

**建议**：v1 自研简单位集；若 P4 过滤成为热点（tag 数量多、过滤条件复杂）再评估 `roaring`，同时需解决 MSRV（见第 6 章）。

---

### 4.8 文本分块

| 候选                      | License | 结论    | 理由                                                                                   |
| ----------------------- | ------- | ----- | ------------------------------------------------------------------------------------ |
| 自研 Chunker              | —       | ✅ v1  | 约 80 行；**必须精确维护 `char_start` / `char_end`**（FR-12 溯源依赖），第三方库的偏移语义不一定满足，验证成本高于自己写      |
| `text-splitter` 0.32.0  | MIT     | ⏳ v2  | 支持按字符/token 分块，能与 tokenizer 联动。v2 引入真实 tokenizer（FR-25 token budget）后值得重新评估          |

---

### 4.9 缓存

| 候选                    | License            | 结论    | 理由                                                        |
| --------------------- | ------------------ | ----- | --------------------------------------------------------- |
| **`moka` 0.12.16**    | (MIT OR Apache-2.0) AND Apache-2.0 | ✅ 推荐 | 并发安全、支持 TTL 与权重、无需外部锁。⚠️ 必须启用 `sync`（或 `future`）feature，否则 `compile_error!` |
| `quick_cache` 0.7.0   | MIT                | ✅ 备选  | 更轻更快，锁竞争更少；无 TTL                                          |
| `lru` 0.18.3          | MIT                | ⏳     | 最轻，但**非并发安全**，需自行套 `Mutex`，容易成为并行检索的锁点                    |

**建议**：用 `moka`，替换 plan.md T3-07 与架构 8.1 中提到的 `lru`。理由：两路召回是并行的（架构 8.1），缓存若在关键路径上用 `Mutex<LruCache>`，会抵消并行带来的收益。

---

### 4.10 Token 计数（v2 / FR-25）

| 候选                  | License | 结论       | 理由                                                                    |
| ------------------- | ------- | -------- | --------------------------------------------------------------------- |
| `tiktoken-rs` 0.12.0 | MIT   | ⏳ v2     | BPE 编码，MIT。但**目标 LLM 的 tokenizer 未必是 tiktoken**，应抽象 `TokenCounter` trait，tiktoken 作为默认实现，字符数估算作为兜底 |

---

### 4.11 工程基础设施（无争议，直接引入）

| crate                        | 版本       | License            | 用途                 |
| ---------------------------- | -------- | ------------------ | ------------------ |
| `serde` / `serde_json`       | 1.0.229 / 1.0.151 | MIT OR Apache-2.0 | 序列化 / 元数据      |
| `thiserror`                  | 2.0.20   | MIT OR Apache-2.0  | 库错误类型             |
| `anyhow`                     | 1.0.104  | MIT OR Apache-2.0  | CLI 错误处理          |
| `clap`                       | 4.6.6    | MIT OR Apache-2.0  | CLI（MSRV 1.85）     |
| `tracing` + `tracing-subscriber` | 0.1.44 / 0.3.23 | MIT | 可观测性（NFR-07）|
| `rayon`                      | 1.12.0   | MIT OR Apache-2.0  | 两路并行 / 批量摄入       |
| `smol_str`                   | 0.3.6    | MIT OR Apache-2.0  | term 短字符串（MSRV 1.89）|
| `crc32fast`                  | 1.5.1    | MIT OR Apache-2.0  | 快照校验              |
| `criterion`（dev）             | 0.8.2    | Apache-2.0 OR MIT  | 基准测试（MSRV 1.86）   |
| `proptest`（dev）              | 1.11.0   | MIT OR Apache-2.0  | 属性测试（分词一致性、round-trip）|
| `tempfile`（dev）              | 3.27.0   | MIT OR Apache-2.0  | 测试临时文件            |
| `cargo-deny`（工具）             | 0.20.2   | MIT OR Apache-2.0  | **依赖 License / 来源 / 重复版本检查** |

**建议增加 `cargo-deny`**：本项目对 License 友好性有明确要求，靠人工看 `Cargo.toml` 不可靠（传递依赖可能有数十个）。`cargo-deny` 可在 CI 里强制"只允许 MIT/Apache-2.0/BSD/BSL/CC0，出现 GPL 系列直接失败"，一次性解决合规问题。

---

### 4.12 明确不引入

| 库                            | 不引入的理由                                       |
| ---------------------------- | -------------------------------------------- |
| `tantivy`（主依赖）               | 手写是项目目的，见 4.1（仅作 dev 基线）                     |
| `dashmap`                    | 7.x 仍是 rc（稳定线 6.2.1）；v1 单写者 + `RwLock` 足够     |
| `sled` 1.0.0-alpha.124       | alpha 版本，且有 KV 需求也不用它                        |
| `redb` / `heed` / `lmdb-rs`  | 持久化路线是内存 + 快照                                |
| `whatlang`                   | 不做按语言路由；需要时 charabia 自带                      |
| `stop-words`                 | 一个词表不值得一个依赖，且中文需自维护                          |
| `fuzzy-matcher` / `strsim`   | 拼写纠错属 v2，且当前无需求                              |
| `axum` / `tokio`             | 服务层明确不做（需求文档第 7 章）                           |
| `statrs` / `ndarray`         | 评测指标自己算（约 50 行）；ndarray 由 fastembed 传递引入即可     |
| `candle-core`                | 已有 fastembed 封装，不直接用                         |

---

## 5. License 汇总与合规建议

### 5.1 引入清单的 License 分布

| License                | 代表 crate                                                        | 宽松？                    |
| ---------------------- | --------------------------------------------------------------- | ---------------------- |
| MIT                    | tantivy(dev)、jieba-rs、charabia、rkyv、bincode、zstd、quick_cache、tiktoken-rs、proptest、cargo-outdated | ✅                      |
| MIT OR Apache-2.0      | serde、thiserror、anyhow、clap、rayon、crc32fast、smol_str、memmap2、instant-distance、roaring、**ort** | ✅                      |
| MIT/Apache-2.0（双轨）      | `hnsw_rs`                                                       | ✅                      |
| Apache-2.0             | **fastembed**、usearch、simsimd、tokenizers                         | ✅                      |
| (MIT OR Apache-2.0) AND Apache-2.0 | `moka`                                                  | ✅                      |
| MIT/BSD-3-Clause       | `rust-stemmers`（v2 可选）                                          | ✅                      |
| BSL-1.0                | `xxhash-rust`（若引入）                                              | ✅（Boost 类，非 copyleft）  |
| CC0-1.0                | `just`（若用，仅构建工具）                                                | ✅（工具类，不构成分发风险）        |

**结论：推荐引入的全部依赖均为宽松许可，无 GPL/LGPL/AGPL 系列，无 SSPL/Commons Clause 等"伪开源"协议。**

### 5.2 非 crate 的 License（容易漏）

| 资产                        | License     | 说明                                          |
| ------------------------- | ----------- | ------------------------------------------- |
| ONNX Runtime 二进制（随 ort 下载） | **MIT**   | Microsoft；`ort/load-dynamic` 可改为链接系统库以规避二进制分发问题 |
| `BAAI/bge-small-zh-v1.5`  | **MIT**     | 上游原始模型（PyTorch / safetensors，无 ONNX）         |
| `Xenova/bge-small-zh-v1.5` | **⚠️ HF 未声明 → 已决策接受** | **fastembed 实际下载的就是这个**（ONNX 90MB）。上游 `BAAI` 为 MIT，转换仓未声明，按"沿用上游 MIT"接受（选项 A，2026-09-02 确认）。残留风险记录于需求文档 8.3 P5，P2 复核 |
| `BAAI/bge-m3`             | **MIT**     | 备选模型                                        |
| `BAAI/bge-reranker-v2-m3` | **Apache-2.0** | v2 Rerank 用（FR-18 尚未接入）                 |
| jieba 默认词典                | MIT（随 jieba-rs） | 若替换自定义词典需注意词典本身的授权                    |
| **T2Ranking 数据集**（清华 THUIR） | **Apache-2.0** | P5 评测数据源（2026-09-03 选定，选型对比见 `p5-design.md` 第 3 章）：30 万真实查询 / 230 万段落 / 4 级专业标注；HF 直下无注册墙。**使用义务**：转换产物入库时需附来源与论文引用（SIGIR 2023），见需求文档 9.4 |

### 5.3 合规建议

1. **本项目采用 `MIT OR Apache-2.0` 双许可**（Rust 生态惯例，与上游依赖兼容度最高）—— ✅ 已写入需求文档 8.1 已定约束
2. **`Cargo.lock` 必须入库**（当前仓库连 git 都没有，P0 T0-03 需处理）—— 这不仅是可复现构建的要求，更是 fastembed 精确锁定 `ort =2.0.0-rc.13` 的必然要求
3. **引入 `cargo-deny`**，配置白名单：`MIT, Apache-2.0, BSD-3-Clause, BSL-1.0, CC0-1.0, Zlib, ISC, Unicode-DFS-2016`，出现其他 License 直接失败
4. 若将来做二进制分发，**ONNX Runtime 的 MIT 声明需随附**；用 `ort/load-dynamic` 链接系统库可简化
5. 中文停用词表若从第三方导入，**需记录来源与授权**（建议自维护，避免这个麻烦）

---

## 6. ⚠️ MSRV 冲突（必须在 P0 决策）

需求文档 NFR-08 定的是 **Rust 1.80+**，而当前 stable 已是 **1.98.0（2026-08-20）**。核查发现多个候选依赖的 MSRV 已超过 1.80：

| crate                  | MSRV    | 是否超过 NFR-08 的 1.80 |
| ---------------------- | ------- | ------------------ |
| `bincode` 3.0.0        | 1.85    | ⚠️ 超                |
| `clap` 4.6.6           | 1.85    | ⚠️ 超                |
| `criterion` 0.8.2      | 1.86    | ⚠️ 超（dev）          |
| `smol_str` 0.3.6       | 1.89    | ⚠️ 超                |
| `cargo-deny` 0.20.2    | 1.88    | ⚠️ 超（工具）           |
| `roaring` 0.11.5       | 1.90    | ⚠️ 超（P4 候选）        |
| `redb` 4.2.0           | 1.90    | ⚠️ 超（不引入）          |
| `croaring` 2.7.0       | 1.95    | ⚠️ 超（不引入）          |
| `instant-distance` 0.6.1 | 1.58  | ✅ 兼容                |
| `jieba-rs` / `fastembed` / `arroy` / `hnsw_rs` | 未声明 | 需实测 |

**建议（三选一）**

| 方案                        | 内容                                    | 评价                                       |
| ------------------------- | ------------------------------------- | ---------------------------------------- |
| **A（推荐）** MSRV 提到 **1.90** | 覆盖除 croaring 外所有候选；用 `rust-toolchain.toml` 固定 | 简单；1.90 发布于 2026 年初，对使用者不算苛刻     |
| B 跟随 stable               | 只保证当前 stable（1.98+）能构建，不承诺 MSRV       | 最省心，但每次升级工具链都可能踩坑                        |
| C 坚守 1.80                 | 需把 clap/bincode/smol_str/criterion 全部锁到旧版本 | **不推荐**：为守一个两年前的旧版本，全面使用过时依赖，得不偿失 |

**落地动作**：修改需求文档 NFR-08（1.80+ → 1.90+），并在 `rust-toolchain.toml` 固定版本。

> ✅ **已采纳（方案 A）**：NFR-08 已改为 1.90+，架构文档新增 ADR-008，plan.md T0-03 增加 `rust-toolchain.toml` pin 1.90。

---

## 7. 对现有文档的修正建议

核查中发现 3 处需要修正的事实问题（**均已于 2026-09-02 修正**）：

### 7.1 架构文档 7.6 关于 bincode 的选型理由有误

**原文**：「序列化用 bincode 1.3：与 `instant-distance` 的 `with-serde` feature 内部依赖的 bincode 版本保持一致，避免引入两套编解码」

**事实**：`instant-distance` 0.6.1 的 `with-serde` feature **只引入 `serde` 和 `serde-big-array`，不含 bincode**（实测其依赖表：`num_cpus`、`ordered-float`、`parking_lot`、`rand`、`rayon`，optional 仅 `indicatif` / `serde` / `serde-big-array`）。

**影响**：bincode 版本可自由选择，不受任何约束。建议改为 `bincode 3.0.0`（当前最新，MIT），或稳妥起见 `2.0.1`。

### 7.2 架构文档 9.1 选型表版本过时

| 项              | 文档写的    | 实际最新                                  |
| -------------- | ------- | ------------------------------------- |
| `fastembed`    | 5.17.4  | **6.0.2**（两者依赖相同，可平滑升，但建议直接上 6.0.2） |
| `arroy`        | 0.5.0   | **0.8.0**（已排除，仅更新文档数字）                |
| `instant-distance` | 0.6.1 | 0.6.1（仍最新，无变化）                        |

### 7.3 R1 风险的应对可以从"绕开"改为"消除"

架构文档 7.5 与风险表 R1 把「instant-distance 无增量插入」当作既定约束，设计了 delta 区方案。建议补充：`hnsw_rs` 可在保持纯 Rust 的前提下原生支持增量插入（见 4.4），P4 阶段做一次 A/B，若达标则删除 delta 区设计。

---

## 8. 落地清单

> **状态：全部 10 条已于 2026-09-02 回写至各文档。**

| #  | 动作                                                                | 已回写位置                                                                 | 阶段   | 状态  |
| -- | ----------------------------------------------------------------- | --------------------------------------------------------------------- | ---- | --- |
| 1  | 添加 `tantivy` 为 **dev-dependency**，写 BM25 对照测试                      | 架构 ADR-007、9.1、12.1 测试清单；plan.md **T1-15**、阶段门槛、测试总表、附录 B             | P1   | ✅   |
| 2  | 添加 `unicode-segmentation` + `unicode-normalization`，替换手写 Unicode 分段 | 架构 7.1、9.1；plan.md T1-04、附录 B                                         | P1   | ✅   |
| 3  | 缓存实现改用 `moka`（替换 `lru`）                                            | 架构 8.1、9.1；plan.md T3-07、附录 B                                         | P3   | ✅   |
| 4  | P4 前做 `hnsw_rs` A/B 原型，决定是否删除 delta 区                              | 架构 7.5、9.2、ADR-002、R1；plan.md **T4-06a / T4-06b**、阶段门槛、附录 B             | P4   | ✅   |
| 5  | 快照 codec 先上 `bincode 3.0.0`，P4 末与 `rkyv` 实测对比                      | 架构 7.6、ADR-005、9.1；plan.md T4-01、附录 B                                 | P4   | ✅   |
| 6  | P5 增加分词对照实验（jieba+自研 vs charabia）                                  | plan.md **T5-09**、里程碑 P5 任务数（8→9）、进度追踪表                               | P5   | ✅   |
| 7  | NFR-08 的 MSRV 从 1.80 改为 1.90，并在 `rust-toolchain.toml` 固定           | 需求文档 **NFR-08**、8.1 已定约束、变更记录；架构 **ADR-008**、封面、9.1；plan.md T0-03、附录 A | P0   | ✅   |
| 8  | 引入 `cargo-deny` 并配置 License 白名单                                    | 架构 ADR-008、9.1、R10；需求文档 **NFR-09**；plan.md T0-06（`make deny`）、附录 A/B   | P0   | ✅   |
| 9  | 修正架构文档 7.6 的 bincode 理由、9.1 的版本号                                   | 架构 7.6、9.1、9.2、14.2 变更记录（v1.1）                                        | 随时   | ✅   |
| 10 | 项目 License 定为 `MIT OR Apache-2.0`；`Cargo.lock` 入库                  | 需求文档 8.1 已定约束（新增 3 行）；架构 ADR-008；plan.md T0-03                       | P0   | ✅   |

**本次回写涉及的文档版本**：`plan.md` v1.0→v1.1、`architecture-design.md` v1.0→v1.1、`requirements-spec.md` v1.0→v1.1。

**剩余未在文档中固化的部分**：`Cargo.toml` / `deny.toml` / `LICENSE-*` / `rust-toolchain.toml` 等**文件本身要到 P0 才创建**（工作区尚无代码），计划与验收标准已在 plan.md T0-03 / T0-06 中写明。

---

## 附录 A 核查方法与数据来源

所有版本号、License、MSRV、依赖构成均于 **2026-09-02** 实测拉取，非凭记忆填写：

| 数据         | 来源                                                                                          |
| ---------- | ------------------------------------------------------------------------------------------- |
| 版本 / License / MSRV | `https://crates.io/api/v1/crates/{name}`（取 `crate.max_version` 与最新 `version.license` / `rust_version`） |
| 依赖构成       | `https://crates.io/api/v1/crates/{name}/{version}/dependencies`                              |
| Feature 定义 | `https://crates.io/api/v1/crates/{name}/{version}` 的 `version.features`                      |
| API 能力     | `https://docs.rs/{crate}/{version}/`（逐页核实方法签名，如 `usearch::Index::add/remove`、`hnsw_rs::Hnsw::insert`、`DistDot` 文档原文） |
| 模型 License | `https://huggingface.co/api/models/{id}` 的 `cardData.license`                                |
| Rust 版本    | releases.rs（stable 1.98.0，2026-08-20 发布）                                                    |

**本文档应在每个大阶段开始前复查一次**（尤其 P4，向量索引选型可能变化）。复查只需重跑附录中的 API 调用。

## 附录 B 建议的最小依赖集（P1 阶段）

```toml
[dependencies]
# 文本处理
jieba-rs = "0.10"
unicode-segmentation = "1.13"
unicode-normalization = "0.1"
smol_str = "0.3"

# 工程基础
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
rayon = "1"
tracing = "0.1"

[dev-dependencies]
tantivy = "0.26"          # 仅作为 BM25 正确性对照基线，不参与构建产物
criterion = "0.8"
proptest = "1"
tempfile = "3"
```

（`fastembed`、`instant-distance`、`bincode`、`crc32fast`、`clap`、`moka` 在 P2~P4 按阶段加入。）
