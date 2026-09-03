# v1 版本收尾 — 设计说明（使用文档 / 评测脚本 / CI / 改名与发布 / 工程收尾）

| 项目    | 内容 |
| ----- | --- |
| 版本    | **v1.2（决策已定，待开工）** |
| 日期    | 2026-09-03 |
| 状态    | **待确认，未开工** |
| 前提    | P0~P5 全部完成（提交 `9d5179a`）；**P6（v2：Rerank / MMR / 自研索引）不在本轮范围** |
| 上游    | `docs/devel/plan.md`、`docs/devel/eval-report.md`、`docs/devel/thirdparty.md` |
| 定位    | 把"能跑通的研究原型"变成"别人能接手、能复现、能持续集成的工程制品"，并正式定名发布 |

---

## 0. 外部评审修正说明（v1.0 → v1.1）

v1.0 经外部评审（DeepSeek）逐条对照仓库实况核验，指出 **1 处严重性低估、1 处事实遗漏、
8 项遗漏、4 处措辞问题**。全部修正**已经本人独立复验后接受**（无一条反驳）：

| # | 评审意见 | 复验结果 | 本版处理 |
| --- | --- | --- | --- |
| 1 | `bench -i` 冲突在 debug 下**直接 panic**，非仅"误导性错误" | ✅ 实测 `./target/debug/idx bench --help` panic | V1-11 升级为**阻塞级**，作 V1-04/05 前置 |
| 2 | `--rrf-weights` 默认 `"1,1"` 与 `RrfFusion::default()` 定稿 `[1,1.5]` 语义分裂 | ✅ 属实 | **新增 V1-14** |
| 3 | CI 无 doc 守门；`warn` 不构成约束 | 合理 | V1-02 改 `deny`；V1-08 加 doc job |
| 4 | `missing_docs` 对 cli（二进制 crate）是 no-op | ✅ cli 无 `lib.rs`、无 pub API | V1-02 聚焦 core |
| 5 | MSRV job 若继承 `rust-toolchain.toml` 则与主 job 冗余 | 合理 | V1-09 改为**独立 toolchain** + `--all-targets` |
| 6 | 承诺的"release build + 冒烟"没进任务表 | ✅ 属实 | 并入 V1-08 job 表 |
| 7 | `git remote -v` 为空，CI 前提不成立 | ✅ 属实 | **新增 V1-16**（CI 前置） |
| 8 | `/tmp` 存档 JSON 不入库，"逐格对账"验收不可持续 | ✅ 属实 | V1-06 增加**存档入库**产出 |
| 9 | 逐桶 p 值不在 JSON，`report.py` 需重算 | ✅ `buckets` 无 p 值，`per_query` 带 `type` 可重算 | V1-06 写明**重算契约** |
| 10 | NOTICE 漏 ONNX Runtime（MIT） | ✅ `thirdparty.md` L160 有记录 | V1-13 补 |
| 11 | 语料已入库，干净机器不需重下 3.5GB | ✅ 属实 | V1-04 明确说明 |
| 12 | NFR-05 的 383MB（整进程）与 24.6MB（向量分量）是两个口径 | ✅ 属实 | V1-05 拆分口径 |
| 13 | "一键复现"未区分**结论复现** vs **逐位复现** | 合理且重要 | 第 9 节验收标准明确区分 |
| 细节 | 验收"6 项说四个"、文档计数口径、`build` 无 `--json` | 属实 | 已逐条修正 |

> 文档计数口径澄清：`docs/devel/` 下 13 份 `.md`（含 `archive/` 1 份历史文档、
> 含本文件）。评审时点为 12 份（未含本文件），两者不矛盾。

---

## 1. 为什么要收尾

P5 结束时项目在**技术验证**上已闭环，但在**工程可用性**上仍是原型状态：

- **知识锁在会话里**：P5 全部评测数字是手工敲命令、从终端抄进 `eval-report.md` 的。
  换个人（或三个月后）要复现得重新拼命令——**评测不可复现，报告数字就不可审计**，
  这直接违背 T5-08 的诚信要求。
- **没有自动化守门**：feature 隔离的 charabia 分支、MSRV 1.90（NFR-08）都可能
  在某次提交后悄悄腐化而无人察觉。
- **文档只面向实现者**：`docs/devel/` 全是开发文档，场景层接入方只能读 README 的
  "快速上手"，**没有 CLI 完整参考、没有库接入指南、没有 rustdoc 强制覆盖**。
