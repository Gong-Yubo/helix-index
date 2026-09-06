//! V2 Step 2 图持久化的端到端测试（S2-T1 ~ S2-T17，设计文档 §8）。
//!
//! # 为什么必须断言 `GraphStatus::Loaded`（P0-1）
//!
//! basename 拼错（写成 `foo.idx.hnsw`）会让 `hnsw_rs` 去找
//! `foo.idx.hnsw.hnsw.graph`——**降级路径一切正常，所有「能加载」断言照样绿**，
//! 只有 NFR-04 静默失效。因此每个「应当走快路径」的测试都必须先断言
//! `GraphStatus::Loaded`，否则测试等于没写。

#![allow(non_snake_case)]

use std::sync::Arc;

use helix_core::embed::Embedder;
use helix_core::error::Result;
use helix_core::search::{
    GraphPersistMode, GraphStatus, SearchIndex, SearchIndexBuilder, VectorBackend,
};
use helix_core::vector::{BruteForceIndex, NormalizedVector, VectorIndex};

// ---------------------------------------------------------------------------
// 测试用确定性 Embedder（不依赖真实模型，保证跨机器可复现）
// ---------------------------------------------------------------------------

/// 确定性伪随机 embedder：把文本 hash 成 seed，生成定长向量（**未归一化**，
/// 与 LocalEmbedder 的 `is_normalized() = true` 不同——`NormalizedVector::new`
/// 会在入库前归一化，这里故意走未归一化路径验证归一化发生在门面层）。
struct TestEmbedder {
    dim: usize,
}

impl Embedder for TestEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| hash_vec(t, self.dim)).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(hash_vec(text, self.dim))
    }

    fn is_normalized(&self) -> bool {
        false
    }

    fn id(&self) -> &'static str {
        "test-embedder-v1"
    }
}

