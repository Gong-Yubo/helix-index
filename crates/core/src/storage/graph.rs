//! 图 sidecar 清单：`GraphManifest` + 原子写 + CRC 校验（V2 Step 2 / ADR-A 方案 C）。
//!
//! # 地位（v2-step2-design §4.2）
//!
//! **图是快照的派生缓存，不是真源**。`foo.idx` 是唯一真源；
//! `foo.idx.hnsw.graph` / `.hnsw.data` 是 `hnsw_rs` 原生格式的派生文件；
//! **`foo.idx.hnsw.manifest` 是唯一原子发布点**——只有它（tmp+fsync+rename）
//! 落地的那一刻，图才算「发布」。manifest 记录父快照 CRC 与两个图文件的
//! CRC，「要么全对、要么全不算」的语义由此成立。
//!
//! # basename 铁律（P0-1）
//!
//! 传给 `file_dump` / `HnswIo::new` 的 basename 是**快照文件名全名**
//! （如 `"foo.idx"`）。`hnsw_rs` 自行追加 `.hnsw.graph` / `.hnsw.data`；
//! 我们再拼一次会得到 `foo.idx.hnsw.hnsw.graph`——降级路径一切正常、
//! 所有「能加载」断言照样绿，只有 NFR-04 静默失效。全 crate **禁止手拼**，
//! 一律走 [`graph_basename`]。
//!
//! 本模块**不依赖 `hnsw_rs`**（便于单测）；唯一 import hnsw_rs 持久化 API
//! 的地方是 `vector/persist.rs`。

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::atomic::{atomic_write, tmp_path};
use crate::error::{Error, Result};

/// manifest 魔数。变更即视为不兼容格式（读取侧拒绝并降级）。
const MAGIC_GMAN: &[u8; 8] = b"GMAN\x01\x00\x00\x00";

/// manifest header 长度：8B magic + 4B manifest_version + 4B crc32。
const MANIFEST_HEADER_LEN: usize = 16;

/// 当前 manifest 格式版本。未来字段变更时 +1，读取侧对未知版本降级（S2-T15）。
pub const MANIFEST_VERSION: u32 = 1;

/// 自有距离标识（C7 / R20）：与 Rust 类型路径解耦，语义变更时显式升版。
pub const DIST_ID: &str = "dot-clamped-v1";

/// 平台指纹取值之一：**little-endian + 64-bit 指针宽**。
pub const PLATFORM_LE64: u32 = 0x01;
/// 平台指纹取值之一：其他组合（大端 / 32-bit 指针宽）。
pub const PLATFORM_OTHER: u32 = 0x02;

/// 当前构建的平台指纹（C4）：裸 f32 + 原生端序 ⇒ 图 sidecar 不可跨平台搬运。
///
/// ⚠️ **必须由构建目标派生，不能硬编码**。`hnsw_rs` 落盘用 `to_ne_bytes()`（原生端序），
/// 并且用 `from_raw_parts` 裸拷贝 f32，图文件本身就是 native-endian。若这里写死
/// `PLATFORM_LE64`，大端 / 32-bit 构建**同样写 0x01、同样接受 0x01** ⇒ CRC 全对、
/// platform 相符，却加载出字节序错误的向量，得到**静默错误的检索结果**——
/// 正是本 PR 要消灭的失败模式（评审 #12 发现 1：`cfg!` 在 const 上下文可用，代价为零）。
pub const PLATFORM_FINGERPRINT: u32 =
    if cfg!(target_endian = "little") && cfg!(target_pointer_width = "64") {
        PLATFORM_LE64
    } else {
        PLATFORM_OTHER
    };

/// `hnsw_rs` 自行追加的图拓扑文件后缀（仅用于**拼路径**，禁止拼进 basename）。
pub const GRAPH_SUFFIX: &str = "hnsw.graph";
/// `hnsw_rs` 自行追加的图数据文件后缀（同上）。
pub const DATA_SUFFIX: &str = "hnsw.data";
/// 我们自己的 manifest 后缀（唯一原子发布点）。
pub const MANIFEST_SUFFIX: &str = "hnsw.manifest";

