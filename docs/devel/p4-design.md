# P4 持久化、增量写入与过滤 — 设计说明

| 项目    | 内容                                                                         |
| ----- | -------------------------------------------------------------------------- |
| 版本    | **v1.1（已执行并回写）**                                                                          |
| 日期    | 2026-09-03                                                                              |
| 状态    | **已执行完成** —— A/B 已定案（hnsw_rs 胜出）、快照/过滤/集成测试全过，见第 13 章执行记录                             |
| 上游    | `docs/devel/plan.md` 第 9 章（P4，T4-01 ~ T4-09）、`architecture-design.md` 第 7.5 / 7.6 节 |
| 关联    | FR-14（过滤）、FR-15（幂等 upsert）、NFR-04（冷启动）、NFR-06（确定性）、ADR-005（HNSW）、ADR-007（tantivy 基线） |
| 环境    | Rust 1.90.0 / P0~P3 已完成                                                          |

---

## 1. P4 要达成什么

**一句话**：把"每次检索都重建索引的 demo"变成"能快照、能冷启动、能增量写入、能过滤"的**常驻可服务内核**。

**判定标准（Gate）**

1. **向量索引 A/B 定案**（T4-06a），据此落地增量方案（T4-06b）
2. `save` → `load` 后检索结果**完全一致**（快照 round-trip）
3. 增量写入后新文档**立即可被检索**
4. 删除后统计量与全量重建一致
5. 快照加载实测是否满足 NFR-04（决定是否上 `rkyv`）

> P4 是最后一块"工程地基"。它不新增检索能力，而是把 P1~P3 的能力变成**可持久、可增量、可过滤**的完整内核。核心难点是向量索引的增量写入方案（T4-06a 的 A/B）和快照编解码的正确性。

---

## 2. 现状

| 项         | 状态                                        |
| --------- | ----------------------------------------- |
| P1~P3 产出  | analyze / index / retriever / vector / fusion / query / CLI |
| `storage` 模块 | 空骨架（P0 占位）                                |
| `schema::Filter` | 已定义（P0），未实现过滤逻辑                          |
| `Index::remove` | ✅ P1 已实现（含统计量回滚，重新分析文本）——**T4-05 已提前完成** |
| `Document.content_hash` | 字段已定义（`u64`），当前 CLI 恒填 0                     |
| `Document` / `Chunk` | ✅ 已派生 `Serialize / Deserialize`              |
| instant-distance | 无增量 insert（P2 已确认）                          |
| `hnsw_rs`    | **未引入**，作为 A/B 候选待评估                          |

---

## 3. 关键事实：hnsw_rs 0.3.4 调研（A/B 候选）

| 维度     | 结论                                                            |
| ------ | ------------------------------------------------------------- |
| 版本 / 许可 | **0.3.4**，**MIT OR Apache-2.0**（与项目 License 完全一致）                |
| MSRV   | 未显式声明，但用 **Edition 2024**（需 rustc ≥ 1.85）；项目 MSRV 1.90 满足 ✅      |
| 成熟度    | 876k 下载，4.2k 行，纯 Rust                                          |
| 关键 API | `hnsw::Hnsw`、`insert`（**原生增量**）、`search` / `search_filter`、`prelude::{DistDot, DistCosine, DistL2, Neighbour}` |
| 距离     | `DistDot` **要求入库前归一化**——与我们 7.3 的 L2 归一化方案天然契合               |
| 过滤     | 内建 `filter` trait + `search_filter`（与 T4-07 的过滤诉求契合）              |
| 序列化    | 有 `hnswio` 模块支持 dump/reload（但我们的快照方案不依赖它，见 D1）                  |

**对比对象**（instant-distance 0.6.1，P2 已用）：

