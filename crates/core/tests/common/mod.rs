//! 集成测试共享的 helper（`tests/common/`）。
//!
//! Rust 集成测试各自是独立 crate，无法共享私有 item；把通用确定性 Embedder 与
//! 文本→向量函数收敛到这里，避免在多份测试文件里重复拷贝（评审建议）。

#![allow(dead_code)] // 供各测试文件按需引用

use helix_core::chunk::Chunker;
use helix_core::embed::Embedder;
use helix_core::error::Result;
use helix_core::search::{SearchIndexBuilder, VectorBackend};

/// 测试用确定性 Embedder（不依赖真实模型，跨机器可复现）。
///
/// 所有 step4 / graph 测试统一用这一个，保证 save→load 的配置指纹一致
/// （fingerprint 含 embedder id + dim）。
pub struct TestEmbedder {
    pub dim: usize,
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

    fn id(&self) -> &'static str {
        "test-embedder-step4-v1"
    }
}

/// 文本 → 确定性向量（LCG，跨进程可复现）。
pub fn hash_vec(text: &str, dim: usize) -> Vec<f32> {
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

/// 向量维度（小维度够测，快）。
pub const DIM: usize = 32;

/// 装配：确定性 embedder + Hnsw + **强制单 chunk**（`Chunker::new(200_000, 0)`）——
/// 让「一个 doc == 一个 chunk_id」，便于断言具体 chunk 的回收。
pub fn builder_hnsw() -> SearchIndexBuilder {
    SearchIndexBuilder::default()
        .embedder(Some(std::sync::Arc::new(TestEmbedder { dim: DIM })))
        .vector_backend(VectorBackend::Hnsw)
        .chunker(Chunker::new(200_000, 0))
        // 写缓冲放大：保证小批量 add 时 pending 不自动 flush（T13 构造前提）
        .batch_size(1024)
}

/// 装配：Brute 后端（无图，确定性精确检索）。
pub fn builder_brute() -> SearchIndexBuilder {
    SearchIndexBuilder::default()
        .embedder(Some(std::sync::Arc::new(TestEmbedder { dim: DIM })))
        .vector_backend(VectorBackend::Brute)
        .chunker(Chunker::new(200_000, 0))
        .batch_size(1024)
}
