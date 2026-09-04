# HelixIndex 项目长期记忆

> 项目 **HelixIndex**（V1 原名 index-demo，2026-09-03 定名）。库 crate `helix-core`，CLI 二进制 `helix`。
> 远端 `https://github.com/Gong-Yubo/helix-index`（private，默认分支 main）。
> **本地工作目录仍是 `index-demo`**（决策 D-E3，未改名）。当前版本 0.2.0（P6 接口重构）。

## 项目定位与立论
面向 Agent 场景的**通用检索引擎内核**（Rust）。场景层（知识库 RAG / Agent 记忆 / 代码检索）后接，内核不做领域假设。
Agent 作为调用方与人类搜索有五个根本差异（query 含 LLM 噪声、结果直接进 prompt 无翻页、在多轮循环关键路径被高频调用、需自查检索质量、Agent 自己零散写入）——这些被翻译为**接口与质量属性约束**，而非领域逻辑。

**质量属性优先级**（冲突时取舍顺序）：正确性/确定性 > 首条精确率 MRR > 延迟 P99 > 召回率 > 资源占用

## 项目约定
- 库与 CLI 分离：`crates/core`（helix-core）+ `crates/cli`（helix）
- 命令：`helix build` / `helix search` / `helix bench`；开发态 `cargo run -p helix -- ...`
- **模块职责边界**：retriever 不与其他 lane 交互；fusion 不回捞正文；query 只编排不实现算法
- 核心能力全部 trait 化：`Analyzer` / `Embedder` / `VectorIndex` / `Retriever` / `FusionStrategy` / `Reranker`
- feature flags：`default = ["local-embed"]`，另有 `remote-embed`、`positions`、`charabia`（验证性，非默认）
- 参数不盲信英文经验值，一律用评测集实测校准

## 不可动摇的坑（每次改动前回顾）
1. BGE 查询侧加 instruction 前缀，入库侧不加
2. 向量入库前 L2 归一化（`cos = 1 - d²/2`）
3. 索引侧与查询侧必须共用同一 Analyzer
4. BM25 用 OR 不用 AND
5. ~~instant-distance 无增量插入 → 主索引 + delta 暴力区~~（已彻底移除，生产路径 HnswRsIndex 原生增量）

## 文档结构（开发文档均在 `docs/devel/`，索引见 `docs/README.md`）
- `plan.md` = **V1 计划（冻结）**；`plan-v2.md` = V2 计划；`v2-stepN-design.md` = V2 分步设计
  （**不用 pX-design 命名**——P 前缀已被阶段号占用，评审 M2 提过）
- `requirements-spec.md`（需求分析说明书）：FR-xx / NFR-xx / 术语表 / 验收指标的**唯一定义源**
- `architecture-design.md`（架构设计说明书）：含 ADR-001~011、质量属性设计、阶段→需求覆盖矩阵
- `thirdparty.md` 三方库调研；`pN-design.md` 各阶段设计；`eval-report.md` 评测报告；`archive/` 仅历史追溯

**文档修改规则**：需求变更只改需求文档，实现变更只改架构文档；架构文档只引用需求编号，不复制需求定义。
**命名约定**（2026-09-02）：文件名一律英文；文档内中文标题保留；交叉引用用英文文件名，路径以仓库根为基准。
**CHANGELOG.md 头部约定「此后每次变更即时追加」**——每次提交必须加条目，别漏。

## 环境
- Rust stable 1.98.0；`rust-toolchain.toml` pin **MSRV 1.90**（NFR-08，原 1.80 不可行）
- ⚠️ **cargo 只在登录 shell 的 PATH 里**，非登录 shell 需先 `export PATH="$HOME/.cargo/bin:$PATH"`
- 入口：`make fmt` / `make lint` / `make test` / `make deny` / `make all`
- git 身份 `GongYubo <gongyubo@gmail.com>`。⚠️ 家目录有 ACL `group:everyone deny delete`，
  `git config --global` 会失败，**改 ~/.gitconfig 必须直接编辑文件内容**
- 模型缓存目录 `~/.cache/helix-index/models`

## 技术约束（详见 docs/devel/thirdparty.md）
- License `MIT OR Apache-2.0`；`Cargo.lock` 必须入库；依赖 License 白名单由 `cargo-deny` 强制（NFR-09）
- `tantivy` 仅 dev 依赖做 BM25 正确性基线（ADR-007），不参与构建产物
- 缓存 `moka`（并发安全 + TTL）；中英分段 `unicode-segmentation`，文本过 NFC
- `fastembed` 6.0.2 精确锁 `ort =2.0.0-rc.13`；**必须 `default-features = false`**（否则引入 NCSA 的 image 链）
  - 中文枚举变体是 `EmbeddingModel::BGESmallZHV15`；实际下载仓 `Xenova/bge-small-zh-v1.5`（HF 未声明 License，已决策接受，沿用上游 MIT）
  - **不自动加 BGE 查询 instruction 前缀** → `embed_query` 自己加 `为这个句子生成表示以用于检索相关文章：`；`get_model_info` 是**关联函数**不是实例方法
  - 输出已 L2 归一化（`output.rs:49`），`NormalizedVector::new` 幂等再归一化（双保险）