/// 图 sidecar 三件套的路径。
#[derive(Debug, Clone)]
pub struct GraphPaths {
    /// 图拓扑文件（`foo.idx.hnsw.graph`）
    pub graph: PathBuf,
    /// 图数据文件（`foo.idx.hnsw.data`，含向量副本）
    pub data: PathBuf,
    /// 清单（`foo.idx.hnsw.manifest`，唯一原子发布点）
    pub manifest: PathBuf,
}

/// 传给 `file_dump` / `HnswIo::new` 的 basename——**快照文件名全名**，不追加任何后缀。
///
/// 例：`/data/foo.idx` → `"foo.idx"`（不是 `"foo.idx.hnsw"`！）。
/// 这是 P0-1 抓到的双后缀 bug 的唯一防线，全 crate 禁止手拼。
/// 由快照路径推算 basename（`"foo.idx"`——**不追加任何后缀**，P0-1）。
///
/// ⚠️ 调用方必须保证 `snapshot` 有文件名：拿不到时无法凭空造一个，
/// 否则 sidecar 会写到 `dir/index.idx.hnsw.*` 这种与快照无关的位置。
/// 两个入口（`SearchIndex::save` / 读侧 `try_load_graph`）已统一拦截。
pub fn graph_basename(snapshot: &Path) -> Option<String> {
    snapshot
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// 快照路径必须有文件名——否则图 sidecar 无处安放（评审 #12 nit）。
///
/// 写侧与读侧共用，避免 `graph_basename` 静默回退成一个拼错的 basename。
pub fn require_file_name(snapshot: &Path) -> Result<()> {
    if snapshot.file_name().is_none() {
        return Err(Error::VectorGraph(format!(
            "快照路径缺少文件名，无法定位图 sidecar: {}",
            snapshot.display()
        )));
    }
    Ok(())
}

/// 由快照路径推算 sidecar 三件套路径。
pub fn graph_paths(snapshot: &Path) -> GraphPaths {
    let dir = snapshot.parent().unwrap_or_else(|| Path::new("."));
    let base = graph_basename(snapshot).unwrap_or_default();
    GraphPaths {
        graph: dir.join(format!("{base}.{GRAPH_SUFFIX}")),
        data: dir.join(format!("{base}.{DATA_SUFFIX}")),
        manifest: dir.join(format!("{base}.{MANIFEST_SUFFIX}")),
    }
}

/// 图 sidecar 清单（v2-step2-design §4.4）。
///
/// **唯一原子发布点**：只有它落地的那一刻，图才算「发布」。
/// 校验失败的任何一行都意味着「降级重建」（图是缓存，丢弃不丢功能）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphManifest {
    // ── 自我描述 ──
    /// 清单格式版本（当前 1；未知版本 → 降级）
    pub manifest_version: u32,
    /// 生产者标识（诊断用，不参与校验）
    pub producer: String,

    // ── 与内核实现的绑定（校验失败即降级）──
    /// `Description.format_version` 的**读回值**（P0-2/N1：落盘 magic 实为
    /// MAGICDESCR_4，勿硬编码任何字面量——dump 后用 `load_description` 读回）
    pub graph_format: u32,
    /// 自有距离标识（C7：`type_name` 被烧进图文件，改名即失效 ⇒ 用自有 id 解耦）
    pub dist_id: String,
    /// 平台指纹（C4：裸 f32 + native endian）：0x01 = LE/64bit
    pub platform: u32,

    // ── 建图参数（P1-3：不在配置指纹里，改常量后旧图会静默错配）──
    /// `Description.max_nb_connection`（内核常量 M=32）
    pub max_nb_connection: u32,
    /// `Description.ef`（内核常量 EF_CONSTRUCTION=300）
    pub ef_construction: u32,

    // ── 与快照的绑定（核心：图的「版本锚点」）──
    /// 父快照正文的 CRC32——save 写出新快照后旧图立即失效（CRC 变了），
    /// 不需要 generation 计数器，也不需要 GC
    pub snapshot_crc: u32,
    /// 父快照正文长度（防同 CRC 不同长度的极端碰撞）
    pub snapshot_len: u64,
    /// 向量维度（必须等于 ConfigFingerprint.dim）
    pub dim: u32,
    /// 图中点数（**含墓碑**，故 >= 快照 vectors 条数：hnsw_rs 无 remove API）
    pub nb_point: u64,

    // ── 两个图文件的完整性 ──
    /// `.hnsw.graph` 的 CRC32
    pub graph_crc: u32,
    /// `.hnsw.graph` 的字节长度
    pub graph_len: u64,
    /// `.hnsw.data` 的 CRC32
    pub data_crc: u32,
    /// `.hnsw.data` 的字节长度
    pub data_len: u64,
}