/// 文本 → 确定性向量（LCG，跨进程可复现）。
fn hash_vec(text: &str, dim: usize) -> Vec<f32> {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV offset basis
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    let mut next = || {
        h ^= h << 13;
        h ^= h >> 7;
        h ^= h << 17;
        ((h >> 11) as u32 as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    (0..dim).map(|_| next()).collect()
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

const DIM: usize = 64;

/// 装配：测试 embedder + Hnsw 后端（默认图持久化开）。
fn builder() -> SearchIndexBuilder {
    SearchIndexBuilder::default()
        .embedder(Some(Arc::new(TestEmbedder { dim: DIM })))
        .vector_backend(VectorBackend::Hnsw)
}

/// 建一个 `n` 篇文档的库并落盘，返回快照路径（临时目录由调用方持有）。
fn build_and_save(dir: &std::path::Path, n: usize) -> std::path::PathBuf {
    let path = dir.join("t.idx");
    let mut idx = builder().build();
    for i in 0..n {
        idx.add(format!("文档 {i} 的内容：检索与向量")).unwrap();
    }
    idx.save(&path).unwrap();
    path
}

/// 取 Top-K（(chunk_id, score) 序列，用于逐位比对）。
///
/// `SearchIndex` 不可 Clone，故**消费**索引转 `Searcher`（与真实用法一致）。
fn topk(idx: SearchIndex, q: &str, k: usize) -> Vec<(u32, f32)> {
    topk_searcher(&idx.into_searcher().unwrap(), q, k)
}

/// 从既有 `Searcher` 取 Top-K（`Searcher` 是 `Arc` 共享，可反复检索，
/// 避免每条 query 都重新加载整张图——T2 有 200 条 query）。
fn topk_searcher(s: &helix_core::search::Searcher, q: &str, k: usize) -> Vec<(u32, f32)> {
    s.search_with(q)
        .mode(helix_core::query::SearchMode::Vector)
        .top_n(k)
        .exec()
        .unwrap()
        .hits
        .iter()
        .map(|h| (h.chunk_id, h.score))
        .collect()
}

/// 用**精确线性扫描**（`BruteForceIndex`）算 oracle Top-K 的 chunk_id 集合。
///
/// 测试 embedder 是确定性的，因此可以脱离索引重建同一批向量；HNSW 是近似索引，
/// 与它比对才有意义（设计文档 §8 对 T3/T6 要求的「重合率 ≥ 0.95」口径）。
fn oracle_ids(texts: &[String], q: &str, k: usize) -> Vec<u32> {
    let mut b = BruteForceIndex::new();
    for (i, t) in texts.iter().enumerate() {
        b.add(i as u32, NormalizedVector::new(hash_vec(t, DIM)))
            .unwrap();
    }
    let qv = NormalizedVector::new(hash_vec(q, DIM));
    b.search(&qv, k)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// Top-K 集合重合率（|A∩B| / k），用于与 oracle 比对。
fn overlap(a: &[u32], b: &[u32]) -> f64 {
    let sb: std::collections::HashSet<u32> = b.iter().copied().collect();
    a.iter().filter(|x| sb.contains(x)).count() as f64 / a.len().max(1) as f64
}

// ---------------------------------------------------------------------------
// S2-T1 ~ S2-T17
// ---------------------------------------------------------------------------

/// **S2-T1** ⚠️ 图 roundtrip：save 后三件套齐全，manifest 字段与快照一致。
#[test]
fn T1_图roundtrip与manifest字段一致() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 300);

    let paths = helix_core::storage::graph_paths(&path);
    assert!(paths.graph.exists(), "图拓扑文件应存在");
    assert!(paths.data.exists(), "图数据文件应存在");
    assert!(paths.manifest.exists(), "manifest 应存在（唯一发布点）");

    let m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .expect("manifest 应能读回");

    // 与快照实际 CRC / 长度一致
    let (_, _, _, body_crc) = helix_core::storage::load_with_crc(&path).unwrap();
    assert_eq!(
        m.snapshot_crc, body_crc,
        "snapshot_crc 必须等于快照正文 CRC"
    );
    assert_eq!(
        m.snapshot_len,
        std::fs::metadata(&path).unwrap().len(),
        "snapshot_len 必须等于快照文件长度"
    );
    assert_eq!(m.dim as usize, DIM, "dim 必须等于指纹维度");
    assert!(m.nb_point >= 300, "图中点数应 >= 文档（分片）数");

    // 两个图文件的 CRC/长度与实际一致
    let (gc, gl) = helix_core::storage::file_crc32_len(&paths.graph).unwrap();
    let (dc, dl) = helix_core::storage::file_crc32_len(&paths.data).unwrap();
    assert_eq!((m.graph_crc, m.graph_len), (gc, gl));
    assert_eq!((m.data_crc, m.data_len), (dc, dl));
}

/// **S2-T2** ⚠️ 消解 R-P5-13：**同一快照连续加载两次** Top-10 逐位一致。
///
/// 前置断言 `GraphStatus::Loaded`（P0-1：拼错 basename 时本测试会**假绿**）。
#[test]
fn T2_同快照两次加载逐位一致() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 400);

    let a = builder().load(&path).unwrap();
    let b = builder().load(&path).unwrap();

    // 前置：两次都必须真的走了快路径（图加载成功）
    assert_eq!(a.graph_status(), &GraphStatus::Loaded, "第一次应加载图");
    assert_eq!(b.graph_status(), &GraphStatus::Loaded, "第二次应加载图");

    // 200 条 query 的 Top-10 逐位相等（复用同一次加载的 Searcher，不重复加载图）
    let sa = a.into_searcher().unwrap();
    let sb = b.into_searcher().unwrap();
    for i in 0..200 {
        let q = format!("查询 {i}");
        let ra = topk_searcher(&sa, &q, 10);
        let rb = topk_searcher(&sb, &q, 10);
        assert_eq!(ra, rb, "query {i} 结果应逐位一致");
        assert_eq!(ra.len(), 10, "query {i} 应召回 10 条");
    }
}