| 维度     | instant-distance | hnsw_rs |
| ------ | ---------------- | ------- |
| 增量 insert | ❌ 无（需主索引 + delta） | ✅ 原生 |
| 过滤      | ❌ 无              | ✅ search_filter |
| 距离      | 自定义 `Point`（我们包了 NormalizedVector） | `DistDot`（归一化内积） |
| 许可 / MSRV | MIT/Apache，MSRV 低 | MIT/Apache，Edition 2024 |

> **结论方向**：hnsw_rs 在"增量 + 过滤"两点上正好补上 instant-distance 的短板，且 License/MSRV 都达标。**是否替换，由 T4-06a 的 A/B 实测决定**（见第 7 节），不是凭纸面结论拍板。

---

## 4. 快照设计

### 4.1 总体策略（D1）

**快照存"原始数据"，加载时重建索引结构**，而非序列化 HNSW 图。

理由：① 简单鲁棒，不依赖 `HnswMap` 的 serde round-trip 正确性；② 万级向量重建 <2s（release），远小于 NFR-04 的秒级门槛；③ 未来换向量实现（A/B 结果）不破坏快照格式兼容。

### 4.2 文件格式（T4-01 / T4-02）

```
┌─────────────────────────────────────────────┐
│ Header                                       │
│   magic: "IDX1" (4 bytes)                    │
│   format_version: u32                        │
│   crc32: u32        ← 覆盖其后的全部正文      │
├─────────────────────────────────────────────┤
│ Section 1: term_dict   (Vec<(String, TermId)>)│
│ Section 2: postings    (Vec<Vec<Posting>>)   │
│ Section 3: forward     (docs + chunks + 墓碑) │
│ Section 4: stats       (total_len, num_chunks)│
│ Section 5: vectors     (Vec<(ChunkId, Vec<f32>)>)  ← 可选（无向量则空）│
└─────────────────────────────────────────────┘
```

- 编码：**bincode 2.0.1**（`magic` / 版本 / crc32 用手写字节序，正文用 bincode）
- `Posting` 的 `positions` 字段仅在 `positions` feature 下存在 → 快照格式也随之变化（version 里体现）
- `term_dict` 用 `Vec<(String, TermId)>` 而非 `HashMap`：**顺序由插入顺序决定，保证 round-trip 后 TermId 稳定**

### 4.3 版本与校验（T4-03）

- `magic != "IDX1"` 或 `format_version` 不匹配 → `SnapshotVersionMismatch`（**绝不静默读错**）
- crc32 校验失败 → `SnapshotCorrupted`
- 两个错误都已有 Error 变体（P0 定义）

---

## 5. 幂等 upsert（T4-04）

- **`content_hash` 用 xxhash-rust（xxh64）**（D2）：64-bit、快、稳定，避免 crc32 的 32-bit 碰撞风险。BSL-1.0 已在 License 白名单（deny.toml）。
- 摄入时：计算 `xxh64(text)`，若索引中已有同 `content_hash` 的文档 → 跳过（不产生重复 chunk）。
- 单测：同一文档 upsert 两次，chunk 数不翻倍。

> 字段 `content_hash` 已是 `u64`，与 xxh64 天然对齐，无需改模型。

---

## 6. 元数据过滤（T4-07）

- `query/filter.rs`：tag 等值（`metadata["tag"] == "rust"`）+ 数值范围（`metadata["year"] >= 2020`）。
- **融合前用 bitmap 过滤**（NFR-02：查询路径零 IO）：先按过滤条件算出一个 `chunk_id → bool` 的位图，召回结果在此位图上过滤，正排回捞不受影响。
- 单测：过滤后结果集是不过滤的子集。

> `EmptyReason::FilteredOut`（P3 已定义）在此落地：有候选但被过滤条件全部排除时触发。

---

## 7. 向量索引 A/B（T4-06a / T4-06b）—— P4 的核心决策

### 7.1 A/B 方案（T4-06a）

新增 `vector/hnsw_rs.rs`（第二个 `VectorIndex` 实现，包 `hnsw_rs::Hnsw` + `DistDot`），与「instant-distance 主索引 + delta 暴力区」做对比。