- **项目尚未定名、尚未有远端**：本地裸仓，无协作与发布基础。

## 2. 现状盘点（v1.1 修正版）

| 项 | 现状 | 本轮动作 |
| --- | --- | --- |
| README | ✅ 定位/快速上手/架构/评测摘要/数据引用/局限 | 补用户指南链接 + 改名为 HelixIndex |
| 开发文档 | ✅ `docs/devel/` 13 份 | 索引同步；**历史文档加注不改写** |
| **使用文档** | ❌ 无 CLI 完整参考、无库接入指南 | **V1-01 新建** |
| rustdoc | ⚠️ `make doc` 可用，无覆盖强制 | V1-02（`deny` 级，聚焦 core） |
| 效果评测 | ⚠️ `idx bench` 能力齐备，靠手工拼命令 | V1-04 脚本化 |
| 性能评测 | ⚠️ NFR 实测散落手工操作 | V1-05 脚本化 |
| 报告生成 | ⚠️ 表格手工抄录（**逐桶 p 值更是全手工算**） | V1-06 由 JSON 生成 |
| CI/CD | ❌ 完全空白 | V1-08~10 |
| **git 远端** | ❌ `git remote -v` 为空，仅本地 `main` | **V1-16**（CI 前置） |
| CHANGELOG / NOTICE | ❌ 无 | V1-12 / V1-13 |
| **`bench` debug 态** | ❌ **任何调用（含 `--help`）直接 panic** | **V1-11 阻塞级，最先修** |
| **`--rrf-weights` 默认** | ❌ `"1,1"` 与定稿 `[1,1.5]` 分裂 | V1-14 |
| 项目名 / 版本号 | ❌ 仍为 index-demo | **V1-15 改名 HelixIndex** |

---

## 3. A. 使用文档

### V1-01 `docs/user-guide.md`（新建）

**Part 1 —— CLI 使用者**

- 命令总览：`build` / `search` / `compare` / `bench`
- **全参数参考**（`bench` 有 20 个参数，含义现散落在 `p5-design.md` 各章）
- 典型工作流：建库落盘 → 秒级检索 → 元数据过滤 → `--explain` → `compare` → `bench`
  （含 `--grid` / `--runs` / `--vector-index brute` 诊断开关用法）
- 数据格式约定：语料 JSONL（`source`/`text`/`metadata`）、judgments 格式
- **已知坑**：`--single-chunk` 评测口径、构建耗时（NFR-03 未达标）、冷启动图重建（D7）、
  **`--rrf-weights` 默认值语义**（修完 V1-14 后此条可删）

**Part 2 —— 库接入方**

- 最小闭环代码（**引用** `examples/search_basic.rs`，不复制——避免两处漂移）
- **六 trait 替换矩阵**：默认实现 / 替换场景 / 影响面

  | trait | 默认实现 | 替换场景 | 影响面 |
  | --- | --- | --- | --- |
  | `Analyzer` | `MixedAnalyzer` | 换分词（charabia，feature 已隔离） | 索引与查询**必须同时换**（R4） |
  | `Embedder` | `LocalEmbedder` | 接远程 embedding / 自研模型 | 向量维度须一致 |
  | `VectorIndex` | `HnswRsIndex` | 换 ANN（`BruteForceIndex` 已现成） | 无 |
  | `Retriever` | `Bm25Retriever` / `VectorRetriever` | 加自定义召回路 | 需同步 fusion 权重 |
  | `FusionStrategy` | `RrfFusion`(k=60/w=1:1.5) | 换融合算法 | 无 |
  | `Reranker` | `NoOpReranker` | 接 rerank 模型（P6） | 无 |

- feature flags 选用建议；性能预期（引用 `eval-report.md` 第 8 节，**不复制数字**）

### V1-02 rustdoc 完善（v1.1 修正）

- **core 加 `#![deny(missing_docs)]`**（不是 `warn`——`warn` 不打断构建，不算守门）
- **cli 不加**：二进制 crate 无 `lib.rs`、无 pub API，`missing_docs` 是 no-op
- 补齐缺失注释（尤其 `query` / `fusion` / `vector` 三个对外 trait）
- 修 `lib.rs` 的"阶段进度 P0（当前）"→ 反映 P5 完成 + 改名后项目名

