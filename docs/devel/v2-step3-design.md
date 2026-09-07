# HelixIndex V2 · Step 3 详细设计（原子快照：崩溃一致性）

> 面向 Agent 场景的通用检索引擎内核——V2 Step 3 的详细设计（**v0.1，待评审**）。
> 本文件回答：快照怎么原子落盘、崩溃在每个窗口留下什么后果、tmp 孤儿怎么回收、
> `hnsw_rs` 写路径的 panic 怎么兜住、怎么用故障注入证明以上全部成立。

| 项 | 内容 |
| --- | --- |
| 版本 | **v0.1（首版，待评审拍板）** |
| 日期 | 2026-09-07 |
| 状态 | 设计完成，等待评审（D-S3-01~06 待拍板）；未开工 |
| 上游 | `plan-v2.md` v0.4（Step 3 / S3-a~e / Q-C3 定级「高（正确性）」）、`requirements-spec.md` v1.7（FR-31 **Must**）、`architecture-design.md` v1.7（§7.6.2 前提段 / ADR-A / R19）、`v2-step2-design.md` v0.3（ADR-A 方案 C，已实施） |
| 范围 | T7-13 原子快照（tmp + fsync + rename + fsync 父目录，D-J3）+ tmp 孤儿回收 + 故障注入测试 + **R19 写路径残余收敛**（S3-e） |
| 非范围 | 图 sidecar 的校验/降级（Step 2 已交付，本文只引用）；compaction 与跨快照体积问题（**Step 4**）；低选择度兜底与 Metrics 可观测（**Step 5**）；`parallel_build` 默认翻转（横切 **T7-21**，独立 PR）；`.gitignore` 补非 tmp 的图 sidecar（横切 **T7-24** / issue #25，本文只补 tmp 模式） |

---

## 评审速读：6 个待拍板决策

> 评审者时间有限时，先看这张表。每条在 §5 有完整取舍与证据。

| # | 决策 | 我的建议 | 阻塞谁 |
| --- | --- | --- | --- |
| **D-S3-01** | tmp 命名规则：现状 manifest 的 tmp 用 `with_extension` 拼出**双后缀怪名**（实测 `foo.idx.hnsw.manifest` → `foo.idx.hnsw.hnsw.manifest.tmp`） | 统一改为**目标路径 + `.tmp` 追加**：`foo.idx.tmp` / `foo.idx.hnsw.manifest.tmp`。纯中间文件名变更，最终文件与格式零影响；且与 `.gitignore` 既有 `*.idx.tmp` 模式对齐 | 阻塞 S3-02/S3-03 |
| **D-S3-02** | `atomic_write` 的落点与签名：放 `graph.rs` 还是新模块；收 `&[u8]` 还是写闭包 | 新模块 `storage/atomic.rs`（`snapshot.rs` 不得反向依赖 graph 语义）；签名收**写闭包** `FnOnce(&mut BufWriter<File>) -> io::Result<()>`——快照 52MB 若按 `&[u8]` 拼整包要多一次全量拷贝 | 阻塞 S3-02~04 |
| **D-S3-03** | 故障注入钩子的形态：`test-util` feature / `#[cfg(test)]` / `#[doc(hidden)]` 常驻原子变量 / 环境变量 | **`#[doc(hidden)]` 常驻 + `AtomicU8`**（方案 B）。`#[cfg(test)]` 对集成测试（`tests/` 是独立 crate）不可见；feature 方案要扩 CI 矩阵；env var 是生产代码里的隐藏行为通道，最差 | 阻塞 S3-05 |
| **D-S3-04** | R19 写路径残余（`DumpInit` 的 `panic_any`，TOCTOU 窗口只能缩小不能归零）怎么收敛 | `catch_unwind(AssertUnwindSafe(dump_graph))`，panic payload 降级为 `Err(VectorGraph)`，**汇入 Step 2 既有的 P0-3 语义链**（Lenient 警告 + `PersistFailed` / Strict Err），不新增状态机。probe 探测保留（第一道防线） | 阻塞 S3-06 |
| **D-S3-05** | tmp 孤儿回收时机：save 前 / save 后 / load 后 / 都做 | **load 成功后 best-effort 删除**（快照本体），**save 前 `File::create` 截断复用**（天然覆盖同名 tmp）；manifest tmp 由既有 `remove_sidecars` 清理（仅改名）。依据：原子协议下 tmp 永不权威，且项目无「并发写同一快照」语义 | 阻塞 S3-04 |
| **D-S3-06** | fsync 给 `save` 引入的额外耗时（52MB 级 fsync）是否接受、要不要「跳过 fsync」逃生舱 | **接受并实测入档**（save 是用户显式调用的低频操作，与 NFR-03 构建口径无关）；**不提供跳过开关**——正确性语义不做成选项 | 影响 S3-08 |

---

## 1. 目标与验收

### 1.1 要解决的问题（plan-v2 §4 Step 3 / Q-C3）

**Q-C3（定级：高（正确性））**：快照本体落盘至今非原子——`storage/snapshot.rs:88` 是
`File::create(path)` 直写 + `flush()`。写一半崩溃（进程被杀 / 掉电 / 磁盘满）会留下
半截 `foo.idx`，下次 `load` 得 `Error::SnapshotCorrupted`——**不是降级重建，是索引丢失**。

对比：图 manifest 在 Step 2 已有完整原子写（`storage/graph.rs:236 write_manifest_atomic`，
tmp → flush → `sync_all` → rename → fsync 父目录）。**真源（快照）反而比派生缓存（manifest）
更脆弱**，这是本 Step 要纠正的倒挂。

### 1.2 对应需求