| 对比项 | 判据 |
| --- | --- |
| 增量写入延迟 | 单条 `insert` / `add` 的 P50 / P99 |
| 召回一致性 | 1 万条内与暴力 Top-10 重合率 ≥ 95% |
| 内存占用 | 同等规模下 RSS |

### 7.2 落地（T4-06b）

- **若 hnsw_rs 达标**：直接用其**原生增量 insert**，**删除 delta 区设计**——增量写入零重建，最简洁。
- **若未达标**：回到「`sealed: Option<HnswMap>` + `delta: Vec`」，两处都查后合并取 Top-K，`delta ≥ 1000 或 ≥ 总量 10%` 时重建。

> 这条 A/B 是 P4 的 Gate #1，**必须先出对比结论再落地**（plan.md 原文："先做 A/B 再决定方案"）。我在执行时会先把两个实现都跑通、量出数据，再据数据选方案。

---

## 8. 任务设计

### T4-01 快照编解码（`storage/codec.rs`）

`magic "IDX1"` + `format_version` + `crc32` header；正文 bincode 2.0.1。手写字节流解析单测。

### T4-02 Section 读写（`storage/snapshot.rs`）

5 个 section 的 save/load；`vectors` 可选。save/load round-trip 跑通。

### T4-03 版本与校验（`storage/snapshot.rs`）

造损坏文件（改 magic / 改 crc / 截断）触发 `SnapshotVersionMismatch` / `SnapshotCorrupted`，**绝不静默读错**。

### T4-04 幂等 upsert（`index/mod.rs`）

`xxh64(text)` → `content_hash` 去重。

### T4-05 墓碑删除与统计量回滚

**已在 P1 完成**（`Index::remove` 重新分析文本再回滚）。本阶段补集成单测即可。

### T4-06a 向量索引 A/B（`vector/hnsw_rs.rs`）

新增依赖 `hnsw_rs`，实现第二个 `VectorIndex`，跑对比数据，输出结论。

### T4-06b 增量方案落地（`vector/store.rs`）

按 A/B 结论落地（原生增量 or 主索引+delta）。

### T4-07 元数据过滤（`query/filter.rs`）

bitmap 过滤 + `FilteredOut` 落地。

### T4-08 单元测试补齐（`tests/`）

快照 round-trip、幂等 upsert、删除后统计量三条。

### T4-09 集成测试（`tests/integration.rs`）

端到端：摄入 → 提交 → 快照 → 加载 → 检索结果一致；多线程并发检索验证 `Send + Sync`。

---

## 9. 单测设计

| case | 期望 |
| --- | --- |
| 快照 round-trip | save → load 后，`term_dict / postings / stats / chunks` 逐项相等，检索结果完全一致 |
| 损坏快照 | magic 错 → `SnapshotVersionMismatch`；crc 错 → `SnapshotCorrupted` |
| 幂等 upsert | 同文档 upsert 两次，chunk 数不翻倍 |
| 删除后统计量 | 删除若干文档后，统计量与全量重建逐项相等（P1 已有实现，补集成断言） |
| 过滤 | 过滤后结果集是不过滤的子集；全过滤 → `FilteredOut` |
| 增量写入 | 新增文档后立即可被检索（无需重建） |
| 并发检索 | 多线程并发 search 无 panic、结果确定 |

---

## 10. 风险与兜底