### V1-03 索引同步

README 指向 `docs/user-guide.md`；`docs/README.md` 增设"使用文档"分区。

---

## 4. B. 评测工具脚本

统一放 `scripts/`（新建），全部满足：`set -euo pipefail`、参数可覆盖、
输出机器可读、**固化 P5 定稿参数**保证一键复现。

### V1-04 `scripts/eval_quality.sh`

```bash
./scripts/eval_quality.sh                                   # 三路对照
./scripts/eval_quality.sh --grid                             # 16 格网格
./scripts/eval_quality.sh --modes bm25 --analyzer charabia    # 分词对照
./scripts/eval_quality.sh --runs 3 --json out.json            # 抖动披露
```

- 默认固化 `--rrf-weights 1,1.5`（定稿值，**不依赖 CLI 默认值**，故 V1-14 未修也不影响脚本正确性）
- **（v1.1 补充）默认直接吃入库的 `data/t2-corpus.jsonl` / `data/t2-queries.jsonl`，
  无需 3.5GB 原始数据；重装配走 `t2_prep.rs` 是可选离线路径**
- **（v1.1 补充）脚本内部走 release 二进制**，debug 会因 V1-11 的 panic 直接失败

### V1-05 `scripts/eval_perf.sh`

| NFR | 测量方式 | 脚本负责 |
| --- | --- | --- |
| NFR-02 延迟 | `bench` 阶段 B（6400 样本/模式） | 解析 P50/P99，比对 <5/<10/<20ms |
| NFR-03 构建 | `build --vectors --single-chunk` 的 embed 分段计时 | 比对 <120s |
| NFR-04 冷启动 | 快照加载 + HNSW 图重建**分别计时** | 分别比对（design 10.2 明确要求勿混） |
| NFR-05 内存 | peak RSS | 见下方口径 |

- **（v1.1 修正）NFR-05 两个口径必须分开报告**：
  - **整进程 peak RSS = 383MB**：`/usr/bin/time -l idx search --index <含向量快照> --mode hybrid "query"`
    （含模型 + 快照 + HNSW 图 + ort 运行时）
  - **向量分量 = 24.6MB**：`12000 × 512 × 4B` 推算值，非实测进程指标
- 跨平台：macOS `/usr/bin/time -l`，Linux `/usr/bin/time -v`，脚本内探测
- **（v1.1 说明）`build` 无 `--json`**，脚本需 parse stdout 的两行计时
  （`embed N 条 耗时 X` / `快照已写入 … 耗时 Y`）。**选 parse 而非给 build 加 `--json`**：
  加 `--json` 会扩大 CLI 表面积，而 parse 两行已足够稳（格式由我们自己控制）

### V1-06 `scripts/report.py`（v1.1 重点修正）

从 `bench --json` 生成 markdown 表格，消除手工抄录。

- 三路总表（双阈值 + NDCG）、分桶表、run-to-run 抖动表、网格 16 格表、延迟表
- **（v1.1 新增产出）存档 JSON 入库**：把 `/tmp/p5-final-3way.json`、`/tmp/p5-nfr-latency.json`
  等迁入 `scripts/fixtures/`，作为可重复执行的对账基准（`/tmp` 会被系统清理，
  不入库则三个月后验收标准无法执行）
- **（v1.1 新增）逐桶符号检验的**重算契约****
  - **事实**：`mode_to_json` 的 `buckets` 只有 `n / recall / mrr / ndcg`，**不含 p 值**；
    `per_query` 带 `qid / type / ndcg`，可据此重算
  - **契约**：report.py 必须严格对齐 `bench::sign_test` 的约定
    （`crates/core/src/bench/metrics.rs`）——**二项精确单侧、零值（并列）排除在 n 之外**；
    否则 p 值无法与 `eval-report.md` 现有表格逐格一致
  - **验证**：用存档 JSON 重放，逐格比对 eval-report 3.3 的
    `exact 49/3/28 (0.011)`、`paraphrase 25/11/44 (1.0)` 等

### V1-07 Makefile 入口

`make eval-quality` / `make eval-perf` / `make report`。

---

## 5. C. CI/CD（v1.1 修正）

### V1-08 `.github/workflows/ci.yml`（主流程）

触发：`push`(main) + `pull_request`。

