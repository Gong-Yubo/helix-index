# HelixIndex V2 · Step 4 详细设计（资源回收：墓碑物理回收 compaction）

> 面向 Agent 场景的通用检索引擎内核——V2 Step 4 的详细设计（**v0.1，待评审**）。
> 本文件回答：删改之后哪些体积在涨、涨在哪个字节、怎么一次性回收干净、
> 重建图之后怎么保证不踩「manifest 未重发」的铁律、以及怎么证明回收前后检索语义没变。

| 项 | 内容 |
| --- | --- |
| 版本 | **v0.3（拍板版，可开工）** |
| 日期 | 2026-09-08 |
| 状态 | **D-S4-01 / D-S4-10 已拍板（2026-09-08）**；D-S4-02~09 评审无异议、按本文建议执行。核心实现（S4-03~S4-07）可开工 |
| 修订记录 | v0.1 首版：三条膨胀路径逐行盘点 + 重新物化方案 + 9 个待拍板决策。<br>**v0.2（PR #28 评审回应）**：新增 **D-S4-10 `compact()` 与写缓冲 `pending` 的交互**（评审 P0：不先 flush 会让 stale chunk_id 污染 `raw_vectors` 与新图，采纳「先 `commit()`」修法，见 §4.1.1 / I8 / T13）；验收 3 的 `nb_point` 口径改为「存活**且有向量**」（§1.3）；D-S4-04 理由重写 + `graph_status` 语义补齐（§4.6 / 附录 A）；§4.3 明确六步作用于**新 `Index` 实例**（对齐 I5）；§4.4 「≤/≥」两层比较分开写；D-S4-03 的确定性改称「**重建确定性**」（独立于 NFR-06）；`CompactionReport` 并入字节级三体积（§4.7）。<br>**v0.3（决策拍板）**：**D-S4-01 = 重编号（方案 B）**、**D-S4-10 = `compact()` 开头先 `self.commit()?`（修法 A）** 两项正式拍板 ⇒ 从「待拍板」移入「已定案」，§9.2 的 Q1 / Q7 结案 |
| 上游 | `plan-v2.md` §4 Step 4（原 Step 5，D-J10 提前）/ issue #21 / `requirements-spec.md` v1.9（FR-30 Should）/ `architecture-design.md` §7.5、§14.1（R22、R25）/ `v2-step1-design.md`（D-S1-01 存活单一真源）/ `v2-step2-design.md`（ADR-A 方案 C）/ `v2-step3-design.md`（`atomic_write`） |
| 范围 | T7-12 墓碑物理回收（重建向量图 + 回收 `raw_vectors` + 重写快照）+ 零散写入 workload 脚本 + CLI/bench 观测入口 |
| 非范围 | 真·物理删除 API（`usearch`，架构 §7.5 已排除）；读写并发下在线 compaction（**Step 8**）；低选择度兜底与 `Metrics` 可观测（**Step 5**）；`parallel_build` 默认值翻转（横切 **T7-21**）；R22「图体积 1.6×」本身（本 Step 只回收墓碑，不压缩存活数据） |

---

## 决策速览：D-S4-01 ~ D-S4-10

> 评审者时间有限时先看这张表。每条在 §5 有完整取舍与证据。
> **v0.3 拍板结果**：**D-S4-01 = 重编号（方案 B）**、**D-S4-10 = `compact()` 开头先 `self.commit()?`**
> ——两项已定案，从「待拍板」移出；其余 D-S4-02~09 评审无异议，按本文建议执行。

| # | 决策 | 结论 | 状态 | 阻塞谁 |
| --- | --- | --- | --- | --- |
| **D-S4-01** | 要不要**重编号** `ChunkId` / `DocId`（这是能否压掉快照正文墓碑槽位的分水岭） | **重编号（方案 B）**。不重编号则 `docs` / `chunks` / `chunk_lens` 的空洞无法消除，验收「快照正文不无界增长」**不可能**达成；替代方案「复用空槽（free list）」被否决——ID 复用会让外部旧引用**静默指向错误内容**，比 ID 变更更危险 | ✅ **已拍板**<br>2026-09-08 | ~~阻塞全 Step~~<br>已解除 |
| **D-S4-02** | 触发方式：手动 / `save` 自动 / 混合 | **手动为主 + 阈值告警**。`helix compact` 显式调用；`save()` 在墓碑比例超阈值时只 **eprintln 告警**（NFR-07 风格可观测），自动 compaction **默认关**。理由：自动会把 10~100s 的重建塞进 `save()`，让写路径耗时不可预测，而 `save` 是 NFR-03/04 口径旁边的敏感点 | ⏳ 评审无异议 | 阻塞 S4-07 |
| **D-S4-03** | 词表里的**死词**（postings 已空但 term 仍在 `term_dict`）是否一并压实 | **压实并 remap TermId**。死词是纯浪费（词串 + 一条空链）；TermId 是**内部**编号（快照里 `term_dict` 与 `postings` 一起导出/导入），remap 不影响外部语义 | ⏳ 评审无异议 | 阻塞 S4-03 |
| **D-S4-04** | compaction 与 `save` 的关系 | 提供 `compact()`（纯内存，不落盘）+ `compact_and_save(path)`（= compact + 既有 `save`）。**铁律由此自动满足**：manifest 重发是 `save()` 的既有步骤，compaction 不再另开一条落盘路径。`Index::compact` 取 `pub(crate)` 是**结构性**理由（它与 `raw_vectors` / 向量索引的替换必须原子发生），门面层 `SearchIndex::compact` 是**有意保留**的内存路径，**落盘入口唯一 = `compact_and_save`**（§4.6） | ⏳ 评审无异议 | 阻塞 S4-07 |
| **D-S4-05** | `flush()` 侧幽灵向量（remove 早于 flush ⇒ 防线①失效，§2.3）是否本 Step 修 | **修**（S4-01，独立小 PR）。它是 Q-C2 同源，且补上后「不经 compaction 也不泄漏」 | ⏳ 评审无异议 | 阻塞 S4-05 验收 |
| **D-S4-06** | workload 规模（10K / 100K）与向量来源 | **默认 10K + 合成 Embedder**（确定性 LCG，dim=512，无模型依赖、秒级）；100K 真实语料标为**扩展验证**（需 embed ≈30min、内存 2×）。体积与回收的判据不依赖语义相关性，故用合成向量是划算的 | ⏳ 评审无异议 | 阻塞 S4-09 |
| **D-S4-07** | 重建图时是否沿用装配的 `parallel_build` | **沿用**（默认串行）。与 `load` 降级重建走同一条 `add_batch`，行为一致；compaction 是低频操作，不值得为此引入非确定性拓扑 | ⏳ 评审无异议 | 阻塞 S4-06 |
| **D-S4-08** | 顺带做 `remove_many(&[DocId])`（架构 §7.5.2 已预留） | **可选**（S4-02，timebox 内）。价值是把「批量删 N 篇 O(N·M)」降到 O(N+M)；与本 Step 主题相邻但非必需 | ⏳ 可选 | 无 |
| **D-S4-09** | ID 变更如何对外声明 | **三处**：`CompactionReport` 字段 + `user-guide.md`「已知坑」一节 + `SearchIndex::compact` 的 rustdoc 显式写「跨 compaction 的持久引用请用 `source` / `content_hash`，不要用 `doc_id` / `chunk_id`」 | ⏳ 评审无异议 | 阻塞 S4-10 |
| **D-S4-10** | `compact()` 与写缓冲 `pending` 的交互（**v0.2 新增，评审 P0**） | **`compact()` 开头先 `self.commit()?`**。否则 `pending` 里仍是旧 chunk_id，紧随的 `save()` → `flush()` 会把 stale id 灌进 `raw_vectors` 与新图。三种修法中它最干净：纯 BM25 下 `pending` 恒空 ⇒ flush 是 no-op；与 S4-01 的 liveness 过滤天然衔接；`save` 的隐式 commit 变 no-op | ✅ **已拍板**<br>2026-09-08 | ~~阻塞 S4-03~S4-07~~<br>已解除 |

---

## 1. 目标与验收

### 1.1 要解决的问题（plan-v2 §4 Step 4 / Q-C2）

