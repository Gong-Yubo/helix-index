//! V2 Step 3：原子快照的**公开 API** 集成测试。
//!
//! # 落点说明（v2-step3-design §7 / D-S3-03 方案 C′）
//!
//! 故障注入类测试（S3-T3~T6、T9）依赖 `pub(crate)` 的 `fault::FAIL_AT` 钩子，
//! 集成层（独立 crate）不可达——全部放 crate 内 `#[cfg(test)]`
//! （`storage/atomic.rs` 承载 T3~T5/T9，`search/index.rs` 承载端到端 T6）。
//! 本文件放**无注入**的公开 API 测试：
//!
//! - 外观与兼容：save 原子性外观、tmp 不残留、旧快照兼容回归；
//! - **S3-TI1~TI5（评审后补强）**：tmp 孤儿回收的 load/save 两侧终态、
//!   半截快照的公开错误面、图三件套命名对齐（D-S3-01 集成级）、
//!   Strict 失败的生命周期（D-S3-07 的用户可见后果）。

#![allow(non_snake_case)]
// ⚠️ 本文件沿用旧所有权 API（`into_searcher` / `into_index`）以**锁住其行为不变**
// （`S8-02` 起二者为 `#[deprecated]` 薄封装）。生产调用点已在 `cli` / `examples` 迁移。
#![allow(deprecated)]

use std::sync::Arc;

use helix_core::embed::Embedder;
use helix_core::error::Error;
use helix_core::search::{
    GraphPersistMode, GraphStatus, SearchIndex, SearchIndexBuilder, VectorBackend,
};

/// 纯 BM25 装配（不依赖真实模型；集成层只能走公开 builder API）。
fn bm25_index() -> SearchIndex {
    SearchIndexBuilder::default().embedder(None).build()
}

/// S3-T1 集成层外观：save 覆盖写后 load 得新内容，且目录里只有目标文件
/// （tmp 被 rename 消费，不残留）。
#[test]
fn save覆盖写后load得到新内容且无tmp残留() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("atomic.idx");

    let mut idx = bm25_index();
    idx.add("第一版内容").unwrap();
    idx.save(&path).unwrap();

    idx.add("第二版新增内容").unwrap();
    idx.save(&path).unwrap();

    // 目录里只有目标文件——两次 save 的 tmp 都被 rename 消费
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["atomic.idx".to_string()], "不应有任何 tmp 残留");

    let loaded = SearchIndexBuilder::default()
        .embedder(None)
        .load(&path)
        .unwrap();
    assert_eq!(loaded.num_chunks(), 2, "load 必须得到新快照（两篇文档）");
}

/// 旧快照兼容回归（C3）：文件格式零变化——直写时代（旧内核）产出的
/// 完整快照文件，新内核照常加载；反之新内核产物旧内核也能读
/// （格式不携带原子性信息，roundtrip 即证）。
#[test]
fn 快照格式兼容回归() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("compat.idx");

    // 新内核 save
    let mut idx = bm25_index();
    idx.add("BM25 是经典关键词检索算法").unwrap();
    idx.add("向量检索计算余弦相似度").unwrap();
    idx.save(&path).unwrap();

    // load（等价于旧内核读新产物——格式未变）
    let loaded = SearchIndexBuilder::default()
        .embedder(None)
        .load(&path)
        .unwrap();
    assert_eq!(loaded.num_chunks(), 2);
    let resp = loaded.into_searcher().unwrap().search("检索").unwrap();
    assert!(
        !resp.hits.is_empty(),
        "doc_freq 语义应保留（两篇都含「检索」）"
    );
}

// ---------------------------------------------------------------------------
// S3-TI1 ~ TI5（评审后补强的集成覆盖）
// ---------------------------------------------------------------------------

const DIM: usize = 64;

/// 确定性伪随机 embedder（与 `graph_persist.rs` 同款：hash → 定长向量，
/// 跨进程可复现，不依赖真实模型）。
struct TestEmbedder {
    dim: usize,
}

impl Embedder for TestEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_documents(&self, texts: &[String]) -> helix_core::error::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| hash_vec(t, self.dim)).collect())
    }

    fn embed_query(&self, text: &str) -> helix_core::error::Result<Vec<f32>> {
        Ok(hash_vec(text, self.dim))
    }

    fn is_normalized(&self) -> bool {
        false
    }

    fn id(&self) -> &'static str {
        "test-embedder-v1"
    }
}