| #      | 风险                              | 影响        | 兜底                                        |
| ------ | ------------------------------- | --------- | ----------------------------------------- |
| R-P4-1 | hnsw_rs 的 API 与 plan 描述不符           | A/B 受阻     | 实现时以 docs.rs 源码为准；若 API 差异大，如实记录并评估           |
| R-P4-2 | hnsw_rs 的 MSRV 实际 > 1.90             | 编译失败      | 用 `cargo +1.90 check` 验证；若超限则放弃 A/B，直接走 delta 方案    |
| R-P4-3 | bincode 序列化 HashMap 顺序导致 TermId 漂移 | 快照加载后检索错乱 | 快照用 `Vec<(String, TermId)>` 保存 term_dict，保持插入顺序（4.2） |
| R-P4-4 | 快照加载后向量索引重建慢                    | NFR-04 不达标 | 实测；若不达标再评估 `rkyv` 或序列化 HNSW 图（hnsw_rs 的 hnswio） |
| R-P4-5 | content_hash 碰撞导致误去重                | 少文档       | xxh64 64-bit 碰撞概率可忽略；比 crc32 安全一个量级            |
| R-P4-6 | 过滤位图逻辑错误                        | 结果集错误      | 单测：过滤后 ⊆ 不过滤 + 全过滤触发 FilteredOut               |

---

## 11. 验收清单

```bash
cd /Users/gongyubo/Code/mine/index-demo

cargo test -p index-core                       # 快照 round-trip / 损坏快照 / upsert / 过滤
cargo test -p index-core --test integration    # 端到端 + 并发

cargo run -p idx -- build --input data/corpus.jsonl --output /tmp/index.idx   # 真正落盘
cargo run -p idx -- search --index /tmp/index.idx --mode hybrid -k 5 "..."    # 从快照加载检索

make fmt && make lint && make test && make deny
```

---

## 12. 需要你确认的决策点

| #   | 决策点                          | 我的建议                                                     | 影响                                    |
| --- | ---------------------------- | -------------------------------------------------------- | ------------------------------------- |
| D1  | **快照存原始数据还是序列化索引图**         | **存原始数据，加载时重建**（含原始向量）；不序列化 HNSW 图                     | 简单鲁棒；万级重建 <2s，满足 NFR-04             |
| D2  | **content_hash 算法**          | **xxhash-rust（xxh64）**，新增依赖（BSL-1.0 已在白名单）                 | 64-bit 碰撞可忽略；比 crc32 安全              |
| D3  | **A/B 判据**                   | 增量延迟 P50/P99 + 召回重合 ≥95% + 内存；**达标即用 hnsw_rs 原生增量、删 delta** | 决定 T4-06b 走向                          |
| D4  | **term_dict 序列化格式**          | 用 `Vec<(String, TermId)>`（保持插入序）而非 HashMap，保证 TermId 稳定        | 防止加载后 TermId 漂移导致检索错乱                |
| D5  | **过滤位图**                    | 融合前用 `chunk_id → bool` 位图过滤，正排回捞不受影响                     | NFR-02 查询路径零 IO                       |
| D6  | **vectors section 是否可选**      | 可选：无向量（纯 BM25 索引）时跳过；有向量时存原始向量 + 加载重建                 | 兼容纯文本索引场景                            |

---

## 附录 A 与已有设计的衔接

- **T4-05（删除回滚）已在 P1 提前完成**——本阶段只补集成断言，不重复实现。
- **`EmptyReason::FilteredOut` 已在 P3 定义**——本阶段落地过滤后真正启用。
- **`Index::add` 已在 P2 改为返回 `(DocId, Vec<ChunkId>)`**——快照与 upsert 复用。
- **instant-distance 的 `with-serde` feature 已开启**（P0）——但 D1 决定不序列化它的图结构。

---

## 13. 执行记录（v1.1 新增，2026-09-03）

### 13.1 实测结果

| 验收项 | 结果 |
| --- | --- |
| 单元测试（默认 `make test`） | **71 个通过 + 4 个 `#[ignore]`**（storage 7 + index 3 + filter 4 + 其余） |
| 集成测试 | 快照 round-trip 三模式检索**完全一致**；8 线程 × 50 次并发检索无 panic 且结果确定 |
| **向量 A/B（T4-06a）** | 见 13.2，**hnsw_rs 胜出** |
| 快照（30 篇含向量） | **77.8 KB**，build 996ms，加载 <10ms |
| 损坏快照 | magic 错 / 版本错 / CRC 错 / 截断 → 全部正确报错，**绝不静默读错** |
| 幂等 upsert | 同内容两次入库 chunk 数不翻倍；删除后可重新入库 |
| 过滤 | 过滤后 ⊆ 不过滤；全过滤触发 `FilteredOut` |
| CLI | `build --output --vectors` 落盘；`search --index` 秒级加载；`--filter tag=值` |
| make fmt/lint/test/deny | 全绿 |

