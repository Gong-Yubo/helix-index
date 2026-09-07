# V2 开发计划复审（plan-v2 审核稿）

| 项 | 内容 |
| --- | --- |
| 版本 | v0.2（**已审核并拍板，结论已回写**） |
| 日期 | 2026-09-07 |
| 审核对象 | `docs/devel/plan-v2.md`（v0.3，含 PR #18 的 Step 2 收尾回写） |
| 结论 | **主体编排不推翻**；7 处调整（A1~A7）**全部采纳**并已回写 `plan-v2.md` **v0.4**；D1~D4 已拍板 |
| 落地 | Step 3 起重排编号（映射见 `plan-v2.md` §附-2）；需求文档升 **v1.7**（NFR-03 双口径 / 新增 NFR-13 / NFR-11 口径 / FR-31 升 Must）；架构文档升 **v1.7** |

## 0.1 拍板结果（2026-09-07）

| # | 问题 | 拍板 | 落地位置 |
| --- | --- | --- | --- |
| **D1** | NFR-03（<120s 不可达）怎么办 | ✅ **拆「首次全量 / 增量」双口径**（首次全量 <240s + 增量追加 < 首次 × 变更比 × 1.2） | `requirements-spec.md` NFR-03；`plan-v2.md` Step 6 + D-J8 |
| **D2** | 低选择度延迟（10 万级 0.1% 档 158ms）是否提前 | ✅ **提前到 V2.0**（S 级，方案已明确）；同时新增 **NFR-13** 纳入口径 | `plan-v2.md` Step 5（T7-22）；`requirements-spec.md` NFR-13 + D-J9 |
| **D3** | Step 5（资源回收）是否提到 Step 4 之前 | ✅ **提前**——重排为 Step 4；原 Step 4（构建性能）顺延为 Step 6 | `plan-v2.md` §4 + D-J10 + §附-2 |
| **D4** | `parallel_build` 默认值翻转怎么走 | ✅ **独立 PR**（横切任务 T7-21，须同步改 S2-T22） | `plan-v2.md`「横切任务」+ D-J11 |

---

## 0. 结论速览

> 计划的骨架（Step 1~9 顺序、D-J1~J7、ADR-A 方案 C）**依然成立**，不需要返工。
> 需要调整的是：**Step 3 的范围已随 ADR-A 变化但计划没跟上；Step 4 的技术前提有一半被既有数据证伪；
> NFR-03 可能根本不可达（需求侧决策）；另有 4 个 S 级小任务应插队。**

| # | 调整项 | 优先级 | 性质 |
| --- | --- | --- | --- |
| A1 | Step 3 范围/验收按 ADR-A 重写（快照本体至今非原子） | **高** | 计划与现状脱节 |
| A2 | Step 4 技术前提需先验证（多 session 并行可能无效） | **高** | 技术归因存疑 |
| A3 | NFR-03 `<120s` 可能不可达，需求侧要决策 | **高** | 需求口径 |
| A4 | 4 个 S 级任务插队（并行默认值 / 低选择度兜底 / Metrics 可观测 / 工程卫生） | 中 | 排期 |
| A5 | Step 5 增加「compaction 后必须重发 manifest」+ 补 workload 脚本；建议提到 Step 4 之前 | 中 | 范围 |
| A6 | Step 6 补三条前置（基线可复现 / 模型下载 / H3 未决） | 中 | 前置条件 |
| A7 | 计划文档自身状态、验收清单、NFR-11/FR-31 口径 | 低 | 文档一致性 |

**建议执行顺序**：Step 3（S）→ A4 插队任务（S×4）→ Step 5（M）→ Step 4（**先 S 级 spike 再决定**）→ Step 6（M）。

---

## 1. 现状核对（事实层）

| 事实 | 证据 |
| --- | --- |
| Step 1 / Step 2 已完成并进 main | main = `9db2c00`（PR #18 于 2026-09-07 14:37 合并，CI 9/9 绿）；链 `68074c9`→`e9ac2b0`→`15c1900`→`1e51156`→`9db2c00` |
| Step 3~9 全部未开工 | plan-v2 §7 进度表 |
| 仅 3 个 open issue | #2（NFR-03/Step 4）、#4（paraphrase 稀释/Step 6 前置）、#5（旧 API deprecated/Step 7） |
| **Step 3 / Step 5 / Step 6 没有跟踪 issue** | issue 列表；标签只有 `V2-Step2 / V2-Step4 / V2-Step6 / V2-Step6+`，缺 `V2-Step3`、`V2-Step5` |
| 100K 合成语料已就位（Step 5 的 fixture 不缺） | `data/synth-100000-corpus.jsonl`（54MB，已 gitignore） |
| ⚠️ 本地 `main` 落后 origin | 本地 `1e51156`，远端 `9db2c00`（PR #18 已合并）——下一步开工前先 fast-forward |