| 需求 | 内容 | 本文动作 |
| --- | --- | --- |
| **FR-31（Must，2026-09-07 由 Should 升）** | 原子快照：save 原子替换，崩溃后要么旧快照要么新快照 | 全文 |
| FR-16（Must） | 索引二进制快照（原子性并入） | 强化其 save 语义 |
| NFR-04 | 完整冷启动 < 2s | 不动格式、不加 IO 路径，唯一影响是 save 多两次 fsync（见 D-S3-06 实测） |
| NFR-07 | 降级可观测 | R19 收敛后 `PersistFailed(reason)` 能承载 panic 信息 |

### 1.3 验收标准（Step 3 完成的定义，可证伪）

1. ⚠️ **崩溃不变式**：在 `save` 序列的**任一注入点**（§4.3 四个 checkpoint）崩溃后，
   `load` 要么拿到**旧快照**、要么拿到**新快照**，**绝不 `SnapshotCorrupted`**（S3-T3~T6）。
2. 快照重写后旧图自动失效（CRC 版本锚点）——Step 2 已有行为，本 Step 改造后**回归不破**（S3-T2）。
3. 崩溃残留的 tmp 孤儿在**下一次成功 save / load** 时被回收（S3-T3 / T5）。
4. ⚠️ **R19 写路径收敛**：`hnsw_rs` dump 期间 panic 不再杀进程——Lenient 下 `save` 仍返回
   Ok 且 `graph_status() == PersistFailed(..)`（S3-T7）。
5. fsync 代价实测入 `eval-report.md`（**给范围不给单点值**，单次波动可达 ±15%）（S3-08）。
6. 守门清单全绿：fmt / clippy `-D warnings` / `cargo test --workspace` / MSRV 1.90 /
   rustdoc / `--no-default-features` / `--features charabia` / cargo-deny。

---

## 2. 现状与问题定位

### 2.1 快照写路径逐行盘点

`crates/core/src/storage/snapshot.rs:69-94`（现状）：

```rust
pub fn save_with_crc(..) -> Result<u32> {
    let snapshot = Snapshot { .. };
    let body = bincode::serde::encode_to_vec(..)?;   // 正文已在内存
    let crc = crc32fast::hash(&body);
    let file = File::create(path)?;                   // ← 直接打开真源文件
    let mut w = BufWriter::new(file);
    codec::write_header(&mut w, crc)?;                // 12B header
    w.write_all(&body)?;                              // ← 直写正文（52MB 级）
    w.flush()?;                                       // ← 只到 OS page cache！
    Ok(crc)
}
```

三个缺陷，一个比一个深：

| # | 缺陷 | 后果 |
| --- | --- | --- |
| 1 | **无 tmp + rename**：直接 `File::create(path)` 截断旧快照再写 | 崩溃在写入中途 ⇒ 半截文件 ⇒ `SnapshotCorrupted` ⇒ **索引丢失**（验收 1 要消灭的形态） |
| 2 | **无 fsync**：`flush()` 只保证写入 OS 页缓存 | rename 之后（若做了 rename）掉电，**目录项可能落地而数据块没落地**——文件存在但是半截。rename 前不 `sync_all` 则更糟：rename 把一个从未持久化的文件变成真源 |
| 3 | **无父目录 fsync** | 即使文件数据 fsync 了，rename 本身的目录项变更也可能掉电后回滚——「看起来 rename 了其实没有」 |

> 缺陷 2/3 正是 Step 2 `write_manifest_atomic` 注释里已经写明的机理
> （`graph.rs:233-235`：「只 fsync 文件不 fsync 目录，rename 可能不落地」），
> 只是当时只给 manifest 做了，没有回补快照本体。plan-v2 复审（A1）将其定名
> 「正确性欠账」：**ADR-A 解决了图与快照的相对一致性，没解决快照自身的绝对原子性**。

### 2.2 崩溃窗口分析（改造前）

`SearchIndex::save` 的完整序列（`search/index.rs:433-461`）：
`commit() → save_with_crc（直写）→ persist_graph（dump 图 → CRC → manifest 原子发布）`。

| 窗口 | 崩溃点 | 改造前后果 | 改造后后果（§4.3） |
| --- | --- | --- | --- |
| W1 | 快照正文写入中途 | **半截 foo.idx → `SnapshotCorrupted`，索引丢失** | 旧快照完好，残留 tmp 孤儿 |
| W2 | 快照写完、图 dump 前 | 新快照 + 旧 manifest → 图降级重建（慢但正确） | 同左（不变） |
| W3 | 图 dump 中（旧图已删） | 半截图 + 无新 manifest → 图降级重建 | 同左（不变） |
| W4 | manifest 发布后 | 全新一致 | 同左（不变） |

**W1 是本 Step 唯一的「正确性事故」窗口**；W2~W4 都是「性能退化」窗口，Step 2 的
降级语义已覆盖（plan-v2 验收也明确「该行为已被 Step 2 覆盖，不重复验收」）。

### 2.3 与图 manifest 原子写的对照（抽取的基础）

`storage/graph.rs:236-265 write_manifest_atomic` 已实现完整五步：
tmp → 写入 → flush → `sync_all` → rename → fsync 父目录。S3-a 要做的就是把这段
机制**从 manifest 特有的编解码里剥出来**，变成 `atomic_write` 通用原语，快照与
manifest 共用一份实现——「共用」不是美观诉求：两份独立实现迟早漂移，而漂移的
那一份就是下一个 Q-C3。