| job | 内容 | 约束 |
| --- | --- | --- |
| `fmt` | `cargo fmt --all -- --check` | 格式 |
| `lint` | `cargo clippy --workspace --all-targets -- -D warnings` | lint |
| `test` | `cargo test --workspace` | 单测 |
| `deny` | `cargo deny check licenses bans sources` | **NFR-09** |
| `doc` | `cargo doc` + `RUSTDOCFLAGS="-D warnings"` | **（v1.1 新增）rustdoc 守门** |
| `build-release` | `cargo build --release --workspace` | 确保评测脚本依赖的二进制可编出 |
| `smoke` | demo 语料（30 篇）秒级 `bench` 冒烟 | 验证 CLI 未被改坏 |

（v1.1：把第 5 节承诺但未入表的 release build + 冒烟正式写入 job 表。）

### V1-09 MSRV 验证（NFR-08）

- **（v1.1 修正）必须独立于 `rust-toolchain.toml`**：用 `dtolnay/rust-toolchain@1.90.0`
  显式装 1.90.0 再 `cargo check`，否则与继承 toolchain 的主 job 完全冗余——
  真正的目的是防止有人把 toolchain 抬到 1.95 后 MSRV 声明悄悄失效
- **加 `--all-targets`**：`cargo check` 默认不编 dev-deps/tests，会漏掉 criterion 等
  dev 依赖的 MSRV 违规

### V1-10 feature 隔离验证

- `--features charabia` 编译 + 测试（防 T5-09 feature 分支腐化）
- `--no-default-features` 编译（验证 `local-embed` 真可选）

### 评测不进 CI（保留 v1.0 判断）

效果评测需 12K 段落 embed（~230s）；性能评测是毫秒级 P99 口径，
共享 runner 上的数字**不具备可引用性，反而会污染 `eval-report`**。
CI 只保留 `build-release` + 冒烟（见 V1-08）。

### V1-16 建立远端并推送（v1.1 新增，CI 前置）

`git remote -v` 当前为空、仅本地 `main`。GitHub Actions 必须在仓库推送到 GitHub 后才生效，
故本项是 V1-08 的硬前置。

---

## 6. D. 工程收尾

### V1-11 修 `bench` 短参冲突 —— **阻塞级**（v1.1 升级）

**现状（实测）**：
- **debug 构建**：`idx bench` 任何调用（**含 `--help`**）直接 panic
  ```
  Command bench: Short option names must be unique ... '-i' is in use by both 'index' and 'input'
  ```
  → `cargo run -p idx -- bench ...`（README 与 eval-report 都在用的开发态入口）**完全打不开**
- **release 构建**：`-i` 被绑到 `--index`，`-i data/corpus.jsonl` → `快照版本不兼容: 文件版本 1869816443`（误导性错误）

**修法**：与 `search` 对齐——**`-i` = `--input`（语料），`--index` 不给短参**。
**优先级最高**：不修则 V1-04/05 脚本的开发调试无法进行。

### V1-14 `bench --rrf-weights` 默认值对齐定稿（v1.1 新增）

- 现状：`RrfFusion::default()` = `[1.0, 1.5]`（P5 定稿），`idx search --mode hybrid` 走它；
  但 `bench` 的 CLI 默认 `"1,1"` 且 `make_searcher` 总是用 CLI 值覆盖融合
  → **裸跑 `idx bench` 拿到的不是定稿结论**
- 修法：CLI 默认值改为 `"1,1.5"`，**或从 `RrfFusion::default()` 派生**（后者更不易漂移）

### V1-12 `CHANGELOG.md`

记录 P0~P5 各阶段产出与 v1.0 能力边界（逆向补写），此后每次变更追加。Keep a Changelog 风格。

### V1-13 `NOTICE`

第三方资产引用义务（依据 `thirdparty.md` 5.2 / 5.3）：
- **T2Ranking 数据集**（Apache-2.0，SIGIR 2023 论文引用）
- **`Xenova/bge-small-zh-v1.5` 模型**（HF 仓未声明 License，P2 决策"沿用上游 MIT 接受"并记录）
- **（v1.1 补充）ONNX Runtime 二进制**（MIT，Microsoft）——`ort/download-binaries` 随构建下载，
  CLI 本身即二进制产物，声明须随附

---

## 7. E. 项目定名与仓库发布（v1.1 新增）

### V1-15 项目改名 **HelixIndex**，版本号 **0.1**

