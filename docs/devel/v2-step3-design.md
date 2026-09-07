# HelixIndex V2 · Step 3 详细设计（原子快照：崩溃一致性）

> 面向 Agent 场景的通用检索引擎内核——V2 Step 3 的详细设计（**v0.3，已拍板，可开工**）。
> 本文件回答：快照怎么原子落盘、崩溃在每个窗口留下什么后果、tmp 孤儿怎么回收、
> `hnsw_rs` 写路径的 panic 怎么兜住、怎么用故障注入证明以上全部成立。

| 项 | 内容 |
| --- | --- |
| 版本 | **v0.3（拍板版：D-S3-01~07 全部按建议采纳，可开工实现）** |
| 日期 | 2026-09-07 |
| 状态 | 评审闭环（v0.1 → 8 条意见全回应 → v0.2）+ **拍板完成（D-S3-01~07，2026-09-07）**；S3-01 完成，S3-02~09 待开工（单 PR） |
| 修订记录 | v0.1 首版；v0.2 回应评审：checkpoint 计数校正（4→3）、D-S3-03 方案重构（B → C′，钩子不进公开面）、验收 3 按快照/manifest tmp 拆分口径、cp1/cp3 矩阵行按真实崩溃形态改写、代码草图笔误修正、Strict 残留行为如实声明（新增 D-S3-07）、panic=abort 约束降级为文档级（库 profile 对宿主无效）；v0.3 拍板：D-S3-01~07 全部按 §0 建议列原样采纳（含 D-S3-07「Strict 补 best-effort 清理」） |
| 上游 | `plan-v2.md` v0.4（Step 3 / S3-a~e / Q-C3 定级「高（正确性）」）、`requirements-spec.md` v1.7（FR-31 **Must**）、`architecture-design.md` v1.7（§7.6.2 前提段 / ADR-A / R19）、`v2-step2-design.md` v0.3（ADR-A 方案 C，已实施） |
| 范围 | T7-13 原子快照（tmp + fsync + rename + fsync 父目录，D-J3）+ tmp 孤儿回收 + 故障注入测试 + **R19 写路径残余收敛**（S3-e） |
| 非范围 | 图 sidecar 的校验/降级（Step 2 已交付，本文只引用）；compaction 与跨快照体积问题（**Step 4**）；低选择度兜底与 Metrics 可观测（**Step 5**）；`parallel_build` 默认翻转（横切 **T7-21**，独立 PR）；`.gitignore` 补非 tmp 的图 sidecar（横切 **T7-24** / issue #25，本文只补 tmp 模式） |

---

## 评审速读：7 个已拍板决策（✅ 2026-09-07，全部按建议列采纳）

> 评审者时间有限时，先看这张表。每条在 §5 有完整取舍与证据。
> **拍板结果**：D-S3-01~07 全部按「建议」列原样采纳——下表第三列即终局结论，
> 各节的备选/可否决讨论仅作存档。

