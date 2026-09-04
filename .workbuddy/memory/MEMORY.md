# HelixIndex 项目长期记忆

> 项目于 2026-09-03 正式定名 **HelixIndex**（版本 0.1.0 → **0.2.0（2026-09-04，P6 接口重构）**）。
> 原名 index-demo；库 crate `index-core` → **`helix-core`**，CLI 二进制 `idx` → **`helix`**。
> 远端：`https://github.com/Gong-Yubo/helix-index`（private，默认分支 main）。
> **本地工作目录仍是 `index-demo`**（决策 D-E3，未改名）。

## 项目定位
面向 Agent 场景的**通用检索引擎内核**（Rust，crate `helix-core`，CLI `helix`）。场景层（企业知识库 RAG / Agent 记忆 / 代码检索）后接，内核不做领域假设。

核心立论：Agent 作为检索调用方与人类搜索有五个根本差异（query 由 LLM 生成含噪声、结果直接进 prompt 无翻页、处于多轮循环关键路径被高频调用、需要自查检索质量、Agent 自己零散写入）。这些差异被翻译为**接口与质量属性约束**，而非领域逻辑。

## 项目约定
- 库与 CLI 分离：`crates/core`（**helix-core**）+ `crates/cli`（**helix**，二进制同名）
- 命令形如 `helix build` / `helix search` / `helix bench`；开发态 `cargo run -p helix -- ...`
- 模块职责边界严格：**retriever 不与其他 lane 交互；fusion 不回捞正文；query 只编排不实现算法**
- 全部核心能力抽象为 trait：`Analyzer` / `Embedder` / `VectorIndex` / `Retriever` / `FusionStrategy` / `Reranker`
- feature flags：`default = ["local-embed"]`，另有 `remote-embed`、`positions`、`charabia`（T5-09 验证性，非默认）
- 参数不盲信英文经验值，一律用评测集实测校准

## 不可动摇的坑（每次改动前回顾）
1. BGE 查询侧加 instruction 前缀，入库侧不加
2. 向量入库前 L2 归一化（`cos = 1 - d²/2`）
3. 索引侧与查询侧必须共用同一 Analyzer
4. BM25 用 OR 不用 AND
5. ~~instant-distance 无增量插入 → 主索引 + delta 暴力区~~（已彻底移除 D8；生产路径 HnswRsIndex 原生增量）

## 文档结构（全部开发文档在 `docs/devel/` 下，索引见 `docs/README.md`）
- `docs/devel/plan.md`（开发计划）：P0~P6 任务级清单（T0-01~T5-09）+ 阶段门槛 Gate + 进度追踪表。**开工前先看这个**
- `docs/devel/requirements-spec.md`（需求分析说明书，10 章）：做什么/为什么/怎么验收。**FR-xx、NFR-xx、术语表、验收指标的唯一定义源**
- `docs/devel/architecture-design.md`（架构设计说明书，14 章）：怎么做/为什么这么做。含 ADR-001~008、质量属性设计、阶段→需求覆盖矩阵
- `docs/devel/thirdparty.md`：第三方库调研（引入/自研判定、License 与 MSRV 核查）
- `docs/devel/archive/requirements-and-design_v1.0.md`：v1.0 合并版，仅历史追溯

**文档修改规则**：需求变更只改需求文档，实现变更只改架构文档；架构文档只引用需求编号，不复制需求定义。
**文档命名约定**（用户要求 2026-09-02）：文件名一律英文；文档内中文标题保留；交叉引用用英文文件名，路径以仓库根为基准（`docs/devel/xxx.md`）。

## 质量属性优先级（冲突时取舍顺序）
正确性/确定性 > 首条精确率 MRR > 延迟 P99 > 召回率 > 资源占用

## 环境
- Rust **已安装**：stable 1.98.0；项目经 `rust-toolchain.toml` pin **1.90.0**（MSRV）。**注意：cargo 只在登录 shell 的 PATH 里**，非登录 shell 需先 `export PATH="$HOME/.cargo/bin:$PATH"`
- 命令入口：`make fmt` / `make lint` / `make test` / `make deny` / `make all`
- **P0 已完成**（提交 5850545）：单条 embedding 推理 **1.58ms（debug）**、模型下载 49s、fastembed+ort 编译 42.5s
- **P0~P5 全部完成（2026-09-03）**。NFR 已实测（见 `eval-report.md` 第 8 节）：NFR-02 延迟达标、NFR-03 构建未达标、NFR-04 快照加载达标但图重建 11.57s、NFR-05 向量 24.6MB 吻合理论
- **P6 接口重构全部完成（2026-09-04）**：门面层 `SearchIndex`/`Searcher`（`search` 模块），
  见 `p6-design.md` 9.1 实施记录。关键新增约定与坑：
  - **`crate::error::Result<T>` 是单泛型别名**，写 `Result<SearchMode, String>` 会撞别名（E0107），用 `std::result::Result` 完整路径
  - **门面层 SearchIndex 不实现 Debug**（含 `Box<dyn VectorIndex>` 无法派生），测试里 `unwrap_err()` 需 `T: Debug` → 改用 `match` 判错误
  - 所有权切换用 **`Arc::try_unwrap`**（非设计的 make_mut）：`Box<dyn VectorIndex>` 不可 Clone
  - 指纹校验 embedder 维度**仅快照含向量时才严格**（纯 BM25 快照装配有无 embedder 均正确）
  - `SearchIndex::embed_elapsed()` 累计真实 embed 耗时；**不能在 commit 处计时**（add_documents 已分批 flush）
  - batch_size 实测 32~256 差异 <20% 且小 batch 略快 → 维持 64

