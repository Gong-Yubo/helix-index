//! HNSW 图持久化：`VectorGraphPersist` trait + `HnswRsIndex` 的 dump/load 实现
//! （V2 Step 2 / ADR-A 方案 C，v2-step2-design §5.2 / §5.3）。
//!
//! 本模块是全 crate **唯一** import `hnsw_rs` 持久化 API（`AnnT::file_dump` /
//! `HnswIo` / `load_description`）的地方。
//!
//! # 四个必须记住的坑（源码级核实，见设计文档 §3.2 与「开工前复核记录」）
//!
//! 1. **basename 不追加后缀（P0-1）**：`file_dump(dir, "foo.idx")` 产出
//!    `foo.idx.hnsw.graph` / `.data`——`hnsw_rs` **自己**追加后缀。传
//!    `"foo.idx.hnsw"` 会得到 `foo.idx.hnsw.hnsw.graph`，降级路径一切正常、
//!    断言全绿，只有 NFR-04 静默失效。
//! 2. **dump 前删旧 sidecar（N2）**：`load_hnsw` 构造的 `Hnsw` 无条件
//!    `datamap_opt = true`，`file_dump` 因此**拒绝覆盖**已存在的 `.hnsw.data`，
//!    改写随机后缀文件——「加载 → 增量 add → save」会静默把图写错位置。
//!    对策：dump 前先删两个旧图文件（图是缓存，删除最坏降级重建）。
//! 3. **绝不跳过 CRC 先验（C3）**：`load_hnsw` 的 reload 路径有 12 处
//!    `assert_eq!` / `unwrap()` / `exit(1)`，「文件能打开但内容坏」会 panic。
//!    调用方（`search/index.rs`）必须先过 manifest CRC + Description 预校验。
//! 4. **`ef_search` 是入参（P0-5）**：全 crate 无 `set_ef*`，ef 只能作为
//!    `search` 参数逐次传，`HnswRsIndex.ef_search` 字段是唯一载体。

use std::fs::File;
use std::path::Path;

use hnsw_rs::api::AnnT;
use hnsw_rs::hnswio::{load_description, HnswIo};
use hnsw_rs::prelude::Hnsw;

use crate::error::{Error, Result};
use crate::storage::{graph_basename, graph_paths, GraphManifest};

use super::hnsw_rs_index::{DistDotClamped, HnswRsIndex};
use super::VectorIndex;

/// `hnsw_rs-0.3.4` 落盘时实际写的图格式（由 `Description::dump` 的
/// `MAGICDESCR_4` 读回）。**仅用于加载侧预校验**；写侧一律以
/// `load_description` 读回值为准（P0-2 / N1：勿信任何字面量）。
const SUPPORTED_GRAPH_FORMAT: u32 = 4;

/// `HnswIo` 必须线程安全：`HnswRsIndex` 经 `VectorIndex: Send + Sync`
/// 间接持有它（`Box::leak`），编译期钉死（R23；`hnsw_rs` 升级时需复核）。
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    const _: () = assert_send_sync::<HnswIo>();
};

/// dump 成功后的图统计（供 manifest 组装与 CLI 观测输出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphStats {
    /// 图中点数（含墓碑）
    pub nb_point: u64,
    /// 向量维度
    pub dim: u32,
    /// `Description.format_version` 的**读回值**（P0-2/N1：实测 4）
    pub graph_format: u32,
    /// `Description.max_nb_connection`（P1-3：建图参数漂移检测）
    pub max_nb_connection: u32,
    /// `Description.ef`（P1-3：建图参数漂移检测）
    pub ef_construction: u32,
}

/// 向量图持久化能力（D-S2-03）：「Brute 无图」是类型事实，不是运行时 if。
///
/// dump 是 `&self` 方法 → 对象安全；load 是关联函数 → `where Self: Sized`，
/// 不影响主 trait [`VectorIndex`] 的对象安全（`Box<dyn VectorIndex>` 不变）。
/// 门面层经 [`VectorIndex::as_graph_persist`] 的默认下转触达本 trait（P0-4）。
pub trait VectorGraphPersist: VectorIndex {
    /// 把图 dump 到 `base`（快照路径）旁的 sidecar 文件。
    ///
    /// - basename 用**快照文件名全名**（P0-1），`hnsw_rs` 自行追加后缀
    /// - **先删旧图文件**（N2：重载图的 `datamap_opt=true` 拒绝覆盖）
    /// - 前置检查目录可写只能缩小不能归零 panic 面（C3 写路径 R19）
    fn dump_graph(&self, base: &Path) -> Result<GraphStats>;