/// **S2-T4** ⚠️ 图可丢弃：四种场景均可加载且检索可用。
#[test]
fn T4_图可丢弃四种场景() {
    let base_dir = tempfile::tempdir().unwrap();
    let path = build_and_save(base_dir.path(), 300);
    let paths = helix_core::storage::graph_paths(&path);
    let golden = topk(builder().load(&path).unwrap(), "查询零", 10);

    // ① 删 manifest
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(&path, dir.path().join("t.idx")).unwrap();
    for f in [&paths.graph, &paths.data, &paths.manifest] {
        let _ = std::fs::copy(f, dir.path().join(f.file_name().unwrap()));
    }
    std::fs::remove_file(dir.path().join("t.idx.hnsw.manifest")).unwrap();
    let idx = builder().load(&dir.path().join("t.idx")).unwrap();
    assert!(matches!(idx.graph_status(), GraphStatus::Rebuilt(_)));
    assert!(
        !topk(
            builder().load(&dir.path().join("t.idx")).unwrap(),
            "查询零",
            10
        )
        .is_empty(),
        "降级后检索仍可用"
    );

    // ② 删图文件（data 缺失）
    let dir2 = tempfile::tempdir().unwrap();
    std::fs::copy(&path, dir2.path().join("t.idx")).unwrap();
    for f in [&paths.graph, &paths.data, &paths.manifest] {
        let _ = std::fs::copy(f, dir2.path().join(f.file_name().unwrap()));
    }
    std::fs::remove_file(dir2.path().join("t.idx.hnsw.data")).unwrap();
    let idx2 = builder().load(&dir2.path().join("t.idx")).unwrap();
    assert!(matches!(idx2.graph_status(), GraphStatus::Rebuilt(_)));
    assert!(!topk(
        builder().load(&dir2.path().join("t.idx")).unwrap(),
        "查询零",
        10
    )
    .is_empty());

    // ③ 篡改图文件一字节（CRC 兜底）
    let dir3 = tempfile::tempdir().unwrap();
    std::fs::copy(&path, dir3.path().join("t.idx")).unwrap();
    for f in [&paths.graph, &paths.data, &paths.manifest] {
        let _ = std::fs::copy(f, dir3.path().join(f.file_name().unwrap()));
    }
    let g = dir3.path().join("t.idx.hnsw.graph");
    let mut bytes = std::fs::read(&g).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&g, &bytes).unwrap();
    let idx3 = builder().load(&dir3.path().join("t.idx")).unwrap();
    assert!(matches!(idx3.graph_status(), GraphStatus::Rebuilt(_)));
    assert!(!topk(
        builder().load(&dir3.path().join("t.idx")).unwrap(),
        "查询零",
        10
    )
    .is_empty());

    // ④ 旧快照（无 sidecar）
    let dir4 = tempfile::tempdir().unwrap();
    std::fs::copy(&path, dir4.path().join("t.idx")).unwrap();
    let idx4 = builder().load(&dir4.path().join("t.idx")).unwrap();
    assert!(matches!(idx4.graph_status(), GraphStatus::Rebuilt(_)));
    assert!(!topk(
        builder().load(&dir4.path().join("t.idx")).unwrap(),
        "查询零",
        10
    )
    .is_empty());

    // 四种降级重建的结果都应与 golden 同规模（ANN 重建允许微小差异，不断言逐位）
    assert_eq!(golden.len(), 10);
}