## 技术约束（2026-09-02 三方库调研后确定，见 docs/thirdparty.md）
- **MSRV 1.90**（NFR-08，原 1.80 不可行）；项目 License `MIT OR Apache-2.0`；`Cargo.lock` 必须入库
- 依赖 License 白名单由 `cargo-deny` 强制（NFR-09）
- `tantivy` 仅作 dev 依赖做 BM25 正确性基线（ADR-007），不参与构建产物
- 缓存用 `moka` 不用 `lru`（并发安全 + TTL）；中英分段用 `unicode-segmentation`，文本过 NFC
- `fastembed` 6.0.2 精确锁定 `ort =2.0.0-rc.13`（预发布，5.x/6.x 相同）；**必须 `default-features = false`**（否则引入 NCSA 的 image 链）；默认把模型下到项目根 `.fastembed_cache/`（已 gitignore）
- 序列化用 **bincode 2.0.1**；**3.0.0 是玩笑发布**（源码仅 `compile_error!`）——**crates.io 的 max_version 不等于可用版本，调研依赖必须验证能编译**
- 中文 embedding 枚举变体是 `EmbeddingModel::BGESmallZHV15`（非 `BGESmallZH`）；实际下载仓为 `Xenova/bge-small-zh-v1.5`（HF 未声明 License，**已决策接受**，沿用上游 MIT；P2 复核）
- **fastembed 6.0.2 不自动加 BGE 查询 instruction 前缀**（全库无 query_embed）→ `embed_query` 必须自己加 `为这个句子生成表示以用于检索相关文章：`，入库侧不加（R2 是真的）；`get_model_info` 是**关联函数**不是实例方法
- **fastembed 输出已 L2 归一化**（`output.rs:49` `.map(normalize)`）；`NormalizedVector::new` 仍幂等再归一化（双保险）
- **CLI 短参约定**：`-i` = `--input`（语料），`--index`（快照）**不给短参**。
  ⚠️ 两者都给 `-i` 时，**debug 构建下 clap 直接 panic**（`Short option names must be unique`），
  release 下才表现为 `-i` 被绑到 `--index` 的误导性错误。P4 在 search 踩过一次，
  T5-03b 写 bench 时又踩一次（V1-11 修复）。新增子命令时注意。
- **CLI 默认值不得与内核定稿值双写**：`--rrf-k` / `--rrf-weights` 等参数应声明为 `Option<T>`，
  未显式传入时**从 `RrfFusion::default()` 等内核默认值派生**（V1-14），
  否则会出现 bench 与 search 的"默认融合"语义分裂。
- **模型缓存目录**：`~/.cache/helix-index/models`（改名时已从 `~/.cache/index-demo` mv 迁移 91MB）
- **HNSW 万条级测试必须 `--release` 且 `#[ignore]`**（debug 构建极慢，2000 条就要 100s+）；ef_construction=300 / ef_search=200（hnsw_rs_index.rs 常量，P5 校准后维持）
- **测试要隔离 HNSW 的随机性**：`hnsw_rs` 用 OS 熵建图（R-P5-13），同一份向量两次建图结果可能不同。
  验证"快照 round-trip 正确性"这类断言**不要用 HnswRsIndex**，改用确定性的 `BruteForceIndex`，
  否则测试会 flaky（已在 CI Linux 上复现过一次失败）