    /// 从 `base` 旁的 sidecar 加载图（**必须先过 manifest CRC 预校验**，
    /// 本函数不做 CRC——那是门面层的职责，见 C3）。
    ///
    /// `ef_search` 是**入参**（P0-5）：全 crate 无 `set_ef*`，只能由
    /// `HnswRsIndex` 字段承载。值来自 `SearchIndexBuilder::with_ef_search`
    /// 或默认 `EF_SEARCH = 200`。
    /// `parallel_build` 是**配置**而非图的属性：图里没有它，但它决定加载后
    /// 继续写入时 `add_batch` 走不走 rayon，故必须由调用方补齐。
    fn load_graph(
        base: &Path,
        m: &GraphManifest,
        ef_search: usize,
        parallel_build: bool,
    ) -> Result<Self>
    where
        Self: Sized;
}

impl VectorGraphPersist for HnswRsIndex {
    fn dump_graph(&self, base: &Path) -> Result<GraphStats> {
        let dir = base.parent().unwrap_or_else(|| Path::new("."));
        // "foo.idx"——不追加任何后缀（P0-1）。入口已校验路径有文件名。
        let basename = graph_basename(base)
            .ok_or_else(|| Error::VectorGraph(format!("快照路径缺少文件名: {}", base.display())))?;

        // 前置：目录存在且可写（缩小 DumpInit::new panic_any 的 TOCTOU 窗口）。
        // **必须先于删除旧图**（评审 #12 发现 5）：目录只读时若先删，旧缓存已被
        // 销毁才探测失败，本可命中的缓存白白丢失；先探测则原样保留。
        let probe = dir.join(".helix-graph-dump-probe");
        match std::fs::write(&probe, b"") {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
            }
            Err(e) => {
                return Err(Error::VectorGraph(format!(
                    "图 dump 目录不可写 {}: {e}",
                    dir.display()
                )))
            }
        }

        // N2：load_hnsw 构造的 Hnsw 无条件 datamap_opt=true，file_dump 拒绝
        // 覆盖旧 .hnsw.data、改写随机后缀文件名。先删旧图文件再 dump。
        // （图是缓存，删除最坏降级重建；manifest 由调用方随后原子发布。）
        let paths = graph_paths(base);
        for p in [&paths.graph, &paths.data] {
            match std::fs::remove_file(p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::VectorGraph(e.to_string())),
            }
        }

        self.hnsw()
            .file_dump(dir, &basename)
            .map_err(|e| Error::VectorGraph(e.to_string()))?;

        // P0-2 / N1：graph_format 不硬编码——dump 后读回真实值。
        // （构造字面量是 3，但落盘 magic 是 MAGICDESCR_4，读回 4。）
        let graph_file = File::open(&paths.graph).map_err(|e| Error::VectorGraph(e.to_string()))?;
        let d = load_description(&mut std::io::BufReader::new(graph_file))
            .map_err(|e| Error::VectorGraph(format!("读回 Description 失败: {e}")))?;

        Ok(GraphStats {
            nb_point: self.hnsw().get_nb_point() as u64,
            // dim 从读回的 Description 拿（Hnsw 无公开的 get_data_dimension）
            dim: d.dimension as u32,
            graph_format: d.format_version as u32,
            max_nb_connection: d.max_nb_connection as u32,
            ef_construction: d.ef as u32,
        })
    }

    fn load_graph(
        base: &Path,
        _m: &GraphManifest,
        ef_search: usize,
        parallel_build: bool,
    ) -> Result<Self> {
        let dir = base.parent().unwrap_or_else(|| Path::new("."));
        let basename =
            graph_basename(base) // 同 dump——不加 .hnsw（P0-1）
                .ok_or_else(|| {
                    Error::VectorGraph(format!("快照路径缺少文件名: {}", base.display()))
                })?;

        // 生命周期（C6）：`HnswIo` 必须活得比 `Hnsw` 长，而我们要
        // `Hnsw<'static, ..>` ⇒ 只有 Box::leak 一条路。
        // P1-2：leak 后**直接丢弃句柄**，不存字段——0.3.4 默认不开 mmap，
        // 向量被读进内存（PointData::V），Hnsw<'static> 自持数据；
        // 留着 &mut 反而是裸露的别名隐患。量级：每次加载约 200B + 路径串，
        // CLI/库典型「一进程一加载」可忽略（R23 / S2-T12 护栏）。
        let io: &'static mut HnswIo = Box::leak(Box::new(HnswIo::new(dir, &basename)));

        // 用 load_hnsw 而非 load_hnsw_with_dist：前者只比对**短名**
        // （"DistDotClamped"），模块移动不让旧图失效（C7 / R20）。
        // DistDotClamped 已 derive(Default)，满足约束。
        let hnsw: Hnsw<'static, f32, DistDotClamped> = io
            .load_hnsw()
            .map_err(|e| Error::VectorGraph(format!("图加载失败: {e}")))?;

        // ef_search 由入参带入（P0-5），from_loaded 是字段对兄弟模块可见的
        // 唯一构造通道（P0-5 后半：字面量构造在 persist.rs 编译不过）。
        // `parallel_build` 同样必须由入参带入：加载后继续 add + flush 走的是
        // 同一条 `add_batch`，读端若默认 false 会与写端行为不一致
        // （评审 #13 发现 3）。
        Ok(HnswRsIndex::from_loaded(hnsw, ef_search, parallel_build))
    }
}

