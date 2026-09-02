# index-demo 项目长期记忆

## 项目定位
面向 Agent 场景的**通用检索引擎内核**（Rust）。场景层（企业知识库 RAG / Agent 记忆 / 代码检索）后接，内核不做领域假设。

核心立论：Agent 作为检索调用方与人类搜索有五个根本差异（query 由 LLM 生成含噪声、结果直接进 prompt 无翻页、处于多轮循环关键路径被高频调用、需要自查检索质量、Agent 自己零散写入）。这些差异被翻译为**接口与质量属性约束**，而非领域逻辑。

## 项目约定
- 库与 CLI 分离：`crates/core`（index-core）+ `crates/cli`（idx）
- 模块职责边界严格：**retriever 不与其他 lane 交互；fusion 不回捞正文；query 只编排不实现算法**
- 全部核心能力抽象为 trait：`Analyzer` / `Embedder` / `VectorIndex` / `Retriever` / `FusionStrategy` / `Reranker`
- feature flags：`default = ["local-embed"]`，另有 `remote-embed`、`positions`
- 参数不盲信英文经验值，一律用评测集实测校准

## 不可动摇的坑（每次改动前回顾）
1. BGE 查询侧加 instruction 前缀，入库侧不加
2. 向量入库前 L2 归一化（`cos = 1 - d²/2`）
3. 索引侧与查询侧必须共用同一 Analyzer
4. BM25 用 OR 不用 AND
5. `instant-distance` 无增量插入 → 主索引 + delta 暴力区

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
- NFR 中的延迟/内存/构建耗时指标均为**目标值待实测**，P5 前不得作为对外承诺

## 技术约束（2026-09-02 三方库调研后确定，见 docs/thirdparty.md）
- **MSRV 1.90**（NFR-08，原 1.80 不可行）；项目 License `MIT OR Apache-2.0`；`Cargo.lock` 必须入库
- 依赖 License 白名单由 `cargo-deny` 强制（NFR-09）
- `tantivy` 仅作 dev 依赖做 BM25 正确性基线（ADR-007），不参与构建产物
- 缓存用 `moka` 不用 `lru`（并发安全 + TTL）；中英分段用 `unicode-segmentation`，文本过 NFC
- `fastembed` 6.0.2 精确锁定 `ort =2.0.0-rc.13`（预发布，5.x/6.x 相同）；**必须 `default-features = false`**（否则引入 NCSA 的 image 链）；默认把模型下到项目根 `.fastembed_cache/`（已 gitignore）
- 序列化用 **bincode 2.0.1**；**3.0.0 是玩笑发布**（源码仅 `compile_error!`）——**crates.io 的 max_version 不等于可用版本，调研依赖必须验证能编译**
- 中文 embedding 枚举变体是 `EmbeddingModel::BGESmallZHV15`（非 `BGESmallZH`）；实际下载仓为 `Xenova/bge-small-zh-v1.5`（HF 未声明 License，**已决策接受**，沿用上游 MIT；P2 复核）
- git 身份：全局 `GongYubo <gongyubo@gmail.com>`。⚠️ 家目录有 ACL `group:everyone deny delete`，`git config --global` 会失败，**改 ~/.gitconfig 必须直接编辑文件内容**（不能用 git config 写）
- P4 先用 `hnsw_rs`（纯 Rust + 原生增量 insert）与「instant-distance + delta」A/B，达标则删掉 delta 区