**Q-C2（中 · 资源）**：墓碑 chunk 永久保留，不物理回收。Step 1 已证实该问题**跨快照永续**——
`SearchIndex::remove` 后 `save()` 原样带走已删向量、`load_with` 全量重灌（`search/index.rs`
注释「会被丢弃」实际不成立）。

Step 1 补上了**正确性**（存活位图谓词挡掉幽灵候选，FR-26 达成），但**资源**层面一条都没回收：

- 向量图里被删的点还在（`hnsw_rs` 无 remove API，架构 §7.5）；
- 图 sidecar 每 `save` 一次就把这些点再写一遍；
- 下次 `load` 从 sidecar 把它们读回来 ⇒ **墓碑跨快照累积**；
- 快照正文里 `docs` / `chunks` 的 `None` 槽位、`chunk_lens` 的死条目、词表里的死词，同样只增不减。

### 1.2 对应需求

| 需求 | 内容 | 本文动作 |
| --- | --- | --- |
| **FR-30（Should）** | 墓碑物理回收（compaction）：长期膨胀治理——回收向量图 + `raw_vectors` + 快照 | 全文 |
| FR-26（Must） | 向量软删除 + 存活过滤 | 不动语义；compaction 后仍需「删除的永远不回来」 |
| FR-16 / FR-29（Must） | 快照 / 图持久化 | compaction 产出的是**更紧凑的普通快照**，`FORMAT_VERSION` 不变 |
| FR-31（Must） | 原子快照 | compaction 落盘复用 `atomic_write`，不引入新的崩溃一致性机制 |
| NFR-04 | 完整冷启动 < 2s | compaction 后必须 `GraphStatus::Loaded`（铁律），否则冷启动退回 ≈10s |
| NFR-06 | 同一快照两次加载逐位一致 | 不受影响（compaction 不是加载路径）；但重建图引入的拓扑抖动需说明（§9 R29） |
| NFR-07 | 降级可观测 | 新增 `TombstoneStats` / `CompactionReport`；`save` 阈值告警 |

### 1.3 验收标准（Step 4 完成的定义，可证伪）

1. **三体积不无界增长**（issue #21 主验收）：在可复现的「零散写入」workload 下
   （N 轮 × 每轮删 X% + 追加 X%，§4.8），**图 sidecar（`.hnsw.graph` + `.hnsw.data`）、
   `raw_vectors` 条数、快照正文字节数**三者：
   - compaction 前随轮次**单调增长**（证明问题真实存在）；
   - 每轮 compaction 后回落到「存活集规模 ±10%」；
   - **第 k 轮 compaction 后的体积与第 1 轮无显著差异**（证明不累积，而非只是变慢）。
2. **compaction 后 `GraphStatus::Loaded`，不是 `Rebuilt`**——必须**重新 `load` 后**断言
   （铁律验收点，架构 §7.5.2）。
3. **图点数归位**：新 manifest 的 `nb_point` **等于**快照中「**存活且有向量**」的 chunk 数
   （不再是「≥」；`load_graph_checked` 步骤 6 的不等式对 compaction 产物退化为等号）。
   ⚠️ **v0.2 修正口径**：不能直接写「== 存活 chunk 数」——§4.4 明确不要求 `raw_vectors`
   覆盖全部存活 chunk，缺向量的存活 chunk 在重建图时缺席，此时 `nb_point` **小于**存活数。
   两层比较见 §4.4。**§4.8 的 churn workload 保证全部存活 chunk 都有向量** ⇒ 在该 workload 下
   本条退化为「`nb_point` == 存活 chunk 数」。（对齐验收 5 的「有向量时」限定。）
4. **检索语义不变**：compaction 前后，同一组 query 的
   - BM25 路 Top-K `(source, text, score)` 序列**逐位一致**（§4.9 有证明骨架）；
   - 向量 / hybrid 路 oracle 重合率 **≥ 0.99**（Step 2 T13 同口径，允许 HNSW 重建的拓扑抖动）。
5. **无墓碑残留**：compaction 后 `docs` / `chunks` 中无 `None`；`chunk_lens.len()` == 存活 chunk 数；
   `term_dict` 中无空 postings 链；`raw_vectors.len()` == 图点数 == 存活 chunk 数（有向量时）。
6. **崩溃安全**：compaction 的落盘走 `atomic_write`，注入崩溃后 `load` 要么旧快照要么新快照，
   **绝不 `SnapshotCorrupted`**（复用 S3-T6 骨架，在 compact 路径再跑一遍）。
7. **可观测 + 可复现**：`TombstoneStats` / `CompactionReport` 有 CLI 输出与单测；
   workload 脚本进 `scripts/`（`scripts/eval_churn.sh` + example `churn_bench`），
   fixture 用 `data/synth-10000-corpus.jsonl`。

---

## 2. 现状与问题定位

### 2.1 三条膨胀路径的逐行盘点

| 路径 | 落点 | 现状（源码级） | 单位成本（512 维） |
| --- | --- | --- | --- |
| **① 向量图（内存 `Hnsw` + sidecar 两文件）** | `vector/hnsw_rs_index.rs`、`vector/persist.rs` | `VectorIndex` trait 只有 `add` / `add_batch`，**无 remove**（`vector/mod.rs:31-83`）；`file_dump` 写全量点（`persist.rs:177`）⇒ 墓碑随每次 `save` 再写一遍；`load_graph` 原样读回 ⇒ **跨快照累积** | **≈2.6 KB/点**（Step 2 实测 12K：graph 7.9~8.1MB + data 23.7MB ⇒ 31.6MB / 12K） |
| **② `raw_vectors`** | `search/index.rs:143` | `SearchIndex::remove` 已 `raw_vectors.retain(is_live_chunk)`（`index.rs:419-424`）⇒ **内存已回收**；⚠️ 但 §2.3 的时序缺口会让已删 chunk 的向量重新进来 | 512×4 = **2 KB/条**（但已被回收） |
| **③ 快照正文** | `storage/snapshot.rs:19-28`、`index/mod.rs:62-77` | `docs: Vec<Option<DocumentDto>>` / `chunks: Vec<Option<Chunk>>` 的 `None` 槽（bincode Option tag **1 B**）；`chunk_lens: Vec<u32>` **push-only、remove 不回收**（`index.rs:193` 只 push）⇒ **4 B/条**；`term_dict` 的死词（`InvertedIndex::remove` 只摘 posting 不摘 term，`inverted.rs:61-72`）⇒ 词串 + 空链 | **≈5~6 B/chunk** + 死词（视词汇） |
| 倒排 postings | `index/mod.rs:236` | ✅ `Index::remove` 物理摘除 | 0 |
| `content_hashes` | `index/mod.rs:213` | ✅ `Index::remove` 摘除 | 0 |
| 字段索引 | `field_index` | ✅ 不入快照，每次 `import` 重建 | 0 |

### 2.2 跨快照永续的因果链

```
remove(doc)  ──► Index::remove 墓碑化 + raw_vectors.retain   （内存里 raw 干净了）
             ──► 但 Hnsw 图里的点还在（无 remove API）
save()       ──► 写快照正文（含 None 槽 / chunk_lens 死条目 / 死词）
             ──► dump 图（含墓碑点）──► 算 CRC ──► 发布 manifest
load()       ──► 图 sidecar 命中 ⇒ 墓碑点被读回内存（nb_point ≥ 存活数）
remove+add   ──► 墓碑累积一层，新点再加一层
save()       ──► 图再大一圈  ⇒  每轮单调增长，永不回落
```

`GraphManifest.nb_point` 的注释已经写明「**含墓碑**，故 >= 快照 vectors 条数」
（`storage/graph.rs:154`），`load_graph_checked` 步骤 6 也按「≥」校验（`persist.rs:310-316`）——
**设计当初就接受了墓碑会留在图里**，本 Step 是把「≥」重新变回「=」。

### 2.3 新发现：`remove` 早于 `flush` 时，防线①失效

架构 §7.5.2 的两道防线中，防线①是「`remove` 主动摘除 `raw_vectors`」。它有一个时序缺口：

```rust
// search/index.rs:280  add() 攒够 batch_size（默认 64）才 flush
if self.pending.len() >= self.cfg.batch_size { self.flush()?; }

// search/index.rs:328-339  flush() 把 pending 全部灌进 raw_vectors 与向量索引，
// ⚠️ 全程没有 liveness 检查
let items: Vec<_> = self.pending.iter().zip(vecs).map(|(p, v)| {
    let nv = NormalizedVector::new(v);
    if let Some(raw) = self.inner.raw_vectors.as_mut() {
        raw.push((p.chunk_id, nv.as_slice().to_vec()));   // ← 已删 chunk 也 push
    }
    (p.chunk_id, nv)
}).collect();
vi.add_batch(&items)?;                                     // ← 已删 chunk 也进图
```