- **加 `#![deny(missing_docs)]` 后必须逐 feature 验证**（charabia 等隔离分支会漏，CI 抓到过）
- git 身份：全局 `GongYubo <gongyubo@gmail.com>`。⚠️ 家目录有 ACL `group:everyone deny delete`，`git config --global` 会失败，**改 ~/.gitconfig 必须直接编辑文件内容**（不能用 git config 写）
- **P4 A/B 已定案（2026-09-03）：hnsw_rs 胜出**——召回同为 0.970，原生增量 1.5ms/条 vs instant-distance 全量重建 31s/万条；生产路径已切 HnswRsIndex。⚠️ **hnsw_rs 的 DistDot 在 aarch64 有 assert!(dot<=1) 浮点断言会 panic**（自匹配 dot=1.0000002）→ 用自定义 DistDotClamped；**crate 名是 hnsw_rs（下划线）**
- **D8 已执行（2026-09-03）：instant-distance 彻底移除**（依赖 + hnsw.rs + ImmutableIndex 变体）；`tests/vector_ab.rs` 改写为 HnswRs vs Brute 真实语料召回对照（2000 段×100 query，Top-10 重合率 0.994）
- **P5 定稿参数**：BM25 `k1=1.5 / b=0.75`（16 格网格）；RRF `k=60 / weights=[1.0, 1.5]`（weights 诊断，向量侧加权）；ef_search 200（100/200/400 校准近似误差 <0.002）。**三路结论：hybrid MRR@10(1)=0.6922 最高，NDCG 0.5183 未超 vector 0.5249（p=0.385，如实记录）；paraphrase 桶 RRF 被弱路 BM25 稀释**
- **P5 评测数据源（2026-09-03 定案，p5-design.md v1.2）：T2Ranking**（THUIR，SIGIR 2023，Apache-2.0，HF `THUIR/T2Ranking` 直下）。dev 24,832 query 带 4 级 TREC qrels；BM25 MRR@10=0.359 论文基线可做外部锚点。转换器 `t2_prep.rs` 固定种子装配 ~320 query + ~12K 段落。**检索方向调研教训：DuReader-retrieval 有注册墙且数据许可未明示；mMARCO-zh 是机翻；Multi-CPR 假负例严重**
- **bincode 2 不支持 serde_json::Value**（serialize_any → AnyNotSupported）→ 快照对 metadata 用 DTO 存 JSON 字符串；且 bincode 2 需显式开 `serde` feature 才有 bincode::serde
- 快照格式：magic "IDX1" + version + crc32(header 后正文)；term_dict 按 TermId 升序导出防漂移；content_hash 用 xxhash-rust xxh64

## V2 约定与坑（2026-09-04 起）
- **V2 设计文档命名 `docs/devel/v2-stepN-design.md`**（不用 pX-design，P 前缀已被阶段号占用，评审 M2 同源问题）；
  `plan.md` = V1 计划（冻结），`plan-v2.md` = V2 计划
- **⚠️⚠️ hnsw_rs `search_filter` 是"半残的下推"（源码逐行核实 0.3.4，2026-09-04 修正过一次，别用旧结论）**：
  `DataId = usize`（hnsw.rs:50，即 insert 传的 chunk_id）；被过滤节点**仍进 candidate_points**
  （hnsw.rs:1026-1027 push 在 1032 的 filter 判断前）→ 连通性不破，能穿过被过滤区域 ✅；
  但不进 return_points 堆（1028-1041），堆上限 ef（1042-1044）。

  三个反直觉的真实机制（v0.1 曾把第 1 条误读为"凑够 ef 才返回"，已被评审推翻）：
  1. **带 filter 时从不 fast return**：983 分支——无 filter 才 `return`（984）；
     有 filter 只 `retain` 剔除未通过过滤的入口点（986-991）后 **fall-through 继续循环**。
     唯一正常终止 = candidate_points 耗尽（while 在 960）。
  2. **堆未满时距离剪枝全程关闭**：1019 `if e_dist_to_p < f_dist_to_p || return_points.len() < ef`
     ⇒ 通过点数 < ef 时**任何邻居都进候选**，候选持续膨胀 ⇒ **整图遍历**。
     ⇒ 判据：**只要 `allowed < ef`，堆恒填不满 ⇒ 剪枝永不开启 ⇒ 必然整图遍历**。
     **这是库层结构性行为，调参治不了**（ef 只能决定"是否进入整图遍历"，不能降低成本）。
  3. **`ef = ef_arg.max(knbn)`**（1519）⇒ 输出上限恒为 `min(knbn,ef) = knbn`。
     **`knbn` 才是输出条数旋钮，`ef` 只是搜索宽度**——想多返回要调 knbn，调 ef 无效。

  ⇒ **定案（v2-step1-design §5.7 双路径）**：
  - 无用户过滤（热路径）→ **走普通 `search()` 保住 fast-return**，`ef` 仍 200，
    `knbn = min(len, ceil(k/alive_ratio)+k).min(1024)` 按删除比例过采样，返回后 `retain(alive)` + 稳定排序 + truncate。
    （用 `search_filter` 跑 AliveOnly 会禁用 fast-return，改变热路径成本结构——这是 P99 敏感路径）
  - 有用户过滤 → `search_filter`，`ef = min(max(4k,k), 256)` **纯宽度**。
    ⚠️ **不要用 `ef.min(allowed)` 夹逼——无效**：allowed<ef 时堆恒不满、剪枝本就关闭；
    allowed≥4k 时该 min 又不生效。低选择度宁可召回不足，用 `Metrics::vector_shortfall` 暴露。