/// 流式计算文件的 CRC32 与长度（不整读进内存——图文件可达数十 MB）。
pub fn file_crc32_len(path: &Path) -> Result<(u32, u64)> {
    let file = File::open(path).map_err(Error::Io)?;
    let mut r = BufReader::new(file);
    let mut hasher = crc32fast::Hasher::new();
    let mut len: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = r.read(&mut buf).map_err(Error::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        len += n as u64;
    }
    Ok((hasher.finalize(), len))
}

/// 读 manifest。文件不存在 → `Ok(None)`（旧快照 / 图被删，走降级重建）；
/// 文件在但 header / CRC / 解码错 → `Ok(None)`（**不是 Err**——图是缓存，
/// 坏了就降级，不阻塞快照本身的加载。与 §4.6 步骤 3 一致）。
pub fn read_manifest(path: &Path) -> Result<Option<GraphManifest>> {
    let Ok(file) = File::open(path) else {
        return Ok(None);
    };
    let mut r = BufReader::new(file);

    // header：magic + manifest_version + crc32（全部小端）
    let mut header = [0u8; MANIFEST_HEADER_LEN];
    if r.read_exact(&mut header).is_err() {
        return Ok(None); // 截断 → 降级
    }
    if &header[0..8] != MAGIC_GMAN {
        return Ok(None);
    }
    // header 版本**先比对再解码**（评审 #12 发现 4）：bincode 2 的
    // `decode_from_slice` 会忽略尾部剩余字节，未来版本若重排/前置字段，
    // v1 读取方会先解出垃圾再撞上 body 里的 `manifest_version` 检查——
    // 结果仍是降级，但那是靠运气。header 里就有一份版本，直接用它。
    let version = u32::from_le_bytes(header[8..12].try_into().expect("4 字节"));
    if version != MANIFEST_VERSION {
        return Ok(None);
    }
    let expected_crc = u32::from_le_bytes(header[12..16].try_into().expect("4 字节"));

    // 正文 + CRC 校验（绝不静默读错）
    let mut body = Vec::new();
    if r.read_to_end(&mut body).is_err() {
        return Ok(None);
    }
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&body);
    if hasher.finalize() != expected_crc {
        return Ok(None);
    }

    // 解码失败（含未知字段的 bincode 错误）→ 降级
    Ok(
        bincode::serde::decode_from_slice(&body, bincode::config::standard())
            .ok()
            .map(|(m, _): (GraphManifest, _)| m),
    )
}

/// 原子写 manifest（V2 Step 3 起委托私有原语 `atomic_write`）：
/// tmp → flush → sync_all → rename → **fsync 父目录**（P1-4）。
///
/// rename 是唯一原子发布点；fsync 父目录保证掉电后 rename 本身持久
/// （只 fsync 文件不 fsync 目录，rename 可能不落地）。Step 3 把这段机制
/// 抽成 `atomic_write` 通用原语后，manifest 与快照共用一份实现——「共用」
/// 不是美观诉求：两份独立实现迟早漂移，漂移的那一份就是下一个 Q-C3。
/// 公开签名不变。
pub fn write_manifest_atomic(path: &Path, m: &GraphManifest) -> Result<()> {
    let body =
        bincode::serde::encode_to_vec(m, bincode::config::standard()).map_err(Error::Codec)?;

    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&body);
    let crc = hasher.finalize();

    // D-S3-01：tmp 名由 `atomic_write` 统一为「目标路径 + .tmp 追加」——
    // 取代旧 `with_extension("hnsw.manifest.tmp")` 拼出的
    // `foo.idx.hnsw.hnsw.manifest.tmp`（双 `hnsw`）怪名。
    atomic_write(path, |w| {
        w.write_all(MAGIC_GMAN)?;
        w.write_all(&MANIFEST_VERSION.to_le_bytes())?;
        w.write_all(&crc.to_le_bytes())?;
        w.write_all(&body)?;
        Ok(())
    })
}

