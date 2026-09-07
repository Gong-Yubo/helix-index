//! 原子写原语：tmp → 写入 → flush → sync_all → rename → fsync 父目录
//! （V2 Step 3 / D-J3，v2-step3-design §4.1，D-S3-01~03）。
//!
//! # 单一实现（D-S3-02）
//!
//! 快照（`snapshot.rs`）与图 manifest（`graph.rs`）共用本模块——两份独立实现
//! 迟早漂移，而漂移的那一份就是下一个 Q-C3。落点在 `atomic.rs` 而非 `graph.rs`：
//! 依赖方向应为 `snapshot.rs / graph.rs → atomic.rs`，塞进 graph.rs 会让快照写入
//! 反向依赖图的模块语义。
//!
//! # 语义保证
//!
//! 本函数返回 `Ok` 后，`path` 要么是旧内容（本函数失败时——rename 未发生），
//! 要么是新内容且已尽力持久化（文件 + 父目录均 fsync）。
//! 失败时 tmp 可能残留（崩溃或写失败），由回收机制处理（设计 §4.4）：
//! save 侧 `File::create` 截断复用，load 侧 best-effort 删除。
//!
//! # tmp 命名（D-S3-01）
//!
//! tmp 路径 = **目标路径 + `.tmp` 追加**（[`tmp_path`]），不用 `with_extension`——
//! 后者对 `foo.idx.hnsw.manifest` 会拼出 `foo.idx.hnsw.hnsw.manifest.tmp` 双后缀怪名。
//!
//! # 故障注入（D-S3-03 方案 C′）
//!
//! [`fault`] 钩子**常驻生产路径**（`pub(crate)`，不进公开面），保证测试路径与
//! 发布路径逐位一致；`FAIL_AT == 0`（默认）时为纯 no-op。注入测试只放 crate 内
//! `#[cfg(test)]`——本模块私有 ⇒ 集成层 `tests/` 本就不可达，公开面真零新增。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// tmp 路径 = 目标路径 + `.tmp`（**追加**，D-S3-01）。
///
/// `foo.idx` → `foo.idx.tmp`；`foo.idx.hnsw.manifest` → `foo.idx.hnsw.manifest.tmp`。
/// 与 `.gitignore` 既有 `*.idx.tmp` 模式天然对齐（manifest tmp 另补 `*.manifest.tmp`）。
pub(crate) fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".tmp");
    PathBuf::from(s)
}

/// 原子写：tmp → 写入 → flush → `sync_all` → rename → fsync 父目录（D-J3）。
///
/// `write` 闭包把内容写进 tmp（快照：header+正文；manifest：magic+版本+CRC+正文）。
/// 收闭包而非 `&[u8]`：快照正文 52MB 级，`&[u8]` 签名逼调用方拼整包要多一次
/// 全量拷贝（D-S3-02）；两个调用方恰好都是「小 header + 大 body」形态。
///
/// # 步骤与失败语义
///
/// 1. `File::create(tmp)`——**截断复用**：上次崩溃的同名 tmp 孤儿由此天然回收
///    （D-S3-05：无须先删，显式删反而引入「删了又建不出来」的空窗）；
/// 2. `write(&mut w)`——内容进 tmp（此时尚未 flush，仅在进程内/页缓存）；
/// 3. `flush` + `sync_all`——数据落盘（不是只到页缓存；否则 rename 可能把
///    从未持久化的文件变成真源，掉电后得到「存在但半截」的文件）；
/// 4. `drop(w)` 显式关句柄——Windows rename 前必须关句柄；抽取后统一为显式
///    `drop`，防止未来有人在闭包外挪动作用域（现状 manifest 用块作用域）；
/// 5. `rename`——**唯一发布点**：在此之前 `path` 从未被触碰（不变式的根基）；
/// 6. `fsync_dir`——让 rename 本身在掉电后持久；best-effort（Windows / 网络
///    文件系统不支持则跳过，错误吞掉）：rename 已发生，报 Err 反而会让调用方
///    误以为快照没写成。
///
/// 注入点（[`fault::checkpoint`]）：1 = 写入后 / 2 = sync 后 / 3 = rename 后。
pub(crate) fn atomic_write(
    path: &Path,
    write: impl FnOnce(&mut BufWriter<File>) -> std::io::Result<()>,
) -> Result<()> {
    let tmp = tmp_path(path);
    let file = File::create(&tmp).map_err(Error::Io)?;
    let mut w = BufWriter::new(file);
    write(&mut w).map_err(Error::Io)?;
    fault::checkpoint(1); // 注入点：写入后、flush 前
    w.flush().map_err(Error::Io)?;
    w.get_ref().sync_all().map_err(Error::Io)?;
    fault::checkpoint(2); // 注入点：sync 后、rename 前
    drop(w); // Windows：rename 前必须关句柄
    std::fs::rename(&tmp, path).map_err(Error::Io)?; // 唯一发布点
    fault::checkpoint(3); // 注入点：rename 后、目录 fsync 前
    fsync_dir(path.parent());
    Ok(())
}

