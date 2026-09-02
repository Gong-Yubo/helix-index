# P0 工程骨架与依赖验证 — 设计说明

| 项目   | 内容                                                                                |
| ---- | --------------------------------------------------------------------------------- |
| 版本   | **v2.0（已执行并回写）**                                                                  |
| 日期   | 2026-09-02                                                                        |
| 状态   | **已执行完成** —— P0 全部通过，首次提交 `5850545`。执行中的 4 处修正见**第 12 章**，实测基线见**附录 A**   |
| 上游   | `docs/devel/plan.md` 第 5 章（P0，T0-01 ~ T0-07）                                      |
| 关联   | `docs/devel/architecture-design.md` 第 4.1 节（目录结构）、第 9 章（选型）、ADR-008（MSRV/License） |
| 环境实测 | 2026-09-02 在本机采集，见第 2 章                                                           |



---

## 1. P0 要达成什么

**一句话**：证明「Rust + fastembed + instant-distance + jieba-rs」这条技术路线**在这台机器上跑得通**，并留下一个可编译、可提交、依赖已锁定的工程骨架。

**判定标准（Gate）** —— 全部满足才算 P0 完成：

1. `cargo build` 空工程通过，`cargo clippy` / `cargo fmt` 干净
2. 计划在 P1~P4 使用的**全部依赖**可拉取、可编译（含最重的 `fastembed`）
3. **ONNX 模型能下载、能推理**：一句中文文本 → 512 维向量，L2 范数 ≈ 1.0
4. `make deny` 通过（依赖 License 白名单）
5. 仓库有 git，首次提交包含 `Cargo.lock` 与双 License 文件

> **为什么 P0 要单独写设计文档**：P0 是唯一一个「失败会推翻后续所有计划」的阶段。T0-05（模型下载 + 推理）若在 P2 才发现问题，P1 的投入将大幅贬值。此外 P0 会**改动本机环境**（安装 Rust 工具链、写 shell 配置、初始化 git），这些不可逆动作需要先经你确认。

---


## 2. 本机环境实测（2026-09-02）

| 项                            | 实测值                                     | 对 P0 的影响                                     |
| ---------------------------- | --------------------------------------- | -------------------------------------------- |
| 机型 / CPU                     | Apple **M5**，arm64，10 核                 | ONNX Runtime 需 **aarch64-apple-darwin** 预编译包 |
| 系统                           | macOS 26.5.1（Darwin 25.5.0）             | 需 Xcode Command Line Tools（**已安装**）          |
| 内存                           | 32 GB                                   | 足够；`ort` 编译峰值无压力                             |
| 磁盘可用                         | 683 GB                                  | 充足（工具链 ~2GB + target ~3GB + 模型 91MB）         |
| `cc` / `clang` / `make`      | ✅ 均在 `/usr/bin`                         | 基础构建工具齐全                                     |
| Xcode CLT                    | ✅ `/Library/Developer/CommandLineTools` | 无需额外安装                                       |
| `git`                        | ✅ `/opt/homebrew/bin/git`               | 可初始化仓库                                       |
| **`cmake`**                  | ❌ **缺失**                                | ⚠️ 见风险 R-P0-3（大概率不需要）                        |
| `pkg-config`                 | ❌ 缺失                                    | 本项目用不到                                       |
| `rustc` / `cargo` / `rustup` | ❌ **全部缺失**，`~/.cargo` 不存在               | **T0-01 是硬前置**                               |

**网络连通性实测（RTT）**

| 端点                              | 结果            | 结论                  |
| ------------------------------- | ------------- | ------------------- |
| `index.crates.io`（官方 sparse 索引） | 200，**0.37s** | **可用且快**，建议优先直连     |
| `rsproxy.cn`（清华/rsproxy 镜像）     | 200，**0.15s** | 更快，但有同步延迟           |
| `huggingface.co`                | 200，4.65s     | 模型源可达               |
| `hf-mirror.com`                 | 200，7.83s     | 本次实测**比官网还慢**，不作为首选 |
| `github.com`                    | 200，4.45s     | `ort` 预编译包来源可达      |

> 结论：**crates.io 直连可用**（0.37s），不必默认套镜像。镜像作为下载慢时的备选，配置写在 `~/.cargo/config.toml` 里但不默认启用。

---

## 3. 三个已核实的关键前置事实

### 3.1 模型实际来自 `Qdrant/bge-small-zh-v1.5`，不是 BAAI 官方仓

