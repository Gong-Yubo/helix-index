//! 远程 Embedder（HTTP，占位）。
//!
//! **推迟到有实际远程服务需求时实现**（p2-design.md D4）。
//! 当前只保留类型占位与 `Error::NoEmbedder` 的语义说明；
//! 实际 HTTP 客户端在 `remote-embed` feature 下于后续阶段落地。

use crate::error::{Error, Result};

use super::Embedder;

/// 远程 HTTP embedding 客户端（占位，未实现）。
#[allow(dead_code)] // remote-embed feature 未启用时是占位
pub struct RemoteEmbedder;

impl Embedder for RemoteEmbedder {
    fn dim(&self) -> usize {
        0
    }

    fn embed_documents(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::NoEmbedder)
    }

    fn embed_query(&self, _text: &str) -> Result<Vec<f32>> {
        Err(Error::NoEmbedder)
    }
}