---

## 2. A1【高】Step 3 范围与验收必须按 ADR-A 重写

**计划现状**：Step 3 = 「T7-13 原子快照（临时文件 + `rename`，D-J3）」，依赖「与 Step 2 共享 ADR-A」，验收「故障注入测试」，量级 S。

**与现状的三处脱节**：

1. **ADR-A 已经把 Step 3 的范围缩小了，计划没写进去。** 架构 §7.6.2 已明确：
   > 「Step 3 只需让 `foo.idx` 自己 tmp+rename，图 sidecar 自动跟随」
   > ——因为 manifest 记父快照 CRC 作版本锚点，快照一换旧图自动失效。
   ⇒ Step 3 是**单文件原子写**，不是「多文件事务」。计划里「覆盖图+快照双文件」的验收描述已经过时。

2. **快照本体确实还是非原子的**（复审实测代码）：
   `storage/snapshot.rs:88` 是 `File::create(path)` 直写 + `flush()`，**既无 tmp+rename，也无 fsync**。
   崩溃中途 ⇒ 半截文件 ⇒ 下次 `load` 得 `Error::SnapshotCorrupted`（**不是降级，是索引没了**）。
   对比图侧：`storage/graph.rs:236 write_manifest_atomic()` 已是 tmp→fsync→rename→fsync(父目录)。

3. **R19（写路径 `panic_any` TOCTOU 残余）与 Step 3 是同一处代码边界**，合并做最省。

**建议改写 Step 3 为 5 项**：

| 子项 | 内容 |
| --- | --- |
| S3-a | 从 `write_manifest_atomic` 抽出通用 `atomic_write(path, bytes)`（tmp → fsync → rename → fsync 父目录），**快照与 manifest 共用一份实现** |
| S3-b | `save_with_crc` 改走 `atomic_write`，并补 fsync（现在连 fsync 都没有） |
| S3-c | tmp 孤儿回收：启动/保存时清理 `*.idx.tmp`、`*.idx.hnsw.manifest.tmp`（改名策略见 A7-④） |
| S3-d | 故障注入测试：**用 `catch_unwind` + 可注入失败点**（在「写完 tmp、未 rename」处 panic），断言原快照完好。**不要用 chmod**（CI 以 root 运行时权限位无效，Step 2 已踩过） |
| S3-e | 顺带收 R19（写路径 `panic_any`） |

**验收改写**：崩溃/中断后 `load` 要么拿到**旧快照**、要么拿到**新快照**，**绝不出现 `SnapshotCorrupted`**；
图 sidecar 因 CRC 锚点自动降级为重建（这一条已被 Step 2 覆盖，不必重复测）。

**排期建议**：排在最前。Q-C3 定级是「高（正确性）」，量级只有 S。

---

## 3. A2【高】Step 4 的技术前提要先验证，再决定投不投 M 级

**计划假设**（T7-09）：embed 串行是瓶颈（`LocalEmbedder` 的 `Mutex` 全程持锁），多 session / 分片锁 + `with_intra_threads` 能提吞吐。

**复审发现的反向证据**（fastembed 6.0.2 源码级核实）：

| 事实 | 证据 |
| --- | --- |
| ONNX Runtime 的 **intra-op 线程默认是「用满所有核」** | `fastembed-6.0.2/src/init.rs:30-33`：`intra_threads: None` 即 `available_parallelism`；我们 `LocalEmbedder::new()` **没有覆盖它** |
| `embed` 是**单 session、按 256 条一批串行**处理 | `src/text_embedding/impl.rs:373`（`.chunks(batch_size)`），`DEFAULT_BATCH_SIZE = 256` |
| **批次大小不是瓶颈**：32/64/128/256 → 62.6 / 59.1 / 54.4 / 51.6 条/s，差异 <20% 且小 batch 反而略快 | `eval-report.md:147-148` |
| **建图并行对 NFR-03 无意义**：225.5s 里 embed 209.9s（93%），建图 11.7s；并行建图省 ~9.5s（4%） | `eval-report.md:232-234` |