| # | 决策 | 建议（= 拍板结果 ✅） | 阻塞谁 |
| --- | --- | --- | --- |
| **D-S3-01** | tmp 命名规则：现状 manifest 的 tmp 用 `with_extension` 拼出**双后缀怪名**（实测 `foo.idx.hnsw.manifest` → `foo.idx.hnsw.hnsw.manifest.tmp`） | 统一改为**目标路径 + `.tmp` 追加**：`foo.idx.tmp` / `foo.idx.hnsw.manifest.tmp`。纯中间文件名变更，最终文件与格式零影响；且与 `.gitignore` 既有 `*.idx.tmp` 模式对齐 | 阻塞 S3-02/S3-03 |
| **D-S3-02** | `atomic_write` 的落点与签名：放 `graph.rs` 还是新模块；收 `&[u8]` 还是写闭包 | 新模块 `storage/atomic.rs`（`snapshot.rs` 不得反向依赖 graph 语义）；签名收**写闭包** `FnOnce(&mut BufWriter<File>) -> io::Result<()>`——快照 52MB 若按 `&[u8]` 拼整包要多一次全量拷贝 | 阻塞 S3-02~04 |
| **D-S3-03** | 故障注入钩子的形态：`test-util` feature / `#[cfg(test)]` / 常驻原子变量 / 环境变量 | **常驻 `pub(crate)` 钩子 + 注入测试内迁 crate 内 `#[cfg(test)]`（方案 C′，v0.2 修订）**。v0.1 的方案 B（`#[doc(hidden)]` 公开）有结构性矛盾：钩子藏在私有 `mod atomic` 里对 `tests/` 同样不可达，要可达就必须公开——「零新增公开项」随之破产。C′ 让公开面**真零新增**；代价只是注入测试不放集成层（T3~T5 本就要直接调 `pub(crate)` 的 `atomic_write`，集成层本来就放不下） | 阻塞 S3-05 |
| **D-S3-04** | R19 写路径残余（`DumpInit` 的 `panic_any`，TOCTOU 窗口只能缩小不能归零）怎么收敛 | `catch_unwind(AssertUnwindSafe(dump_graph))`，panic payload 降级为 `Err(VectorGraph)`，**汇入 Step 2 既有的 P0-3 语义链**（Lenient 警告 + `PersistFailed` / Strict Err），不新增状态机。probe 探测保留（第一道防线） | 阻塞 S3-06 |
| **D-S3-05** | tmp 孤儿回收时机：save 前 / save 后 / load 后 / 都做 | **load 成功后 best-effort 删除**（快照本体），**save 前 `File::create` 截断复用**（天然覆盖同名 tmp）；manifest tmp 由既有 `remove_sidecars` 清理（仅改名，**只在 save 路径触发**——load-only 部署会保留 manifest tmp 孤儿，无害）。依据：原子协议下 tmp 永不权威，且项目无「并发写同一快照」语义 | 阻塞 S3-04 |
| **D-S3-06** | fsync 给 `save` 引入的额外耗时（52MB 级 fsync）是否接受、要不要「跳过 fsync」逃生舱 | **接受并实测入档**（save 是用户显式调用的低频操作，与 NFR-03 构建口径无关）；**不提供跳过开关**——正确性语义不做成选项 | 影响 S3-08 |
| **D-S3-07** | Strict 模式下图 dump 失败（含 panic 收敛后的 Err）返回前，要不要也 best-effort 清理 sidecar（现状 `index.rs:503` 直接上抛、不清理，残留半截图文件 + 已失效的旧 manifest 直到下次成功 save） | **✅ 清理**：与 P0-3「图是缓存」精神一致（失败的缓存不留尸体）；清理自身失败不升级、不改变 Err 语义。现状功能上也安全（图是缓存，load 侧有 manifest CRC 把关），拍板取更干净的一边 | 影响 S3-06 |

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

1. ⚠️ **崩溃不变式**：在 `save` 序列的**任一注入点**（§4.3 三个 checkpoint，cp1~cp3）崩溃后，
   `load` 要么拿到**旧快照**、要么拿到**新快照**，**绝不 `SnapshotCorrupted`**（S3-T3~T6）。
   注：注入只覆盖**快照写窗口**（`save` 内首次 `atomic_write`）；图 dump 与 manifest 发布
   窗口（§4.3 矩阵末两行 / W2~W4）是性能退化窗口，Step 2 语义已覆盖，不做端到端注入复验
   （`FAIL_AT` 单发命中，端到端注入必先落在快照那次调用上——见 §7 T6 注）。
2. 快照重写后旧图自动失效（CRC 版本锚点）——Step 2 已有行为，本 Step 改造后**回归不破**（S3-T2）。
3. 崩溃残留的 tmp 孤儿被回收，**按类型分口径**（v0.2 拆分）：**快照 tmp**（`*.idx.tmp`）——
   下一次成功 **save**（`File::create` 截断复用）或 **load**（成功后 best-effort 删除）皆可回收；
   **manifest tmp**（`*.manifest.tmp`）——仅在**下一次 save** 时回收（`remove_sidecars` 或
   截断复用；load 路径不调用它）。load-only 部署会永久保留 manifest tmp 孤儿——无害
   （tmp 永不权威），但属已知行为（S3-T3 / T5 / T8）。
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
| cp1 | 快照 tmp 写入后、flush/sync 前 | **旧**（未动） | 有（**内容不定**，非权威） | 旧 manifest 绑旧 CRC | 旧快照 + 图命中（若图曾与旧快照对齐）或降级；**绝不损坏** |
| cp2 | 快照 tmp sync 后、rename 前 | **旧**（未动） | 有（完整） | 同上 | 同上；tmp 为完整新内容但**永不权威**，按孤儿回收 |
| cp3（进程崩溃） | 快照 rename 后、目录 fsync 前——kill / panic | **新** | **无**（rename 已消费本 save 的 tmp） | 旧 manifest 绑旧 CRC → **失效** | 新快照 + 图降级重建（慢而正确） |
| cp3（掉电） | 同窗口——目录 fsync 前掉电，rename 在极少数 FS 上可回滚 | **旧**（rename 回滚） | 有（完整新内容，非权威） | 同 cp1/cp2 | 旧快照 + 图命中或降级；不变式不破 |
| — | 图 dump 中 / manifest 发布前 | 新 | — | 半截图 + 旧 manifest | 新快照 + 降级重建（Step 2 已覆盖，W3） |
| — | manifest 发布后 | 新 | — | 新 | 全新一致（W4） |

