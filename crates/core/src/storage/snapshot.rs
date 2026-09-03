//! 快照读写（T4-02 / T4-03）。
//!
//! 正文 = bincode(`Snapshot`)；文件 = 12 字节 header（magic + version + crc32）+ 正文。
//! 加载时校验 magic、版本、CRC，**任何一项不符都报错，绝不静默读错**。

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
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
}

/// 保存索引（含可选向量）到 `path`。
pub fn save(path: &Path, index: &Index, vectors: &[(ChunkId, Vec<f32>)]) -> Result<()> {
    let snapshot = Snapshot {
        sections: index.export(),
        vectors: vectors.to_vec(),
    };

    let body = bincode::serde::encode_to_vec(&snapshot, bincode::config::standard())
        .map_err(Error::Codec)?;

    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&body);
    let crc = hasher.finalize();

    let file = File::create(path).map_err(Error::Io)?;
    let mut w = BufWriter::new(file);
    codec::write_header(&mut w, crc).map_err(Error::Io)?;
    w.write_all(&body).map_err(Error::Io)?;
    w.flush().map_err(Error::Io)?;
    Ok(())
}

/// 加载结果：索引 + 原始向量（调用方据此重建向量索引）。
pub type LoadedSnapshot = (Index, Vec<(ChunkId, Vec<f32>)>);

/// 从 `path` 加载，返回 `(索引, 原始向量)`。向量索引由调用方重建。
pub fn load(path: &Path) -> Result<LoadedSnapshot> {
    let file = File::open(path).map_err(Error::Io)?;
    let mut r = BufReader::new(file);

    // 1. header（magic + 版本）
    let expected_crc = codec::read_header(&mut r)?;

    // 2. 正文 + CRC 校验
    let mut body = Vec::new();
    r.read_to_end(&mut body).map_err(Error::Io)?;
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&body);
    if hasher.finalize() != expected_crc {
        return Err(Error::SnapshotCorrupted);
    }

    // 3. 解码
    let snapshot: Snapshot = bincode::serde::decode_from_slice(&body, bincode::config::standard())
        .map_err(Error::Decode)
        .map(|(s, _)| s)?;

    Ok((Index::import(snapshot.sections), snapshot.vectors))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::MixedAnalyzer;
    use crate::document::{Chunk, Document};

    fn build_index() -> Index {
        let analyzer = MixedAnalyzer::new();
        let mut index = Index::new();
        for (i, text) in ["BM25 检索算法", "向量检索"].iter().enumerate() {
            let doc = Document {
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

    #[test]
    fn 快照roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.idx");

        let index = build_index();
        let vectors = vec![(0u32, vec![1.0, 0.0]), (1u32, vec![0.0, 1.0])];
        save(&path, &index, &vectors).unwrap();

        let (loaded, lv) = load(&path).unwrap();
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
        save(&path, &index, &[]).unwrap();
        let (_, lv) = load(&path).unwrap();
        assert!(lv.is_empty());
    }

    #[test]
    fn crc损坏拒绝() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.idx");
        let index = build_index();
        save(&path, &index, &[]).unwrap();

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
        save(&path, &index, &[]).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        // 只保留 header —— 解码失败或 CRC 失败，总之必须报错
        std::fs::write(&path, &bytes[..12]).unwrap();
        assert!(load(&path).is_err());
    }
}
