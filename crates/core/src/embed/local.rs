//! 本地 Embedder：fastembed + `Xenova/bge-small-zh-v1.5`（512 维）。
//!
//! - 查询侧加 BGE instruction 前缀，入库侧不加（风险 R2）
//! - 输出已 L2 归一化（fastembed 源码 `output.rs:49` 的 `.map(normalize)` 佐证）
//! - 缓存目录指到仓库外，避免误提交模型（p0-design.md 12.4 的遗留 TODO）

use std::sync::Mutex;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use crate::error::{Error, Result};

use super::Embedder;

/// BGE 中文查询侧的官方 instruction 前缀。
pub const BGE_ZH_QUERY_PREFIX: &str = "为这个句子生成表示以用于检索相关文章：";

/// 默认缓存目录（仓库外）。
fn default_cache_dir() -> std::path::PathBuf {
    dirs_home()
        .join(".cache")
        .join("helix-index")
        .join("models")
}

/// 解析用户主目录（无 `dirs` 依赖，读环境变量兜底）。
fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// 本地 embedding 实现。内部用 `Mutex` 包裹 `TextEmbedding`（其 `embed` 需 `&mut self`）。
pub struct LocalEmbedder {
    inner: Mutex<TextEmbedding>,
    dim: usize,
}

impl LocalEmbedder {
    /// 构造并触发模型下载（首次约 49s）。
    pub fn new() -> Result<Self> {
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallZHV15)
                .with_cache_dir(default_cache_dir())
                .with_show_download_progress(false),
        )
        .map_err(|e| Error::Embedding(e.to_string()))?;

        // 取模型维度（bge-small-zh-v1.5 = 512；`get_model_info` 是关联函数）
        let dim = TextEmbedding::get_model_info(&EmbeddingModel::BGESmallZHV15)
            .map(|info| info.dim)
            .unwrap_or(512);

        Ok(Self {
            inner: Mutex::new(model),
            dim,
        })
    }
}

impl Embedder for LocalEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut model = self.inner.lock().expect("embedder 锁已中毒");
        model
            .embed(texts, None)
            .map_err(|e| Error::Embedding(e.to_string()))
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let prefixed = format!("{BGE_ZH_QUERY_PREFIX}{text}");
        let mut model = self.inner.lock().expect("embedder 锁已中毒");
        let mut out = model
            .embed(vec![prefixed], None)
            .map_err(|e| Error::Embedding(e.to_string()))?;
        Ok(out.pop().expect("单条查询应返回一个向量"))
    }

    fn is_normalized(&self) -> bool {
        true
    }

    fn id(&self) -> &'static str {
        "bge-small-zh-v1.5"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 依赖模型下载，默认跳过：`cargo test -- --ignored`
    #[test]
    #[ignore]
    fn 查询侧前缀生效() {
        let e = LocalEmbedder::new().unwrap();
        let doc = e.embed_documents(&["检索".to_string()]).unwrap();
        let query = e.embed_query("检索").unwrap();
        // R2 防护：同一文本，查询侧与入库侧向量必须不同（前缀存在）
        assert_ne!(doc[0], query);
        assert_eq!(e.dim(), 512);
        assert!(e.is_normalized());
    }

    /// 依赖模型下载，默认跳过。
    #[test]
    #[ignore]
    fn 输出已归一化() {
        let e = LocalEmbedder::new().unwrap();
        let v = e.embed_documents(&["测试文本".to_string()]).unwrap();
        let norm: f32 = v[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3);
    }
}
