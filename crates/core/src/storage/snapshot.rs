//! 快照读写（T4-02 / T4-03）。
//!
//! 正文 = bincode(`Snapshot`)；文件 = 12 字节 header（magic + version + crc32）+ 正文。
//! 加载时校验 magic、版本、CRC，**任何一项不符都报错，绝不静默读错**。

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::index::{Index, SnapshotSections};
use crate::types::ChunkId;

use super::codec;

/// 快照正文（header 之后的部分）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub sections: SnapshotSections,
    /// 原始向量（可选 section：纯 BM25 索引为空）。
    /// 加载后由调用方重建向量索引（D1：存原始数据，不序列化 HNSW 图）。
    pub vectors: Vec<(ChunkId, Vec<f32>)>,
    /// 配置指纹（FORMAT_VERSION 2 新增，p6-design 8.2）：记录建库时的
    /// analyzer / embedder / 维度 / chunker，load 时校验装配一致性（修 B1/B2）。
    pub fingerprint: ConfigFingerprint,
}

/// 快照记录的配置指纹（p6-design 8.2）。
///
/// load 时与"当前装配"比对，不一致报 [`Error::ConfigMismatch`]——
/// 这是修 B1（`search --index` 写死 `MixedAnalyzer`，charabia 建库会静默换分词器）
/// 的根本手段：让"用错分词器"从静默降级变成显式报错。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigFingerprint {
    /// 分词器身份（`Analyzer::id`，如 "mixed" / "charabia"）
    pub analyzer_id: String,
    /// 模型身份（`Embedder::id`，如 "bge-small-zh-v1.5"；空串 = 纯 BM25）
    pub embedder_id: String,
    /// 向量维度（纯 BM25 为 0）
    pub dim: u32,
    /// 分块参数 (chunk_chars, overlap_chars)
    pub chunker: (usize, usize),
}

impl std::fmt::Display for ConfigFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "analyzer={}, embedder={}, dim={}, chunker={}/{}",
            self.analyzer_id,
            if self.embedder_id.is_empty() {
                "<none>"
            } else {
                &self.embedder_id
            },
            self.dim,
            self.chunker.0,
            self.chunker.1
        )
    }
}

/// 保存索引（含可选向量）到 `path`，返回快照**正文的 CRC32**（P0-6）。
///
/// 返回的 CRC 供图 sidecar manifest 的 `snapshot_crc` 填写——它是图的
/// 「版本锚点」：save 写出新快照后旧图立即失效（CRC 变了 → 降级重建）。
///
/// # 原子性（V2 Step 3 / FR-31）
///
/// 落盘走私有原语 `atomic_write`：tmp → 写入 → flush → `sync_all` →
/// rename → fsync 父目录。**rename 之前 `path` 从未被触碰**——崩溃后要么旧快照
/// 要么新快照，绝不产生半截文件（Q-C3 / `SnapshotCorrupted` 的根因由此消灭）。
/// 代价是 save 多两次 fsync（D-S3-06：接受、实测入档、不提供跳过开关）。
/// 文件格式零变化（C3：原子性是写协议变更，不是格式变更）。
pub fn save_with_crc(
    path: &Path,
    index: &Index,
    vectors: &[(ChunkId, Vec<f32>)],
    fingerprint: &ConfigFingerprint,
) -> Result<u32> {
    let snapshot = Snapshot {
        sections: index.export(),
        vectors: vectors.to_vec(),
        fingerprint: fingerprint.clone(),
    };

    let body = bincode::serde::encode_to_vec(&snapshot, bincode::config::standard())
        .map_err(Error::Codec)?;

    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&body);
    let crc = hasher.finalize();

    // 闭包而非 `&[u8]`（D-S3-02）：header 与正文各写各的，不拼整包
    // （52MB 级正文少一次全量拷贝）。
    super::atomic::atomic_write(path, |w| {
        codec::write_header(w, crc)?; // 12B header（codec 不动）
        w.write_all(&body)
    })?;
    Ok(crc)
}