⇒ **时序：add(doc) → remove(doc) → commit()/save()**（默认 `batch_size=64` 下极其常见，
CLI 逐条 `add` 后删除就是这条路径），已删 chunk 的向量会进 `raw_vectors` 与 HNSW 图，
并随之落盘、跨快照永续。

**影响定性**：这是**资源问题，不是正确性问题**——存活位图仍在检索期把它们挡掉（FR-26 不破，
`GraphStatus` 也不受影响）。但它让「删除后体积不降」更明显，且直接污染验收 1 的 `raw_vectors` 口径。

**修复取舍（S4-01）**：`flush` 时**先整批 embed、再按 liveness 过滤入库**
（`raw_vectors.push` 与 `add_batch` 之前过滤），**不改变 `embed_documents` 的入参组成**。
理由：`fastembed` 的推理是否受 batch 组成影响未经实测（Step 2 的教训是「别赌」），
而「白算一条 embed」的代价只是时间。若后续实测证明 batch 组成无关，可再改「先过滤再 embed」
（列 §9 未决 Q5）。

### 2.4 量级对比：为什么图是大头，但正文也必须堵

以 10 万 chunk、1 万次删改为例（按 §2.1 单位成本外推，**均为估算，待 §4.8 workload 实测替换**）：

| 项 | 1 万次删改的残留 | 占比 |
| --- | --- | --- |
| 图 sidecar | 1 万 × 2.6 KB ≈ **26 MB** | ≈ 99.8% |
| 快照正文（槽位 + `chunk_lens`） | 1 万 × ~6 B ≈ **60 KB** | ≈ 0.2% |

⇒ **图是绝对大头（约 440:1）**。但验收明确写了「三者均不无界增长」，且正文的压实是
**同一趟重写的边际成本**（都已经在重建索引了，顺手 remap 即可）⇒ 一起做。
这一对比也决定了：**如果 timebox 不够，S4-a（图重建）可单独先合**，它解决 99.8% 的体积。

---

## 3. 设计约束（来自既定事实，本文不重新论证）

| 约束 | 出处 | 对本 Step 的含义 |
| --- | --- | --- |
| **图 = 快照的派生缓存，manifest 是唯一原子发布点** | ADR-A / `v2-step2-design.md` v0.3 | compaction 重建图后**必须重发 manifest**；旧图文件可随时丢弃 |
| **存活状态的单一真源在 `Index.forward`**（向量侧不加墓碑） | D-S1-01 / 架构 ADR-010 | compaction 的存活集**只能**来自 `Index.alive_chunks()`，不得另设一套 |
| **写路径已有 `atomic_write`** | Step 3 / `storage/atomic.rs` | compaction **不需要**新的崩溃一致性机制：内存重建 + 一次既有 `save` 即可 |
| **`FORMAT_VERSION` 保持 2** | Step 2 / `codec.rs:24` | compaction 不引入新格式：产物是「更紧凑的普通快照」，双向兼容 |
| **`hnsw_rs` 无 remove API** | 架构 §7.5 | 「回收」只能是**重建**，不能是「原地摘点」 |
| **`remove` 每次全量 `retain`，批量删是 O(N·M)** | 架构 §7.5.2 | 已知代价；`remove_many` 为可选 S4-02 |
| 单写者语义（无并发写同一快照） | Step 3 / C5 | compaction 期间独占 `SearchIndex`，无需考虑并发读者（**Step 8** 才解决） |

---

## 4. 详细设计

### 4.1 总览：一次「按存活集重新物化」

```
SearchIndex::compact()
  ├─ 0. self.commit()?  ← 【v0.2 新增，D-S4-10】必须先清空写缓冲（见 §4.1.1）
  ├─ 1. 取存活集：Index.alive_chunks()（唯一真源）
  ├─ 2. 建 ID 映射：old_chunk → new_chunk（稠密、保序）、old_doc → new_doc
  ├─ 3. Index::compact()：正排稠密化 + 倒排 remap + 词表压实 + chunk_lens/content_hashes/字段索引
  ├─ 4. raw_vectors：过滤死 chunk + remap
  ├─ 5. 向量索引：从 raw_vectors 全量重建 HnswRsIndex（沿用 ef_search / parallel_build）
  └─ 6. 返回 CompactionReport（before/after 条数、耗时、回收量）

SearchIndex::compact_and_save(path) = compact() + 既有 save()
  └─ save() 内：commit()（no-op，pending 已空）→ save_with_crc（atomic_write）
                → dump 新图 → CRC → 发布新 manifest  ← 铁律自动满足
```

**三条设计论点**（先立论，再展开）：

1. **不需要新的崩溃一致性机制**。compaction 只在内存里重建，落盘复用 Step 3 的
   `save_with_crc`；中途崩溃 = 旧快照完好（原子协议下 tmp 永不权威）。
   本 Step 唯一要新增的是**崩溃前的内存重建失败**处理——重建过程中若 `Err`，
   必须保证 `SearchIndex` 不被留在半压实状态（见 §4.9 不变式 I5）。
2. **不需要新格式**。`FORMAT_VERSION` 保持 2，compaction 产物与「从头建一个同样的库」
   在字节层面不同（chunk_id 不同），但与「普通快照」结构上完全一样 ⇒ 旧版本能读新快照。
3. **不引入 `usearch`**。「物理删除」的需求由「重建图」满足，代价是 O(存活数) 的重建时间
   （12K 实测建图 ≈10~11.5s ⇒ 约 0.85 ms/点；**100K 预估 60~100s，待实测**），
   换来的是零新依赖、零格式变更。

### 4.1.1 【P0】`compact()` 必须先 `commit()`（D-S4-10）

**问题**：`add()` 在把 chunk 压入 `pending` **之前**就已经通过 `Index::add` → `forward.insert_chunk`
分配了真实 `chunk_id`（`search/index.rs:243-290`）；`pending` 只在有 embedder 时填充，攒到
`batch_size`（默认 64）才 flush。若 `compact()` 一上来就重编号，`self.pending: Vec<PendingChunk>`
里存的仍是**旧 chunk_id**；紧接着 `compact_and_save` 调用的 `save()` 第一步就是 `self.commit()?`
→ `flush()`（`search/index.rs:349-351` / `307-343`），会把这些**旧 id 的向量**灌进 `raw_vectors`
与新图——而这些 id 在新 forward 里要么不存在、要么错位指向别的 chunk ⇒ **幽灵点**，且
`raw_vectors.len()` / `nb_point` 口径全乱（直接打穿验收 3 与 I7）。

**修法（采纳第一种）**：`compact()` 的**第一条语句**是 `self.commit()?`。

```rust
pub fn compact(&mut self) -> Result<CompactionReport> {
    self.commit()?;   // ← 先把 pending 落进 raw_vectors / 图，再谈重编号
    // ... 步骤 1~6
}
```

**为什么是它**：

1. **与 S4-01 天然衔接**：先 flush 时，已删 chunk 的向量在 flush 侧就被 liveness 过滤掉了
   （D-S4-05）；compact 第 4 步再过滤一遍只是防御性兜底。两条防线叠加而非互相掩盖。
2. **纯 BM25 下无副作用**：`flush()` 在 `pending.is_empty()` 时直接 `return Ok(())`
   （`search/index.rs:308-310`）；无 embedder 时 `pending` 恒空 ⇒ `commit()` 是 no-op。
3. **语义自洽**：`pending` 非空 ⇒ 一定有 embedder（只有 embedder 存在时 `add` 才 push）
   ⇒ 「compact 前先 flush」所需的条件自动满足，不需要额外判空或返回错误。
4. **`save()` 的隐式 commit 变成 no-op** ⇒ 步骤 0 之后 `compact_and_save` 里不存在第二次 flush，
   时序只有一条，推理成本低。

> ⚠️ **对 §4.5 论点 2 的修正**：原文写「重建**不需要 embedder**」，仍成立，但要加限定——
> **compact 过程本身**不需要 embedder（向量来自 `raw_vectors` 真源），
> 但「compact 前的 flush」需要 embedder；而需要 flush 的前提是 `pending` 非空，
> `pending` 非空的前提又是有 embedder ⇒ 不产生新约束。