顺带发现一个既有瑕疵（实证）：tmp 名用 `path.with_extension("hnsw.manifest.tmp")`
拼接，对 `foo.idx.hnsw.manifest` 产生 **`foo.idx.hnsw.hnsw.manifest.tmp`**（双 `hnsw`，
因 `with_extension` 替换的是最后一个扩展名 `manifest`，stem 是 `foo.idx.hnsw`，
再拼 `hnsw.manifest.tmp`）。功能无碍（tmp 是中间文件、rename 后消失，
`remove_sidecars` 的清理用同一拼法所以能对上），但 S3-a 统一命名时顺手修正（D-S3-01）。

### 2.4 R19 写路径残余（S3-e 的标的）

`hnsw_rs-0.3.4` 的 `DumpInit::new` 在打不开输出文件时 **`panic_any`**（`hnswio.rs:208/226`，
证据见 `v2-step2-design.md` 附录 B）。Step 2 的对策是 dump 前探测目录可写
（`vector/persist.rs:101-115` 的 `.helix-graph-dump-probe`），但这只**缩小** TOCTOU 窗口：
探测通过之后、`hnsw_rs` 真正 open 之前，文件系统状态仍可能变化（磁盘满、配额、并发删目录）。
`panic_any` 不是 `Err`，调用侧无法用 `?` 接住——**进程直接死**。

Step 2 把这条记为 R19 残余并指派给本 Step（S3-e）。收敛手段见 §4.6：
`catch_unwind` 把 panic 降级为 `Err`，汇入既有 P0-3 缓存失败语义链。

---

## 3. 设计约束（来自 ADR-A / Step 2 的既定事实，本文不重新论证）

| # | 约束 | 出处 |
| --- | --- | --- |
| C1 | **manifest 是图的唯一原子发布点**；图 = 快照的派生缓存，可随时丢弃 | ADR-A（方案 C） |
| C2 | **快照 CRC 是图的版本锚点**：save 写出新快照（CRC 变）后旧 manifest 自动失效 ⇒ Step 3 只需让 `foo.idx` 自己 tmp+rename，图 sidecar 自动跟随 | 架构 §7.6.2 |
| C3 | **`FORMAT_VERSION` 保持 2**：原子性是**写协议**变更，不是**文件格式**变更——旧快照照常加载，新快照旧内核也能读（旧内核不知道原子性，读到的就是完整文件） | plan-v2 Step 3 范围段 |
| C4 | **`save_with_crc` 是快照写唯一入口**（`SearchIndex::save` 与测试都经它；CLI `build` → `SearchIndex::save`；bench 持久化场景走 `--index`）——单点改造即全覆盖 | 源码核实（grep `save_with_crc`/`storage::save`） |
| C5 | **无「并发写同一快照」语义**（R19 读路径残余同源声明）——孤儿回收的删除动作据此被认为安全 | 架构 14.1 R19 |
| C6 | **平台事实**：POSIX 上 `fsync` 目录需要 `File::open(dir)` + `sync_all`；Windows 上 `File::open` 目录会失败 → 只能 best-effort 跳过（现状 `write_manifest_atomic` 已如此，保持）；`std::fs::rename` 在 Windows 用 `MOVEFILE_REPLACE_EXISTING`，覆盖语义 OK | std 文档 / 现状代码 |
| C7 | **测试禁用 chmod 造失败**（CI 以 root 运行时权限位无效，Step 2 已踩）；用「目标位置被同名目录占据」造 `File::create` 失败更稳 | plan-v2 Step 3 风险段 |

---

## 4. 详细设计

### 4.1 `atomic_write`：单一实现（S3-a / D-S3-02）

新模块 `crates/core/src/storage/atomic.rs`，导出一个函数 + 两个伴生：

```rust
/// 原子写：tmp → 写入 → flush → sync_all → rename → fsync 父目录（D-J3）。
///
/// `write` 闭包把内容写进 tmp（快照：header+正文；manifest：magic+版本+CRC+正文）。
/// 收闭包而非 `&[u8]`：快照正文 52MB 级，拼整包要多一次全量拷贝（D-S3-02）。
///
/// 语义保证：本函数返回 Ok 后，`path` 要么是旧内容（本函数失败时），
/// 要么是新内容且已尽力持久化（文件 + 父目录均 fsync）。
/// 失败时 tmp 可能残留（崩溃或写失败），由回收机制处理（§4.4）。
pub fn atomic_write(
    path: &Path,
    write: impl FnOnce(&mut BufWriter<File>) -> std::io::Result<()>,
) -> Result<()> {
    let tmp = tmp_path(path);                          // D-S3-01：path + ".tmp" 追加
    let file = File::create(&tmp)?;                    // 截断复用：上次孤儿同名 tmp 直接覆盖
    let mut w = BufWriter::new(file);
    write(&mut w)?;
    fault::checkpoint(1);                              // ← 注入点：写入后、flush 前
    w.flush()?;
    w.get_ref().sync_all()?;                           // ← 数据落盘（不是只到页缓存）
    fault::checkpoint(2);                              // ← 注入点：sync 后、rename 前
    drop(w);                                           // Windows：rename 前必须关句柄
    std::fs::rename(&tmp, path)?;                      // ← 唯一发布点
    fault::checkpoint(3);                              // ← 注入点：rename 后、目录 fsync 前
    fsync_dir(path.parent());
    Ok(())
}

/// tmp 路径 = 目标路径 + ".tmp"（追加，不用 with_extension——见 §2.3 双后缀实证）。
pub fn tmp_path(path: &Path) -> PathBuf;

/// fsync 父目录，best-effort（Windows / 网络文件系统不支持则跳过，错误吞掉）。
fn fsync_dir(dir: Option<&Path>);
```

要点：

- **两个调用方共用**：
  - `write_manifest_atomic`（graph.rs）重构为：编码 + 算 CRC → `atomic_write(path, |w| {写 magic+版本+CRC+正文})`。**公开签名不变**。
  - `save_with_crc`（snapshot.rs）重构为：编码 + 算 CRC → `atomic_write(path, |w| {`codec::write_header(w, crc)`?; `w.write_all(&body)`?})`。**公开签名不变**（仍返回正文 CRC 供图锚点）。