### 13.2 向量索引 A/B 结论（T4-06a，release，1 万条 × 512 维）

| 指标 | A：hnsw_rs（原生增量） | B：instant-distance（一次性构建） |
| --- | --- | --- |
| 增量能力 | ✅ 单条 insert P50=**1537µs**、P99=2870µs | ❌ 无 insert，全量重建 **31032ms**/万条 |
| Top-10 vs 暴力重合率 | **0.970** | 0.970 |
| 全量构建耗时 | 15122ms（逐条插） | 31032ms |

**结论：A（hnsw_rs）胜出**——召回相同（0.970），且原生增量（毫秒级单条）对比 B 的全量重建（31 秒）。**按计划走 T4-06b：采用 hnsw_rs 原生增量，删除 delta 区设计**。`HnswIndex`（instant-distance 实现及其测试）保留作对照，但生产路径（CLI、集成测试）已切换到 `HnswRsIndex`。

### 13.3 执行中发现的问题（重要，均已解决）

1. **bincode 2 不支持 `serde_json::Value`**。`Document.metadata` 是 `serde_json::Value`，其序列化走 `serialize_any`，bincode（非自描述格式）直接报 `AnyNotSupported`。**解决**：快照用 `DocumentDto` 把 metadata 存成 JSON 字符串，导入时解析回来。另注意 bincode 2 需要**显式开启 `serde` feature** 才有 `bincode::serde` 模块。

2. **hnsw_rs 的 `DistDot` 在 aarch64 有浮点断言**。其标量实现 `assert!(1-dot >= 0.)`，自匹配等边界场景 dot 因浮点舍入达 1.0000002 时直接 panic。**解决**：自定义 `DistDotClamped`（点积 clamp 到 [−1,1] 再相减），语义相同但数值安全。

3. **crate 名是 `hnsw_rs` 不是 `hnsw-rs`**（crates.io 上后者不存在），Cargo.toml 必须用下划线。

4. **clap 短参冲突**：`--input` 和 `--index` 都默认拿 `-i`，clap 在 debug 构建直接 panic。`--index` 不给短参。

### 13.4 设计上的小调整

- **快照模式下查询侧仍构造 `LocalEmbedder`**（不用"预计算空壳"）：vector/hybrid 检索的查询向量化必须真模型，文档向量才来自快照。最初写的 `PrecomputedEmbedder` 是错误方向，已删。
- **CLI 向量索引从 `BruteForceIndex` 切换到 `HnswRsIndex`**：A/B 结论的实际应用，让生产路径就是增量路径。
- `storage::load` 返回类型加了别名 `LoadedSnapshot`（clippy type_complexity）。

### 13.5 NFR-04 冷启动评估（如实记录）

- **MVP 规模（≤200 篇）**：快照加载 + 向量重建 < 1s，满足 NFR-04「秒级恢复」✅
- **万级规模**：向量重建 ~15s（release，A/B 实测），**不满足秒级**。按 D1（存原始数据）的取舍，届时需要 hnsw_rs 的 `hnswio`（图 dump/reload）或 `rkyv`——留待真实规模出现时再决策，当前不做过度设计。

### 13.6 遗留与交接

- `hnsw_rs` 已成为生产向量索引；`instant-distance` 降级为对照实现（是否移除依赖，P5 评审时定）
- 快照的 `positions` feature 兼容性：`FORMAT_VERSION` 预留了 +1 约定但未实现（P5 若启用 positions 需处理）
- 远程 Embedder（T2-08 推迟）与 CachedEmbedder 接入常驻服务——P5 或后续