**改动范围**（已实测评估）：

| 范围 | 规模 | 处理方式 |
| --- | --- | --- |
| 代码 `index_core` 引用 | **56 处 / 8 文件** | 批量替换（crate 名决定后） |
| `Cargo.toml` | workspace + core + cli 的 `package.name`、cli 二进制名 | 手工改 |
| 版本号 | 现为 `0.1.0`（**已符合"0.1"**，确认即可） | 确认 |
| 活跃文档 | README、docs/README、requirements-spec、architecture-design、plan、thirdparty、eval-report（**21 处 / 13 文件**） | 替换 |
| **历史文档** | `p0~p5-design.md`、`archive/` | **加注不改写**（沿用既有约定，见 `p1-design.md` 先例） |

**命名方案**（待用户拍板，见 D-E1）：

| 方案 | 库 crate | CLI 二进制 | 命令形如 |
| --- | --- | --- | --- |
| E1（建议） | `helix-core` | `helix` | `helix build` / `helix search` |
| E2 | `helix-index` | `helix` | 同上 |
| E3 | `helix-index-core` | `helix-index` | `helix-index build` |

**目录名**：当前工作目录为 `index-demo`。是否同步改为 `helix-index` 待定（见 D-E3）。

### V1-16 推送到 GitHub 个人仓 `helix-index`

1. 创建远端仓（GitHub 个人账号，仓名 `helix-index`）
2. `git remote add origin` + 推送 `main`
3. 验证 GitHub Actions 被触发（V1-08 的前提）

---

## 8. 任务清单与执行顺序（v1.1）

| 编号 | 任务 | 依赖 |
| --- | --- | --- |
| **V1-11** | **修 `bench -i` panic（阻塞级）** | — |
| **V1-15** | **改名 HelixIndex + 版本号 0.1** | D-E1 拍板 |
| **V1-16** | **建立 GitHub 远端并推送** | V1-15（改名后再推，避免历史里两套名字） |
| V1-00 | 存档 JSON 入库 `scripts/fixtures/` | — |
| V1-04 | `scripts/eval_quality.sh` | **V1-11** |
| V1-05 | `scripts/eval_perf.sh` | **V1-11** |
| V1-06 | `scripts/report.py`（含逐桶 p 值重算契约） | V1-00, V1-04 |
| V1-07 | Makefile 入口 | V1-04/05/06 |
| V1-08 | CI 主流程 | **V1-16** |
| V1-09 | MSRV job（独立 toolchain + `--all-targets`） | V1-08 |
| V1-10 | feature 隔离 job | V1-08 |
| V1-14 | `--rrf-weights` 默认值对齐 | — |
| V1-01 | `docs/user-guide.md` | V1-15（名字定后再写文档） |
| V1-02 | rustdoc（`deny(missing_docs)`，聚焦 core） | V1-15 |
| V1-03 | 索引同步 | V1-01 |
| V1-12 | CHANGELOG | — |
| V1-13 | NOTICE（含 ONNX Runtime） | — |

**建议执行顺序**：

1. **先解阻塞**：V1-11（bench panic）→ V1-14（默认值分裂）——两个小改动，立刻让 CLI 可用且语义一致
2. **定名与发布**：D-E1 拍板 → V1-15 改名 → V1-16 推 GitHub
3. **评测脚本**（价值最高）：V1-00 → V1-04 → V1-05 → V1-06 → V1-07
4. **CI/CD**：V1-08 → V1-09 → V1-10
5. **文档**：V1-01 → V1-02 → V1-03
6. **零散**：V1-12 → V1-13

## 9. 验收标准（v1.1 明确区分两种"复现"）

> **口径区分（v1.1 新增，避免误判"验收失败"）**：
> - **逐位复现不存在**：`eval-report` 3.4 已记录 vector/hybrid 有 run-to-run 抖动
>   （NDCG 极差 0.0024 / 0.0023，R-P5-13：HNSW 图每次构建不同）。
> - **"结论复现"**（干净机器跑 `make eval-quality`）：结论与显著性一致即可——
>   hybrid 的 MRR/Recall 最高、NDCG 略低于 vector，且差异在抖动容差内。
> - **"逐格一致"**（仅适用于存档 JSON 重放）：验证的是"手工抄录无错"，
>   **不是**跑分可复现。