- **`File::create` 对 tmp 的截断语义就是「save 前回收」**：同名孤儿 tmp 无须先删（D-S3-05）。
- **`drop(w)` 显式关句柄再 rename**：现状 manifest 实现用块作用域达到同一目的，抽取后统一为显式 `drop`，防止未来有人在闭包外挪动作用域。
- `fsync_dir` 保持现状的吞错语义（`let _ = d.sync_all()`）：目录 fsync 是**尽力而为的加固**，
  失败不构成 `save` 失败——rename 已发生，报 Err 反而会让调用方误以为快照没写成。

### 4.2 `save_with_crc` 接线（S3-b）

改造后：

```rust
pub fn save_with_crc(..) -> Result<u32> {
    let snapshot = Snapshot { .. };
    let body = bincode::serde::encode_to_vec(..)?;
    let crc = { /* crc32fast 不变 */ };

    atomic_write(path, |w| {
        codec::write_header(w, crc)?;      // 12B header（codec 不动）
        w.write_all(&body)
    })?;
    Ok(crc)
}
```

- **格式零变化**：header 布局、`effective_version()`、正文 bincode 编码全部不动（C3）。
- **内存不变**：正文本来就在内存里（`encode_to_vec`），闭包写入不引入额外拷贝。
- `save` / `load` 兼容签名不动；`SearchIndex::save` 的调用序列**一行不改**
  （`commit → save_with_crc → persist_graph` 顺序保持——先快照后图，
  manifest 锚定的 CRC 自然指向新快照）。

### 4.3 save 全序列的崩溃窗口矩阵（验收 1 的证明骨架）

改造后，`SearchIndex::save` 在每个注入点（§4.5 的 checkpoint）崩溃的后果：

| 注入点 | 崩溃时刻 | `foo.idx`（真源） | tmp 残留 | 图 sidecar | 下次 `load` 的结果 |
| --- | --- | --- | --- | --- | --- |
| cp1 | 快照 tmp 写入后、flush/sync 前 | **旧**（未动） | 有（半截） | 旧 manifest 绑旧 CRC | 旧快照 + 图命中（若图曾与旧快照对齐）或降级；**绝不损坏** |
| cp2 | 快照 tmp sync 后、rename 前 | **旧**（未动） | 有（完整） | 同上 | 同上；tmp 为完整新内容但**永不权威**，按孤儿回收 |
| cp3 | 快照 rename 后、目录 fsync 前 | **新** | 可能有 | 旧 manifest 绑旧 CRC → **失效** | 新快照 + 图降级重建（慢而正确） |
| — | 图 dump 中 / manifest 发布前 | 新 | — | 半截图 + 旧 manifest | 新快照 + 降级重建（Step 2 已覆盖，W3） |
| — | manifest 发布后 | 新 | — | 新 | 全新一致（W4） |

**核心不变式**：rename 之前 `foo.idx` 从未被触碰 ⇒ cp1/cp2 下真源恒为旧值；
rename 之后真源恒为新值且数据已 `sync_all`。`SnapshotCorrupted` 在任何窗口都**不可达**。

> 注：cp3 之后目录 fsync 前的掉电在极少数文件系统上理论可回滚 rename（回到旧快照）——
> 这仍是「旧或新」二选一，不变式不破；这正是 fsync_dir 尽力而为即可的原因。

### 4.4 tmp 孤儿回收（S3-c / D-S3-05)

| 回收点 | 动作 | 依据 |
| --- | --- | --- |
| `save_with_crc` 进入时 | **不显式删**——`File::create` 截断同名 tmp，天然覆盖 | C5：无并发写，同名 tmp 必是自己的或上次崩溃的 |
| `load_with_crc` 成功后 | best-effort 删除 `tmp_path(path)`（快照本体已验证完好，同名 tmp 必为孤儿） | 原子协议：**tmp 永不权威**——即便它比 `foo.idx` 新，也没有任何路径会去读它 |
| `remove_sidecars`（既有） | 清理 `tmp_path(manifest)`（manifest tmp）——**仅改命名**：把 `with_extension` 拼法换成 `tmp_path`，消除双 `hnsw` 怪名 | D-S3-01；写侧 `write_manifest_atomic` 同步换，两边必须一次改齐（漏一边 = 清理失效，S3-T1 兜底） |

`.gitignore` 补一行 `*.manifest.tmp`（`*.idx.tmp` 已有）。**边界声明**：非 tmp 的图
sidecar（`*.hnsw.graph` / `*.hnsw.data` / `*.hnsw.manifest`）ignore 归横切 T7-24
（issue #25），本文不越界。

### 4.5 故障注入钩子（S3-d 的前置设施 / D-S3-03）

```rust
/// storage/atomic.rs 内，#[doc(hidden)]（unstable，仅供测试）。
pub mod fault {
    /// 0 = 不注入（默认）。1..=3 = 在 atomic_write 的第 N 个 checkpoint panic。
    /// checkpoint 语义见 atomic_write 内标注（写入后 / sync 后 / rename 后）。
    pub static FAIL_AT: std::sync::atomic::AtomicU8 = AtomicU8::new(0);

    pub(crate) fn checkpoint(n: u8) {
        if Self::FAIL_AT.load(Relaxed) == n {
            panic!("fault-inject: atomic_write checkpoint {n}");
        }
    }
}
```

方案取舍（D-S3-03 完整论证）：