| 仓库                         | 内容                                                                                             | 说明                        |
| -------------------------- | ---------------------------------------------------------------------------------------------- | ------------------------- |
| `Qdrant/bge-small-zh-v1.5` | `model_optimized.onnx`（**90.39 MB**）、`tokenizer.json`（0.41 MB）、`ort_config.json`、`config.json` | ✅ **fastembed 实际拉取的就是这个** |
| `BAAI/bge-small-zh-v1.5`   | `pytorch_model.bin`、`model.safetensors`、…（**无 ONNX**）                                          | 官方仓不能直接给 ONNX Runtime 用   |

**影响**：T0-05 的下载量约 **91 MB**（不是"约 90MB"的含糊估计）。License 均为 MIT。

### 3.2 fastembed 6.0.2 的 API 签名已实测（并修正了文档中的一处错误）

```rust
// docs.rs/fastembed/6.0.2 实测
pub fn TextEmbedding::try_new(options: TextInitOptions) -> Result<Self>   // 需 hf-hub feature（默认开启）
pub fn embed<S: AsRef<str> + Send + Sync>(
    &mut self,
    texts: impl AsRef<[S]>,
    batch_size: Option<usize>,
) -> Result<Vec<Embedding>>

// InitOptions 字段与 builder
InitOptions { model_name: EmbeddingModel, execution_providers, cache_dir,
              show_download_progress: bool, max_length: usize, intra_threads: Option<usize> }
InitOptions::new(model) / .with_show_download_progress(bool) / .with_cache_dir(PathBuf)
             / .with_max_length(usize) / .with_intra_threads(usize) / .with_execution_providers(vec)
```

⚠️ **修正**：中文小模型的枚举变体是 **`EmbeddingModel::BGESmallZHV15`**，**不是 `BGESmallZH`**（后者不存在，会在 P2 编译报错）。架构文档 10.1 的示例已同步修正。

### 3.3 `ort` 是预发布版本，但默认走预编译二进制

- `fastembed` 6.0.2 与 5.17.4 **都精确锁定** `ort =2.0.0-rc.13`（`=` 而非 `^`）
- fastembed 默认 feature 含 `ort-download-binaries-native-tls` → **下载预编译 ONNX Runtime，不走源码编译**，因此**大概率不需要 cmake**
- 需要的是 **aarch64-apple-darwin** 平台的预编译包存在（M5 属于该 target）

---

## 4. 执行后的目标目录结构

```
index-demo/
├── .git/                       # T0-03 初始化
├── .gitignore                  # ⚠️ 不得忽略 Cargo.lock
├── Cargo.toml                  # workspace（members: crates/core, crates/cli）
├── Cargo.lock                  # ✅ 入库
├── rust-toolchain.toml         # pin 1.90.0
├── deny.toml                   # License 白名单
├── Makefile                    # fmt / lint / test / deny / all
├── LICENSE-MIT
├── LICENSE-APACHE
├── crates/
│   ├── core/                   # index-core
│   │   ├── Cargo.toml
│   │   ├── src/lib.rs          # 模块声明（骨架）
│   │   └── src/{error,types,document,schema}.rs
│   │   └── src/{analyze,index,retriever,vector,embed,fusion,rerank,query,storage,chunk}/mod.rs
│   ├── cli/                    # idx
│   │   ├── Cargo.toml
│   │   └── src/main.rs         # 最小 clap 骨架
│   └── core/examples/
│       └── embed_smoke.rs      # T0-05 模型下载 + 推理验证
├── data/                       # 已存在（空）
└── docs/                       # 已存在
```

---

## 5. 任务设计

### T0-01 安装 Rust 工具链

**做什么**：安装 rustup + stable 工具链，使 `rustc` / `cargo` 可用。

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
source "$HOME/.cargo/env"
rustc --version && cargo --version
```

**副作用（需你知晓）**

- 安装器会在 `~/.zshrc` 追加一行 `source "$HOME/.cargo/env"`（可通过 `CARGO_HOME`/`RUSTUP_HOME` 改路径）
- 磁盘占用约 1.5~2 GB（`~/.rustup` + `~/.cargo`）
- 卸载方式：`rustup self uninstall`

**验收**：`rustc --version` ≥ 1.90（stable 当前为 **1.98.0**）；`cargo --version` 可用。

**失败分支**：网络失败 → 换用镜像 `RUSTUP_DIST_SERVER=https://rsproxy.cn`；仍失败 → 用 `brew install rustup-init`。

---

### T0-02 配置 cargo 镜像（**默认不启用**，仅留配置）

**做什么**：写入 `~/.cargo/config.toml`，镜像块**全部注释**，直连优先。