**核心不变式**：rename 之前 `foo.idx` 从未被触碰 ⇒ cp1/cp2 下真源恒为旧值；
rename 之后真源恒为新值且数据已 `sync_all`。`SnapshotCorrupted` 在任何窗口都**不可达**。

> 注 1（cp3 二态，v0.2）：「进程崩溃」与「掉电」是**两个互斥的终态**，不会同时出现——
> kill 时 rename 对 VFS 已生效（`foo.idx` = 新、tmp 已消费）；掉电回滚时 rename 落地失败
> （`foo.idx` = 旧、tmp 还在）。两者都满足「旧或新」二选一。
>
> 注 2（cp1 的 tmp 内容，v0.2）：**不对 tmp 内容做任何断言**。真实崩溃下 52MB 正文经
> BufWriter（8KB 缓冲）边写边落盘，kill 时 tmp 至多缺尾部 <8KB；而**进程内注入**的
> panic 走栈展开，`BufWriter::drop` 会把缓冲尾部尽力刷进文件，tmp 反而常是完整的——
> 「半截」形态在 catch_unwind 世界里造不出来，只有真实 kill / 掉电能产生。
> 测试只断言 tmp 的**存在性与回收**（T3/T4），不断言内容完整度。

### 4.4 tmp 孤儿回收（S3-c / D-S3-05)

| 回收点 | 动作 | 依据 |
| --- | --- | --- |
| `save_with_crc` 进入时 | **不显式删**——`File::create` 截断同名 tmp，天然覆盖 | C5：无并发写，同名 tmp 必是自己的或上次崩溃的 |
| `load_with_crc` 成功后 | best-effort 删除 `tmp_path(path)`（快照本体已验证完好，同名 tmp 必为孤儿） | 原子协议：**tmp 永不权威**——即便它比 `foo.idx` 新，也没有任何路径会去读它 |
| `remove_sidecars`（既有） | 清理 `tmp_path(manifest)`（manifest tmp）——**仅改命名**：把 `with_extension` 拼法换成 `tmp_path`，消除双 `hnsw` 怪名 | D-S3-01；写侧 `write_manifest_atomic` 同步换，两边必须一次改齐（漏一边 = 清理失效，S3-T8 兜底）。⚠️ **触发面只在 save 路径**（`persist_graph` 各分支 + Lenient 清理，v0.2 核实：`index.rs:484/488/493/507`，load 从不调用）⇒ manifest tmp 孤儿只被**下一次 save** 回收，load-only 部署会一直留着（无害，非权威）——验收 3 已按此拆分口径 |

`.gitignore` 补一行 `*.manifest.tmp`（`*.idx.tmp` 已有）。**边界声明**：非 tmp 的图
sidecar（`*.hnsw.graph` / `*.hnsw.data` / `*.hnsw.manifest`）ignore 归横切 T7-24
（issue #25），本文不越界。

### 4.5 故障注入钩子（S3-d 的前置设施 / D-S3-03）

```rust
/// storage/atomic.rs 内，`pub(crate)`（**不进公开面**，D-S3-03 方案 C′，v0.2）。
///
/// ⚠️ 性能代价声明：`checkpoint` 在**生产路径**无条件执行，每次 `atomic_write`
/// 共 3 次 relaxed 原子读（FAIL_AT == 0 时为纯 no-op）。相对 52MB 级 fsync 可忽略，
/// **后续优化不得以此为由移除钩子**——移除等于抽掉 S3-T3~T6 全部注入测试的地基。
pub(crate) mod fault {
    use std::sync::atomic::{AtomicU8, Ordering};

    /// 0 = 不注入（默认）。1..=3 = 在 atomic_write 的第 N 个 checkpoint panic。
    /// checkpoint 语义见 atomic_write 内标注（写入后 / sync 后 / rename 后）。
    /// 单发语义：store 一次只命中**第一个**走到匹配 checkpoint 的调用——
    /// `save` 里 `atomic_write` 被调两次（快照、manifest），注入必先落在快照那次。
    pub(crate) static FAIL_AT: AtomicU8 = AtomicU8::new(0);

    pub(crate) fn checkpoint(n: u8) {
        if FAIL_AT.load(Ordering::Relaxed) == n {
            panic!("fault-inject: atomic_write checkpoint {n}");
        }
    }
}
```