| 方案 | 优点 | 缺点 | 结论 |
| --- | --- | --- | --- |
| A. `test-util` feature | 生产二进制零代码 | CI 矩阵要加一档（`--features test-util`），守门清单膨胀；漏跑即测试静默消失 | 备选 |
| B. **`#[doc(hidden)]` 常驻 + AtomicU8** | 集成测试（`tests/`）直接可用，CI 零改动；每次 `atomic_write` 多 3 次 relaxed 原子读，相对 52MB fsync 可忽略 | 隐藏 API 理论上可被外部误用 | **推荐**（防误用：doc(hidden) + 文档注明 unstable + `FAIL_AT` 默认值断言测试 S3-T9） |
| C. `#[cfg(test)]` | 零暴露 | 单元测试专属——`tests/` 是独立 crate，看不见宿主 crate 的 `cfg(test)`，注入点又恰在集成路径上 | 否 |
| D. 环境变量 | 零签名变化 | 生产代码读 env 改行为 = 隐藏通道，比隐藏 API 更糟；还要处理 env 解析错误 | 否 |

测试侧用法（`catch_unwind` 是**测试**的职责，不是生产代码的——生产代码自身不会 panic）：

```rust
let r = std::panic::catch_unwind(|| {
    fault::FAIL_AT.store(2, Relaxed);
    atomic_write(&path, |w| w.write_all(b"new"))
});
fault::FAIL_AT.store(0, Relaxed);           // 无论成败都复位（防测试间泄漏）
assert!(r.is_err());
assert_eq!(std::fs::read(&path)?, b"old");  // 旧快照完好
```

### 4.6 R19 收敛：`dump_graph` 的 `catch_unwind`（S3-e / D-S3-04）

新增 `pub(crate) fn dump_graph_caught`（落 `vector/persist.rs`，与 `VectorGraphPersist`
同处；`search/index.rs` 的 `write_graph_sidecar` 改调它）：

```rust
/// dump_graph 的 panic 边界（R19 写路径残余收敛，S3-e）。
///
/// `hnsw_rs` 的 `DumpInit::new` 打不开输出文件时 panic_any（hnswio.rs:208/226），
/// 不是 Err，`?` 接不住——TOCTOU 窗口在 probe 之外的部分由此兜底。
/// panic 在此降级为 Err(VectorGraph)，汇入 Step 2 P0-3 语义链：
/// Lenient → 警告 + GraphStatus::PersistFailed + save 仍 Ok；Strict → Err。
pub(crate) fn dump_graph_caught(
    g: &dyn VectorGraphPersist,
    path: &Path,
) -> Result<GraphStats> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| g.dump_graph(path)))
        .map_err(|payload| Error::VectorGraph(format!(
            "hnsw_rs 图 dump panic（R19）：{}",
            panic_message(&payload)     // downcast &str / String，其余给 "<非文本 payload>"
        )))?
}
```

设计论证：

- **`AssertUnwindSafe` 的正当性**：闭包只捕获 `&dyn VectorGraphPersist` 与 `&Path`；
  `dump_graph` 是 `&self`（只读），panic 不可能留下半更新的内存图。文件侧的半截产物
  由既有失败路径 `remove_sidecars` 清理（`search/index.rs:504-511` 原样复用）。
  ⇒ **不新增任何状态机**，panic 只是多了一种进入既有 Err 分支的方式。
- **probe 保留**（第一道防线）：目录只读等可预判的失败仍走 Err，不必经历 panic 的
  栈展开开销与 stderr 噪音。两层防线各管一段：probe 管「可预判」，catch_unwind 管「TOCTOU 残余」。
- **已知残余（如实声明）**：
  1. `panic = "abort"` 构建下 `catch_unwind` 无效。当前 workspace `[profile.release]`
     未设置 panic 策略（默认 unwind）——**在 Cargo.toml 加注释钉死「不得设 panic=abort」**
     （S3-06 的一部分），因为那会静默废掉本防线。
  2. panic 会先经默认 panic hook 打印到 stderr，再进入我们的警告输出。库内不宜全局
     `set_hook`（污染宿主），接受这点噪音；文档声明。
  3. 读路径残余（校验通过后文件被并发改写仍可能 panic）**不在本 Step 范围**——
     那是「无并发写同一快照」语义的边界（C5），catch_unwind 对读路径无意义
     （读路径 panic 时快照已加载、不涉及落盘一致性）。

---

## 5. 决策记录（D-S3-01 ~ D-S3-06）

> 编号沿用项目惯例（Step 2 用 D-S2-xx）。以下每条 = §0 速读表的展开。

### D-S3-01 tmp 命名统一为「目标路径 + `.tmp` 追加」

- **现状**（实证，本机 rustc 验证）：`Path::new("foo.idx.hnsw.manifest").with_extension("hnsw.manifest.tmp")`
  → `foo.idx.hnsw.hnsw.manifest.tmp`（双 `hnsw`）。
- **改为追加**：`foo.idx.tmp` / `foo.idx.hnsw.manifest.tmp`——与 `.gitignore` 既有
  `*.idx.tmp` 天然对齐（快照 tmp 无须新增 ignore 规则），manifest tmp 只需一条 `*.manifest.tmp`。
- **风险**：纯中间文件名变更，最终产物路径与格式零影响；唯一约束是
  `write_manifest_atomic`（写）与 `remove_sidecars`（清）**必须同 PR 改齐**，
  S3-T1 的「tmp 不残留」断言兜底。

### D-S3-02 `atomic_write` 落点 `storage/atomic.rs`、签名收写闭包

- **不放 graph.rs**：依赖方向应为 `snapshot.rs / graph.rs → atomic.rs`；
  塞进 graph.rs 会让快照写入反向依赖图的模块语义。