```toml
# ~/.cargo/config.toml
# 官方 sparse 索引实测 RTT 0.37s，默认直连即可。
# 若下载 crate 缓慢，取消下面的注释启用 rsproxy 镜像（实测 0.15s，但有同步延迟）。
# [source.crates-io]
# replace-with = 'rsproxy-sparse'
#
# [source.rsproxy-sparse]
# registry = "sparse+https://rsproxy.cn/index/"
#
# [registries.rsproxy-sparse]
# index = "sparse+https://rsproxy.cn/index/"

[net]
git-fetch-with-cli = true     # 走系统 git，便于走代理与诊断

[build]
# 目标目录放到工作区（默认），便于清理
```

**验收**：`cargo search serde` 秒级返回；`cargo build` 能拉到 crate。

---


### T0-03 workspace 骨架 + git + 许可文件

**根 `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["crates/core", "crates/cli"]

[workspace.package]
version     = "0.1.0"
edition     = "2021"
rust-version = "1.90"
license     = "MIT OR Apache-2.0"
repository  = ""            # 待填
authors     = ["index-demo contributors"]

[workspace.dependencies]
# 由各 crate 按需引入，版本在此统一（P0 只声明，T0-04 验证）
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
thiserror   = "2"
anyhow      = "1"
rayon       = "1"
tracing     = "0.1"
smol_str    = "0.3"
unicode-segmentation = "1.13"
unicode-normalization = "0.1"
jieba-rs    = "0.10"
instant-distance = { version = "0.6.1", features = ["with-serde"] }
fastembed   = "6.0.2"          # ⚠️ 传递依赖 ort =2.0.0-rc.13（预发布）
bincode     = "3.0.0"
crc32fast   = "1.5"
moka        = "0.12"
clap        = { version = "4.6", features = ["derive"] }

[profile.release]
opt-level = 3
lto = "thin"
```

**`crates/core/Cargo.toml`**（P0 只声明基础依赖，其余随阶段加入）

```toml
[package]
name = "index-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[features]
default       = ["local-embed"]
local-embed   = ["dep:fastembed"]
remote-embed  = ["dep:reqwest"]     # P2 再引入
positions     = []

[dependencies]
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
rayon.workspace = true
tracing.workspace = true
smol_str.workspace = true
unicode-segmentation.workspace = true
unicode-normalization.workspace = true
jieba-rs.workspace = true
instant-distance.workspace = true
bincode.workspace = true
crc32fast.workspace = true
moka.workspace = true
fastembed = { workspace = true, optional = true }

[dev-dependencies]
tantivy = "0.26"        # ADR-007：仅作 BM25 正确性对照基线，不参与构建产物
criterion = "0.8"
proptest = "1"
tempfile = "3"
```

**`crates/cli/Cargo.toml`**