方案取舍（D-S3-03 完整论证；v0.2 依评审意见重构——v0.1 的方案 B 存在结构性矛盾；
**✅ 已拍板采 C′**）：

| 方案 | 优点 | 缺点 | 结论 |
| --- | --- | --- | --- |
| A. `test-util` feature | 生产二进制零代码 | CI 矩阵要加一档（`--features test-util`），守门清单膨胀；漏跑即测试静默消失 | 否 |
| B′. `#[doc(hidden)]` **公开**常驻钩子 | `tests/` 集成层可直接设 `FAIL_AT`，CI 零改动；每次 `atomic_write` 3 次 relaxed 读可忽略 | **必须公开可达**（`storage` 子模块全私有 + 选择性 `pub use`，v0.2 核实 `storage/mod.rs:15-25`）⇒ 「零新增公开项」破产，且 doc(hidden) 只藏文档不藏可达性——外部依赖理论上能设 `FAIL_AT` 制造 panic | 否（存档：若未来需要 T6 进集成层再议） |
| **C′. 常驻 `pub(crate)` 钩子 + 注入测试内迁** | 公开面**真零新增**；CI 零改动；钩子常驻 ⇒ 测试路径与发布路径**逐位一致**；3 次 relaxed 读代价同 B′ | 注入测试（T3~T6、T9）必须放 crate 内 `#[cfg(test)]`，不能进 `tests/`；全局 `FAIL_AT` 与同二进制内并行测试互扰，需串行化（见下） | **✅ 已拍板（2026-09-07）** |
| D. 环境变量 | 零签名变化 | 生产代码读 env 改行为 = 隐藏通道，比隐藏 API 更糟；还要处理 env 解析错误 | 否 |

> v0.1 方案 B 的矛盾（评审指出）：钩子放进私有 `mod atomic` 后，即便内部写 `pub static
> FAIL_AT`，`tests/`（独立 crate）也够不到它——`cfg(test)` 不可见的问题只是换了藏法，
> 原样存在。要可达就必须公开暴露，B 实际上就是 B′。而 T3~T5、T9 本就要**直接调用**
> `pub(crate)` 的 `atomic_write`（往裸文件写 `b"old"`/`b"new"` 那种粒度），集成层
> 本来就放不下——真正受影响的只有端到端 T6，而它在 `search/index.rs` 的
> `#[cfg(test)]` 里同样能走完整的公开 API（`SearchIndex::save` → reload）。
> 结论：C′ 的代价几乎为零，换来公开面真干净。

测试侧用法（`catch_unwind` 是**测试**的职责，不是生产代码的——生产代码自身不会 panic）：

```rust
let r = std::panic::catch_unwind(|| {
    fault::FAIL_AT.store(2, Ordering::Relaxed);
    atomic_write(&path, |w| w.write_all(b"new"))
});
fault::FAIL_AT.store(0, Ordering::Relaxed);           // 无论成败都复位（防测试间泄漏）
assert!(r.is_err());
assert_eq!(std::fs::read(&path)?, b"old");  // 旧快照完好
```

⚠️ **并行互扰防护（v0.2 新增）**：C′ 下注入测试与同 crate 的其他单测共用一个测试二进制
（cargo 默认多线程跑），`FAIL_AT` 是进程级全局——A 测试设 `FAIL_AT=2` 的同时 B 测试的
`atomic_write` 恰好路过 checkpoint 2 就会被误杀。对策：注入测试共用一把测试内
`static MTX: Mutex<()>` 串行（持有期间才 store 非零值），或把整个注入矩阵并进单个
`#[test]` 顺序执行；复位用 guard（`Drop` 里 store 回 0）而非裸 store，防断言失败路径泄漏。

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
  清理**按模式分口径**（v0.2 核实 `index.rs:501-513`）：**Lenient** 走既有失败路径
  `remove_sidecars`（`index.rs:507-509`，best-effort，清理失败只警告）；**Strict**
  （`index.rs:503`）现状直接 `return Err(e)` **不清理**——残留半截
  `*.hnsw.graph`/`*.hnsw.data`（`dump_graph` 先删旧图再写）+ 已失效的旧 manifest，
  直到下一次成功 save 的 dump 先删再写才被覆盖。功能上安全（图是缓存、load 侧有
  manifest CRC 把关，CRC 不匹配即降级重建），但目录里留尸体——**D-S3-07 已拍板：
  Strict 返回 `Err` 前也补 best-effort `remove_sidecars`**（清理失败不升级、不改 Err
  语义；与 P0-3「整个写图链路都按缓存语义处理」的精神一致，S3-06 落地）。
  ⇒ **不新增任何状态机**，panic 只是多了一种进入既有 Err 分支的方式。