- **闭包而非 `&[u8]`**：快照正文 52MB 级（12K 语料），`&[u8]` 签名逼调用方拼
  「header+正文」整包 = 一次全量拷贝；闭包让 header 与正文各写各的（`codec::write_header`
  签名 `&mut impl Write` 不用改）。两个调用方（快照 / manifest）恰好都是
  「小 header + 大 body」形态，闭包是共同分母。
- **代价**：闭包让 fault checkpoint 只能打在「整段写入之后」——但这正是有意义的粒度
  （写入中途的崩溃由 OS 保证 tmp 非权威，无须注入验证；见 §4.3 矩阵，cp1 已覆盖
  「写了但没落盘」的形态）。

### D-S3-03 故障注入钩子：`#[doc(hidden)]` 常驻（方案 B）

完整对比见 §4.5 表格。补充防误用三件套：
1. `#[doc(hidden)]`——不出现在文档与自动补全；
2. 模块文档首行注明「unstable，仅供测试，生产代码禁止触碰」；
3. S3-T9 断言 `FAIL_AT` 初始值为 0 且一次未注入的 `atomic_write` 行为与改造前逐位一致
   （roundtrip 复用既有快照测试，自动覆盖）。

### D-S3-04 R19 收敛走 `catch_unwind` + 既有 P0-3 语义链

完整论证见 §4.6。备选与否决：

| 备选 | 否决理由 |
| --- | --- |
| 给 hnsw_rs 提 patch（把 `panic_any` 改 Err） | 上游依赖，升级节奏不受我们控制；0.3.4 锁定版本下不可行 |
| dump 前用 `create_new` 预占两个图文件路径再删 | `hnsw_rs` 自己 `create(true).truncate(true)` 打开（`hnswio.rs:199-202`），预占-删除-真开之间窗口更大，纯负优化 |
| 只靠 probe、接受残余 | 与 plan-v2 S3-e 的指派（「顺带收 R19」）直接冲突 |

### D-S3-05 孤儿回收时机：load 后删、save 靠截断

- **不在 save 前显式删 tmp**：`File::create` 的截断就是删除 + 重建，显式删是冗余 syscall；
  且「删了再建」反而引入一个（极小的）窗口：删成功、create 失败，留不下任何 tmp——
  无害但也没收益。
- **load 成功后删**：这是唯一能回收「最后一次 save 崩溃残留」的时机（那次 save 不会再来了）。
  删除依据 C5（无并发写）：若真有并发 save 正在写 tmp，删掉它会让对方 `File::create`
  的句柄悬空……但该语义本就不存在，不为它设计。
- **manifest tmp**：生命周期跟随 `remove_sidecars`（save 失败清理 / 无图场景清理），
  已有行为，仅随 D-S3-01 改名。

### D-S3-06 fsync 代价：接受、实测、不开逃生舱

- 52MB 快照的 `sync_all` 在普通 SSD 上量级约 50~300ms，HDD 上可达秒级。save 是
  **用户显式调用的低频操作**（CLI `build` / 宿主落库），不触碰任何 NFR 限额
  （NFR-02 查询 / NFR-03 构建 / NFR-04 冷启动都不含 save）。
- **不提供 `skip_fsync` 选项**：fsync 是正确性语义（§2.1 缺陷 2 的直接修复），
  做成开关等于把「要不要丢数据」交给调用方——与「不得静默读错」同一级别的
  非选项。`--no-graph-persist` 是性能逃生舱，管的是缓存；真源没有逃生舱。
- S3-08 在 12K 语料实测改造前后 `save` 耗时，**给范围**（三次以上取 min~max），
  写入 `eval-report.md` 新小节（§8.x，编号顺延）。

---

## 6. 影响面与兼容性

| 维度 | 影响 |
| --- | --- |
| 快照格式 | **零变化**（C3）：magic / `effective_version()` / CRC 布局 / bincode 编码全不动；`FORMAT_VERSION` 保持 2 |
| 公开 API | **零新增公开项**（`atomic_write` / `tmp_path` 为 `pub(crate)` 或 storage 内部；`fault` 模块 `#[doc(hidden)]`）。`save_with_crc` / `save` / `load` / `write_manifest_atomic` 签名全部不变 |
| 行为变化 | ① save 落盘变成原子（对外可见的唯一变化：崩溃不再产生半截快照）；② save 变慢（fsync，量级见 D-S3-06 实测任务）；③ manifest tmp 中间文件名变化（双 hnsw → 追加式）；④ `hnsw_rs` dump panic 从「进程死」变为「警告 + PersistFailed」 |
| 旧快照兼容 | 旧内核写的快照（直写产物）就是完整文件，新代码照常加载；新内核写的快照旧内核照常加载——原子性不进格式 |
| 测试影响 | 既有快照 roundtrip / CRC 系列测试**零修改**应全绿（这是 S3-T2 的内容）；`原子写不留tmp残留`（graph.rs 单测）随 D-S3-01 改名自动适配 |
| CI | 无新 job、无新 feature 档（D-S3-03 方案 B 的核心收益） |
| `.gitignore` | +1 行 `*.manifest.tmp` |

---

## 7. 测试计划

> 编号 S3-Tn。带 ⚠️ 的是**必测**（直接对应验收）。全部新增测试集中在
> `crates/core/tests/atomic_snapshot.rs`（集成层）与既有模块的 `#[cfg(test)]`（单元层）。