**被否决的两种修法**（记录以免重复讨论）：

- *让 `Index::compact()` 顺带 remap `pending`*：可行，但要把门面层的写缓冲概念下沉到
  `Index` 层，破坏「`Index` 只管正排/倒排」的分层 ⇒ 不采纳。
- *`pending` 非空时 `compact()` 返回 `Err`*：把状态管理推给调用方。CLI 路径安全
  （`load` 后 `pending` 恒空），但库 API 会让调用方踩坑，且错误原因不直观 ⇒ 不采纳。

**不变式化**：见 §4.9 **I8**（`compact()` 返回时 `pending` 必然为空）。
**测试**：见 §7 **T13**。

### 4.2 ID 重编号与两张映射表

按 **old id 升序遍历**分配 new id ⇒ **相对顺序保持**（这是 §4.9 中「BM25 逐位一致」的基石）：

```rust
// 只示意，非最终签名
struct IdRemap {
    chunk: Vec<Option<ChunkId>>,  // old_chunk -> new_chunk，None = 墓碑
    doc:   Vec<Option<DocId>>,    // old_doc   -> new_doc
}
```

- `chunks` 稠密化后 `Chunk.chunk_id` 与 `Chunk.doc_id` **都要改写**（`document.rs:99-112`）；
- `DocRecord.doc_id` 同样改写；
- 新 `ForwardStore` 用 `ForwardStore::import(dense_docs, dense_chunks)` 构造——
  它会调用 `rebuild()` 自动重建 `alive`（全活）与 `doc_chunk_count`（`forward.rs:171-201`），
  **不需要手抄状态**（D-S1-01：存活状态的唯一重建入口）。

**为什么是「重编号」而不是「复用空槽（free list）」**（D-S4-01 ✅ 已拍板为「重编号」）：
free list 能让「删改平衡」时体积恒定，且 ID 数值不跳变，看似更省。但它让
`doc_id = 7` 在删除后**指向一篇全新的文档**——场景层持有的旧引用会**静默拿到错误内容**。
相比之下，重编号让旧引用**失效**（通常是报错或查不到），失效远好于静默错误 ⇒ **否决 free list**。

### 4.3 `Index::compact()` 六步

```rust
impl Index {
    pub(crate) fn compact(&mut self) -> IndexCompactStats { ... }
}
```

⚠️ **执行模型（对齐 §4.9 I5，v0.2 明确）**：下列六步**全部作用于一个新构造的 `Index` 实例**，
**不是**就地修改 `self`。实现上先在临时变量里把新 `Index` 建好（复用 `import` 路径），
六步全部成功后才**整体替换** `self` 的字段；任一步返回 `Err` 就直接 `return`，`self` 未被触碰
⇒ 不存在「就地改到一半再回滚」的中间态。下表的「搬移」= 从 `self` 读、往新实例写。

替换时序（门面层 `SearchIndex::compact` 侧，三者**一起换**，顺序固定）：

```
新 Index ─┐
新 raw_vectors ─┼─► 全部构建成功 ─► 一次性 self.inner = { index, raw_vectors, vector_index }
新 vector_index ─┘                    （失败 ⇒ 三者全部丢弃，旧状态原样保留）
```

| 步 | 动作 | 关键点 |
| --- | --- | --- |
| 1 | 建 `IdRemap`（§4.2） | 顺序 = old id 升序 |
| 2 | 正排：dense `docs` / `chunks` + `ForwardStore::import`（自动 `rebuild`） | `alive` 全活；`doc_chunk_count` 重算 |
| 3 | 倒排：按 **TermId 升序遍历**词表，每条链 `filter_map(remap)` 保序；**空链的词整条摘除并重排 TermId** | 保序 ⇒ postings 仍按 chunk_id 升序 |
| 4 | `chunk_lens`：`new_lens[new_id] = old_lens[old_id]`，长度 = 存活数 | 值不变（dl 不变 ⇒ BM25 分数不变） |
| 5 | `content_hashes`：重建 `HashMap<content_hash, new_doc_id>` | 幂等 upsert 语义保持 |
| 6 | `field_index`：`rebuild(&new_docs)`（O(N)） | 与 `import` 同路径（不入快照） |
| — | `stats` | **不动**：`num_chunks` / `total_len` 本就是活计数 ⇒ `avgdl` 不变 |

### 4.4 `raw_vectors`：过滤 + remap

```rust
let alive = index.alive_chunks();
let mut new_raw: Vec<(ChunkId, Vec<f32>)> = Vec::with_capacity(alive_count);
for (old_id, v) in raw_vectors.drain(..) {
    if let Some(new_id) = remap.chunk[old_id as usize] {
        new_raw.push((new_id, v));   // 向量本身不动（不重新归一化、不重新 embed）
    }
}
new_raw.sort_by_key(|(id, _)| *id);  // 与插入顺序对齐（chunk_id 升序）
```

⚠️ **规则（防止过度假设）**：**不要求** `raw_vectors` 覆盖全部存活 chunk。
历史快照可能存在「存活 chunk 缺向量」的情况（纯 BM25 快照、embedder 装配变化）。
compaction 只做**过滤**，不补、不造；缺向量的存活 chunk 在重建图时自然缺席，
与 `load` 降级重建的语义完全一致。

**这里有两层比较，别混**（v0.2 修正，评审 P2）：

- **图点数 vs 存活 chunk 数**：**≤**（缺向量的存活 chunk 在图里缺席）。
- **图点数 vs 快照向量条数**：仍满足 **≥**——缺向量的 chunk 在「向量条数」与「图点数」两边
  同时缺席，故该不等式对 compaction 产物退化为**等号**。
  这与 `load_graph_checked` 步骤 6 的「图中点数 ≥ 快照向量条数」（`persist.rs:310-316`，
  因图含墓碑）**口径一致、不冲突**：它比较的对象是「向量条数」而非「存活 chunk 数」。

### 4.5 向量索引重建

```rust
let mut vi = HnswRsIndex::with_capacity(entries.len().max(1024))
    .with_ef_search(ef_search)              // 沿用装配（P0-5：全 crate 无 set_ef*）
    .with_parallel_build(cfg.parallel_build); // D-S4-07：沿用装配，默认串行
vi.add_batch(&entries)?;                     // 与 load 降级重建同一条路径
```

- 与 `load_with` 的降级分支（`search/index.rs:616-623`）**逐行同源** ⇒ 行为一致、只需抽一个
  `rebuild_vector_index(raw_vectors, ef_search, parallel_build)` 供两处复用；
- 重建**不需要 embedder**（向量来自 `raw_vectors` 真源）⇒ 纯 BM25 装配也能 compact
  （此时 `vector_index = None`，跳过第 5 步）。
  ⚠️ **v0.2 限定**：指 compact **过程本身**不需要 embedder；`compact()` 的
  **步骤 0 `commit()`** 需要 flush 时才用得上 embedder，而 flush 只在 `pending` 非空时发生、
  `pending` 非空又蕴含「有 embedder」⇒ 不构成新约束（§4.1.1）。
- Brute 后端：同样走 `BruteForceIndex::from_entries`（重建是 O(N) 拷贝，成本可忽略）。

### 4.6 `compact_and_save`：铁律如何被既有链路自动满足

架构 §7.5.2 的铁律：「compaction 重建图后**必须重发 manifest**，否则新图永远匹配不上、
每次冷启动都走降级重建」。在本设计中它**不需要额外的代码保证**：

```
save()（既有，search/index.rs:438）
  ├─ save_with_crc  → atomic_write → 新 body_crc
  └─ persist_graph  → write_graph_sidecar
        ├─ dump_graph_caught（先删旧 .graph/.data，N2）
        ├─ 算两文件 CRC
        └─ write_manifest_atomic（新 nb_point / 新 graph_crc / 新 snapshot_crc）← 重发
```

唯一要钉死的约束是：**compaction 之后不能「只 dump 图」而不走 `save()`**。
`Index::compact` 定 `pub(crate)`，`SearchIndex::compact` 是**有意保留**的内存路径（D-S4-04），
但**落盘的唯一入口是 `compact_and_save()`**。