- **probe 保留**（第一道防线）：目录只读等可预判的失败仍走 Err，不必经历 panic 的
  栈展开开销与 stderr 噪音。两层防线各管一段：probe 管「可预判」，catch_unwind 管「TOCTOU 残余」。
- **已知残余（如实声明，v0.2 修订第 1 条）**：
  1. `panic = "abort"` 构建下 `catch_unwind` 无效。**helix-core 是库**——panic 策略由
     **最终二进制所在 workspace** 的 profile 决定，本仓库 Cargo.toml 的
     `[profile.release]`（及注释）**对被宿主嵌入编译的 helix-core 不生效**。因此防线
     分两级：① 本 workspace：Cargo.toml 注释钉死「不得设 panic=abort」（约束自家
     CLI / 测试构建，S3-06 的一部分）；② 嵌入宿主：**文档级约束**——架构 v1.8 的 R19
     残余表与 README（crate 文档首页）写明「宿主不得以 `panic=abort` 编译 helix-core，
     否则 R19 的 `catch_unwind` 防线静默失效」（S3-09 文档回写时落笔）。无编译期检测
     手段（库无法在编译期断言宿主 profile）。
  2. panic 会先经默认 panic hook 打印到 stderr，再进入我们的警告输出。库内不宜全局
     `set_hook`（污染宿主），接受这点噪音；文档声明。
  3. 读路径残余（校验通过后文件被并发改写仍可能 panic）**不在本 Step 范围**——
     那是「无并发写同一快照」语义的边界（C5），catch_unwind 对读路径无意义
     （读路径 panic 时快照已加载、不涉及落盘一致性）。

---

## 5. 决策记录（D-S3-01 ~ D-S3-07）

> 编号沿用项目惯例（Step 2 用 D-S2-xx）。以下每条 = §0 速读表的展开。
>
> **✅ 拍板（2026-09-07）**：D-S3-01~07 **全部按各节建议采纳**——本节结论即为终局，
> 各节内残留的「备选 / 可否决」讨论仅作决策过程存档，不再构成待办。

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

### D-S3-03 故障注入钩子：常驻 `pub(crate)` + 注入测试内迁（方案 C′，v0.2 修订）

完整对比见 §4.5 表格（v0.2 重构：v0.1 推荐的方案 B 经评审指出存在结构性矛盾——
私有模块里的 `#[doc(hidden)] pub static` 对 `tests/` 仍不可达，要可达就必须公开，
「零新增公开项」随之不成立；且注入测试 T3~T5/T9 本就要直接调 `pub(crate)` 的
`atomic_write`，集成层放不下）。C′ 要点：

1. `fault` 为 `pub(crate) mod`（常驻生产路径，不进公开面，也不 cfg 剔除——
   保证测试路径与发布路径逐位一致）；
2. 注入测试 T3~T6、T9 全部放 crate 内 `#[cfg(test)]`（`storage/atomic.rs` 单测承载
   T3~T5/T9，`search/index.rs` 单测承载端到端 T6——同样走完整公开 API）；
3. `tests/atomic_snapshot.rs` 只放**无注入**的公开 API 集成测试（save 原子性外观 +
   tmp 不残留 + 旧快照兼容回归）；
4. 并行互扰防护：注入测试共用测试内 `static Mutex` 串行 + guard 复位（§4.5 末）。

防泄漏仍保留 T9（`FAIL_AT == 0` 时 `atomic_write` 行为与改造前逐位一致，roundtrip
复用既有快照测试自动覆盖）。（存档）曾考虑 B′ 路线——若更看重「T6 必须是集成测试」
可公开 doc(hidden) 钩子，§6 公开口径改为「零新增**稳定**公开项，唯一例外 fault 钩子」；
拍板采 C′，B′ 不采。

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
  已有行为，仅随 D-S3-01 改名。⚠️ v0.2 核实限定：`remove_sidecars` **只在 save 路径**
  被调用（`index.rs:484/488/493/507`，load 从不调）⇒ manifest tmp 孤儿只被下一次
  save 回收；load-only 部署会一直留着（无害，tmp 永不权威）——验收 3 已按此拆分。

### D-S3-06 fsync 代价：接受、实测、不开逃生舱

