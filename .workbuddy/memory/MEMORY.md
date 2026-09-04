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