- 序列化 **bincode 2.0.1**（3.0.0 是玩笑发布，源码仅 `compile_error!`——**crates.io max_version ≠ 可用版本**）
  - bincode 2 不支持 `serde_json::Value`（AnyNotSupported）→ 快照 metadata 用 DTO 存 JSON 字符串；需显式开 `serde` feature
- 快照格式：magic "IDX1" + version + crc32（header 后正文）；term_dict 按 TermId 升序导出防漂移；content_hash 用 xxhash-rust xxh64

## 已定稿参数与结论
- **P4 A/B（2026-09-03）：hnsw_rs 胜出**——召回同为 0.970，原生增量 1.5ms/条 vs instant-distance 全量重建 31s/万条。
  ⚠️ **DistDot 在 aarch64 有 `assert!(dot<=1)` 浮点断言会 panic**（自匹配 dot=1.0000002）→ 用自定义 `DistDotClamped`；**crate 名是 hnsw_rs（下划线）**
- **P5 定稿**：BM25 `k1=1.5 / b=0.75`；RRF `k=60 / weights=[1.0, 1.5]`；ef_search 200（校准近似误差 <0.002）。
  三路结论：hybrid MRR@10(1)=0.6922 最高；NDCG 0.5183 未超 vector 0.5249（p=0.385，如实记录）；paraphrase 桶 RRF 被弱路 BM25 稀释
- **P5 评测数据源：T2Ranking**（THUIR，SIGIR 2023，Apache-2.0，HF `THUIR/T2Ranking` 直下），转换器 `t2_prep.rs`。
  调研教训：DuReader-retrieval 有注册墙且许可未明示；mMARCO-zh 是机翻；Multi-CPR 假负例严重
- NFR 实测（eval-report §8）：NFR-02 延迟达标、NFR-03 构建未达标、NFR-04 快照加载达标但图重建 11.57s、NFR-05 向量 24.6MB 吻合理论
- batch_size 32~256 差异 <20% 且小 batch 略快 → 维持 64

## CLI 约定
- 短参 `-i` = `--input`（语料）；`--index`（快照）**不给短参**。⚠️ 两者都给 `-i` 时，**debug 构建 clap 直接 panic**
  （`Short option names must be unique`），release 才表现为误导性错误。已踩两次（P4 search、T5-03b bench）
- **默认值不得与内核定稿值双写**：`--rrf-k` / `--rrf-weights` 等声明为 `Option<T>`，未传时从 `RrfFusion::default()` 等内核默认值派生（V1-14）

## 测试与 CI 的坑
- **HNSW 万条级测试必须 `--release` 且 `#[ignore]`**（debug 下 2000 条就 100s+）
- **测试要隔离 HNSW 随机性**：`hnsw_rs` 用 OS 熵建图，同一份向量两次建图结果可能不同。
  验证快照 round-trip 这类断言**改用确定性 `BruteForceIndex`**，否则 flaky（CI Linux 复现过失败）
- ⚠️ **`hnsw_rs::search` 有固有近似误差：即使 `knbn == len`、`ef=200` 也可能返回不足 `len` 条**
  （20 点图采样 200 次：191 满 / 8 次少 1 / 1 次少 2 ≈ 4.5% 缺口）。库层行为，非参数 bug，path A/B 都受影响。
  ⇒ ① 对"返回条数"的严格断言留余量 ② **验收配比让存活数 ≥ 2×K**（K=5/存活 5 曾假红，改 30 删 20 留 10 后 50 次 0 失败）
- **验收测试要反向验证**：摘掉修复后测试必须变红。断言"结果不含幽灵 chunk_id"这类**修复前后都真**的是假测试（F1 教训）
- **加 `#![deny(missing_docs)]` 后必须逐 feature 验证**（charabia 等隔离分支会漏）
- **stable clippy 1.98 新 lint 会误伤旧代码**：`map_or`→`is_some_and`/`is_none_or`、`%2==0`→`is_multiple_of`、`println!(&x)`→去 `&`。修法对 MSRV 1.90 安全

## 工具环境坑
- **macOS BSD grep 不支持 `\|`**（ripgrep 才支持）：Bash 里 `grep "a\|b"` 静默不匹配并返回 exit 1，
  极易误判「代码里没有某符号」。**一律用 Grep 工具**
- **并行 Edit 同一文件会互相覆盖**：同一条消息对同一文件发两次 Edit，后一次基于旧快照写入，前一次丢失（工具仍报 success）。同文件多次编辑必须串行