- 52MB 快照的 `sync_all` 在普通 SSD 上量级约 50~300ms，HDD 上可达秒级。save 是
  **用户显式调用的低频操作**（CLI `build` / 宿主落库），不触碰任何 NFR 限额
  （NFR-02 查询 / NFR-03 构建 / NFR-04 冷启动都不含 save）。
- **不提供 `skip_fsync` 选项**：fsync 是正确性语义（§2.1 缺陷 2 的直接修复），
  做成开关等于把「要不要丢数据」交给调用方——与「不得静默读错」同一级别的
  非选项。`--no-graph-persist` 是性能逃生舱，管的是缓存；真源没有逃生舱。
- S3-08 在 12K 语料实测改造前后 `save` 耗时，**给范围**（三次以上取 min~max），
  写入 `eval-report.md` 新小节（§8.x，编号顺延）。

### D-S3-07（v0.2 新增）Strict 失败路径是否补 sidecar 清理

- **现状**（评审核实）：`write_graph_sidecar` 失败时，Lenient 分支（`index.rs:504-512`）
  best-effort `remove_sidecars` + 警告 + `PersistFailed`；**Strict 分支（`index.rs:503`）
  直接 `return Err(e)`，不清理**。R19 收敛后 Strict 下的 panic 同样走这条不清理的路径，
  残留：半截 `*.hnsw.graph`/`*.hnsw.data`（dump 先删旧图）+ 已失效的旧 manifest
  （其 CRC 锚已不匹配新快照，load 时图必降级重建）——**功能安全，目录留尸体**。
- **建议：补清理**。Strict 返回 `Err` 前加一次 best-effort `remove_sidecars`
  （清理自身失败不升级、不改变 Err 的类型与语义）。理由：与 P0-3「整个写图链路
  都按缓存语义处理」的精神一致——失败的缓存不留尸体；Strict 的语义是「失败上抛」，
  不是「失败且留垃圾」。
- **代价**：一次 best-effort 删除（几微秒），无语义风险（被删的都是垃圾：半截图
  必 CRC 失败、旧 manifest 的锚已死）。
- **✅ 拍板（2026-09-07）：采纳建议——补清理**（S3-06 落地）。（存档）曾留可否决
  口：现状功能上也安全（评审确认），若认为 Strict 应保持「最小动作、直接上抛」的
  纯粹性可维持现状——拍板不采此路线。

---

## 6. 影响面与兼容性

| 维度 | 影响 |
| --- | --- |
| 快照格式 | **零变化**（C3）：magic / `effective_version()` / CRC 布局 / bincode 编码全不动；`FORMAT_VERSION` 保持 2 |
| 公开 API | **零新增公开项**（D-S3-03 方案 C′，已拍板）：`atomic_write` / `tmp_path` / `fault` 均为 `pub(crate)`，注入测试内迁 crate 内 `#[cfg(test)]`——含 doc(hidden) 在内的**任何形式**都不新增公开项。`save_with_crc` / `save` / `load` / `write_manifest_atomic` 签名全部不变 |
| 行为变化 | ① save 落盘变成原子（对外可见的唯一变化：崩溃不再产生半截快照）；② save 变慢（fsync，量级见 D-S3-06 实测任务）；③ manifest tmp 中间文件名变化（双 hnsw → 追加式）；④ `hnsw_rs` dump panic 从「进程死」变为「警告 + PersistFailed」 |
| 旧快照兼容 | 旧内核写的快照（直写产物）就是完整文件，新代码照常加载；新内核写的快照旧内核照常加载——原子性不进格式 |
| 测试影响 | 既有快照 roundtrip / CRC 系列测试**零修改**应全绿（这是 S3-T2 的内容）；`原子写不留tmp残留`（graph.rs 单测）随 D-S3-01 改名自动适配 |
| CI | 无新 job、无新 feature 档（D-S3-03 方案 C′ 与 B′ 共同的核心收益） |
| `.gitignore` | +1 行 `*.manifest.tmp` |

---

## 7. 测试计划

> 编号 S3-Tn。带 ⚠️ 的是**必测**（直接对应验收）。
> **测试落点（v0.2，随 D-S3-03 方案 C′ 调整）**：注入类测试（T3~T6、T9）全部放
> crate 内 `#[cfg(test)]`——`storage/atomic.rs` 单测承载 T3~T5/T9（直接调
> `pub(crate)` 的 `atomic_write`），`search/index.rs` 单测承载端到端 T6（走完整公开
> API）；`tests/atomic_snapshot.rs`（集成层）只放**无注入**的公开 API 测试：
> save 原子性外观（写新→load 得新）、tmp 不残留、旧快照兼容回归。