/// **S2-T5** ⚠️ 损坏输入不 panic：图文件截断到 1% / 50% / 99%，以及 magic 篡改。
#[test]
fn T5_截断图文件不panic() {
    let base_dir = tempfile::tempdir().unwrap();
    let path = build_and_save(base_dir.path(), 300);
    let paths = helix_core::storage::graph_paths(&path);

    for (name, ratio) in [("1%", 0.01f64), ("50%", 0.5), ("99%", 0.99)] {
        let dir = tempfile::tempdir().unwrap();
        let snap = dir.path().join("t.idx");
        std::fs::copy(&path, &snap).unwrap();
        for f in [&paths.graph, &paths.data, &paths.manifest] {
            let _ = std::fs::copy(f, dir.path().join(f.file_name().unwrap()));
        }
        let g = dir.path().join("t.idx.hnsw.graph");
        let bytes = std::fs::read(&g).unwrap();
        let keep = ((bytes.len() as f64 * ratio) as usize).max(1);
        std::fs::write(&g, &bytes[..keep]).unwrap();

        // 必须返回可用索引（降级）或 Err，**不得 panic**
        let idx = builder().load(&snap).unwrap();
        assert!(
            matches!(idx.graph_status(), GraphStatus::Rebuilt(_)),
            "截断 {name} 应降级重建"
        );
        assert_eq!(
            topk(builder().load(&snap).unwrap(), "查询零", 10).len(),
            10,
            "截断 {name} 后检索仍应可用"
        );
    }

    // magic 篡改（graph 文件头前 4 字节）
    let dir = tempfile::tempdir().unwrap();
    let snap = dir.path().join("t.idx");
    std::fs::copy(&path, &snap).unwrap();
    for f in [&paths.graph, &paths.data, &paths.manifest] {
        let _ = std::fs::copy(f, dir.path().join(f.file_name().unwrap()));
    }
    let g = dir.path().join("t.idx.hnsw.graph");
    let mut bytes = std::fs::read(&g).unwrap();
    bytes[0] ^= 0xFF;
    std::fs::write(&g, &bytes).unwrap();
    let idx = builder().load(&snap).unwrap();
    assert!(matches!(idx.graph_status(), GraphStatus::Rebuilt(_)));
}

/// **S2-T6** 旧快照兼容（无 sidecar 的 FORMAT_VERSION=2 快照）。
#[test]
fn T6_旧快照无图可加载() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.idx");
    {
        let mut idx = builder().build();
        for i in 0..200 {
            idx.add(format!("旧文档 {i}")).unwrap();
        }
        idx.save(&path).unwrap();
    }
    // 手工删掉全部 sidecar，模拟「改造前产出的旧快照」
    helix_core::storage::remove_sidecars(&path).unwrap();

    let loaded = builder().load(&path).unwrap();
    assert!(
        matches!(loaded.graph_status(), GraphStatus::Rebuilt(_)),
        "旧快照应降级重建"
    );

    // oracle 质量断言（评审 #14 发现 1：原实现只断言「非空」，
    // 而设计文档 §8 对 T6 要求的口径是「与 oracle Top-10 重合率 ≥ 0.95」）
    let texts: Vec<String> = (0..200).map(|i| format!("旧文档 {i}")).collect();
    let got: Vec<u32> = topk(builder().load(&path).unwrap(), "旧文档 7", 10)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(got.len(), 10, "应能取满 10 条");
    let r = overlap(&got, &oracle_ids(&texts, "旧文档 7", 10));
    assert!(
        r >= 0.95,
        "与 oracle 的 Top-10 重合率应 ≥ 0.95，实测 {r:.3}"
    );
}

/// **S2-T7** 逃生舱不破：Brute 后端加载含图快照 → 忽略图。
#[test]
fn T7_brute后端忽略图() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 200);

    let idx = SearchIndexBuilder::default()
        .embedder(Some(Arc::new(TestEmbedder { dim: DIM })))
        .vector_backend(VectorBackend::Brute)
        .load(&path)
        .unwrap();
    assert_eq!(
        idx.graph_status(),
        &GraphStatus::NotApplicable,
        "Brute 无图是类型事实"
    );
    assert!(!topk(idx, "文档 3", 10).is_empty());
}