#### `graph_status` 在内存 compact 后的语义（v0.2 补充）

`GraphStatus` 描述的是**磁盘 sidecar 的状态**（`Loaded` / `Rebuilt` / `PersistFailed` /
`NotApplicable`），不是内存图的状态。因此：

- **内存-only `compact()` 不改变 `graph_status`**——此时磁盘 manifest 仍指向旧图，
  字段保持 load 时的原值才如实。⚠️ 实现时**不要**顺手改成 `Loaded`，那会让「磁盘已就位」
  的断言在磁盘尚未跟上时误绿。
- 检索期用的是**重建后的内存图**（已剔除墓碑点）⇒ 与 `graph_status` 的旧值并存不矛盾：
  前者是内存态，后者是磁盘态。
- **落盘后由 `persist_graph` 更新**为 `Loaded`（图 dump + manifest 重发成功），
  这正是验收 2 的断言点（T7）。
- 推论：内存 compact 后若**不** `save` 就进程退出，下次 `load` 拿到的是旧快照 + 旧图——
  二者互相匹配，安全（图是缓存，Step 2 ADR-A），只是回收收益丢失。

⚠️ **旧 manifest 的处理**：`save` 会写新 manifest 覆盖旧的；若 `save` 中途失败，
Lenient 下 `remove_sidecars` 已清理（Step 2 P0-3 + Step 3 D-S3-07）⇒ 不会留下「指向旧图的僵尸 manifest」。

### 4.7 触发策略与可观测

```rust
/// 墓碑统计（compaction 前的决策依据，NFR-07 可观测）
pub struct TombstoneStats {
    pub chunks_total: usize,   // chunks.len()（含墓碑）
    pub chunks_alive: usize,
    pub docs_total: usize,
    pub docs_alive: usize,
    pub graph_points: usize,   // vector_index.len()（含墓碑）
    pub raw_vectors: usize,
    pub tombstone_ratio: f64,  // 1 - alive/total（chunk 口径）
}

/// 三体积（字节）：`.idx` / `.hnsw.graph` / `.hnsw.data`
#[derive(Clone, Copy, Default)]
pub struct SizeBytes {
    pub snapshot: u64,
    pub graph: u64,
    pub data: u64,
}

/// 一次 compaction 的结果报告
pub struct CompactionReport {
    pub before: TombstoneStats,
    pub after: TombstoneStats,
    /// 三体积（字节）。**内存-only `compact()` 下 `bytes_after` 恒为 `None`**
    /// ——磁盘此时还没变，填任何值都是撒谎；CLI `--json` 消费者据此自行 `fs::metadata`。
    pub bytes_before: Option<SizeBytes>,   // 读当前（或指定）idx 路径的 fs::metadata
    pub bytes_after: Option<SizeBytes>,    // 仅 compact_and_save 填写
    pub reclaimed_chunks: usize,
    pub reclaimed_docs: usize,
    pub reclaimed_terms: usize,       // 摘除的死词数
    pub reclaimed_graph_points: usize,
    pub vector_rebuild_ms: u128,
    pub total_ms: u128,
    pub remapped: bool,               // false = 无墓碑，本次是 no-op（ID 未变）
    pub graph_status: GraphStatus,    // v0.2：落盘后的最终状态；内存-only 时为「磁盘旧值」
}
```

- `SearchIndex::tombstone_stats()` → `TombstoneStats`（**只读、无副作用**），CLI `--dry-run` 用它；
- `save()` 在墓碑占比 ≥ 阈值（建议 `tombstone_ratio ≥ 0.2` 且 `chunks_total ≥ 1024`）时打印一行
  告警 `[提示] 墓碑占比 {:.1}%，建议运行 helix compact`（D-S4-02：**只告警，不自动执行**）；
- `CompactionReport.remapped == false` 时，`doc_id` / `chunk_id` **一个都没变**（no-op 保证，验收 T11）；
- **字节体积进 report 的理由**：验收 1 的判据就是三体积，`--json` 消费者（A/B 脚本）不该再自己
  调 `fs::metadata` 去猜三个文件名——库返回自包含数据，CLI 直接透传（v0.2 采纳评审建议）。

### 4.8 CLI、example 与 workload 脚本

**CLI（`helix compact`）**

```
helix compact --index data/foo.idx [--output data/foo-c.idx] [--dry-run] [--json]
```

- 默认**原地**写同路径（`atomic_write` 保证崩溃安全）；`--output` 另存（用于 A/B 对比体积）；
- `--dry-run`：只打印 `TombstoneStats` 与预估回收量，**不写任何文件**；
- 打印：`before → after` 条数、三体积（取自 `CompactionReport.bytes_before/after`，
  §4.7；`--dry-run` 时 CLI 自己 `fs::metadata`）、耗时、重建后 `graph_status`；
- ⚠️ 已知代价：`load` 会按配置指纹校验 ⇒ **需要装配同一个 embedder**（模型加载几秒，
  但**不会**调 embed）。这点要写进 `user-guide.md`。

**example `crates/core/examples/churn_bench.rs`**

体积/回收实验不依赖语义相关性 ⇒ 内置**确定性合成 Embedder**（LCG，dim=512，L2 归一化，
固定 `id = "synth-512"`），10 万级也能在分钟级跑完，且可复现。

```
cargo run --release -p helix-core --example churn_bench -- \
    --corpus data/synth-10000-corpus.jsonl --size 10000 \
    --rounds 5 --churn 0.1 --out /tmp/churn
```

每轮：随机删 10% 文档 → 追加 10% 新文档 → `save` → 记录
（快照字节 / graph 字节 / data 字节 / `raw_vectors` 条数 / 图点数 / 冷启动耗时 / `GraphStatus`）；
末轮后 `compact_and_save` → 再记录一次。输出 CSV + 判定行（验收 1 的三条判据自动 PASS/FAIL）。

**脚本 `scripts/eval_churn.sh`**：编排（多规模 × 多 churn 档）+ 调用 `churn_bench` +
把结果表贴进 `eval-report.md` §8.8（**新增**）。默认 `--size 10000`，`--size 100000` 为扩展验证
（真实 embedder 时需 ≈30min，故 100K 档建议仍用合成 embedder，或 Step 6 的 embed 缓存就位后再跑真实档）。

### 4.9 不变式清单（正确性红线，测试逐条对应）

| # | 不变式 | 为什么成立 |
| --- | --- | --- |
| **I1** | 存活集不变：`compact` 前后，**存活 chunk / doc 的集合（按 `source` + `text`）完全相同** | 存活集来自 `alive` 位图（唯一真源），只做稠密化不做筛选 |
| **I2** | BM25 统计量不变：`num_chunks` / `total_len` / `avgdl` / 每个词的 `df` / 每个 chunk 的 `dl` 全部不变 | §4.3 第 4、6 步只搬移下标，不改值 |
| **I3** | **BM25 检索逐位一致** | ① `df`/`dl`/`avgdl` 不变 ⇒ 同 chunk 同分；② 每条 postings 链**保序 + 保内容** ⇒ 浮点累加顺序一致 ⇒ 分数**逐位**相同；③ `bm25.rs:129-134` 的排序是 `(score 降序, chunk_id 升序)` **全序**，而 remap 是单调的 ⇒ 输出顺序不变。**唯一变化是 `chunk_id` 的数值**，故断言比对 `(source, text, score)` |
| **I4** | 向量路允许微变，但召回不退化 | 图是重建的，拓扑必然不同 ⇒ 用 oracle 重合率 ≥0.99 断言（Step 2 T13 口径） |
| **I5** | **重建失败 ⇒ 索引保持 compaction 前的完整状态**（不留半压实） | 实现顺序：先在**临时变量**里建好新 `Index` / 新 `raw_vectors` / 新向量索引，**全部成功后**才整体替换 `self.inner`。任何一步 `Err` 都原样返回，旧状态未被动过 |
| **I6** | compaction 后**磁盘**要么旧快照要么新快照 | 落盘复用 `atomic_write`（Step 3）；`compact()` 本身不碰磁盘 |
| **I7** | `raw_vectors.len() ≤ 存活 chunk 数`，且每条都是存活 chunk | §4.4 的过滤规则（不补不造） |
| **I8** | **`compact()` 返回时 `self.pending` 必为空**，且 `raw_vectors` / 图中**不含 stale（旧）chunk_id** | D-S4-10：步骤 0 先 `commit()` ⇒ 重编号时缓冲已空；后续 `save()` 的隐式 `commit()` 退化为 no-op（T13 断言） |