| # | 测试 | 断言 |
| --- | --- | --- |
| **S3-T1** ⚠️ | `atomic_write` 基础 + tmp 不残留 | ① 首写 / 覆盖写 roundtrip；② 成功后目录中只有目标文件（无 tmp）；③ 目标路径被**同名目录**占据（C7：不用 chmod）→ `Err`，不 panic |
| **S3-T2** ⚠️ | 既有快照测试全绿（回归） | `snapshot.rs` 现有 6 个测试（roundtrip / 空 / CRC 损坏 / 截断 / with_crc / CRC 变化）**零修改**通过——证明格式与语义未漂移 |
| **S3-T3** ⚠️ | **注入 cp2（sync 后、rename 前，plan-v2 钦定点）** | `catch_unwind(atomic_write)` 后：① `foo.idx` 内容 == 旧值；② tmp 存在且内容 == 新值（但**永不权威**）；③ 随后 `load_with_crc` 成功且得旧内容；④ load 触发回收，tmp 消失 |
| **S3-T4** | 注入 cp1（写入后、flush 前） | 不变式与 T3 的 ①③④ 相同（cp1/cp2 同属 rename 前，真源均未动）；tmp **内容不定**（v0.2：注入 panic 的栈展开会令 `BufWriter::drop` 尽力刷缓冲，tmp 常反而完整；真实半截态无法进程内复现，见 §4.3 注 2）——只断言存在性与回收，**不断言内容** |
| **S3-T5** | 注入 cp3（rename 后、目录 fsync 前） | ① `foo.idx` 内容 == 新值；② `load_with_crc` 成功得新内容；③ 残留 tmp（若有）被 load 回收 |
| **S3-T6** ⚠️ | **端到端**（验收 1 的直接对应） | 真实 `SearchIndex`（小语料）：save 前注入各 checkpoint + `catch_unwind(save)` → 重新 `SearchIndex::load`：要么旧要么新，**绝不 `SnapshotCorrupted`**；且检索功能可用。**注入范围声明（v0.2）**：`FAIL_AT` 单发命中，端到端注入只覆盖**快照写窗口**（save 内首次 `atomic_write`）；图 dump / manifest 发布窗口（W2~W4）不重复注入——Step 2 语义已覆盖（性能退化窗口），manifest 侧 `atomic_write` 协议本身由 T3~T5 在同一实现上直接覆盖 |
| **S3-T7** ⚠️ | **R19：dump panic 不杀进程**（验收 4） | 单元层（`vector/persist.rs` `#[cfg(test)]`）：mock 一个 `dump_graph` 直接 panic 的 `VectorGraphPersist` 实现 → `dump_graph_caught` 返回 `Err(VectorGraph(含 panic 信息))`；接入门面层后 Lenient 语义 = 既有 S2-T19 断言（save Ok + `PersistFailed`） |
| **S3-T8** | manifest tmp 改名一致性 | `write_manifest_atomic` 成功 / 失败（同名目录占据）后，`remove_sidecars` 均不留 `*.manifest.tmp`（写、清两边用的是同一 `tmp_path`） |
| **S3-T9** | fault 钩子默认关闭 | `FAIL_AT == 0` 时 `atomic_write` 行为与 T1 完全一致（防钩子泄漏进生产语义） |
| **S3-T10** | fsync 代价实测（非 CI，本地） | 12K 语料：改造前后 `save` 耗时各测 ≥3 次取 min~max，入 `eval-report.md`（S3-08 交付） |

> 测试技法备忘（沿 Step 2 教训）：所有 `catch_unwind` 后**立即复位** `FAIL_AT`——用
> guard（`Drop` 里 store 回 0）而非裸 store，防断言失败路径泄漏到下一个测试；注入测试
> 共用一把测试内 `static Mutex<()>` 串行（`FAIL_AT` 是进程级全局，同二进制并行测试会
> 互扰，见 §4.5 末）；「写文件必失败」场景一律用同名目录占据，不用 chmod。

---

## 8. 实施任务拆分（S3-01 ~ S3-09）

> 量级 S（plan-v2）。默认**单个实现 PR**（S3-02~S3-08 代码 + 测试 + 文档回写同 PR）：
> Step 2 的教训是堆叠 PR 的坑（#13/#14 显示 MERGED 却未进 main），S 量级不值得拆。
> 如评审认为单 PR 过大，拆分线在 S3-05（代码 PR / 测试+文档 PR）。