- [ ] 干净机器 `make eval-quality` 复现 P5 **结论**（含抖动容差，非逐位）
- [ ] `make eval-perf` 输出的 NFR 表与 `eval-report.md` 第 8 节一致（含未达标项）
- [ ] `scripts/report.py` 用**入库**存档 JSON 生成的表格与现有报告**逐格一致**
      （含逐桶 p 值，遵循 `sign_test` 约定）
- [ ] CI 全绿：**全部 job**（fmt / lint / test / deny / doc / build-release / smoke / MSRV / feature 隔离 ×2）
- [ ] `docs/user-guide.md` 覆盖 CLI 全参数 + 六 trait 替换矩阵
- [ ] `cargo doc` 在 `RUSTDOCFLAGS="-D warnings"` 下零警告
- [ ] `idx bench --input x` 与 `-i x` 行为一致；**debug 构建下 `bench --help` 不 panic**
- [ ] `idx bench` 裸跑默认权重 = `[1, 1.5]`（与 `idx search --mode hybrid` 一致）
- [ ] 项目已改名 HelixIndex、版本号 0.1、代码与活跃文档无 `index-demo` 残留
- [ ] 代码已推送 GitHub `helix-index`，Actions 被触发

## 10. 决策点（待确认）

| 编号 | 决策 | 建议 |
| --- | --- | --- |
| D-A1 | 使用文档用 Markdown 还是 mdBook？ | **Markdown**（体量不足 mdBook 收益门槛，免维护构建链路） |
| D-B1 | 评测脚本语言：Bash+Python 还是 Rust xtask？ | **Bash+Python**（跨平台内存测量用 Python 更稳，xtask 对本项目过重） |
| D-C1 | CI 平台 | **GitHub Actions** |
| D-C2 | 评测是否进 CI？ | **否**（见第 5 节理由） |
| D-D1 | CHANGELOG 是否逆向补写 P0~P5？ | **是** |
| **D-E1** | **crate 与二进制命名** | ✅ **已定：E1 —— 库 crate `helix-core`，CLI 二进制 `helix`**（命令形如 `helix build` / `helix search`；发布前再核 crates.io 占用） |
| **D-E2** | **GitHub 仓可见性** | ✅ **已定：private**（个人仓，项目尚未正式发布；待 README/CHANGELOG 完备后再转 public） |
| **D-E3** | **本地目录是否改名** | ✅ **已定：暂不改** —— 本地目录保持 `index-demo`，远端仓名用 `helix-index`；不打断当前工作区与会话状态 |
| **D-E4** | **历史文档（p0~p5-design、archive）是否改写项目名？** | **加注不改写**（沿用 `p1-design.md` 既有先例，保留历史原貌） |

### 命名映射表（D-E1 已定，**V1-15 已实施完成**）

| 原名 | 新名 | 说明 | 状态 |
| --- | --- | --- | --- |
| `index-core`（库 crate） | **`helix-core`** | Rust 引用名 `index_core` → `helix_core`（56 处 / 8 文件） | ✅ |
| `idx`（CLI crate + 二进制） | **`helix`** | 命令 `idx build` → `helix build`；`#[command(name)]` 与 about 同步 | ✅ |
| `index-demo`（项目名） | **HelixIndex** | 活跃文档 7 份（README / docs/README / requirements-spec / architecture-design / plan / thirdparty / eval-report） | ✅ |
| 版本号 | **0.1.0** | 原本即 `0.1.0`，确认无需改动 | ✅ |
| GitHub 仓 | **`helix-index`**（private） | V1-16 | ⬜ |
| 本地目录 | **保持 `index-demo`** | D-E3，不打断工作区 | ✅ |
| 历史文档（p0~p5-design、archive） | 加注不改写 | D-E4，7 份均已在标题下加改名注记 | ✅ |

**实施附带项**：模型缓存目录 `~/.cache/index-demo/models` → `~/.cache/helix-index/models`
（**已 mv 迁移 91MB，避免重新下载模型**；已用 `helix search --mode hybrid` 冒烟验证加载正常）。

**刻意未改的 `idx`**：代码里的 `let mut idx = HnswRsIndex::...` 等**局部变量名**
（index 缩写）、以及快照文件扩展名 `.idx`（`test.idx` 等，受 `.gitignore` 的 `*.idx` 规则约束）——
这些不是项目名，改名会破坏语义。