⇒ **「多开 session」大概率是和满核的 intra-op 抢核**，收益不确定甚至为负。
真正没试过的杠杆是 **Execution Provider**：`ort-2.0.0-rc.13` 有 `coreml` feature（`Cargo.toml:145`），fastembed 暴露 `with_execution_providers` —— **macOS aarch64 上 CoreML EP 是计划里从未评估过的一条路**。

**建议**：T7-09 拆成 **S 级 spike**，出数据再决定是否投 M 级。

| 实验 | 口径 |
| --- | --- |
| E1 单 session（现状基线） | 12K 语料 embed 209.9s |
| E2 2~4 session × `intra_threads` 分片（如 4 session × 每 session N/4 线程） | 同语料同口径 |
| E3 **CPU EP vs CoreML EP** | 同上；⚠️ 数值可能变化 ⇒ 必须同时重测 **NFR-06 确定性**与相关性基线（MRR@10(1)=0.6922） |

> ⚠️ 若 E3 有效，它会改变向量数值 ⇒ 快照的 `ConfigFingerprint` 需要把「execution provider」纳入（否则 CoreML 建库、CPU 加载会静默错配，这正是 B1/B2 那类坑）。这一条要写进 Step 4 的设计前提。

---

## 4. A3【高】NFR-03 `<120s` 可能不可达 —— 这是需求决策，不是技术问题

**已排除的路径**（都有据）：

| 杠杆 | 结论 |
| --- | --- |
| 量化模型 | ❌ D-J6 已否；**复审核实 fastembed 6.0.2 确实没有 bge-small-zh 量化变体**（量化仅 `NomicEmbedTextV15Q` / `ParaphraseMLMiniLML12V2Q`，均非中文主力） |
| 调 batch_size | ❌ 已测，差异 <20%（eval-report 8.2） |
| 并行建图 | ❌ 只占 4% |
| 多 session 并行 | ⚠️ 存疑（A2），且瓶颈是 ort 推理本身 |

⇒ 现状 12K 语料 225.5s（折 1 万 chunk ≈188s），**距 120s 差 1.6×**。在「不换模型、不量化」的前提下，**没有任何已识别路径能补上这 1.6×**。

**请从三个选项里定一个（推荐 ①）**：

| 选项 | 内容 | 代价 |
| --- | --- | --- |
| **① 拆双口径（推荐）** | NFR-03 改为「**首次全量** 1 万 chunk < 240s（按实测定）+ **增量追加** 10% 文档 < 首次全量 × 10% × 1.2」 | 诚实反映「ort CPU 推理」这一硬约束；增量口径才是 Agent 场景的真实使用形态 |
| ② 降级 + 记录偏离 | NFR-03 从 Must 降 Should，在需求文档与 README「已知局限」写明实测值与根因 | 发布门槛里少一条硬指标 |
| ③ 换更小模型 | 换 `paraphrase-multilingual-MiniLM`（384 维）等 | **相关性全线重测**（MRR 基线 0.6922 会动），且中文质量大概率下降 —— 与「相关性优先」的取舍顺序冲突 |

> 无论选哪个，**Step 4 的验收标准都要跟着改**，否则 Step 4 做完了也交不了差。

---

## 5. A4【中】建议插队的 4 个 S 级任务

| # | 任务 | 依据 | 建议搭载 |
| --- | --- | --- | --- |
| **A4-①** | **`parallel_build` 默认翻转为「开」** | 12K 真实语料 11.676s → 2.182s（**5.35×**），50K 合成 5.26×，真实语料 oracle 重合率无差异（0.995/0.995）；NFR-06 口径是「同快照两次**加载**」，已排除「同批两次建库不一致」的代价 | ⚠️ 必须同步改 **S2-T22**（现断言「默认必须串行」）。建议独立 PR，不塞进 Step 3 |
| **A4-②** | **低选择度暴力兜底（把 R18 从 V2.1 提前）** | 10 万级 sel-0.1% 档 vector P99 = **158.45ms**（限额 10ms，超 16×）；数据已支持的廉价方案：`allowed` 很小时绕开 ANN 直接暴力扫描（allowed=100 时约 **0.05ms**） | 需同时在 NFR-02 补「10 万级 + 低选择度」口径或**新增 NFR-13**，否则它永远在口径外 |
| **A4-③** | **`query::Metrics` 可观测化** | 现只 `tracing::info!` 输出，**外部拿不到实例 ⇒ 无法单测、无法被 bench 采集**；而 `vector_shortfall` 正是「V2.1 是否引入 prefilter」的判据。Step 4 的 NFR-10/11 也要靠它 | 顺带修 `query/metrics.rs:14` 里「见 issue #7」的失效引用（#7 已关闭）。作为 **Step 4 的前置** |
| **A4-④** | **工程卫生（一次 PR 打包）** | ① CI 只在 `branches:[main]` 触发 ⇒ 堆叠 PR 至今跑不到 CI（Step 2 已吃过亏）；② `.gitignore` **没盖住图 sidecar 与 manifest tmp**（实测 `data/t2-index.idx.hnsw.graph`、`*.hnsw.manifest.tmp` 均未被忽略，31MB 级文件有误提交风险）；③ 远端 12 条分支（11 条已合并待清理） | 建议加 `on: workflow_dispatch` + `push: branches-ignore: main`，并补 `*.idx.hnsw.*` / `*.idx.*.tmp` |