| # | 任务 | 对应 | 依赖 | 量级 |
| --- | --- | --- | --- | --- |
| **S3-01** | 本文评审 + D-S3-01~07 拍板，回写本文状态（**✅ 已完成**：v0.1 → v0.2 评审回应 → v0.3 拍板，PR #26） | — | — | S |
| **S3-02** | `storage/atomic.rs`：`atomic_write`（写闭包签名）+ `tmp_path` + `fsync_dir` + `fault` 钩子 + 单测（T1 / T9） | S3-a | S3-01 | S |
| **S3-03** | `write_manifest_atomic` 改走 `atomic_write`（含 D-S3-01 改名）；`remove_sidecars` 的 tmp 清理同步换 `tmp_path`（T8） | S3-a | S3-02 | S |
| **S3-04** | `save_with_crc` 改走 `atomic_write`（S3-b）；`load_with_crc` 成功后孤儿回收（S3-c）；既有测试回归（T2） | S3-b/c | S3-02 | S |
| **S3-05** | 故障注入测试 T3~T6、T9（crate 内 `#[cfg(test)]`：`storage/atomic.rs` 承载 T3~T5/T9、`search/index.rs` 承载 T6；见 §7 落点说明）+ `tests/atomic_snapshot.rs` 无注入集成测试（save 原子性外观 / tmp 不残留 / 兼容回归） | S3-d | S3-04 | S |
| **S3-06** | R19：`dump_graph_caught`（catch_unwind + payload 降级）接入 `write_graph_sidecar`；Cargo.toml 钉「不得 panic=abort」注释（**仅约束本 workspace**，宿主约束走文档，见 §4.6）；Strict 失败路径补 best-effort `remove_sidecars`（**D-S3-07 已拍板**）；单测 T7 | S3-e | S3-03 | S |
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
| `catch_unwind` 在 panic=abort 构建下静默失效 | R19 防线失效且无人知晓 | **两级防线（v0.2）**：本 workspace——Cargo.toml 注释钉死；嵌入宿主——架构 R19 残余表 + README 声明「宿主不得以 panic=abort 编译 helix-core」（库自身 profile 对宿主构建不生效，只能文档级约束，S3-09 回写）。无编译期检测手段 | 接受（文档级防线） |
| `fault` 钩子被误用 | 非预期 panic | C′ 下 `pub(crate)`——**外部根本不可达**（v0.2：比 v0.1 的 doc(hidden) 公开方案少一整类风险）；T9 默认值断言；panic 是「失败可见」不是「静默错数据」 | 接受 |
| 注入测试并行互扰（`FAIL_AT` 进程级全局） | 偶发 flaky：无辜测试被误杀 | 注入测试共用 static Mutex 串行 + guard 复位（§4.5 末 / §7 备忘） | — |
| Windows 上目录 fsync 跳过 | rename 目录项持久性弱一层 | C6 既有事实（Step 2 同款）；不变式仍是「旧或新」 | 接受 |
| 快照写闭包内 `codec::write_header` 失败的中间态 | tmp 内容不定（非权威） | 不变式覆盖（rename 前真源未动）；不对 tmp 内容断言（§4.3 注 2）；无专门测试 | — |
| Strict 失败残留半截图文件（R19 收敛后 panic 也走此路径） | 目录留垃圾到下次成功 save；无正确性影响 | D-S3-07 已拍板：Strict 返回 Err 前补 best-effort `remove_sidecars`（S3-06 落地） | 随实现销账 |

**实现期待定项**（v0.3：拍板已闭环，以下三项均为实现时的工程选择，不再阻塞开工）：

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

/// ⚠️ 仅供测试（pub(crate)，不进公开面——D-S3-03 方案 C′，v0.2）
pub(crate) mod fault {
    pub(crate) static FAIL_AT: AtomicU8;          // 0 = 不注入（默认）
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
| tmp | 存在（**内容不定**，非权威——不断言完整度）→ load 后被回收 | 可能存在 → load 后被回收 |
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
| Strict 分支失败不清理 sidecar | `crates/core/src/search/index.rs:503`（Lenient 清理在 `:504-512`；v0.2 评审核实） |
| `remove_sidecars` 仅 save 路径调用 | `crates/core/src/search/index.rs:484,488,493,507`（load 路径零调用点，v0.2 评审核实） |
| storage 子模块私有 + 选择性 `pub use` | `crates/core/src/storage/mod.rs:15-25`（v0.2 评审核实：私有 `mod atomic` 内的钩子对 `tests/` 不可达） |
| S3-a~e 子任务定义 | `docs/devel/plan-v2.md` §4 Step 3 |