/// 文本 → 确定性向量（FNV + xorshift，跨进程可复现）。
fn hash_vec(text: &str, dim: usize) -> Vec<f32> {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
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

/// HNSW 装配（测试 embedder，图持久化默认开）。
fn hnsw_builder() -> SearchIndexBuilder {
    SearchIndexBuilder::default()
        .embedder(Some(Arc::new(TestEmbedder { dim: DIM })))
        .vector_backend(VectorBackend::Hnsw)
}

/// 拼与写侧 D-S3-01 同款的 tmp 路径（集成层拿不到 `tmp_path`，手工追加 `.tmp`）。
fn tmp_of(path: &std::path::Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".tmp");
    std::path::PathBuf::from(s)
}

/// **S3-TI1** ⚠️ load 回收快照 tmp 孤儿（D-S3-05 load 侧的公开 API 终态）。
///
/// 注入崩溃在集成层造不出来（钩子是 `pub(crate)`），但「崩溃后 tmp 孤儿存在」
/// 的**终态**可以直接伪造：手工写一个同名 `.tmp`（内容任意——原子协议下
/// tmp 永不权威，即便它比真源新也没有任何路径会去读它）。
/// load 成功后必须 best-effort 回收，而不是把垃圾留给下一个十年。
#[test]
fn TI1_load回收快照tmp孤儿() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ti1.idx");

    let mut idx = bm25_index();
    idx.add("孤儿回收前的内容").unwrap();
    idx.save(&path).unwrap();

    // 伪造上次崩溃残留的 tmp
    let tmp = tmp_of(&path);
    std::fs::write(&tmp, b"crash orphan garbage").unwrap();

    let loaded = SearchIndexBuilder::default()
        .embedder(None)
        .load(&path)
        .unwrap();
    assert_eq!(loaded.num_chunks(), 1);
    assert!(!tmp.exists(), "load 成功后 tmp 孤儿应被回收");
}

/// **S3-TI2** save 截断复用 tmp 孤儿（D-S3-05 save 侧）：同名 tmp 是上次崩溃
/// 的半截文件，`atomic_write` 的 `File::create` **天然截断**它并 rename 发布——
/// 无须先删（显式删反而引入「删了又建不出来」的空窗，设计 §4.1 步骤 1）。
#[test]
fn TI2_save截断复用tmp孤儿() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ti2.idx");
    let tmp = tmp_of(&path);

    let mut idx = bm25_index();
    idx.add("第一版").unwrap();
    idx.save(&path).unwrap();

    // 伪造半截 tmp（比真源「新」也无所谓——tmp 永不权威）
    std::fs::write(&tmp, b"half-written crash residue").unwrap();

    idx.add("第二版").unwrap();
    idx.save(&path).expect("save 必须截断复用孤儿 tmp 而非报错");
    assert!(!tmp.exists(), "成功 save 的 rename 应消费同名 tmp");

    let loaded = SearchIndexBuilder::default()
        .embedder(None)
        .load(&path)
        .unwrap();
    assert_eq!(loaded.num_chunks(), 2, "load 必须得到新快照而非孤儿内容");
}

/// **S3-TI3** ⚠️ 半截快照的公开错误面：**Err，绝不 panic**。
///
/// 原子写让「半截文件」在生产里不可达，但磁盘坏、外部截断等仍可能造出它——
/// 此时必须给出干净的错误（这是崩溃不变式「要么旧要么新」失败时的最后防线）：
/// - 正文截断（header 完整、正文缺尾）→ CRC 必失配 → `SnapshotCorrupted`；
/// - 头部截断（不足 12 字节）→ 读头 `Err`（Io）。
#[test]
fn TI3_半截快照报错不panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ti3.idx");

    let mut idx = bm25_index();
    idx.add("BM25 关键词检索").unwrap();
    idx.add("向量余弦相似度").unwrap();
    idx.save(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();

    // 中段截断：header 完整、正文缺尾 → CRC 失配
    let half = dir.path().join("ti3_half.idx");
    std::fs::write(&half, &bytes[..bytes.len() / 2]).unwrap();
    let err = match SearchIndexBuilder::default().embedder(None).load(&half) {
        Ok(_) => panic!("半截快照必须报错"),
        Err(e) => e,
    };
    assert!(
        matches!(err, Error::SnapshotCorrupted),
        "正文截断应为 SnapshotCorrupted，得到 {err:?}"
    );

    // 头部截断：不足 12 字节 header → 读头 Err（Io UnexpectedEof）
    let tiny = dir.path().join("ti3_tiny.idx");
    std::fs::write(&tiny, &bytes[..8]).unwrap();
    let err = match SearchIndexBuilder::default().embedder(None).load(&tiny) {
        Ok(_) => panic!("头部截断必须报错"),
        Err(e) => e,
    };
    assert!(matches!(err, Error::Io(_)), "头部截断应为 Io，得到 {err:?}");
}