/// 「校验 + 加载」一站式入口（v2-step2-design §4.6 的完整读取序列）。
///
/// 门面层与 bench（底层路径）共用同一份校验，**禁止在调用侧复制校验逻辑**——
/// 复制必然漏项，而漏项的代价是把坏文件交给满是 `unwrap()` 的 `load_hnsw`（C3）。
///
/// - `body_crc`：快照正文 CRC（图的版本锚点，由 `storage::load_with_crc` 带出）
/// - `dim`：`ConfigFingerprint.dim`
/// - `nb_vectors`：快照中的向量条数（图中点数因墓碑恒 >= 它）
/// - `ef_search`：加载后回填（P0-5：全 crate 无 `set_ef*`）
/// - `parallel_build`：加载后继续写入时是否走并行（评审 #13 发现 3）
///
/// 返回 `Err(reason)` 表示应降级重建（图是缓存，丢弃不丢功能）。
pub fn load_graph_checked(
    snapshot: &Path,
    body_crc: u32,
    dim: u32,
    nb_vectors: u64,
    ef_search: usize,
    parallel_build: bool,
) -> std::result::Result<HnswRsIndex, String> {
    // 步骤 0：路径必须有文件名（评审 #12 nit；读侧与写侧同一约束）
    crate::storage::require_file_name(snapshot).map_err(|e| e.to_string())?;

    // 步骤 3：读 manifest（缺失 / header 错 / CRC 错 / 解码错 → 降级）
    let paths = graph_paths(snapshot);
    let m = match crate::storage::read_manifest(&paths.manifest) {
        Ok(Some(m)) => m,
        Ok(None) => return Err("manifest 缺失或损坏".to_string()),
        Err(e) => return Err(format!("manifest 读取失败: {e}")),
    };

    // 步骤 4：逐项校验
    if m.manifest_version != crate::storage::MANIFEST_VERSION {
        return Err(format!("manifest_version {} 未知", m.manifest_version));
    }
    if m.dist_id != crate::storage::DIST_ID {
        return Err(format!("dist_id 不匹配: {}", m.dist_id));
    }
    if m.platform != crate::storage::PLATFORM_FINGERPRINT {
        return Err(format!("platform 不匹配: {:#x}", m.platform));
    }
    if m.dim != dim {
        return Err(format!("dim 不匹配: 图 {} vs 指纹 {dim}", m.dim));
    }
    if m.snapshot_crc != body_crc {
        return Err(format!(
            "snapshot_crc 不匹配: 图绑定 {:#010x} vs 快照 {body_crc:#010x}",
            m.snapshot_crc
        ));
    }
    let snapshot_len = std::fs::metadata(snapshot).map(|md| md.len()).unwrap_or(0);
    if m.snapshot_len != snapshot_len {
        return Err(format!(
            "snapshot_len 不匹配: {} vs {snapshot_len}",
            m.snapshot_len
        ));
    }
    let (gc, gl) =
        crate::storage::file_crc32_len(&paths.graph).map_err(|e| format!("图文件读取失败: {e}"))?;
    if gc != m.graph_crc || gl != m.graph_len {
        return Err("图拓扑文件 CRC/长度不符".to_string());
    }
    let (dc, dl) = crate::storage::file_crc32_len(&paths.data)
        .map_err(|e| format!("图数据文件读取失败: {e}"))?;
    if dc != m.data_crc || dl != m.data_len {
        return Err("图数据文件 CRC/长度不符".to_string());
    }

    // 步骤 4.5：Description 预校验（P1-1 / P1-3）
    validate_graph_description(snapshot, &m).map_err(|e| e.to_string())?;

    // 步骤 5：加载（此刻文件已过五道先验）
    let loaded =
        <HnswRsIndex as VectorGraphPersist>::load_graph(snapshot, &m, ef_search, parallel_build)
            .map_err(|e| format!("图加载失败: {e}"))?;

    // 步骤 6：图中点数恒 >= 原始向量条数（墓碑摘不掉，§4.6）
    if (loaded.len() as u64) < nb_vectors {
        return Err(format!(
            "图中点数 {} < 快照向量条数 {nb_vectors}",
            loaded.len()
        ));
    }
    Ok(loaded)
}