---

## 5. 决策记录（D-S4-01 ~ D-S4-10）

> **图例**：✅ = 已拍板（按此实现）；⏳ = 评审无异议、按本文建议执行（实现中若发现问题再回评审）。
> **2026-09-08 拍板**：**D-S4-01（重编号）**、**D-S4-10（`compact()` 先 `commit()`）**。

### D-S4-01 是否重编号 `ChunkId` / `DocId` ✅ 已拍板（2026-09-08）

> **✅ 决策：采纳方案 B「重编号」**（见下表）。由此确定：
> `ChunkId` / `DocId` 是**进程内、快照内的不稳定标识**，compaction 后可被重编号；
> D-S4-09 的三处对外声明（rustdoc + `CompactionReport.remapped` + `user-guide.md`）
> 由「可选」升级为**必做项**；R27（外部持久引用失效）的缓解措施按已缓解执行。

| 方案 | 图体积 | 快照正文 | ID 语义 | 结论 |
| --- | --- | --- | --- | --- |
| A 只重建图，不重编号 | ✅ 回收 99.8% | ❌ 空洞 + `chunk_lens` 死条目 + 死词**仍在**，无界增长 | 不变 | 不满足验收 1 |
| **B 重编号（建议）** | ✅ | ✅ 全压干净 | **变**：旧 `doc_id`/`chunk_id` 失效 | ✅ **采纳（已拍板）** |
| C 保留 ID + sparse→dense 映射表进快照 | ✅ | 部分（把洞换成映射表，4 B/条，比 5~6 B/条省得有限） | 不变 | 复杂度高、收益低，否决 |
| D 复用空槽（free list） | ✅（删改平衡时） | ✅（同上） | **更危险**：旧引用静默指向新文档 | 否决（§4.2） |

**代价与缓解**：ID 变更 ⇒ 需要 D-S4-09 的三处声明；`Hit` 里同时带 `source` / `text` / `metadata`
（`query/response.rs:11-26`），场景层的持久引用应改用 `source`（或 `content_hash`）。

### D-S4-02 触发方式

- **自动（`save` 内触发）**：省事，但把 10~100s 的重建塞进 `save()`，
  使写路径耗时**不可预测**；且 `save` 是 NFR-03/04 口径旁边的敏感点，评审难以接受隐式重活。
- **手动（建议）**：`helix compact` 显式调用，行为可预测；
  `save()` 只在超阈值时 `eprintln` 告警（NFR-07 风格：问题必须可见，但不自作主张）。
- **可选项**：`CompactionPolicy { auto: bool, min_ratio: f64, min_abs: usize }` 留字段，
  **默认 `auto: false`**；待 §4.8 workload 出实测后再决定是否翻默认。

### D-S4-03 死词是否一并压实

`InvertedIndex::remove` 只删 posting 不摘 term（`inverted.rs:61-72`）⇒ 词表随历史单调增长。
死词是**纯浪费**（词串 + 一条空 postings 链），且摘除它会连带 **TermId remap**。
TermId 是内部编号（快照里 `term_dict` 与 `postings` 一起导出/导入，`inverted.rs:109-131`），
**不出现在任何公开 API**（`Index::term_id` 返回 TermId，但调用方只用它取 postings，
且 compaction 后 in-memory 一致）⇒ remap 安全。**建议压实**。

⚠️ **实现约束**：TermId 重排必须**按旧 TermId 升序**遍历，保证 `export` 的**重建确定性**
（v0.2 更名，见下方注记）。

> **「重建确定性」≠ NFR-06**（v0.2 修正，评审 P2）：NFR-06 是「**同一快照两次加载**逐位一致」，
> 约束的是**加载**路径；这里要的是「**同一逻辑状态两次 compact 产出字节一致**」，约束的是
> **重建**路径。两者是不同性质，混称会让读者误以为 NFR-06 覆盖了 compact（§6 又写
> 「NFR-06 不受影响，compaction 不是加载路径」，口径会打架）。本文统一称后者为
> **重建确定性**，T2 / T11 的断言也按此口径写。

### D-S4-04 compaction 与 `save` 的关系

见 §4.6。**结论：保留门面层的内存版 `compact()`，但落盘入口唯一。**

> **v0.2 重写理由**（原表述「公开 `Index::compact` 等于允许只 compact 不 save」自相矛盾——
> 门面层的 `SearchIndex::compact` 本身就是公开的内存版入口，困惑只是从 `Index` 层挪到了门面层）。

- **`Index::compact` 取 `pub(crate)`**：`Index` 是低层内部结构，暴露它等于开放了
  「绕过门面的一致性约定」；且它与 `raw_vectors` / 向量索引的替换必须原子发生（I5 / §4.3），
  单独调用它必然得到不一致状态 ⇒ 这是**结构性的**理由，不是「怕人误用」。
- **`SearchIndex::compact` 公开且是「有意的内存路径」**：语义在 rustdoc 里显式声明——
  「只在内存重建，**不落盘**；`graph_status` 保持磁盘旧值（§4.6）；要持久化请用
  `compact_and_save`」。它的用途是**先试后存**（`--dry-run` 的库级等价物）与**纯内存索引**
  （从不 `save` 的用法也要能回收墓碑）。
- **`compact_and_save(path)` 是落盘的唯一入口** ⇒ 「compaction 后必须重发 manifest」这条铁律
  在公开面上不可能被绕过。

### D-S4-10 `compact()` 与写缓冲 `pending` 的交互 ✅ 已拍板（2026-09-08，v0.2 新增，评审 P0）

> **✅ 决策：采纳修法 A——`compact()` 的第一条语句是 `self.commit()?`。**
> S4-03~S4-07（核心实现）据此开工；不变式 **I8** 与测试 **T13** 转为强制项。

完整论证见 **§4.1.1**。一句话：`add()` 在入 `pending` 之前就分配了 `chunk_id`，
不先 flush 就重编号 ⇒ 旧 id 会在紧随的 `save()` 里被灌进 `raw_vectors` 与新图（幽灵点）。

| 修法 | 评价 |
| --- | --- |
| **A `compact()` 开头先 `self.commit()?`** | ✅ **采纳并已拍板**：与 S4-01 的 liveness 过滤衔接；纯 BM25 下 no-op；`save` 的隐式 commit 退化为 no-op；`pending` 非空 ⇒ 必有 embedder，语义自洽 |
| B `Index::compact()` 顺带 remap `pending` | 可行，但把门面层写缓冲概念下沉进 `Index`，破坏分层 ⇒ 否决 |
| C `pending` 非空时返回 `Err` | 把状态管理推给调用方，库 API 易踩 ⇒ 否决 |

**阻塞范围**：~~S4-03 ~ S4-07（核心实现）动工前必须定稿~~ ⇒ **已拍板，阻塞解除**。
不变式 I8、测试 T13 转为强制项，实现时必须落地。

### D-S4-05 flush 侧幽灵向量（§2.3）

修，且**独立成 S4-01 小 PR**（与本 Step 主体解耦，先合先收益）。
方式：`flush()` 中**整批 embed 之后、入库之前**按 `index.is_live_chunk()` 过滤。

### D-S4-06 workload 规模与向量来源

见 §4.8。默认 10K + 合成 embedder（确定性、无模型下载、分钟级）；
100K 真实语料为**扩展验证**（真实 embed ≈30min / 10 万条，内存 2×）。

### D-S4-07 重建图是否沿用 `parallel_build`

沿用（默认串行）。compaction 是低频操作，不值得为加速引入**拓扑不可复现**（C8）；
且与 `load` 降级重建同源 ⇒ 行为一致。

### D-S4-08 `remove_many`

架构 §7.5.2 已预留。价值：把「批量删 N 篇 O(N·M)」降到 O(N+M)（一次 `retain` 而非 N 次）。
**可选**（S4-02），不阻塞本 Step 的验收。

### D-S4-09 ID 变更的对外声明

三处（见速读表）：rustdoc + `CompactionReport.remapped` 字段 + `user-guide.md` 已知坑。
声明口径：**「`doc_id` / `chunk_id` 是进程内、快照内的不稳定标识；跨 compaction 的持久引用请用
`source`（或 `content_hash`）」**。

