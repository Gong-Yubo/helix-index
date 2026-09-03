//! posting 列表项：某个词项在某个分片中的出现记录。

use serde::{Deserialize, Serialize};

use crate::types::ChunkId;

/// 倒排表项
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Posting {
    /// 命中该词项的分片 ID
    pub chunk_id: ChunkId,
    /// 该词项在此分片中的词频（term frequency）
    pub tf: u32,
    /// 词项在分片中的所有位置（仅在 `positions` feature 下存在）
    #[cfg(feature = "positions")]
    pub positions: Vec<u32>,
}

impl Posting {
    pub fn new(chunk_id: ChunkId, tf: u32) -> Self {
        Self {
            chunk_id,
            tf,
            #[cfg(feature = "positions")]
            positions: Vec::new(),
        }
    }

    #[cfg(feature = "positions")]
    pub fn with_positions(chunk_id: ChunkId, tf: u32, positions: Vec<u32>) -> Self {
        Self {
            chunk_id,
            tf,
            positions,
        }
    }
}