/// 保存索引（含可选向量）到 `path`（兼容签名，内部转发并丢弃 CRC）。
pub fn save(
    path: &Path,
    index: &Index,
    vectors: &[(ChunkId, Vec<f32>)],
    fingerprint: &ConfigFingerprint,
) -> Result<()> {
    save_with_crc(path, index, vectors, fingerprint).map(|_| ())
}

/// 加载结果：索引 + 原始向量 + 配置指纹（调用方据此重建向量索引 + 校验装配）。
pub type LoadedSnapshot = (Index, Vec<(ChunkId, Vec<f32>)>, ConfigFingerprint);

/// 带正文 CRC 的加载结果：`snapshot_crc` 供图 manifest 校验（§4.6 步骤 4）。
pub type LoadedSnapshotWithCrc = (Index, Vec<(ChunkId, Vec<f32>)>, ConfigFingerprint, u32);

/// 从 `path` 加载，带出快照正文的 CRC32（供图 sidecar 的版本锚点校验）。
pub fn load_with_crc(path: &Path) -> Result<LoadedSnapshotWithCrc> {
    let file = File::open(path).map_err(Error::Io)?;
    let mut r = BufReader::new(file);

    // 1. header（magic + 版本）
    let expected_crc = codec::read_header(&mut r)?;

    // 2. 正文 + CRC 校验
    let mut body = Vec::new();
    r.read_to_end(&mut body).map_err(Error::Io)?;
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&body);
    let actual_crc = hasher.finalize();
    if actual_crc != expected_crc {
        return Err(Error::SnapshotCorrupted);
    }

    // 3. 解码
    let snapshot: Snapshot = bincode::serde::decode_from_slice(&body, bincode::config::standard())
        .map_err(Error::Decode)
        .map(|(s, _)| s)?;

    // 4. tmp 孤儿回收（V2 Step 3 / D-S3-05）：快照本体已验证完好，
    // 同名 tmp 必为上次崩溃的孤儿（原子协议下 tmp 永不权威——即便它比
    // 快照新，也没有任何路径会去读它）。best-effort 删除，失败不影响加载。
    let _ = std::fs::remove_file(super::atomic::tmp_path(path));

    Ok((
        Index::import(snapshot.sections),
        snapshot.vectors,
        snapshot.fingerprint,
        actual_crc,
    ))
}