> ⚠️ **v0.3：D-S4-01 拍板为「重编号」后，本条由「建议」升级为「必做」**——ID 变更已是既定事实，
> 三处声明缺一不可，S4-07 / S4-10 必须落地。

---

## 6. 影响面与兼容性

| 面 | 影响 |
| --- | --- |
| **快照格式** | **零变更**。`FORMAT_VERSION` 保持 2；compaction 产物是普通快照，旧版本可加载（chunk_id 数值不同但结构一致） |
| **公开 API** | **新增**：`SearchIndex::compact`（内存，不落盘）/ `compact_and_save` / `tombstone_stats`、`TombstoneStats`、`CompactionReport`、`SizeBytes`（+ 可选 `remove_many`）。**无破坏性变更**：`Index::compact` 取 `pub(crate)`。**落盘入口唯一** = `compact_and_save`（D-S4-04） |
| **ID 语义** | **变更**（D-S4-01，✅ 已拍板为「重编号」）：compaction 后 `doc_id` / `chunk_id` 可被重编号。需 D-S4-09 三处声明（**已由「可选」升级为必做**） |
| **NFR-04（冷启动 <2s）** | 正向：compaction 后图命中 ⇒ `Loaded`；若漏发 manifest 则退回 ≈10s（铁律已由 §4.6 结构性排除） |
| **NFR-06（同快照两次加载逐位一致）** | 不受影响（compaction 不是加载路径）。⚠️ 另有**重建确定性**（同一逻辑状态两次 compact 产出字节一致，D-S4-03）——独立于 NFR-06 的强性质，勿混称 |
| **NFR-03（构建耗时）** | 不受影响：compaction 是显式运维操作，不在构建口径内 |
| **CLI** | 新增 `helix compact` 子命令（不改动既有子命令） |
| **故障注入 / 崩溃** | 复用 Step 3 的 `atomic_write`；不新增注入点（compaction 的落盘就是 `save`） |
| **依赖** | 零新增 |

---

## 7. 测试计划（S4-T1 ~ S4-T13）

| # | 测试 | 层 | 断言要点 |
| --- | --- | --- | --- |
| **T1** | `Index::compact` 稠密化 | 单元 | `docs` / `chunks` 无 `None`；`chunk_lens.len()` == 存活数；`num_chunks` 不变；`alive_count` 不变 |
| **T2** | 倒排 remap + 死词摘除 + **重建确定性** | 单元 | 每个词的 `df` 与 compact 前一致；`postings` 链内 `chunk_id` 升序；无空链；`term_id(term)` 仍可用；**同一状态两次 `compact()` 的 `export` 字节完全一致**（D-S4-03 的重建确定性口径，独立于 NFR-06） |
| **T3** | **BM25 逐位一致（I3）** | 单元/集成 | 20 组 query 的 Top-10 `(source, text, score)` 序列与 compact 前**完全相同**（`f32` 逐位） |
| **T4** | 向量路 oracle 重合率 ≥0.99（I4） | 集成 | 复用 Step 2 T13 口径（真实语料 12K，串行建图） |
| **T5** | **幽灵向量防线（D-S4-05）** | 集成 | `add(d)` → `remove(d)`（未 flush）→ `commit` → `save` → `load`：快照 `vectors` 与图中**不含**该 chunk（T5 在 S4-01 落地后即绿） |
| **T6** | 图 sidecar 回收 | 集成 | churn N 轮后 `compact_and_save`：`.hnsw.data` + `.hnsw.graph` 字节数回落到存活集规模 ±10%；`nb_point` == **存活且有向量**的 chunk 数（验收 3；本 workload 下即存活数） |
| **T7** | **manifest 重发 / 铁律（验收 2）** | 集成 | compaction 并 `save` 后**重新 `load`** ⇒ `graph_status == GraphStatus::Loaded`（不是 `Rebuilt`）；冷启动 <2s |
| **T8** | 快照正文回收 | 集成 | churn 前后正文字节数对比；`docs` / `chunks` 无 `None`；`chunk_lens.len()` == 存活数 |
| **T9** | **跨快照不累积（验收 1 第三条）** | 集成 | 3 轮「load → remove 10% → add 10% → save → compact」：第 3 轮 compaction 后体积与第 1 轮差异 <10% |
| **T10** | 崩溃安全（I6） | 集成 | 复用 S3-T6 骨架：compact 落盘期间注入崩溃 ⇒ `load` 得旧或新，绝不 `SnapshotCorrupted` |
| **T11** | no-op 保证 | 单元 | 无墓碑时 `compact()` 返回 `remapped == false`，且**所有 `chunk_id` / `doc_id` 原样不变**（保护「误调用也不改变 ID」） |
| **T12** | 无向量 / Brute 后端 | 单元 | 纯 BM25（`vector_index = None`）与 `Brute` 后端下 `compact()` 正常（跳过第 5 步 / 走 `from_entries`） |
| **T13** | **写缓冲交互（I8 / D-S4-10）** | 集成 | `add(d)` → **不 flush**（`pending` 非空）→ `compact_and_save` → `load`：① 快照 `vectors` 与图中**不含 stale 旧 chunk_id**；② 该 chunk 的**向量与正文都在**（没有被 flush 漏掉、也没有错位）；③ `raw_vectors.len()` == `nb_point` == 存活且有向量的 chunk 数；④ 对照：`remove(d)` 后再走同路径，该 chunk **完全不出现**（与 T5 互补，T5 测 flush 侧、T13 测 compact 侧） |

> **T13 的构造要点**：默认 `batch_size = 64`，所以必须**显式控制在 64 条以内**
> （或临时把 `batch_size` 调大）才能保证进入 compact 时 `pending` 非空；
> 否则测试会退化成「pending 已空」的平凡情形还照样全绿（Step 2 的
> `PARALLEL_INSERT_THRESHOLD` 教训：隐式耦合会让测试静默失效）。建议给
> `SearchIndexBuilder` 加 `batch_size`，并在 T13 里**先断言 `pending` 非空**再 compact。

> ⚠️ 沿 Step 2 教训：**所有体积断言除「<2s」类硬判定外一律给范围**（实测单次波动 ±10~15%）；
> 体积判据统一写「±10%」而非定值。

---

## 8. 实施任务拆分（S4-01 ~ S4-10）

| # | 任务 | 交付 | 量级 |
| --- | --- | --- | --- |
| **S4-01** | flush 侧幽灵向量防线（D-S4-05） | `search/index.rs::flush` 按 liveness 过滤入库 + T5 | S（**独立 PR，可先合**） |
| **S4-02** | `remove_many(&[DocId])`（可选，D-S4-08） | 批量删除入口 + 单测 | S |
| **S4-03** | `Index::compact`：正排稠密化 + `IdRemap` | `index/mod.rs`、`forward.rs` 复用 `import` | M |
| **S4-04** | `Index::compact`：倒排 remap + 死词摘除 + `chunk_lens` / `content_hashes` / 字段索引 | T1、T2 | M |
| **S4-05** | raw_vectors 过滤 + remap（I7） | `search/index.rs` | S |
| **S4-06** | 向量索引重建（D-S4-07）+ `rebuild_vector_index` 与 `load` 降级路径复用 | T12 | M |
| **S4-07** | 门面层：`compact`（**含步骤 0 `commit()`**，D-S4-10）/ `compact_and_save` / `tombstone_stats` + `TombstoneStats` / `CompactionReport`（含字节三体积）+ `save` 阈值告警（D-S4-02） | T7、T11、**T13** | M |
| **S4-08** | CLI `helix compact`（`--dry-run` / `--output` / `--json`） | CLI + `user-guide.md` | S |
| **S4-09** | workload：example `churn_bench` + `scripts/eval_churn.sh` + 实测入 `eval-report.md` §8.8 | T6、T8、T9、T10 | M |
| **S4-10** | 文档回写：架构 §7.5.3（新增 compaction 小节）+ 风险 R26~R30、需求 FR-30 验收注记、plan-v2 进度、CHANGELOG、README 索引 | 文档 | S |