/// 删除快照旁的全部 sidecar（图 + data + manifest + 可能的 tmp 残留）。
///
/// 用途：① 无向量 / 非 Hnsw 后端 save 时清理僵尸文件；② dump 前删旧图
/// （N2：重载图的 `datamap_opt=true` 使 `file_dump` 拒绝覆盖，必须先删）。
/// 图是缓存：删除最坏导致下次冷启动降级重建，不会损坏索引。
pub fn remove_sidecars(snapshot: &Path) -> Result<()> {
    let GraphPaths {
        graph,
        data,
        manifest,
    } = graph_paths(snapshot);
    // tmp 残留（rename 前崩溃）尽力清理，失败不阻塞。
    // D-S3-01：与写侧 `write_manifest_atomic` 用同一 `tmp_path` 拼法——
    // 两边必须一次改齐（漏一边 = 清理失效，S3-T8 兜底）。
    let tmp = tmp_path(&manifest);
    let _ = std::fs::remove_file(&tmp);
    // NotFound 是常态（本就没有 sidecar），忽略之；其他错误（权限等）上抛
    for p in [graph, data, manifest] {
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;

    fn sample() -> GraphManifest {
        GraphManifest {
            manifest_version: MANIFEST_VERSION,
            producer: "helix-core-test".to_string(),
            graph_format: 4, // N1：读回值（落盘 magic 是 MAGICDESCR_4）
            dist_id: DIST_ID.to_string(),
            platform: PLATFORM_FINGERPRINT,
            max_nb_connection: 32,
            ef_construction: 300,
            snapshot_crc: 0xDEAD_BEEF,
            snapshot_len: 123_456,
            dim: 512,
            nb_point: 12_000,
            graph_crc: 1,
            graph_len: 8_300_000,
            data_crc: 2,
            data_len: 24_800_000,
        }
    }

    #[test]
    fn manifest_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foo.idx.hnsw.manifest");
        write_manifest_atomic(&path, &sample()).unwrap();
        let got = read_manifest(&path).unwrap().expect("应能读回");
        assert_eq!(got, sample());
    }

    #[test]
    fn manifest缺失返回None() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("none.idx.hnsw.manifest");
        assert!(read_manifest(&path).unwrap().is_none());
    }

    #[test]
    fn manifest篡改一字节降级() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.idx.hnsw.manifest");
        write_manifest_atomic(&path, &sample()).unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        // CRC 失败 → Ok(None)（降级），不是 Err
        assert!(read_manifest(&path).unwrap().is_none());
    }

    #[test]
    fn manifest截断降级() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trunc.idx.hnsw.manifest");
        write_manifest_atomic(&path, &sample()).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..MANIFEST_HEADER_LEN]).unwrap();
        assert!(read_manifest(&path).unwrap().is_none());
    }

    #[test]
    fn magic篡改降级() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("magic.idx.hnsw.manifest");
        write_manifest_atomic(&path, &sample()).unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] = b'X';
        std::fs::write(&path, &bytes).unwrap();
        assert!(read_manifest(&path).unwrap().is_none());
    }

    #[test]
    fn 原子写不留tmp残留() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("foo.idx.hnsw.manifest");
        write_manifest_atomic(&path, &sample()).unwrap();
        write_manifest_atomic(&path, &sample()).unwrap(); // 覆盖写一遍

        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1, "目录里只应有 manifest 一个文件");
    }

    /// S3-T8：**写侧与清侧 tmp 命名一次改齐**的测试兜底（D-S3-01 命门）。
    ///
    /// 手法同 C7（CI root 下 chmod 无效）：manifest 落点被**同名目录**占据令
    /// `write_manifest_atomic` 失败 → tmp 残留 → `remove_sidecars`（清侧）必须用
    /// 同一 `tmp_path` 拼法才能清掉它。若未来任一侧改了拼法而另一侧没跟上，
    /// 本测试必红——而不是静默留下永远清不掉的孤儿。
    #[test]
    fn manifest写失败后remove_sidecars清掉tmp_T8() {
        let dir = tempfile::tempdir().unwrap();
        let snap = dir.path().join("t8.idx");
        let manifest = graph_paths(&snap).manifest;
        std::fs::create_dir(&manifest).unwrap(); // 同名目录占据 rename 落点

        let r = write_manifest_atomic(&manifest, &sample());
        assert!(r.is_err(), "rename 到目录上必须失败");
        let tmp = tmp_path(&manifest);
        assert!(tmp.exists(), "写失败时 manifest tmp 应残留（回收对象）");

        // 清理顺序保证：tmp 在三件套循环**之前**删——即便 manifest 位被目录占据
        // 使 remove_file 上抛（macOS EPERM / Linux EISDIR，异常态如实报错），
        // tmp 也已被清掉。故这里容忍 Err、只断言 tmp 消失（本测试只兜命名对齐）。
        let _ = remove_sidecars(&snap);
        assert!(!tmp.exists(), "清侧必须用同一 tmp_path 拼法，否则清理失效");
    }

    /// P0-1 的核心防回归：basename 必须是快照**文件名全名**。
    #[test]
    fn basename是快照全名() {
        assert_eq!(
            graph_basename(Path::new("/data/foo.idx")),
            Some("foo.idx".to_string())
        );
        assert_eq!(
            graph_basename(Path::new("foo.idx")),
            Some("foo.idx".to_string())
        );
        // ⚠️ 绝不能返回 "foo.idx.hnsw"
        assert_ne!(
            graph_basename(Path::new("/data/foo.idx")),
            Some("foo.idx.hnsw".to_string())
        );
        // 无文件名时**不回退**假 basename（评审 #12 nit）——交由入口报错
        assert_eq!(graph_basename(Path::new("/")), None);
        assert_eq!(graph_basename(Path::new("..")), None);
        assert!(require_file_name(Path::new("/")).is_err());
        assert!(require_file_name(Path::new("/data/foo.idx")).is_ok());
    }

    #[test]
    fn 三件套路径拼装() {
        let p = graph_paths(Path::new("/data/foo.idx"));
        assert_eq!(p.graph, PathBuf::from("/data/foo.idx.hnsw.graph"));
        assert_eq!(p.data, PathBuf::from("/data/foo.idx.hnsw.data"));
        assert_eq!(p.manifest, PathBuf::from("/data/foo.idx.hnsw.manifest"));
        // 双后缀防回归：不允许出现 hnsw.hnsw
        assert!(!p.graph.to_string_lossy().contains("hnsw.hnsw"));
    }

    #[test]
    fn 相对路径与无父目录() {
        // 无目录前缀时 parent() 得到空路径，join 后仍是相对名（行为与实现一致）
        let p = graph_paths(Path::new("foo.idx"));
        assert_eq!(p.graph, PathBuf::from("foo.idx.hnsw.graph"));
        assert_eq!(
            graph_basename(Path::new("foo.idx")),
            Some("foo.idx".to_string())
        );
    }

    #[test]
    fn file_crc32_len流式正确() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let (crc, len) = file_crc32_len(&path).unwrap();
        assert_eq!(len, 200_000);

        // 与 crc32fast 一次性计算对齐
        let mut h = crc32fast::Hasher::new();
        h.update(&data);
        assert_eq!(crc, h.finalize());
    }

    #[test]
    fn remove_sidecars容忍不存在() {
        let dir = tempfile::tempdir().unwrap();
        let snap = dir.path().join("foo.idx");
        // 没有任何 sidecar 时应是 no-op
        remove_sidecars(&snap).unwrap();
    }

    #[test]
    fn remove_sidecars删干净() {
        let dir = tempfile::tempdir().unwrap();
        let snap = dir.path().join("foo.idx");
        let p = graph_paths(&snap);
        std::fs::write(&p.graph, b"g").unwrap();
        std::fs::write(&p.data, b"d").unwrap();
        write_manifest_atomic(&p.manifest, &sample()).unwrap();

        remove_sidecars(&snap).unwrap();
        assert!(!p.graph.exists());
        assert!(!p.data.exists());
        assert!(!p.manifest.exists());
    }
}