/// **S3-TI4** ⚠️ 图三件套命名对齐（D-S3-01 的集成级证据）。
///
/// 含向量的 save 后目录**恰好**是快照 + 3 个 sidecar、零 `.tmp`（basename 拼错
/// 会产生随机后缀文件，S2-T10 单元层已兜；这里从目录清单角度再兜一次）。
/// 另：伪造的 manifest tmp 孤儿在下一次 save 中被 `write_manifest_atomic`
/// 的截断复用天然消费（dump 的 N2 前置删除只删 graph/data，不碰 manifest）。
#[test]
fn TI4_图三件套命名对齐零tmp() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ti4.idx");

    let mut idx = hnsw_builder().build();
    for i in 0..50 {
        idx.add(format!("文档 {i} 的内容：检索与向量")).unwrap();
    }
    idx.save(&path).unwrap();
    assert_eq!(
        idx.graph_status(),
        &GraphStatus::Loaded,
        "首次 save 应走完整链路"
    );

    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "ti4.idx".to_string(),
            "ti4.idx.hnsw.data".to_string(),
            "ti4.idx.hnsw.graph".to_string(),
            "ti4.idx.hnsw.manifest".to_string(),
        ],
        "目录应恰好是快照 + 三件套，不得有任何 tmp / 随机后缀残留"
    );

    // manifest tmp 孤儿 → 下一次 save 截断复用（消费而非残留）
    let manifest_tmp = dir.path().join("ti4.idx.hnsw.manifest.tmp");
    std::fs::write(&manifest_tmp, b"orphan").unwrap();
    idx.add("再补一篇文档").unwrap();
    idx.save(&path).unwrap();
    assert!(
        !manifest_tmp.exists(),
        "manifest tmp 孤儿应被下一次 save 的原子写消费"
    );

    // 重载仍走快路径（命名对齐的最终裁判：manifest 绑定的路径上就是新图）
    let loaded = hnsw_builder().load(&path).unwrap();
    assert_eq!(loaded.graph_status(), &GraphStatus::Loaded);
}

/// **S3-TI5** ⚠️ Strict 失败后的完整生命周期（D-S3-07 的用户可见后果）。
///
/// T18 只断言了「Err + 状态」，本测试补完生命周期：写图失败 → **快照已完整
/// 落盘且可加载**（降级重建；残留的旧 manifest 因 CRC 锚失配被拒，绝不假快路径）
/// → 清障 → 再次 save 回到快路径。Strict 的「失败上抛」绝不能让调用方丢索引。
#[test]
fn TI5_strict失败后快照仍可加载并恢复() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ti5.idx");

    let mut idx = hnsw_builder().graph_mode(GraphPersistMode::Strict).build();
    for i in 0..50 {
        idx.add(format!("文档 {i} 的内容")).unwrap();
    }
    idx.save(&path).unwrap();
    assert_eq!(idx.graph_status(), &GraphStatus::Loaded);

    // graph 落点被同名目录占据（C7：跨平台稳定的失败手法，chmod 对 CI root 无效）。
    // 首次 save 已有 graph 文件——先删掉它再放目录，等价于「file_dump 要建文件时
    // 发现落点是目录」的真实失败形态。
    let graph = helix_core::storage::graph_paths(&path).graph;
    std::fs::remove_file(&graph).unwrap();
    std::fs::create_dir(&graph).unwrap();

    idx.add("新增文档 CHANGEZZZ").unwrap();
    idx.save(&path)
        .expect_err("Strict 下写图失败必须升级为 Err");
    assert!(
        matches!(idx.graph_status(), GraphStatus::PersistFailed(_)),
        "失败状态应如实记录，实测 {:?}",
        idx.graph_status()
    );

    // 快照必须已是新内容且可加载：旧 manifest 若残留，CRC 锚必失配 → 降级重建
    let loaded = hnsw_builder().load(&path).unwrap();
    assert!(
        matches!(loaded.graph_status(), GraphStatus::Rebuilt(_)),
        "失败后的残留 sidecar 绝不能让 load 假装快路径，实测 {:?}",
        loaded.graph_status()
    );
    let hits = loaded
        .into_searcher()
        .unwrap()
        .search("新增文档 CHANGEZZZ")
        .unwrap();
    assert!(
        hits.hits.iter().any(|h| h.text.contains("CHANGEZZZ")),
        "Strict 失败后索引必须可用且含最后一次 save 的内容"
    );

    // 清障 → 再次 save → 恢复快路径
    std::fs::remove_dir_all(&graph).unwrap();
    idx.save(&path).unwrap();
    assert_eq!(
        idx.graph_status(),
        &GraphStatus::Loaded,
        "清障后再次 save 应回到快路径"
    );
}