/// 从 `path` 加载，返回 `(索引, 原始向量, 配置指纹)`。向量索引由调用方重建。
pub fn load(path: &Path) -> Result<LoadedSnapshot> {
    load_with_crc(path).map(|(i, v, f, _)| (i, v, f))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::MixedAnalyzer;
    use crate::document::{Chunk, DocRecord};

    fn build_index() -> Index {
        let analyzer = MixedAnalyzer::new();
        let mut index = Index::new();
        for (i, text) in ["BM25 检索算法", "向量检索"].iter().enumerate() {
            let doc = DocRecord {
                doc_id: 0,
                source: format!("doc-{i}"),
                metadata: serde_json::json!({"tag": "rust"}),
                content_hash: 100 + i as u64,
            };
            let chunk = Chunk {
                chunk_id: 0,
                doc_id: 0,
                ordinal: 0,
                text: text.to_string(),
                char_start: 0,
                char_end: 0,
            };
            index.add(doc, vec![chunk], &analyzer).unwrap();
        }
        index
    }

    fn fingerprint() -> ConfigFingerprint {
        ConfigFingerprint {
            analyzer_id: "mixed".to_string(),
            embedder_id: String::new(),
            dim: 0,
            chunker: (512, 64),
        }
    }

    #[test]
    fn 快照roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.idx");

        let index = build_index();
        let vectors = vec![(0u32, vec![1.0, 0.0]), (1u32, vec![0.0, 1.0])];
        save(&path, &index, &vectors, &fingerprint()).unwrap();

        let (loaded, lv, _fp) = load(&path).unwrap();
        assert_eq!(loaded.num_chunks(), index.num_chunks());
        assert_eq!(loaded.total_len(), index.total_len());
        assert_eq!(lv, vectors);

        // 检索能力保留：doc_freq 一致
        let analyzer = MixedAnalyzer::new();
        let _ = analyzer;
        assert_eq!(loaded.doc_freq("bm25"), index.doc_freq("bm25"));
        assert_eq!(loaded.doc_freq("检索"), index.doc_freq("检索"));
    }

    #[test]
    fn 空向量快照() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.idx");
        let index = build_index();
        save(&path, &index, &[], &fingerprint()).unwrap();
        let (_, lv, _fp) = load(&path).unwrap();
        assert!(lv.is_empty());
    }

    #[test]
    fn crc损坏拒绝() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.idx");
        let index = build_index();
        save(&path, &index, &[], &fingerprint()).unwrap();

        // 篡改正文一个字节（header 12 字节之后）
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        assert!(matches!(load(&path), Err(Error::SnapshotCorrupted)));
    }

    #[test]
    fn 截断文件拒绝() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trunc.idx");
        let index = build_index();
        save(&path, &index, &[], &fingerprint()).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        // 只保留 header —— 解码失败或 CRC 失败，总之必须报错
        std::fs::write(&path, &bytes[..12]).unwrap();
        assert!(load(&path).is_err());
    }

    /// P0-6：save_with_crc 与 load_with_crc 的 CRC 必须一致（图的版本锚点）。
    #[test]
    fn with_crc往返一致() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crc.idx");
        let index = build_index();
        let vectors = vec![(0u32, vec![1.0, 0.0])];
        let saved_crc = save_with_crc(&path, &index, &vectors, &fingerprint()).unwrap();

        let (_, lv, _, loaded_crc) = load_with_crc(&path).unwrap();
        assert_eq!(saved_crc, loaded_crc, "save 与 load 的正文 CRC 必须一致");
        assert_eq!(lv, vectors);
    }

    /// 快照重写后 CRC 必须变化（旧图 manifest 因此自然失效）。
    #[test]
    fn 快照变更后crc变化() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rev.idx");
        let mut index = build_index();
        let v1 = vec![(0u32, vec![1.0, 0.0])];
        let c1 = save_with_crc(&path, &index, &v1, &fingerprint()).unwrap();

        // 再加一篇文档后重写
        let analyzer = MixedAnalyzer::new();
        let doc = DocRecord {
            doc_id: 0,
            source: "doc-x".to_string(),
            metadata: serde_json::json!({}),
            content_hash: 999,
        };
        let chunk = Chunk {
            chunk_id: 0,
            doc_id: 0,
            ordinal: 0,
            text: "新增文本".to_string(),
            char_start: 0,
            char_end: 0,
        };
        index.add(doc, vec![chunk], &analyzer).unwrap();
        let v2 = vec![(0u32, vec![1.0, 0.0]), (1u32, vec![0.0, 1.0])];
        let c2 = save_with_crc(&path, &index, &v2, &fingerprint()).unwrap();

        assert_ne!(c1, c2, "内容不同 CRC 应不同（版本锚点的成立前提）");
    }

    /// V2 Step 3 / D-S3-05：load 成功后 best-effort 回收快照 tmp 孤儿。
    /// 这是唯一能回收「最后一次 save 崩溃残留」的时机（那次 save 不会再来了）。
    #[test]
    fn load后回收tmp孤儿() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orphan.idx");
        let index = build_index();
        save(&path, &index, &[], &fingerprint()).unwrap();

        // 伪造上次崩溃残留的 tmp 孤儿（内容任意——tmp 永不权威）
        let tmp = crate::storage::atomic::tmp_path(&path);
        std::fs::write(&tmp, b"orphan").unwrap();

        load(&path).unwrap(); // 必须成功（孤儿不影响加载）
        assert!(!tmp.exists(), "load 成功后 tmp 孤儿应被回收");

        // 孤儿不影响 CRC 语义
        let (_, _, _, crc) = load_with_crc(&path).unwrap();
        let saved = save_with_crc(&path, &index, &[], &fingerprint()).unwrap();
        assert_eq!(crc, saved);
    }
}