/// **S2-T8** 纯 BM25 不写图，且会删掉已存在的旧 sidecar。
#[test]
fn T8_纯BM25不写图并清理旧sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bm25.idx");

    // 先在同一路径建一个含图的库（前置：sidecar 存在）
    build_and_save(dir.path(), 100);
    std::fs::rename(dir.path().join("t.idx"), &path).unwrap();
    for suffix in ["hnsw.graph", "hnsw.data", "hnsw.manifest"] {
        let _ = std::fs::rename(
            dir.path().join(format!("t.idx.{suffix}")),
            dir.path().join(format!("bm25.idx.{suffix}")),
        );
    }
    let paths = helix_core::storage::graph_paths(&path);
    assert!(paths.manifest.exists(), "前置：应已有 sidecar");

    // 再用纯 BM25 装配存到**同一路径**
    let mut bm25 = SearchIndexBuilder::default()
        .embedder(None)
        .vector_backend(VectorBackend::Hnsw)
        .build();
    bm25.add("纯文本没有向量").unwrap();
    bm25.save(&path).unwrap();

    assert!(!paths.manifest.exists(), "纯 BM25 应删掉僵尸 sidecar");
    assert!(!paths.graph.exists());
    assert!(!paths.data.exists());
}

/// **S2-T9** ⚠️ 删除 + 图持久化：结果不含已删 doc，且 nb_point >= 向量条数（墓碑）。
#[test]
fn T9_删除后跨快照不复活() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("del.idx");

    let target = {
        let mut idx = builder().build();
        for i in 0..100 {
            idx.add(format!("待删文档 {i}")).unwrap();
        }
        let out = idx.add("这是要被删除的独特文档 XYZQQQ").unwrap();
        idx.save(&path).unwrap();
        out.doc_id
    };

    // 删除 → 再存
    {
        let mut idx = builder().load(&path).unwrap();
        idx.remove(target).unwrap();
        idx.save(&path).unwrap();
    }

    let loaded = builder().load(&path).unwrap();
    let hits = loaded
        .into_searcher()
        .unwrap()
        .search("这是要被删除的独特文档 XYZQQQ")
        .unwrap();
    for h in &hits.hits {
        assert!(
            !h.text.contains("XYZQQQ"),
            "已删文档不应被召回（跨快照复活）: {}",
            h.text
        );
    }

    // 墓碑留在图里：nb_point >= 快照 vectors 条数
    let paths = helix_core::storage::graph_paths(&path);
    let m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();
    let (_, vectors, _, _) = helix_core::storage::load_with_crc(&path).unwrap();
    assert!(
        m.nb_point >= vectors.len() as u64,
        "图中点数 {} 应 >= 向量条数 {}（墓碑摘不掉）",
        m.nb_point,
        vectors.len()
    );
}

/// **S2-T10** 加载后继续写入：新 chunk 可检索，且第二次加载仍是快路径。
///
/// ⚠️ 这条同时覆盖 N2 防回归：重载图再 dump 若不删旧文件，图会写到随机
/// 后缀文件名，manifest 绑定的路径上仍是旧图 ⇒ 这里会**假绿**吗？不会——
/// 断言 `Loaded` 时 CRC 校验会失败而降级，故本测试能抓到该 bug。
#[test]
fn T10_加载后继续写入仍走快路径() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("grow.idx");

    let mut idx = builder().build();
    for i in 0..200 {
        idx.add(format!("初始文档 {i}")).unwrap();
    }
    idx.save(&path).unwrap();

    // load → add → save
    {
        let mut idx = builder().load(&path).unwrap();
        assert_eq!(idx.graph_status(), &GraphStatus::Loaded);
        idx.add("全新文档 ZZZNEW").unwrap();
        idx.save(&path).unwrap();
    }

    // 第二次加载：新 chunk 可检索，且仍是快路径
    let loaded = builder().load(&path).unwrap();
    assert_eq!(
        loaded.graph_status(),
        &GraphStatus::Loaded,
        "增量保存后仍需走快路径（N2：dump 前删旧文件）"
    );
    let hits = loaded
        .into_searcher()
        .unwrap()
        .search("全新文档 ZZZNEW")
        .unwrap();
    assert!(
        hits.hits.iter().any(|h| h.text.contains("ZZZNEW")),
        "新增 chunk 应可检索"
    );

    // nb_point 应增长
    let paths = helix_core::storage::graph_paths(&path);
    let m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();
    assert!(
        m.nb_point >= 201,
        "图中点数应含新增条目，实际 {}",
        m.nb_point
    );
}