## P6 门面层坑（详见 p6-design.md §9.1）
- **`crate::error::Result<T>` 是单泛型别名**，写 `Result<SearchMode, String>` 会撞别名（E0107）→ 用 `std::result::Result` 完整路径
- **门面层 SearchIndex 不实现 Debug**（含 `Box<dyn VectorIndex>`）→ 测试里 `unwrap_err()` 需 `T: Debug`，改用 `match`
- 所有权切换用 **`Arc::try_unwrap`**（非设计的 make_mut）：`Box<dyn VectorIndex>` 不可 Clone
- 指纹校验：embedder 维度**仅快照含向量时才严格**
- `SearchIndex::embed_elapsed()` 累计真实 embed 耗时，**不能在 commit 处计时**（add_documents 已分批 flush）

## V2 核心机制（完整推导见 `docs/devel/v2-step1-design.md`，这里只留最易踩的结论）
- **⚠️⚠️ hnsw_rs `search_filter` 是"半残的下推"**（源码逐行核实 0.3.4，修正过一次，别用旧结论）：
  `DataId = usize`（insert 传的 chunk_id）；被过滤节点**仍进 candidate_points** → 连通性不破能穿过过滤区 ✅；
  但不进 return_points 堆。三个反直觉机制：
  1. **带 filter 时从不 fast return**：无 filter 才 `return`；有 filter 只 `retain` 入口点后 fall-through，唯一正常终止 = candidate 耗尽
  2. **堆未满时距离剪枝全程关闭**（`return_points.len() < ef`）⇒ **只要 `allowed < ef` 就必然整图遍历**。
     库层结构性行为，调参治不了（ef 只决定"是否进入整图遍历"，不能降成本）
  3. **`ef = ef_arg.max(knbn)`** ⇒ 输出上限恒 `min(knbn,ef) = knbn`。**`knbn` 才是条数旋钮，`ef` 只是宽度**
- ⇒ **双路径定案（设计 §5.7）**：无用户过滤（热路径）走普通 `search()` 保 fast-return，ef=200，
  `knbn = min(len, ceil(k/alive_ratio)+k).min(1024)` 过采样后 `retain(alive)` + 稳定排序 + truncate；
  有用户过滤走 `search_filter`，`ef = min(max(4k,k), 256)` 纯宽度。⚠️ **别用 `ef.min(allowed)` 夹逼——无效**
- **软删除单一真源在 `Index.forward`**（D-S1-01）：存活位图与 chunks 的 Some/None 同入口维护；
  **`VectorIndex` 不加 `tombstone`**（双写必漂移；load 时 raw_vectors 全量重灌，向量侧状态无法从快照恢复）。
  位图自研 `Vec<u64>` 不引 roaring（ID 稠密、`count_ones()` O(1) 供 ef 策略与空集短路用）
- ⚠️ **别再写"HashSet 破坏 NFR-06"——那是伪证**：`allowed` 只用于 `lane.retain(...)`（保 Vec 序），
  lane 顺序由 sort(score→chunk_id) 决定，与 HashSet 迭代序无关（评审 P2-1 已驳倒）
- **过滤谓词用惰性判定，不要 O(N) 展开**（D-S1-09）：`contains(id) = alive(id) && doc_bits[doc_of(id)]`，两次 O(1)。
  字段索引只建 **doc 级**位图；`allowed_count` 靠 `doc_chunk_count: Vec<u32>` 对匹配文档求和。
  v0.1 的 `expand_to_chunks` 每 query 一次 O(N) 全扫，**没消掉 O(N)**（Q-I1 病根）
- ⚠️ **Q-C1 真实危害是「名额被占」不是「召回脏数据」**：幽灵候选进不了最终 hits（回捞时 `Index::chunk(id)`
  对墓碑返 `None` 直接 continue）。可观测危害是 **要 5 条只给 2 条**，且 Agent 无法区分语义。
  ⇒ **验收必须断言 `hits.len() == k`**
- **空结果两条分支必须同口径跑 `query_has_hits` 探针**，遵循「query 侧信号优先」，否则「query 无命中 + 过滤排空」
  误报 `FilteredOut`。⚠️ 该探针是 **BM25 词典探针**，Vector 模式下会误报 `AllTermsUnmatched`（已知边界）
- **`search/index.rs` 旧注释「已删向量会被丢弃」是错的**：`raw_vectors` 无移除路径 → save 原样导出 → load 全量重灌
  ⇒ 幽灵候选**跨快照永续**。防线①：存活位图检索期过滤；防线②：`SearchIndex::remove` 摘 `raw_vectors`
- `FilterT` 需 `Send + Sync`（Hybrid 两路在 `rayon::join` 共享谓词引用）
- 已知未修优化点：`ForwardStore::chunk_ids_of_doc` 是 O(N) 全扫，而 `Index::remove` 每次删除都调它 ⇒ 删除 O(N)
- **字段索引高基数降级是已知取舍**：单字段 >1024 个不同值即永久降级 → 该字段所有过滤退回 O(N) 全扫
  （毫秒时间戳类 Range 过滤最常见的真实场景正是受害者）。`degraded` **粘滞是有意设计**——降级窗口内被跳过的值
  不在索引里，复位会产生假阴性。**别"好心修掉"**（注：save/load 后 rebuild 会复位，存在细微前后差异）