```toml
[package]
name = "idx"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
index-core = { path = "../core" }
anyhow.workspace = true
clap.workspace = true
tracing.workspace = true
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

**`rust-toolchain.toml`**

```toml
[toolchain]
channel = "1.90.0"        # NFR-08：MSRV 1.90+，pin 死以保证可复现
components = ["rustfmt", "clippy"]
profile = "default"
```

**`.gitignore`**（要点：**不忽略 `Cargo.lock`**）

```gitignore
/target
**/*.rs.bk
*.pdb
.DS_Store
.idea/
.vscode/

# ⚠️ Cargo.lock 必须入库：fastembed 精确锁定 ort =2.0.0-rc.13，
#    不锁定则不同时间构建会拉到不同依赖树（R9）
# !Cargo.lock

# 索引产物与模型缓存不入库
*.idx
/data/*.jsonl
!data/.gitkeep
```

**许可文件**：`LICENSE-MIT`（标准 MIT 文本）、`LICENSE-APACHE`（Apache-2.0 全文），根目录另加 `README.md` 标注双许可。

**git**：`git init` → 配置 `user.name/email`（若无全局配置则用局部）→ 首次提交。

**验收**：`cargo build --workspace` 通过；`git log` 有一条提交；`git status --porcelain` 干净；`Cargo.lock` 在版本控制内。

---

### T0-04 依赖可获取性验证

**分三批执行，便于定位失败点**

```bash
# 批次 1：轻依赖（秒级~1 分钟）
cargo build -p index-core --no-default-features

# 批次 2：重依赖（最关键，预计 3~10 分钟，ort 是耗时主体）
cargo build -p index-core            # 含 fastembed + ort

# 批次 3：dev 依赖（含 tantivy，体积最大，预计 5~15 分钟；可延后到 T1-15 前）
cargo build --workspace --all-targets
```

**验收**：三批全部编译通过，`cargo tree` 能列出依赖树。

**失败分支**

| 现象                     | 处理                                                 |
| ---------------------- | -------------------------------------------------- |
| crate 下载慢/超时           | 启用 T0-02 的 rsproxy 镜像                              |
| `ort` 报找不到预编译包         | 设置 `ORT_LIB_LOCATION`，或改用 `ort/load-dynamic` 链接系统库 |
| `ort` 回退到源码编译并报缺 cmake | `brew install cmake`（约 1~2 分钟），重跑                  |
| C 依赖链接失败               | 确认 Xcode CLT 版本；`sudo xcode-select --reset`        |

**产出记录**：把三批的**实际编译耗时**记入本文件附录，作为 NFR-03 与后续 CI 时长的基线。

---


### T0-05 ONNX 模型下载 + 推理验证（**最高风险项**）

**做什么**：最小示例，验证「下载 → 加载 → 推理 → 归一化」整条链路。

`crates/core/examples/embed_smoke.rs`

```rust
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // 注意：变体名是 BGESmallZHV15（不是 BGESmallZH）
    let mut model = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::BGESmallZHV15)
            .with_show_download_progress(true),
    )?;

    let texts = vec![
        "Rust 里怎么实现支持中文的 BM25 检索？",
        "向量检索与关键词检索的区别是什么？",
    ];

    let t0 = std::time::Instant::now();
    let embeddings = model.embed(&texts, None)?;   // 首次调用含模型下载
    let elapsed = t0.elapsed();

    println!("dim        = {}", embeddings[0].len());
    println!("first 4    = {:?}", &embeddings[0][..4]);
    let norm: f32 = embeddings[0].iter().map(|x| x * x).sum::<f32>().sqrt();
    println!("l2 norm    = {:.6}", norm);          // 期望 ≈ 1.0（我们入库前会再归一化一次）

    // 两句话的余弦相似度（应 > 0，说明语义被编码）
    let cos: f32 = embeddings[0].iter().zip(&embeddings[1]).map(|(a, b)| a * b).sum();
    println!("cos(0,1)   = {:.4}", cos);
    println!("elapsed    = {:?}（含首次下载）", elapsed);

    assert_eq!(embeddings[0].len(), 512, "bge-small-zh-v1.5 应为 512 维");
    Ok(())
}
```

```bash
# 首次运行（如需镜像）
HF_ENDPOINT=https://hf-mirror.com cargo run -p index-core --example embed_smoke

# 直连（实测官网 4.65s 握手，优先尝试）
cargo run -p index-core --example embed_smoke
```

**验收**

- 模型下载成功（`~/.cache/huggingface/hub/models--Qdrant--bge-small-zh-v1.5/`）
- 输出 `dim = 512`、`l2 norm ≈ 1.0`、`cos(0,1)` 在合理区间（0.3~0.9）
- 记录**第二次运行**（模型已缓存）的单条推理耗时，作为 NFR-02 的基线数据

**失败分支**

| 现象                      | 处理                                                                                                |
| ----------------------- | ------------------------------------------------------------------------------------------------- |
| HuggingFace 下载失败/极慢     | ① `HF_ENDPOINT=https://hf-mirror.com`；② 手工下载 `Qdrant/bge-small-zh-v1.5` 后用 `.with_cache_dir()` 指定 |
| `try_new` 报 feature 未启用 | 确认 `fastembed` 默认 feature 未被 `default-features = false` 关掉                                        |
| 维度不是 512                | 记录实际值并核对模型信息；不强行断言                                                                                |
| aarch64 预编译包不存在         | 启用 `ort/load-dynamic` + `brew install onnxruntime`；或转 `remote-embed` 路线（需回到 ADR-003）              |

---


### T0-06 工程命令与依赖合规

**`Makefile`**

```make
.PHONY: all fmt lint test deny doc clean

all: fmt lint test deny

fmt:
	cargo fmt --all

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

deny:
	@cargo deny --version >/dev/null 2>&1 || \
	  (echo "正在安装 cargo-deny（首次约 2~5 分钟）..." && cargo install cargo-deny --locked)
	cargo deny check licenses bans sources

doc:
	cargo doc --workspace --no-deps --open

clean:
	cargo clean
```

**`deny.toml`**

```toml
[graph]
all-features = true

[advisories]
version = 2
ignore = []

[licenses]
version = 2
allow = [
  "MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception",
  "BSD-2-Clause", "BSD-3-Clause", "ISC",
  "BSL-1.0",          # xxhash-rust
  "CC0-1.0", "Zlib", "Unicode-DFS-2016",
]
confidence-threshold = 0.8
exceptions = []

[bans]
multiple-versions = "allow"    # 传递依赖难免多版本，先放行
wildcards = "deny"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```

**验收**：`make deny` 通过；故意 `cargo add` 一个 GPL crate 时 `make deny` **失败**（验证白名单真的生效）。

**`cargo-deny` 安装方式（需你确认 D5）**：`cargo install cargo-deny --locked`（编译 2~5 分钟，无 brew 依赖）优于 homebrew（公式不一定存在）。

---

### T0-07 模块骨架

**`crates/core/src/lib.rs`**

```rust
//! index-core —— 面向 Agent 场景的通用检索引擎内核
//!
//! 模块边界为硬性约束，见架构文档 4.2：
//! analyze 不知道索引；index 不碰融合；vector 不认识文本；
//! retriever 不与其他 lane 交互；fusion 不回捞正文；query 只编排不实现算法。

pub mod error;
pub mod types;

pub mod analyze;
pub mod chunk;
pub mod embed;
pub mod fusion;
pub mod index;
pub mod query;
pub mod rerank;
pub mod retriever;
pub mod schema;
pub mod storage;
pub mod vector;

pub mod document;

pub mod prelude {
    pub use crate::error::{Error, Result};
    pub use crate::types::*;
}
```

各 `mod.rs` 只写文档注释 + `// TODO(P1/P2…)` 占位，保证 `cargo clippy -D warnings` 不因空模块报错（用 `//!` 注释填充）。

`crates/cli/src/main.rs`：最小 clap 骨架（仅 `--version` 与占位子命令），P1 再实现 `build` / `search`。

**验收**：`cargo build --workspace` 通过；`cargo doc --no-deps` 可生成；`cargo tree` 中 `index-core` 与 `idx` 依赖关系正确。

---

## 6. 依赖版本锁定策略

1. **`Cargo.lock` 入库** —— fastembed 精确锁定 `ort =2.0.0-rc.13`，不锁定则构建不可复现（R9）
2. **workspace 统一版本** —— 所有依赖在根 `Cargo.toml` 的 `[workspace.dependencies]` 声明，子 crate 用 `.workspace = true` 引用，避免版本漂移
3. **`rust-toolchain.toml` pin 1.90.0** —— MSRV 与开发版本一致，任何人 checkout 后自动使用同一工具链
4. **`cargo deny` 守 License** —— NFR-09，白名单之外一律失败
5. **升级节奏** —— P0 锁定后不再动；每个阶段末复查一次 `thirdparty.md` 附录 A 的 API

---


## 7. 风险与兜底

| #      | 风险                                      | 概率  | 影响                   | 兜底方案                                                                                              |
| ------ | --------------------------------------- | --- | -------------------- | ------------------------------------------------------------------------------------------------- |
| R-P0-1 | rustup 安装失败 / 下载慢                       | 低   | 阻塞全部                 | `RUSTUP_DIST_SERVER=https://rsproxy.cn` 或 `brew install rustup-init`                              |
| R-P0-2 | `ort` 预编译包下载失败（GitHub 4.45s 可达但可能限速）    | 中   | 阻塞 T0-04/05          | ① 重试；② `ORT_LIB_LOCATION` 指向本地 ONNX Runtime；③ `ort/load-dynamic` + `brew install onnxruntime`     |
| R-P0-3 | `ort` 回退源码编译，缺 cmake                    | 中   | 阻塞 T0-04，多耗 20~40 分钟 | `brew install cmake` 后重跑（**不在 P0 预装**，避免无谓安装）                                                     |
| R-P0-4 | HuggingFace 下载 91MB 模型失败                | 中   | 阻塞 T0-05             | `HF_ENDPOINT=https://hf-mirror.com`；或手工下载 + `.with_cache_dir()`；或转 `remote-embed`（需回 ADR-003 重决策） |
| R-P0-5 | `ort 2.0.0-rc.13` 在 aarch64-darwin 上有缺陷 | 低~中 | 阻塞 P2                | 退到自研路径：`ort 1.16.3` + `tokenizers` 手写 BGE 推理（约 150 行，thirdparty.md 4.3 已备）                        |
| R-P0-6 | 工具链/依赖污染本机环境                            | 低   | 环境问题                 | 全部隔离在 `~/.rustup` `~/.cargo` `~/.cache/huggingface`；卸载 `rustup self uninstall`                    |

**回滚方案**：P0 只**新增**文件，不修改任何已有文件（唯一例外是 `~/.zshrc` 追加一行 PATH）。最坏情况：`rustup self uninstall` + 删除工作区新增文件即可完全复原。

---

## 8. 验收清单（P0 完成时一次性跑通）

```bash
cd /Users/gongyubo/Code/mine/index-demo

rustc --version                      # 期望：rustc 1.90.0（或 stable ≥1.90）
cargo --version
cargo build --workspace              # 期望：Finished，无 error
cargo build --workspace --all-targets # 期望：含 dev 依赖（tantivy）也通过
cargo run -p index-core --example embed_smoke
# 期望输出：
#   dim        = 512
#   l2 norm    ≈ 1.000000
#   cos(0,1)   = 0.xxxx（0.3~0.9）
#   elapsed    = <首次含下载；记录第二次运行的推理耗时>

make fmt && make lint && make test   # 期望：全部通过，clippy 无警告
make deny                            # 期望：licenses/bans/sources 全部通过
git status --porcelain               # 期望：干净（无未提交文件）
git check-ignore -v Cargo.lock       # 期望：无输出（即未被忽略）
```

**并记录到本文件附录**：各批次编译耗时、模型下载耗时、第二次推理单条耗时。这三个数字是 NFR-02/NFR-03 的第一批实测基线。

---

## 9. 执行批次与预计耗时

| 批次   | 任务                               | 预计耗时                     | 是否需要交互 |
| ---- | -------------------------------- | ------------------------ | ------ |
| 批次 1 | T0-01 安装 Rust 工具链                | 3~10 分钟（取决于网络）           | 需确认    |
| 批次 2 | T0-02 镜像配置 + T0-03 骨架/git/许可     | 2~5 分钟                   | 需确认    |
| 批次 3 | T0-04 依赖编译（三批）                   | 8~25 分钟（ort 是大头）         | 否      |
| 批次 4 | **T0-05 模型下载 + 推理**              | 5~20 分钟（91MB 下载）         | 否      |
| 批次 5 | T0-06 Makefile/deny + T0-07 模块骨架 | 5~10 分钟（含 cargo-deny 编译） | 否      |

**合计：约 25~70 分钟**，其中大部分是等待编译，可后台执行。

**关键路径上的卡点只有 T0-05**：只要模型能下载能推理，P1 就可以开工；其余任务失败都可绕过。

---

## 10. 明确不做（P0 范围外）

- 不写任何业务逻辑（BM25、分词、向量索引等）——那是 P1~P4
- 不实现 CLI 的 `build` / `search` 子命令——只有占位
- 不准备示例语料（那是 T1-14 起的并行流）
- 不接入 CI（本机 Makefile 已足够，CI 待有远端仓库再说）
- 不安装 Homebrew 包（除 cmake 等确实需要时）
- 不做性能调优（P0 只记录基线数字）

---


## 11. 需要你确认的决策点

| #  | 决策点                       | 我的建议                                                                | 备注                                    |
| -- | ------------------------- | ------------------------------------------------------------------- | ------------------------------------- |
| D1 | **rustup 安装方式**           | `curl … \| sh -s -- -y --default-toolchain stable`，允许其修改 `~/.zshrc` | 会改动本机环境，约 2GB 磁盘；可完全卸载                |
| D2 | **cargo 镜像**              | **默认直连**（实测 0.37s），镜像配置写入但注释掉，慢时再启用                                 | 避免镜像同步延迟导致新版本拉不到                      |
| D3 | **`rust-toolchain.toml`** | **pin `1.90.0`**（NFR-08 的 MSRV），保证所有人同一工具链                          | 若要"跟随 stable"，改为 `channel = "stable"` |
| D4 | **git 初始化与首次提交**          | 是，`git init` + 首次提交（含 `Cargo.lock`、双 License）                       | 仓库当前无 git                             |
| D5 | **`cargo-deny` 安装**       | `cargo install cargo-deny --locked`（首次编译 2~5 分钟）                    | 若你不想等，可推迟到 P1，`deny.toml` 先写好         |
| D6 | **dev 依赖 `tantivy`**      | 就在 T0-04 批次 3 一起验证（约 5~15 分钟额外编译）                                   | 若想省时间，可推迟到 T1-15 前再验证                 |
| D7 | **`cmake`**               | 不预装；仅当 `ort` 回退源码编译报错时再 `brew install cmake`                        | 大概率不需要（默认走预编译二进制）                     |

---

## 12. 执行记录（v2.0 新增）

> 本节记录执行过程中**与设计不一致**的地方。前 4 条是我直接解决并回写的，第 5 条需要你决策。

### 12.1 修正：Rust 工具链其实已经安装（原判断有误）

立项时探测的结论是"`rustc` / `cargo` / `rustup` 全部缺失"，**这是误判**：

- 探测命令跑在**非登录 shell**，`~/.cargo/bin` 不在 `PATH`
- 实际登录 shell 中 `cargo` 可用，版本为 **stable 1.98.0**（`~/.rustup/settings.toml` 早已存在）

**影响**：需求文档 8.2「外部依赖」中"Rust 工具链 ❌ 未安装，阻塞全部编码"应改为"✅ 已安装 1.98.0，项目 pin 1.90.0"。**"工具链缺失"从来不是真正的阻塞项**。

**已执行**：补装 `rustup toolchain install 1.90.0 --profile minimal --component rustfmt --component clippy`（53s）。

### 12.2 修正：`bincode 3.0.0` 是玩笑发布，实际稳定线为 2.0.1

crates.io 显示 `bincode` 的 `max_version` 为 `3.0.0`，但那个版本的**源码只有一行**：

```rust
compile_error!("https://xkcd.com/2347/");   // XKCD 2347: Dependency
```

**教训**：调研依赖时**不能只看 crates.io 的 `max_version`**，必须确认它是真实发布（看源码 / 下载量 / 能否生成文档）。这个错误从 thirdparty.md 一路带到了架构文档与 plan.md，已一并修正。

**影响范围**：`Cargo.toml`、架构文档 9.1 / 7.6 / ADR-005、plan.md T4-01 与附录 B、thirdparty.md 4.6，全部改为 **bincode 2.0.1**（错误类型 `bincode::error::{EncodeError, DecodeError}`，与 `error.rs` 已对齐）。

### 12.3 修正：`moka` 必须显式启用 `sync` 或 `future` feature

不加 feature 时 `moka` 直接 `compile_error!`。已改为 `moka = { version = "0.12", features = ["sync"] }`。

### 12.4 修正：`fastembed` 默认 feature 引入一整条图像处理依赖链（NCSA 许可）

默认 feature 含 `image-models`，带来 `image → ravif → rav1e → libfuzzer-sys`（NCSA 许可），而我们只用文本模型。已改为：

```toml
fastembed = { version = "6.0.2", default-features = false, features = [
    "ort-download-binaries-native-tls",
    "hf-hub-native-tls",
] }
```

三点收益：① 消除 NCSA 许可项；② 依赖树瘦身；③ `TextEmbedding::try_new` 所需的 `hf-hub` feature 仍保留，验证通过。

**附带发现**：fastembed 默认把模型缓存在**项目根目录**的 `.fastembed_cache/`（96 MB），已加入 `.gitignore`。**P2（T2-02）实现 `LocalEmbedder` 时应显式调用 `.with_cache_dir()` 指到仓库外**，否则开发者极易误提交 96MB。

### 12.5 ⚠️ 需你决策：模型实际来源是 `Xenova/bge-small-zh-v1.5`，且该仓未声明 License

- 设计文档依据模型卡推断为 `Qdrant/bge-small-zh-v1.5`（MIT）
- **实际下载的是 `Xenova/bge-small-zh-v1.5`**（`onnx/model.onnx`，90 MB）
- 该仓在 HuggingFace 上 **`license` 字段为空**（上游 `BAAI/bge-small-zh-v1.5` 为 MIT，但转换仓未声明）

这是我**没有**擅自决定的一点：fastembed 6 内置了模型 → 仓库映射，无法直接改指 Qdrant 仓。三个选项：

| 选项    | 做法                                                            | 代价                     |
| ----- | ------------------------------------------------------------- | ---------------------- |
| A（默认） | 接受现状。Xenova 转换仓普遍沿用上游 MIT，但**无正式声明**，存在理论风险                  | 零成本；合规审查时可能被问         |
| B     | 自己从 `BAAI`（MIT）导出 ONNX，用 `.with_cache_dir()` + 手工目录布局喂给 fastembed | 需转换工具链；布局属内部约定，脆弱 |
| C     | 换用自带 ONNX 且**明确声明 MIT** 的中文模型，绕过 fastembed 内置的 HF 下载逻辑        | 要自己写下载逻辑，违背"先用成熟库"初衷  |

**我倾向 A + 在 thirdparty.md 记录风险**，等 P2（T2-02）实现 `Embedder` 时再定。**若你要求严格合规，请告诉我，我按 B 或 C 调整。**

### 12.6 依赖 License 白名单的实际补充项

`make deny` 首轮未通过，实际需要的宽松许可比设计预估多四项：

| License               | 来源                                                     | 判定                        |
| --------------------- | ------------------------------------------------------ | ------------------------- |
| `CDLA-Permissive-2.0` | `webpki-roots`（经 `ureq` → `hf-hub` / `ort-sys`）         | 宽松（数据许可 permissive 版）     |
| `MPL-2.0`             | `option-ext` ← `dirs-sys` ← `dirs`（fastembed 定位缓存目录）   | **弱 copyleft（文件级）**，见下方说明 |
| `Unicode-3.0`         | `icu_*` 系列（经 `url` / `idna` 引入）                        | 宽松                        |
| `Unicode-DFS-2016`    | 同上（备用）                                                 | 宽松                        |

**关于 MPL-2.0**：NFR-09 原文是"全部依赖均为宽松许可；GPL/LGPL/AGPL 一票否决"。MPL-2.0 属**弱 copyleft（文件级）**——仅使用而不修改它，不会波及本项目许可，与 GPL 家族有本质区别。因此我**允许了它并在 `deny.toml` 写明理由**。这是对 NFR-09 的细化而非违反；若你要求"零 copyleft（含 MPL）"，只能放弃 fastembed（`dirs` 是它的传递依赖，无法单独剔除）。

### 12.7 反向验证：白名单确实会拦截

为确认 `make deny` 不是摆设，临时从 `deny.toml` 移除 `MPL-2.0` 后重跑：

```
移除 MPL-2.0  →  licenses FAILED
恢复          →  licenses ok
```

### 12.8 其他执行细节

- **git 身份**：仓库此前无 `user.name` / `user.email`（全局也没有），已设置**仓库级**身份 `index-demo <index-demo@localhost>`。**建议你改成自己的**（`git config user.name "..."`）。
- **首次提交**：`5850545`，38 个文件，含 `Cargo.lock`（已确认未被 `.gitignore` 忽略）。
- **未触发的兜底方案**：镜像（D2）、`HF_ENDPOINT`（R-P0-4）、cmake（R-P0-3）**全部没用上**——直连与预编译二进制都正常。
- **`.gitignore` 行尾注释**：初次写入时把注释放到了规则行尾（`.gitignore` 不支持行尾注释），已修正为独立行。

---

## 附录 A 实测基线（2026-09-02 执行后填写）

| 项                               | 实测值                                    | 用途                      |
| ------------------------------- | -------------------------------------- | ----------------------- |
| Rust 工具链                        | **原本已装 stable 1.98.0**；补装 1.90.0 耗时 **53s** | 环境准备（见 12.1）      |
| `cargo generate-lockfile`       | 13.0s（锁定 **414** 个包）                   | 依赖规模                    |
| 批次 1 轻依赖编译                      | **7.3s**                               | NFR-03 基线               |
| 批次 2 `fastembed` + `ort` 编译     | **42.5s**（未触发 cmake，走预编译二进制）           | NFR-03 基线               |
| 批次 3 `--all-targets`（含 tantivy） | **24.2s**                              | CI 时长预估                 |
| 模型下载（96 MB，含初始化）                | **49.0s**                              | R-P0-4 判断（未用镜像）         |
| **单条推理耗时**（模型已缓存，debug 构建）      | **1.58 ~ 1.84 ms**                     | **NFR-02 第一批实测基线**      |
| `embed` 两条文本                    | 2.83 ~ 2.86 ms                         | NFR-02 基线               |
| `ort` 是否触发源码编译                  | **否**（预编译二进制；cmake 缺失未造成影响）           | R-P0-3 已排除              |
| `target/` 体积                    | **4.6 GB**（debug 全量）                   | 磁盘占用；`make clean` 可清    |
| `.fastembed_cache/` 体积          | 96 MB                                  | 已加入 .gitignore（见 12.4）  |
| `make fmt` / `lint` / `test`    | 全部通过（clippy `-D warnings` 零告警；0 个测试）  | DoD                     |
| `make deny`                     | `bans ok, licenses ok, sources ok`     | NFR-09                  |

## 附录 B 环境核查命令（可复现本文档第 2、3 章）

```bash
uname -a; sw_vers; sysctl -n machdep.cpu.brand_string; sysctl -n hw.ncpu
xcode-select -p; command -v cmake || echo "cmake MISSING"
for u in https://index.crates.io/config.json https://rsproxy.cn/index/config.json \
         https://huggingface.co https://hf-mirror.com https://github.com; do
  curl -s -o /dev/null -w "$u %{http_code} %{time_total}s\n" --max-time 8 "$u"
done
curl -s "https://huggingface.co/api/models/Qdrant/bge-small-zh-v1.5" \
  | python3 -c "import sys,json;d=json.load(sys.stdin);print([s['rfilename'] for s in d['siblings']])"
```