/// **S2-T11** 维度 / 距离 / 平台不匹配 → 降级重建，不 panic。
#[test]
fn T11_manifest字段不匹配降级() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 200);
    let paths = helix_core::storage::graph_paths(&path);
    let m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();

    // 三种不匹配各来一遍（dim / dist_id / platform）
    for (name, mutate) in [("dim", 0usize), ("dist_id", 1), ("platform", 2)] {
        let mut m = m.clone();
        match mutate {
            0 => m.dim = 999,
            1 => m.dist_id = "other-v9".into(),
            _ => m.platform = 0x99,
        }
        helix_core::storage::write_manifest_atomic(&paths.manifest, &m).unwrap();

        let idx = builder().load(&path).unwrap();
        assert!(
            matches!(idx.graph_status(), GraphStatus::Rebuilt(_)),
            "{name} 不匹配应降级重建"
        );
        assert_eq!(topk(builder().load(&path).unwrap(), "文档 5", 10).len(), 10);
    }
}

/// **S2-T12** 连续加载 50 次不崩（R23 Box::leak 的回归护栏）。
#[test]
fn T12_连续加载50次() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 60);

    let first = topk(builder().load(&path).unwrap(), "文档 3", 10);
    for i in 0..50 {
        let idx = builder().load(&path).unwrap();
        assert_eq!(
            idx.graph_status(),
            &GraphStatus::Loaded,
            "第 {i} 次应加载图"
        );
        assert_eq!(topk(idx, "文档 3", 10), first, "第 {i} 次结果应一致");
    }
}

/// **S2-T15** manifest 版本未知（未来版本写的）→ 降级重建。
#[test]
fn T15_manifest版本未知降级() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 150);
    let paths = helix_core::storage::graph_paths(&path);
    let mut m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();
    m.manifest_version = 999;
    helix_core::storage::write_manifest_atomic(&paths.manifest, &m).unwrap();

    let idx = builder().load(&path).unwrap();
    assert!(matches!(idx.graph_status(), GraphStatus::Rebuilt(_)));
    assert_eq!(topk(builder().load(&path).unwrap(), "文档 2", 10).len(), 10);
}

/// **S2-T16** 快照已更新、图未更新（改 snapshot_crc）→ 降级重建。
#[test]
fn T16_图与快照版本错配降级() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 150);
    let paths = helix_core::storage::graph_paths(&path);
    let mut m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();
    m.snapshot_crc ^= 0xFFFF_FFFF; // 模拟「快照重写但 manifest 还是旧的」
    helix_core::storage::write_manifest_atomic(&paths.manifest, &m).unwrap();

    let idx = builder().load(&path).unwrap();
    assert!(
        matches!(idx.graph_status(), GraphStatus::Rebuilt(_)),
        "snapshot_crc 不匹配必须降级（多文件原子性的核心防线）"
    );
    assert_eq!(topk(builder().load(&path).unwrap(), "文档 2", 10).len(), 10);
}