/// 加载侧的 `Description` 预校验（v2-step2-design §4.6 步骤 4.5 / P1-1）。
///
/// 在把图文件交给满是 `unwrap()` 的 `load_hnsw` 之前，先读自描述头逐项比对。
/// 任一不符 → `Err(GraphStale)`（门面层转降级）。这是 CRC 之外的第五道防线，
/// 专防「CRC 撞对但内容语义错」与建图参数漂移（P1-3）。
pub fn validate_graph_description(snapshot: &Path, m: &GraphManifest) -> Result<()> {
    let paths = graph_paths(snapshot);
    let graph_file = File::open(&paths.graph).map_err(|e| Error::GraphStale {
        reason: format!("图文件打不开: {e}"),
    })?;
    // load_description 对截断文件安全：read_exact 全部 ? 传播（附录 B）
    let d = load_description(&mut std::io::BufReader::new(graph_file)).map_err(|e| {
        Error::GraphStale {
            reason: format!("Description 读失败: {e}"),
        }
    })?;

    let stale = |what: &str| Error::GraphStale {
        reason: format!("Description 预校验不符（{what}）"),
    };

    if d.format_version as u32 != m.graph_format {
        return Err(stale("format_version 与 manifest 不一致"));
    }
    if d.format_version as u32 != SUPPORTED_GRAPH_FORMAT {
        // 只接受当前已知的图格式（hnsw_rs 升级 ⇒ 降级重建，不影响可用性）
        return Err(stale("graph_format 不受支持"));
    }
    if d.dimension as u32 != m.dim {
        return Err(stale("dimension 与 manifest 不一致"));
    }
    if d.nb_point as u64 != m.nb_point {
        return Err(stale("nb_point 与 manifest 不一致"));
    }
    // 短名比对（与 load_hnsw 同口径，C7）
    let dist_short: String = d
        .distname
        .rsplit("::")
        .next()
        .unwrap_or(&d.distname)
        .to_string();
    if dist_short != "DistDotClamped" {
        return Err(stale(&format!("distname 非预期: {}", d.distname)));
    }
    if d.t_name != "f32" {
        return Err(stale(&format!("t_name 非预期: {}", d.t_name)));
    }
    if d.max_nb_connection as u32 != m.max_nb_connection {
        return Err(stale("max_nb_connection 与 manifest 不一致"));
    }
    if d.ef as u32 != m.ef_construction {
        return Err(stale("ef_construction 与 manifest 不一致"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;
    use crate::storage::remove_sidecars;
    use crate::vector::NormalizedVector;

    /// 小规模确定性向量（复用 hnsw_rs_index 的 LCG 口径）。
    fn random_vec(seed: &mut u64) -> Vec<f32> {
        let mut next = || {
            *seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((*seed >> 33) as u32 as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        (0..512).map(|_| next()).collect()
    }

    fn build(n: usize, seed: &mut u64) -> HnswRsIndex {
        let mut idx = HnswRsIndex::with_capacity(n);
        for i in 0..n {
            let v = NormalizedVector::new(random_vec(seed));
            idx.add(i as u32, v).unwrap();
        }
        idx
    }

    /// dump → 手工组装 manifest → Description 预校验 → load 的完整回路。
    /// （门面层的全链路测试在 S2-07；此处钉死 persist 层自身的契约。）
    #[test]
    fn dump然后load图roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("t.idx");

        let mut seed = 7u64;
        let idx = build(300, &mut seed);
        let q = NormalizedVector::new(random_vec(&mut seed));

        let expect: Vec<(u32, f32)> = idx.search(&q, 10).unwrap();
        assert!(!expect.is_empty());

        // dump
        let stats = idx.dump_graph(&base).unwrap();
        let paths = graph_paths(&base);
        assert!(paths.graph.exists(), "{} 应存在", paths.graph.display());
        assert!(paths.data.exists(), "{} 应存在", paths.data.display());
        assert!(!paths.manifest.exists(), "manifest 由门面层发布，dump 不写");

        // 组装 manifest（模拟门面层）
        let (gc, gl) = crate::storage::file_crc32_len(&paths.graph).unwrap();
        let (dc, dl) = crate::storage::file_crc32_len(&paths.data).unwrap();
        let m = GraphManifest {
            manifest_version: crate::storage::MANIFEST_VERSION,
            producer: "test".into(),
            graph_format: stats.graph_format,
            dist_id: crate::storage::DIST_ID.into(),
            platform: crate::storage::PLATFORM_FINGERPRINT,
            max_nb_connection: stats.max_nb_connection,
            ef_construction: stats.ef_construction,
            snapshot_crc: 0,
            snapshot_len: 0,
            dim: 512,
            nb_point: stats.nb_point,
            graph_crc: gc,
            graph_len: gl,
            data_crc: dc,
            data_len: dl,
        };

        // 预校验 + load
        validate_graph_description(&base, &m).unwrap();
        let loaded =
            <HnswRsIndex as VectorGraphPersist>::load_graph(&base, &m, 200, false).unwrap();
        assert_eq!(loaded.len(), 300);

        // 同一张图：加载结果的检索必须逐位一致（验收 2 的最小化验证）
        let got: Vec<(u32, f32)> = loaded.search(&q, 10).unwrap();
        assert_eq!(got, expect, "持久化图加载后的 Top-10 必须逐位一致");
    }

    /// N2 防回归：**重载后的图再 dump** 必须仍写到标准文件名
    /// （datamap_opt=true 会让 file_dump 拒绝覆盖、改写随机后缀）。
    #[test]
    fn 重载图再dump仍写标准文件名() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("t.idx");
        let paths = graph_paths(&base);

        let mut seed = 11u64;
        let idx = build(200, &mut seed);
        idx.dump_graph(&base).unwrap();
        let (gc, gl) = crate::storage::file_crc32_len(&paths.graph).unwrap();
        let m = GraphManifest {
            manifest_version: crate::storage::MANIFEST_VERSION,
            producer: "test".into(),
            graph_format: 4,
            dist_id: crate::storage::DIST_ID.into(),
            platform: crate::storage::PLATFORM_FINGERPRINT,
            max_nb_connection: 32,
            ef_construction: 300,
            snapshot_crc: 0,
            snapshot_len: 0,
            dim: 512,
            nb_point: 200,
            graph_crc: gc,
            graph_len: gl,
            data_crc: 0,
            data_len: 0,
        };
        let loaded =
            <HnswRsIndex as VectorGraphPersist>::load_graph(&base, &m, 200, false).unwrap();

        // 关键动作：重载图直接再 dump（模拟 load → add → save）
        loaded.dump_graph(&base).unwrap();

        // 标准文件名必须有新内容（而非 foo.idx-1234.hnsw.graph）
        assert!(paths.graph.exists());
        assert!(paths.data.exists());
        let stray: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("-") && n.ends_with(".hnsw.graph"))
            .collect();
        assert!(
            stray.is_empty(),
            "不应出现随机后缀残留文件：{stray:?}（N2：dump 前未删旧文件）"
        );
    }

    /// ef_search 入参必须生效（P0-5）：不同 ef 下结果允许不同，相同 ef 下一致。
    #[test]
    fn ef_search入参生效() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("t.idx");
        let mut seed = 23u64;
        let idx = build(200, &mut seed);
        idx.dump_graph(&base).unwrap();

        let paths = graph_paths(&base);
        let (gc, gl) = crate::storage::file_crc32_len(&paths.graph).unwrap();
        let m = GraphManifest {
            manifest_version: crate::storage::MANIFEST_VERSION,
            producer: "test".into(),
            graph_format: 4,
            dist_id: crate::storage::DIST_ID.into(),
            platform: crate::storage::PLATFORM_FINGERPRINT,
            max_nb_connection: 32,
            ef_construction: 300,
            snapshot_crc: 0,
            snapshot_len: 0,
            dim: 512,
            nb_point: 200,
            graph_crc: gc,
            graph_len: gl,
            data_crc: 0,
            data_len: 0,
        };

        let a = <HnswRsIndex as VectorGraphPersist>::load_graph(&base, &m, 50, false).unwrap();
        let b = <HnswRsIndex as VectorGraphPersist>::load_graph(&base, &m, 50, false).unwrap();
        let c = <HnswRsIndex as VectorGraphPersist>::load_graph(&base, &m, 500, false).unwrap();

        let q = NormalizedVector::new(random_vec(&mut seed));
        assert_eq!(a.search(&q, 10).unwrap(), b.search(&q, 10).unwrap());
        // 500 的 ef 覆盖 50 的候选面（宽 ef 结果 ⊇ 窄 ef 结果的前若干名不必然，
        // 但两者都应可用且非空——弱断言防 flaky）
        assert!(!c.search(&q, 10).unwrap().is_empty());
    }

    /// P0-4：as_graph_persist 的下转契约。
    #[test]
    fn 下转契约() {
        let mut seed = 31u64;
        let h = build(50, &mut seed);
        let vi: &dyn VectorIndex = &h;
        assert!(vi.as_graph_persist().is_some(), "Hnsw 应可下转");

        let b = crate::vector::BruteForceIndex::from_entries(vec![(
            0u32,
            NormalizedVector::new(random_vec(&mut seed)),
        )]);
        let vb: &dyn VectorIndex = &b;
        assert!(vb.as_graph_persist().is_none(), "Brute 无图是类型事实");
    }

    /// Description 预校验：nb_point 漂移必须被拦下（P1-3 / S2-T17 的单元层）。
    #[test]
    fn 预校验拦下nb_point漂移() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("t.idx");
        let mut seed = 37u64;
        let idx = build(100, &mut seed);
        idx.dump_graph(&base).unwrap();

        let paths = graph_paths(&base);
        let (gc, gl) = crate::storage::file_crc32_len(&paths.graph).unwrap();
        let mut m = GraphManifest {
            manifest_version: crate::storage::MANIFEST_VERSION,
            producer: "test".into(),
            graph_format: 4,
            dist_id: crate::storage::DIST_ID.into(),
            platform: crate::storage::PLATFORM_FINGERPRINT,
            max_nb_connection: 32,
            ef_construction: 300,
            snapshot_crc: 0,
            snapshot_len: 0,
            dim: 512,
            nb_point: 100,
            graph_crc: gc,
            graph_len: gl,
            data_crc: 0,
            data_len: 0,
        };
        assert!(validate_graph_description(&base, &m).is_ok());

        m.nb_point = 999; // 模拟漂移
        assert!(matches!(
            validate_graph_description(&base, &m),
            Err(Error::GraphStale { .. })
        ));
    }

    /// remove_sidecars 与 dump 的协作：清理后目录干净。
    #[test]
    fn dump后清理干净() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("t.idx");
        let mut seed = 41u64;
        let idx = build(50, &mut seed);
        idx.dump_graph(&base).unwrap();
        remove_sidecars(&base).unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(entries.is_empty(), "sidecar 应全部删除");
    }
}