---

## 6. A5【中】Step 5 要补一条 Step 2 之后才成立的新复杂度

- **compaction 重建图后必须重发 manifest**：图重建 ⇒ `nb_point`/`graph_crc` 全变 ⇒ 不重发 manifest 就永远走降级（白重建一次）；重发了才恢复 NFR-04。这是 Step 2 之前不存在的设计点，计划里没写。
- **验收要覆盖三个体积**：向量图 sidecar（`.hnsw.graph` + `.hnsw.data`）/ `raw_vectors` / 快照正文 —— 计划已写，保留。
- **缺 workload 脚本**：「长期零散写入」目前没有可复现脚本（`scripts/` 只有 `eval_quality.sh` / `eval_filter.sh` / `eval_perf.sh` / `gen_synth_corpus.py`），需新增。fixture 已齐（100K 合成语料）。
- **排期建议：提到 Step 4 之前**。理由：① 正确性/资源类，优先于性能；② 与 ADR-A 上下文相邻（同批做最省）；③ Step 4 的收益不确定（A2），先做确定的。

---

## 7. A6【中】Step 6 缺三条前置

| 前置 | 说明 |
| --- | --- |
| **基线可复现** | 验收写的是「MRR@10(1) 相对 P5 基线 0.6922 可测提升」，但计划没要求**先复现基线**。建议 Step 6 第一件事：用 `scripts/eval_quality.sh` + T2Ranking qrels 跑出当前基线与 CI 波动范围，否则「提升」无法判定（Step 1 已经吃过「重合率 0.685~0.948 波动」的亏） |
| **模型下载可行性** | 复审已核实 API 可用：`TextRerank::try_new(RerankInitOptions)`、`RerankerModel::BGERerankerV2M3` 均存在（`fastembed-6.0.2`）。但该模型含外置 `model.onnx.data`（**GB 级**）⇒ 需先确认下载体积与镜像可达性；CI 沿用 `#[ignore]` + 本地缓存 |
| **H3 至今未决** | §4.0 H3（精排候选窗口 + NFR-12）**仍标「Step 6 开工前澄清」但没结论**。当前编排是融合后 `take(k)` 再 rerank；放开到 `candidate_k`(3k) 会让 rerank 成为延迟主导项 ⇒ **窗口参数与 NFR-12 必须先定** |

---

## 8. A7【低】计划文档自身的一致性

| # | 问题 | 建议 |
| --- | --- | --- |
| ① | 头部写「v0.3（计划草案，已消化评审，**待定稿**）」，但 D-J1~J7 已定案、ADR-A 已拍板、Step 1/2 已完成 | 升 **v0.4**：「已定稿；Step 1/2 已完成，Step 3+ 进行中」，日期更新 |
| ② | §6 验收门槛 8 项**全 ⬜**，其中 Step 1/2 的 4 项实际已达成 | 勾选已达成项，或按步骤标注达成情况 |
| ③ | §8 标题「文档回写计划（**本计划评审通过后执行**）」 | Step 1/2 的回写已执行 ⇒ 改「执行中：Step 1/2 已完成」 |
| ④ | NFR-04 数字不一致：需求文档写「快照 76.9ms + 图 23.3ms」，plan-v2 写「57~77ms / 23~37ms」 | 统一为**范围**（Step 2 已定的规矩：除「<2s」判定外一律给范围） |
| ⑤ | **FR-31（原子快照）优先级 = Should**，但 Q-C3 定级「高（正确性）」且 Step 3 是 V2.0 必做 | 升 **Must**（与 FR-16 同） |
| ⑥ | **NFR-11「增量写入后立即可查」与实现语义不符**：`add` 之后必须 `commit()` 才对检索可见（`search/index.rs:5-6`，对齐 Lucene/tantivy） | 改口径为「**commit 后**立即可查；写延迟有界（= flush 一批的耗时）」 |
| ⑦ | Step 3/5/6 无跟踪 issue；缺 `V2-Step3` / `V2-Step5` 标签 | 补 issue + 标签（沿用「一个步骤一个 issue + 排期标签」的约定） |