| # | 测试 | 断言 |
| --- | --- | --- |
| **S3-T1** ⚠️ | `atomic_write` 基础 + tmp 不残留 | ① 首写 / 覆盖写 roundtrip；② 成功后目录中只有目标文件（无 tmp）；③ 目标路径被**同名目录**占据（C7：不用 chmod）→ `Err`，不 panic |
| **S3-T2** ⚠️ | 既有快照测试全绿（回归） | `snapshot.rs` 现有 6 个测试（roundtrip / 空 / CRC 损坏 / 截断 / with_crc / CRC 变化）**零修改**通过——证明格式与语义未漂移 |
| **S3-T3** ⚠️ | **注入 cp2（sync 后、rename 前，plan-v2 钦定点）** | `catch_unwind(atomic_write)` 后：① `foo.idx` 内容 == 旧值；② tmp 存在且内容 == 新值（但**永不权威**）；③ 随后 `load_with_crc` 成功且得旧内容；④ load 触发回收，tmp 消失 |
| **S3-T4** | 注入 cp1（写入后、flush 前） | 不变式与 T3 的 ①③④ 相同（cp1/cp2 同属 rename 前，真源均未动）；tmp 为半截——同样被回收 |
| **S3-T5** | 注入 cp3（rename 后、目录 fsync 前） | ① `foo.idx` 内容 == 新值；② `load_with_crc` 成功得新内容；③ 残留 tmp（若有）被 load 回收 |
| **S3-T6** ⚠️ | **端到端**（验收 1 的直接对应） | 真实 `SearchIndex`（小语料）：save 前注入各 checkpoint + `catch_unwind(save)` → 重新 `SearchIndex::load`：要么旧要么新，**绝不 `SnapshotCorrupted`**；且检索功能可用 |
| **S3-T7** ⚠️ | **R19：dump panic 不杀进程**（验收 4） | 单元层（`vector/persist.rs` `#[cfg(test)]`）：mock 一个 `dump_graph` 直接 panic 的 `VectorGraphPersist` 实现 → `dump_graph_caught` 返回 `Err(VectorGraph(含 panic 信息))`；接入门面层后 Lenient 语义 = 既有 S2-T19 断言（save Ok + `PersistFailed`） |
| **S3-T8** | manifest tmp 改名一致性 | `write_manifest_atomic` 成功 / 失败（同名目录占据）后，`remove_sidecars` 均不留 `*.manifest.tmp`（写、清两边用的是同一 `tmp_path`） |
| **S3-T9** | fault 钩子默认关闭 | `FAIL_AT == 0` 时 `atomic_write` 行为与 T1 完全一致（防钩子泄漏进生产语义） |
| **S3-T10** | fsync 代价实测（非 CI，本地） | 12K 语料：改造前后 `save` 耗时各测 ≥3 次取 min~max，入 `eval-report.md`（S3-08 交付） |

> 测试技法备忘（沿 Step 2 教训）：所有 `catch_unwind` 后**立即复位** `FAIL_AT`（用
> guard 或 finally 语义），防测试失败时泄漏到下一个测试；「写文件必失败」场景一律用
> 同名目录占据，不用 chmod。

---

## 8. 实施任务拆分（S3-01 ~ S3-09）

> 量级 S（plan-v2）。默认**单个实现 PR**（S3-02~S3-08 代码 + 测试 + 文档回写同 PR）：
> Step 2 的教训是堆叠 PR 的坑（#13/#14 显示 MERGED 却未进 main），S 量级不值得拆。
> 如评审认为单 PR 过大，拆分线在 S3-05（代码 PR / 测试+文档 PR）。

| # | 任务 | 对应 | 依赖 | 量级 |
| --- | --- | --- | --- | --- |
| **S3-01** | 本文评审 + D-S3-01~06 拍板，回写本文状态 | — | — | S |
| **S3-02** | `storage/atomic.rs`：`atomic_write`（写闭包签名）+ `tmp_path` + `fsync_dir` + `fault` 钩子 + 单测（T1 / T9） | S3-a | S3-01 | S |
| **S3-03** | `write_manifest_atomic` 改走 `atomic_write`（含 D-S3-01 改名）；`remove_sidecars` 的 tmp 清理同步换 `tmp_path`（T8） | S3-a | S3-02 | S |
| **S3-04** | `save_with_crc` 改走 `atomic_write`（S3-b）；`load_with_crc` 成功后孤儿回收（S3-c）；既有测试回归（T2） | S3-b/c | S3-02 | S |
| **S3-05** | 故障注入测试 T3~T6（`tests/atomic_snapshot.rs`） | S3-d | S3-04 | S |
| **S3-06** | R19：`dump_graph_caught`（catch_unwind + payload 降级）接入 `write_graph_sidecar`；Cargo.toml 钉「不得 panic=abort」注释；单测 T7 | S3-e | S3-03 | S |
| **S3-07** | `.gitignore` + `*.manifest.tmp`（S3-c 后半；非 tmp 的 sidecar ignore 归 T7-24，不越界） | S3-c | S3-03 | XS |
| **S3-08** | fsync 代价实测（12K，范围值）入 `eval-report.md`（T10） | — | S3-04 | S |
| **S3-09** | 文档回写：架构 v1.8（§7.6.2 前提段销账「快照本体至今非原子」/ R19 残余更新 / 变更记录）、需求（FR-31 验收注记落地）、`plan-v2`（Step 3 进度 ✅ + 验收对照）、`CHANGELOG`、`docs/README.md` 索引、本文「实施结果」段 | — | S3-05~08 | S |

> 注：S3-01 即本 PR；S3-02~S3-08 为实现 PR 的内容；S3-09 在实现 PR 内完成
> （单 PR 方案下不单独拆）。

---

## 9. 风险与未决问题

> 编号沿用架构文档第 14 章的 R 系列。本 Step 不新增 R 条目，只**收敛既有 R19 的写路径残余**
> 与**销掉 Q-C3**。R19 收敛后的残余见 §4.6「已知残余」三条（panic=abort 构建无效 /
> panic 噪音 / 读路径不在范围）。