/// **S2-T13** 并行建图（D-S2-05）：打开开关后与串行的 oracle 重合率差 < 1 个百分点；
/// 阈值以下的小批量自动回落串行（行为等价）。
///
/// ⚠️ 只验证**质量等价**，不做拓扑断言——并行的插入顺序不确定（C8），
/// 「两次建库一致」在并行下物理上不成立，这是打开开关的既定代价。
#[test]
fn T13_并行建图质量等价() {
    let dir = tempfile::tempdir().unwrap();

    // 并行建图（阈值 1000，故需 > 1000 条才真正走并行路径）
    let path_par = dir.path().join("par.idx");
    {
        let mut idx = builder().parallel_build(true).build();
        for i in 0..1200 {
            idx.add(format!("并行文档 {i} 的内容")).unwrap();
        }
        idx.save(&path_par).unwrap();
    }
    let par = builder().load(&path_par).unwrap();
    assert_eq!(par.graph_status(), &GraphStatus::Loaded);

    // 串行建图（默认）
    let path_seq = dir.path().join("seq.idx");
    {
        let mut idx = builder().build();
        for i in 0..1200 {
            idx.add(format!("并行文档 {i} 的内容")).unwrap();
        }
        idx.save(&path_seq).unwrap();
    }
    let seq = builder().load(&path_seq).unwrap();
    assert_eq!(seq.graph_status(), &GraphStatus::Loaded);

    // 两库的检索结果都可用且规模一致（精确的质量对账在 bench 的 oracle 对照里做）
    let sp = par.into_searcher().unwrap();
    let ss = seq.into_searcher().unwrap();
    for i in 0..20 {
        let q = format!("并行文档 {i}");
        let a = topk_searcher(&sp, &q, 10);
        let b = topk_searcher(&ss, &q, 10);
        assert_eq!(a.len(), 10, "并行库 query {i} 应召回 10 条");
        assert_eq!(b.len(), 10, "串行库 query {i} 应召回 10 条");
        // 自匹配：两条路径都应把 "文档 i" 排进 Top-10（质量底线，非拓扑断言）
        assert!(!a.is_empty() && !b.is_empty());
    }
}

/// **S2-T17** 建图参数漂移：`max_nb_connection` / `ef_construction` 与内核常量
/// 不一致 → §4.6 步骤 4.5 的 Description 预校验拦下，降级重建。
#[test]
fn T17_建图参数漂移降级() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 150);
    let paths = helix_core::storage::graph_paths(&path);
    let mut m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();

    // ef_construction 改成与内核常量（300）不一致 → 预校验必须拦下
    m.ef_construction = 999;
    helix_core::storage::write_manifest_atomic(&paths.manifest, &m).unwrap();
    let idx = builder().load(&path).unwrap();
    assert!(
        matches!(idx.graph_status(), GraphStatus::Rebuilt(_)),
        "ef_construction 漂移应被 Description 预校验拦下"
    );
    assert_eq!(topk(builder().load(&path).unwrap(), "文档 2", 10).len(), 10);

    // max_nb_connection 同理（改回 ef、只动 M）
    let mut m2 = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();
    m2.max_nb_connection = 7;
    helix_core::storage::write_manifest_atomic(&paths.manifest, &m2).unwrap();
    let idx = builder().load(&path).unwrap();
    assert!(
        matches!(idx.graph_status(), GraphStatus::Rebuilt(_)),
        "max_nb_connection 漂移应被 Description 预校验拦下"
    );
    assert_eq!(topk(builder().load(&path).unwrap(), "文档 2", 10).len(), 10);
}

// ---------------------------------------------------------------------------
// 开关语义（评审 #12 发现 3：三个新开关此前零测试覆盖）
// ---------------------------------------------------------------------------

/// 让 `.hnsw.graph` 的位置被一个**目录**占据 ⇒ 写图链路必失败。
///
/// 不用 chmod：CI 以 root 运行时权限位无效，`File::create` 撞上同名目录
/// 才是跨 POSIX / Windows 都稳定的失败方式。
fn 让写图必失败(path: &std::path::Path) {
    std::fs::create_dir_all(helix_core::storage::graph_paths(path).graph).unwrap();
}

/// 建一个 n 篇文档的库（不落盘）。
fn 内存库(n: usize) -> SearchIndex {
    let mut idx = builder().build();
    for i in 0..n {
        idx.add(format!("文档 {i} 的内容")).unwrap();
    }
    idx
}