---

## 9. 建议调整后的排期

> ✅ **本节建议已被采纳并落地**：实际编号与顺序以 `plan-v2.md` v0.4 为准（Step 3~7 重排，
> 映射见其 §附-2）。下文保留复审当时的提议，供追溯。

| 顺序 | 内容 | 量级 | 依赖 / 决策点 |
| --- | --- | --- | --- |
| 0 | 本地 `main` fast-forward 到 `9db2c00`；补 issue/标签 | — | — |
| 1 | **Step 3 原子快照**（S3-a~e，含 R19） | S | 无 |
| 2 | **A4-④ 工程卫生**（CI 触发 / gitignore / 分支清理） | S | 无 |
| 3 | **A4-① `parallel_build` 默认翻转** | S | 需同步改 S2-T22 |
| 4 | **A4-② 低选择度暴力兜底** + NFR 口径 | S | **决策点 D2** |
| 5 | **Step 5 墓碑回收**（含 manifest 重发 + workload 脚本） | M | Step 1 |
| 6 | **A4-③ Metrics 可观测** | S | Step 4 前置 |
| 7 | **Step 4 spike**（E1/E2/E3）+ **决策 A3**（NFR-03 口径） | S → 视数据决定 M | **决策点 D1** |
| 8 | Step 4 主体（增量构建 T7-11 + 并发压测 T7-17 ± embed 并行） | M | spike 结论 |
| 9 | **Step 6 精排**（先定 H3 + 复现基线） | M | **决策点 D3** |

---

## 10. 需要拍板的 4 个问题

| # | 问题 | 选项 |
| --- | --- | --- |
| **D1** | NFR-03（1 万 chunk 构建 <120s，实测 225.5s）怎么办？ | ① 拆「首次全量 / 增量」双口径（推荐）② 降 Should + 记已知偏离 ③ 换更小模型（相关性重测） |
| **D2** | 低选择度延迟爆炸（10 万级 0.1% 档 158ms）是否从 V2.1 提前到 V2.0？ | ① 提前（S 级，方案已明确）② 留在 V2.1，但先补 NFR 口径 |
| **D3** | Step 5 是否提到 Step 4 之前？ | ① 提前（推荐：正确性优先 + 与 ADR-A 上下文相邻）② 维持原顺序 |
| **D4** | `parallel_build` 默认值翻转怎么走？ | ① 独立 PR（推荐，含改 S2-T22）② 并入 Step 3 ③ 暂不翻（保持显式开启） |

---

## 附：本次复审的证据清单

- 计划/需求/架构：`plan-v2.md`（v0.3）、`requirements-spec.md`（v1.6）、`architecture-design.md` §7.6.2、§14.1（R18~R25）、`eval-report.md` §8.2/§10
- 代码事实：`storage/snapshot.rs:88`（快照非原子）、`storage/graph.rs:236`（manifest 原子写）、`search/index.rs:5-6`（add/commit 可见性）、`search/index.rs:433-461`（save 顺序）、`search/config.rs:153` 与 `vector/hnsw_rs_index.rs:88`（`parallel_build` 默认 false）、`query/metrics.rs:1-15`（不可观测）
- 依赖源码（已核实 fastembed 6.0.2）：`src/init.rs:30-33`（intra_threads 默认满核）、`src/init.rs:133`（`with_intra_threads`）、`src/text_embedding/impl.rs:373`（256 批串行）、`src/models/text_embedding.rs:46`（无中文量化变体）、`src/models/reranking.rs:6-11`（`BGERerankerV2M3` 存在，含 `model.onnx.data`）、`src/reranking/impl.rs:41`（`TextRerank::try_new`）
- 依赖源码（ort 2.0.0-rc.13）：`Cargo.toml:145` `coreml = ["ort-sys/coreml"]`
- 仓库状态：`gh pr view 18`（MERGED `9db2c00`，CI 9/9）、`gh issue list`（#2/#4/#5 open）、`gh label list`（缺 Step3/5）、`git check-ignore`（sidecar 与 manifest tmp 未被忽略）