| 风险 | 影响 | 应对 | 残余 |
| --- | --- | --- | --- |
| fsync 在慢盘（HDD / 网络卷）上拖慢 save | 用户可感知的落库延迟 | D-S3-06 实测入档；save 低频；真源不提供跳过开关 | 接受（性能，非正确性） |
| `catch_unwind` 在未来某构建配置（panic=abort）下静默失效 | R19 防线失效且无人知晓 | Cargo.toml 注释钉死 + 本文 §4.6 声明；无编译期检测手段 | 接受（文档级防线） |
| `fault` 钩子被外部代码误用 | 非预期 panic | doc(hidden) + unstable 声明 + T9 默认值断言；panic 是「失败可见」不是「静默错数据」 | 接受 |
| Windows 上目录 fsync 跳过 | rename 目录项持久性弱一层 | C6 既有事实（Step 2 同款）；不变式仍是「旧或新」 | 接受 |
| 快照写闭包内 `codec::write_header` 失败的中间态 | tmp 半截 | 不变式覆盖（rename 前真源未动）；无专门测试 | — |

**未决问题（需评审补充意见）**：

1. **S3-T7 的 mock 落点**：mock `VectorGraphPersist` 需要实现完整 `VectorIndex` trait
   （若干无关方法）。若实现时发现 trait 面过大，可给 `VectorIndex` 加
   `unimplemented!()` 默认实现的测试替身，或把 `dump_graph_caught` 的入参收窄为
   `impl FnOnce() -> Result<GraphStats>`——倾向后者（更窄的闭包 = 更少的 mock 面），
   实现时定。
2. **`atomic_write` 是否值得对 `data/*.snapshot`（脚本产物）生效**：脚本走
   CLI `build` → `SearchIndex::save` → 自动生效，无须额外动作——此处仅确认无遗漏入口
   （已核实 C4，列出备查）。
3. **eval-report 小节编号**：§8.x 顺延还是独立 §9——实现时看当时文档结构定，不影响设计。

---

## 附录 A：新增 / 变更 API 一览

```rust
// ── storage/atomic.rs（新增模块）──
pub(crate) fn atomic_write(
    path: &Path,
    write: impl FnOnce(&mut BufWriter<File>) -> std::io::Result<()>,
) -> Result<()>;
//   tmp → write → flush → sync_all → rename → fsync 父目录
//   checkpoint 1/2/3 = 写入后 / sync 后 / rename 后（fault 注入点）

pub(crate) fn tmp_path(path: &Path) -> PathBuf;   // path + ".tmp" 追加（D-S3-01）

/// ⚠️ unstable，仅供测试（doc(hidden)）
pub mod fault {
    pub static FAIL_AT: AtomicU8;                 // 0 = 不注入（默认）
}

// ── storage/graph.rs（变更：内部实现，签名不变）──
pub fn write_manifest_atomic(path: &Path, m: &GraphManifest) -> Result<()>;
//   编码 + CRC 后改走 atomic_write；tmp 名从双 hnsw 怪名改为追加式

// ── storage/snapshot.rs（变更：内部实现，签名不变）──
pub fn save_with_crc(..) -> Result<u32>;          // 改走 atomic_write，仍返回正文 CRC
pub fn load_with_crc(path: &Path) -> Result<..>;  // 成功后 best-effort 回收 tmp 孤儿

// ── vector/persist.rs（新增）──
pub(crate) fn dump_graph_caught(
    g: &dyn VectorGraphPersist,
    path: &Path,
) -> Result<GraphStats>;                          // R19：catch_unwind → Err(VectorGraph)
```

## 附录 B：checkpoint × 断言对照（S3-T3~T6 的执行脚本）

| checkpoint | T3/T4/T6 断言（rename 前） | T5/T6 断言（rename 后） |
| --- | --- | --- |
| `foo.idx` 内容 | == 旧快照字节（或旧索引可加载） | == 新快照（新文档可检索） |
| `load` 结果 | Ok，语义 == 旧；`GraphStatus` 允许 Loaded 或 Rebuilt（图锚旧 CRC） | Ok，语义 == 新；图必 Rebuilt（manifest 绑旧 CRC，直到下一次 save） |
| 损坏检查 | `!matches!(.., Err(SnapshotCorrupted))` 且 `load` 必须 Ok | 同左 |
| tmp | 存在（半截或完整）→ load 后被回收 | 可能存在 → load 后被回收 |
| 端到端（T6） | `SearchIndex::load` 后旧文档在、新文档不在 | `SearchIndex::load` 后新文档在 |

## 附录 C：本文引用的项目内证据

| 证据 | 位置 |
| --- | --- |
| 快照直写无原子性 | `crates/core/src/storage/snapshot.rs:88-92` |
| manifest 原子写（抽取源） | `crates/core/src/storage/graph.rs:236-265` |
| tmp 双 hnsw 怪名 | `crates/core/src/storage/graph.rs:244,279`（本机 rustc 实证输出 `foo.idx.hnsw.hnsw.manifest.tmp`） |
| probe 探测（R19 第一道防线） | `crates/core/src/vector/persist.rs:101-115` |
| P0-3 缓存失败语义链（复用） | `crates/core/src/search/index.rs:497-513` |
| `DumpInit` panic_any | `hnsw_rs-0.3.4 hnswio.rs:208/226`（转引自 `v2-step2-design.md` 附录 B，已源码核实） |
| dump 原地 truncate 打开 | `hnsw_rs-0.3.4 hnswio.rs:199-202`（同上） |
| 「快照本体至今非原子」前提段 | `docs/devel/architecture-design.md:805-811`（v1.7） |
| S3-a~e 子任务定义 | `docs/devel/plan-v2.md` §4 Step 3 |