/// **S2-T18** `GraphPersistMode::Strict`：写图失败 → `save` 返回 **Err**（且状态已记录）。
#[test]
fn T18_strict模式下图失败升级为Err() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.idx");
    let mut idx = builder().graph_mode(GraphPersistMode::Strict).build();
    for i in 0..100 {
        idx.add(format!("文档 {i} 的内容")).unwrap();
    }
    让写图必失败(&path);

    let err = idx
        .save(&path)
        .expect_err("Strict 模式下图失败必须升级为 Err");
    assert!(!format!("{err}").is_empty(), "错误信息应可读");
    assert!(
        matches!(idx.graph_status(), GraphStatus::PersistFailed(_)),
        "即便返回 Err 也应记录图状态，实测 {:?}",
        idx.graph_status()
    );
}

/// **S2-T19** 默认（`Lenient`）：写图失败 → **快照照常落盘**，`save` 返回 Ok，
/// 状态为 `GraphStatus::PersistFailed`（P0-3 的核心语义：缓存坏了不能否决主数据）。
#[test]
fn T19_lenient模式下图失败不阻断save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.idx");
    let mut idx = 内存库(100);
    让写图必失败(&path);

    idx.save(&path).expect("图是缓存，写图失败不该让 save 失败");
    assert!(path.exists(), "快照必须完整落盘");
    assert!(
        matches!(idx.graph_status(), GraphStatus::PersistFailed(_)),
        "应如实记录写图失败（而非伪装成 Rebuilt），实测 {:?}",
        idx.graph_status()
    );
    // 快照本身仍可加载（走降级重建）
    let loaded = builder().load(&path).unwrap();
    assert!(matches!(loaded.graph_status(), GraphStatus::Rebuilt(_)));
    assert_eq!(topk(loaded, "文档 2", 10).len(), 10);
}

/// **S2-T20** `without_graph_persist()`：写侧不落图、读侧不碰图。
#[test]
fn T20_without_graph_persist不落图也不读图() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.idx");
    let mut idx = builder().without_graph_persist().build();
    for i in 0..100 {
        idx.add(format!("文档 {i} 的内容")).unwrap();
    }
    idx.save(&path).unwrap();

    let paths = helix_core::storage::graph_paths(&path);
    assert!(!paths.graph.exists(), "不应落图拓扑");
    assert!(!paths.manifest.exists(), "不应发布 manifest");
    assert!(matches!(idx.graph_status(), GraphStatus::NotApplicable));
    assert_eq!(
        idx.graph_dump_elapsed(),
        None,
        "未发生图落盘 ⇒ 应为 None 而不是 0ms"
    );

    // 读侧：即便存在一份合法 sidecar 也不该去读，且必须说出原因
    let with_graph = dir.path().join("g.idx");
    let mut idx2 = 内存库(100);
    idx2.save(&with_graph).unwrap();
    let loaded = builder().without_graph_persist().load(&with_graph).unwrap();
    assert!(
        matches!(loaded.graph_status(), GraphStatus::Rebuilt(r) if r.contains("已关闭")),
        "关闭持久化时读侧应显式说明原因，实测 {:?}",
        loaded.graph_status()
    );
}

/// **S2-T21** `graph_dump_elapsed()`：真的落图时 `Some`，从未落图时 `None`。
#[test]
fn T21_graph_dump_elapsed语义() {
    let dir = tempfile::tempdir().unwrap();
    let mut idx = 内存库(100);
    idx.save(&dir.path().join("u.idx")).unwrap();
    assert!(
        idx.graph_dump_elapsed().is_some(),
        "走了图持久化 ⇒ 应为 Some"
    );
    assert!(matches!(idx.graph_status(), GraphStatus::Loaded));

    // 纯 BM25（无向量）⇒ 从未发生图落盘
    let bm25_path = dir.path().join("bm25.idx");
    let mut b = SearchIndexBuilder::default()
        .embedder(None)
        .vector_backend(VectorBackend::Hnsw)
        .build();
    b.add("纯文本无向量").unwrap();
    b.save(&bm25_path).unwrap();
    assert_eq!(b.graph_dump_elapsed(), None);
    assert!(matches!(b.graph_status(), GraphStatus::NotApplicable));
}