> **建议 PR 切分**（沿用 Step 3 教训：base 一律 `main`，一个 PR 一个主题）：
> ① S4-01（小，先合）→ ② S4-03~S4-07（核心，单 PR，含 T1~T4/T7/T11/T12/**T13**）→
> ③ S4-08 + S4-09（CLI + workload + 实测）→ ④ S4-10（文档回写）。
>
> **v0.3 开工状态**：D-S4-01 / D-S4-10 已拍板 ⇒ **① 与 ② 均可开工**（不再有阻塞项）。
> ② 的实现必须包含 `compact()` 的步骤 0 `commit()`（D-S4-10）、不变式 I8 与测试 T13。

---

## 9. 风险与未决问题

### 9.1 新增风险（拟写入架构 §14.1，编号 R26~R30）

| # | 风险 | 影响 | 对策 | 状态 |
| --- | --- | --- | --- | --- |
| **R26** | **compaction 期间内存峰值 ≈2×**（旧 Index/图 + 新 Index/图并存） | 100K 级可能 OOM | 先按正确性实现；缓解手段（正排原地重排、分步替换）待 S4-09 实测后决定 | 待实测 |
| **R27** | **ID 重编号破坏外部持久引用** | 场景层引用的 `doc_id` 失效 | D-S4-09 三处声明；`source` / `content_hash` 作稳定键；`CompactionReport.remapped` 可观测 | 已缓解 |
| **R28** | **compaction 耗时**（重建图 O(N)：12K ≈10~11.5s，100K 预估 60~100s，**待实测**） | 长时间独占 `SearchIndex` | 手动触发（D-S4-02）；CLI 打印耗时；100K 实测入 eval-report | 待实测 |
| **R29** | **重建图引入拓扑抖动**（NFR-06 口径不破，但与「增量建库」的历史评测基线不再逐位可比） | 评测可比性 | 文档说明 + oracle 重合率 ≥0.99 断言（T4） | 已接受 |
| **R30** | compaction 期间**无法服务**（单写者语义，无并发读者） | 运维窗口 | V2.0 接受；在线 compaction 归 **Step 8** | 已接受 |

### 9.2 未决问题（需评审或实测回答）

- **~~Q1~~** ✅ **已拍板（2026-09-08）**：D-S4-01 采纳**重编号（方案 B）**。
- **Q2**：`save()` 的告警阈值（建议 ratio ≥0.2 且 total ≥1024）是否合适？
- **Q3**：`raw_vectors` 缺向量的存活 chunk（§4.4）在 CLI 上要不要告警？
- **Q4**：100K 档用合成 embedder 还是真实 embedder？（时间 vs 真实性）
- **Q5**：`flush` 能否**先过滤再 embed**（§2.3）？需先实测 `fastembed` 的 batch 组成无关性。
- **Q6**：compaction 要不要顺带把 `content_hashes` 里指向墓碑 doc 的残留清掉？
  （现状 `Index::remove` 已摘 `content_hash`，理论上无残留 ⇒ 只需 T1 断言，不需要额外代码）
- **~~Q7~~** ✅ **已拍板（2026-09-08）**：D-S4-10 采纳「`compact()` 开头先 `commit()`」（修法 A）；§4.1.1 的 B / C 不再考虑。

---

## 附录 A：新增 / 变更 API 一览

```rust
// crates/core/src/search/index.rs（公开）
impl SearchIndex {
    pub fn tombstone_stats(&self) -> TombstoneStats;

    /// 在**内存**里按存活集重新物化一次并重建向量图。**不落盘**。
    ///
    /// - 内部第一步是 `self.commit()?`（D-S4-10）：先把写缓冲 flush 掉再重编号，
    ///   否则 `pending` 中的旧 `chunk_id` 会污染 `raw_vectors` 与新建的图。
    /// - **`graph_status` 保持磁盘旧值**（描述的是 sidecar 状态，此时磁盘尚未更新）。
    /// - ⚠️ **ID 可能变更**：跨 compaction 的持久引用请用 `source` / `content_hash`，
    ///   不要用 `doc_id` / `chunk_id`（D-S4-09）。
    pub fn compact(&mut self) -> Result<CompactionReport>;

    /// `compact()` + 既有 `save()`——**落盘的唯一入口**，manifest 重发由 `save` 链路保证。
    pub fn compact_and_save(&mut self, path: &Path) -> Result<CompactionReport>;

    // 可选（D-S4-08）
    pub fn remove_many(&mut self, doc_ids: &[DocId]) -> Result<()>;
}

pub struct TombstoneStats { /* 见 §4.7 */ }
pub struct CompactionReport { /* 见 §4.7；含 bytes_before/bytes_after/graph_status */ }
pub struct SizeBytes { /* 见 §4.7 */ }

// crates/core/src/index/mod.rs（pub(crate)，D-S4-04）
impl Index {
    pub(crate) fn compact(&mut self) -> IndexCompactStats;
}

// crates/core/src/vector/*（内部复用）
pub(crate) fn rebuild_vector_index(
    entries: &[(ChunkId, NormalizedVector)],
    ef_search: usize,
    parallel_build: bool,
) -> Result<Box<dyn VectorIndex>>;
```

**公开面增量**：3 个方法 + 3 个结构体（`TombstoneStats` / `CompactionReport` / `SizeBytes`）
（+1 个可选方法）。**无破坏性变更**。

## 附录 B：churn workload 的执行脚本（草案）

```bash
# 1) 10K 默认档（合成 embedder，分钟级）
scripts/eval_churn.sh --size 10000 --rounds 5 --churn 0.10

# 2) 100K 扩展档（D-S4-06；真实 embedder 时 ≈30min，建议配合 --synth-embedder）
scripts/eval_churn.sh --size 100000 --rounds 3 --churn 0.05 --synth-embedder

# 3) 单跑核心 example（调参与调试用）
cargo run --release -p helix-core --example churn_bench -- \
    --corpus data/synth-10000-corpus.jsonl --size 10000 --rounds 5 --churn 0.1
```

输出（`--out` 目录）：每轮一行 CSV
`round, alive_chunks, graph_points, raw_vectors, snapshot_bytes, graph_bytes, data_bytes, cold_ms, graph_status`
+ 末行 `compacted,...`；判定行自动给 PASS/FAIL（验收 1 的三条判据 + 验收 2 `Loaded`）。

## 附录 C：本文引用的项目内证据

| 结论 | 出处 |
| --- | --- |
| `VectorIndex` 无 remove API | `crates/core/src/vector/mod.rs:31-83`；`vector/hnsw_rs_index.rs:149-175` |
| 图 dump 写全量点（含墓碑） | `crates/core/src/vector/persist.rs:143-195`（`file_dump`） |
| `nb_point` 含墓碑、加载侧按「≥」校验 | `storage/graph.rs:154`；`vector/persist.rs:310-316` |
| **v0.2 新增：`add()` 在入 `pending` 前已分配 `chunk_id`（P0 的根源）** | `search/index.rs:243-290`（`Index::add` 分配 → 之后才 `pending.push`） |
| **v0.2 新增：`commit() == flush()`，且 `flush` 对空 `pending` 是 no-op** | `search/index.rs:307-310`（早退）、`:349-351`（`commit` 转发 `flush`） |
| **v0.2 新增：`save()` 第一步即 `self.commit()?`** | `search/index.rs:438-441`（p6-design 6.2：不允许带未刷缓冲落盘） |
| `remove` 已摘 `raw_vectors`，但 `flush` 无 liveness 检查 | `search/index.rs:419-424` vs `search/index.rs:328-339` |
| `chunk_lens` push-only | `index/mod.rs:193`（push）、`:344`（读）、`:409`（import） |
| `InvertedIndex::remove` 不摘 term | `index/inverted.rs:61-72` |
| `forward` 的 `alive` 唯一重建入口 | `index/forward.rs:171-201`（`import` + `rebuild`） |
| 快照正文结构 | `storage/snapshot.rs:19-28`；`index/mod.rs:62-77` |
| `save` 序列含 manifest 重发 | `search/index.rs:438-466`（`save` → `persist_graph` → `write_graph_sidecar`） |
| BM25 排序是全序（score 降序 + chunk_id 升序） | `retriever/bm25.rs:129-135` |
| 铁律原文 | `architecture-design.md:728` |
| Step 4 任务与验收原文 | `plan-v2.md:267-282`；issue #21 |
| 12K 图体积实测（单位成本外推依据） | `plan-v2.md:205-217`（graph 7.9~8.1MB + data 23.7MB，dump 43.5ms） |
| 降级重建耗时（12K ≈10~11.5s） | `plan-v2.md:212` |