/// fsync 父目录，best-effort（错误吞掉）。
///
/// POSIX 上 `fsync` 目录需要 `File::open(dir)` + `sync_all`；Windows 上
/// `File::open` 目录会失败 → 静默跳过（C6，现状 `write_manifest_atomic` 同款）。
/// 目录 fsync 是尽力而为的加固，失败不构成 `save` 失败。
fn fsync_dir(dir: Option<&Path>) {
    if let Some(dir) = dir {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
}

/// 故障注入钩子（D-S3-03 方案 C′：常驻 `pub(crate)`，不进公开面）。
///
/// ⚠️ 性能代价声明：`checkpoint` 在**生产路径**无条件执行，每次 [`atomic_write`]
/// 共 3 次 relaxed 原子读（`FAIL_AT == 0` 时为纯 no-op，线程本地 `ARMED` 仅在
/// `FAIL_AT` 非零时才被触碰——生产路径永不发生）。相对 52MB 级 fsync 可忽略，
/// **后续优化不得以此为由移除钩子**——移除等于抽掉 S3-T3~T6 全部注入测试的地基。
pub(crate) mod fault {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU8, Ordering};

    /// 0 = 不注入（默认）。1..=3 = 在 `atomic_write` 的第 N 个 checkpoint panic。
    /// checkpoint 语义见 [`atomic_write`] 内标注（写入后 / sync 后 / rename 后）。
    ///
    /// 单发语义：store 一次只命中**第一个**走到匹配 checkpoint 的调用——
    /// `save` 里 `atomic_write` 被调两次（快照、manifest），注入必先落在快照那次。
    pub(crate) static FAIL_AT: AtomicU8 = AtomicU8::new(0);

    thread_local! {
        /// 当前线程是否允许注入命中（测试 opt-in 硬化）。
        ///
        /// 设计 §4.5 的对策是注入测试共用一把 Mutex 串行；但这只隔离了注入测试
        /// 彼此——同二进制里**不持锁**的其他单测（快照 roundtrip、manifest 系列）
        /// 同样会路过 checkpoint，撞上持有期间的非零 `FAIL_AT` 会被误杀（flaky）。
        /// 线程本地 opt-in 把互扰彻底归零：checkpoint 先查 `FAIL_AT`（生产路径
        /// 恒 0，短路），非零时再要求当前线程已 `arm`——无辜线程绝不会被误杀。
        static ARMED: Cell<bool> = const { Cell::new(false) };
    }

    /// 注入点。`FAIL_AT` 非零且当前线程已 arm 时 panic。
    pub(crate) fn checkpoint(n: u8) {
        if FAIL_AT.load(Ordering::Relaxed) == n && ARMED.with(|a| a.get()) {
            panic!("fault-inject: atomic_write checkpoint {n}");
        }
    }

    /// 注入测试的串行化 + 复位 guard（§4.5 末的互扰防护，落地为随用随取）。
    ///
    /// - `acquire()`：拿全 crate 唯一的注入锁（所有注入测试共用一把，串行执行）；
    /// - `arm(n)`：当前线程 opt-in + 设 `FAIL_AT`；
    /// - `Drop`：**无论测试成败**都把 `FAIL_AT` 复位为 0、撤销 opt-in——
    ///   用 guard 而非裸 store，防断言失败路径泄漏到下一个测试（§7 备忘）。
    #[cfg(test)]
    pub(crate) struct InjectionGuard(
        // 持有即语义（串行化），字段本身从不读取——dead_code 误报
        #[allow(dead_code)] std::sync::MutexGuard<'static, ()>,
    );

    #[cfg(test)]
    impl InjectionGuard {
        /// 取注入锁（阻塞直到其他注入测试完成）。锁中毒时穿透继续——
        /// 前一个注入测试 panic 本身就是预期行为，不能让中毒放大成死锁。
        pub(crate) fn acquire() -> Self {
            static MTX: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
            let m = MTX.get_or_init(|| std::sync::Mutex::new(()));
            Self(m.lock().unwrap_or_else(|e| e.into_inner()))
        }

        /// 当前线程 opt-in 注入并设置崩溃点。
        pub(crate) fn arm(&self, n: u8) {
            ARMED.with(|a| a.set(true));
            FAIL_AT.store(n, Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    impl Drop for InjectionGuard {
        fn drop(&mut self) {
            FAIL_AT.store(0, Ordering::Relaxed);
            ARMED.with(|a| a.set(false));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;

    /// S3-T1①：首写 / 覆盖写 roundtrip。
    #[test]
    fn 基础roundtrip与覆盖写() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.idx");

        atomic_write(&path, |w| w.write_all(b"old")).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"old");

        atomic_write(&path, |w| w.write_all(b"new")).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    /// S3-T1②：成功后目录中只有目标文件（tmp 被 rename 消费，不残留）。
    #[test]
    fn 成功后无tmp残留() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.idx");
        atomic_write(&path, |w| w.write_all(b"data")).unwrap();
        atomic_write(&path, |w| w.write_all(b"data2")).unwrap(); // 覆盖写一遍

        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1, "目录里只应有目标文件");
        assert_eq!(
            entries[0].as_ref().unwrap().file_name(),
            path.file_name().unwrap()
        );
    }

    /// S3-T1③：目标路径被**同名目录**占据 → `Err`，不 panic
    /// （C7：CI 以 root 运行时 chmod 权限位无效，一律用同名目录造失败）。
    #[test]
    fn 目标被同名目录占据时报错() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.idx");
        std::fs::create_dir(&path).unwrap(); // 同名目录占据 rename 的落点

        let r = atomic_write(&path, |w| w.write_all(b"data"));
        assert!(r.is_err(), "rename 到目录上必须失败");
        assert!(
            tmp_path(&path).exists(),
            "失败时 tmp 允许残留（由回收机制处理）"
        );
    }

    /// S3-T9：`FAIL_AT == 0`（默认）时行为与 T1 完全一致——防钩子泄漏进生产语义。
    #[test]
    fn 默认不注入行为一致_T9() {
        assert_eq!(fault::FAIL_AT.load(std::sync::atomic::Ordering::Relaxed), 0);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t9.idx");
        atomic_write(&path, |w| w.write_all(b"plain")).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"plain");
        assert!(!tmp_path(&path).exists());
    }

    /// S3-T3 ⚠️：注入 cp2（sync 后、rename 前，plan-v2 钦定点）。
    ///
    /// 不变式：rename 之前真源从未被触碰。tmp 在 sync 后内容完整，
    /// 但**永不权威**（原子协议：tmp 没有任何被读取的路径）。
    #[test]
    fn 注入cp2真源不动tmp是孤儿_T3() {
        let g = fault::InjectionGuard::acquire();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t3.idx");
        atomic_write(&path, |w| w.write_all(b"old")).unwrap();

        g.arm(2);
        let r = std::panic::catch_unwind(|| atomic_write(&path, |w| w.write_all(b"new")));
        drop(g); // 无论成败都复位（防泄漏）

        assert!(r.is_err(), "cp2 注入应 panic");
        assert_eq!(std::fs::read(&path).unwrap(), b"old", "rename 前真源未动");
        let tmp = tmp_path(&path);
        assert!(tmp.exists(), "rename 未发生，tmp 必残留");
        assert_eq!(
            std::fs::read(&tmp).unwrap(),
            b"new",
            "cp2 在 sync_all 之后：tmp 内容完整（但永不权威）"
        );

        // 回收路径 A（save 侧）：下一次成功写截断复用，孤儿 tmp 被消费
        atomic_write(&path, |w| w.write_all(b"old2")).unwrap();
        assert!(!tmp.exists(), "成功写后同名 tmp 应被 rename 消费");
        assert_eq!(std::fs::read(&path).unwrap(), b"old2");
    }

    /// S3-T4：注入 cp1（写入后、flush 前）。不变式与 T3 相同（rename 前真源未动）。
    /// tmp **内容不定**（注入 panic 的栈展开会令 `BufWriter::drop` 尽力刷缓冲，
    /// tmp 常反而完整；真实半截态只有 kill / 掉电能产生，进程内造不出来——
    /// 设计 §4.3 注 2）：只断言存在性与回收，不断言内容。
    #[test]
    fn 注入cp1真源不动_T4() {
        let g = fault::InjectionGuard::acquire();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t4.idx");
        atomic_write(&path, |w| w.write_all(b"old")).unwrap();

        g.arm(1);
        let r = std::panic::catch_unwind(|| atomic_write(&path, |w| w.write_all(b"new")));
        drop(g);

        assert!(r.is_err(), "cp1 注入应 panic");
        assert_eq!(std::fs::read(&path).unwrap(), b"old", "rename 前真源未动");
        assert!(tmp_path(&path).exists(), "rename 未发生，tmp 必残留");
    }

    /// S3-T5：注入 cp3（rename 后、目录 fsync 前）。真源已是新值且数据已 sync。
    #[test]
    fn 注入cp3真源已是新值_T5() {
        let g = fault::InjectionGuard::acquire();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t5.idx");
        atomic_write(&path, |w| w.write_all(b"old")).unwrap();

        g.arm(3);
        let r = std::panic::catch_unwind(|| atomic_write(&path, |w| w.write_all(b"new")));
        drop(g);

        assert!(r.is_err(), "cp3 注入应 panic");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"new",
            "rename 已发生：真源为新值（进程崩溃态；掉电回滚态见设计 §4.3 注 1）"
        );
        assert!(!tmp_path(&path).exists(), "rename 已消费本 save 的 tmp");
    }
}