- **软删除的单一真源在 `Index.forward`**（V2 Step 1 D-S1-01）：存活位图与 chunks 的 Some/None 同入口维护，
  检索时以**位图谓词**传给向量路；**`VectorIndex` 不加 `tombstone`**（双写必漂移；且 load 时 raw_vectors
  全量重灌，向量侧状态无法从快照恢复）。位图自研 `Vec<u64>` 不引 roaring（ID 稠密、内存小、
  `count_ones()` O(1) 供 ef 策略与空集短路决策）。
  ⚠️ **别再写"现状 HashSet 破坏 NFR-06"——那是伪证**：`allowed` 只用于 `lane.retain(...)`（保 Vec 序），
  lane 顺序由 sort(score→chunk_id) 决定，与 HashSet 迭代序无关（评审 P2-1 已驳倒）
- **过滤谓词用惰性判定，不要 O(N) 展开**（D-S1-09）：
  `contains(chunk_id) = alive(chunk_id) && doc_bits[doc_of(chunk_id)]`，两次 O(1)。
  字段索引只建 **doc 级**位图；`allowed_count` 靠 `ForwardStore::doc_chunk_count: Vec<u32>` 对匹配文档求和（O(|匹配|)）。
  v0.1 的 `expand_to_chunks`（doc 位图 → chunk 位图）**每 query 一次 O(N) 全扫**，只把 O(N) 次 JSON 求值
  换成 O(N) 次位运算，**O(N) 没消掉**——而 Q-I1 的病根正是 O(N)。惰性判定使每 query 成本与语料规模解耦
- **`search/index.rs` 旧注释「已删向量会被丢弃」是错的**：`raw_vectors` 无移除路径 → save 原样导出 →
  load 全量重灌 ⇒ 幽灵候选**跨快照永续**。修法靠存活位图在检索期过滤（V2 Step 1）
- **`ForwardStore::chunk_ids_of_doc` 是 O(N) 全扫**，而 `Index::remove` 每次删除都调它 ⇒ 删除是 O(N)。
  已记录，未修（V2 后续优化点）
- `FilterT` 需 `Send + Sync`（Hybrid 两路在 `rayon::join` 共享谓词引用）

## V2 Step 1 实施期结论（2026-09-04，S1-01~S1-09 完成）
- **⚠️ Q-C1 的真实危害是「名额被占」，不是「召回脏数据」**：幽灵候选**进不了最终 hits**——
  `search_parts` 回捞时 `Index::chunk(id)` 对墓碑返回 `None` 直接 `continue`（`query/searcher.rs:222-225`）。
  真正可观测危害：Top-K 的 K 个位置混进幽灵、回捞时才被丢弃 ⇒ **要 5 条只给 2 条**，
  而 Agent 无法区分「库里只有 2 条」与「有 5 条但 3 个位置被幽灵占了」。
  ⇒ **验收必须断言 `hits.len() == k`；断言"结果不含幽灵 chunk_id"是修复前后都为真的假测试**
  （T1/T2 初版踩过：把谓词摘掉后测试仍全绿，做「临时摘掉修复→确认测试变红」的反向验证才暴露）
- **空结果原因两条分支必须同口径跑 `query_has_hits` 探针**：空集短路分支（filter 排空）与
  `fused.is_empty()` 分支都要遵循「query 侧信号优先」，否则「query 无命中 + 过滤排空」会误报
  `FilteredOut`（Agent 会去调过滤条件，而问题在 query 词）。由 T16 抓出
- **stable clippy 1.98 新 lint 会误伤旧代码**：`map_or` → `is_some_and`/`is_none_or`、`% 2 == 0`
  → `is_multiple_of`、`println!(&x)` → 去 `&`。修法对 MSRV 1.90 安全（`is_some_and` 1.70 /
  `is_none_or` 1.82 / `is_multiple_of` 1.87）
- **macOS BSD grep 不支持 `\|`**（ripgrep 才支持）：Bash 里用 `grep "a\|b"` 会静默不匹配并返回 exit 1，
  极易误判「代码里没有某符号」。**一律用 Grep 工具**，别在 Bash 里写 `\|`
- **并行 Edit 同一文件会互相覆盖**：同一条消息里对同一文件发两次 Edit，后一次基于旧快照写入，
  前一次修改丢失（工具仍报 success）。同文件多次编辑必须串行
