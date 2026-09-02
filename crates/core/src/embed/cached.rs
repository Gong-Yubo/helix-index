//! 查询向量缓存（FR-20）。
//!
//! Agent 多轮循环中大量 query 重复（"再查一下刚才那个"），缓存 `embed_query`
//! 的结果可避免重复调用嵌入模型。只缓存查询侧，**不缓存 `embed_documents`**
//! （入库是一次性的，且量大，缓存收益低）。
//!
//! 用 `moka`（并发安全 + 有界容量），不用 `lru`（非并发安全，需套 `Mutex`）。

use moka::sync::Cache;

use crate::error::Result;

use super::Embedder;

/// 带查询缓存的 Embedder 装饰器。
pub struct CachedEmbedder {
    inner: Box<dyn Embedder>,
    cache: Cache<String, Vec<f32>>,
}

impl CachedEmbedder {
    /// `capacity` 为缓存容量上限（LRU 淘汰）。
    pub fn new(inner: Box<dyn Embedder>, capacity: u64) -> Self {
        let cache = Cache::builder().max_capacity(capacity).build();
        Self { inner, cache }
    }

    pub fn into_inner(self) -> Box<dyn Embedder> {
        self.inner
    }
}

impl Embedder for CachedEmbedder {
    fn dim(&self) -> usize {
        self.inner.dim()
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // 入库侧不缓存
        self.inner.embed_documents(texts)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        if let Some(v) = self.cache.get(text) {
            return Ok(v);
        }
        let v = self.inner.embed_query(text)?;
        self.cache.insert(text.to_string(), v.clone());
        Ok(v)
    }

    fn is_normalized(&self) -> bool {
        self.inner.is_normalized()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn 缓存命中不重复计算() {
        // 用一个可共享计数的 Embedder：Arc<Mutex<usize>>
        struct SharedCounting(Arc<std::sync::Mutex<usize>>);
        impl Embedder for SharedCounting {
            fn dim(&self) -> usize {
                2
            }
            fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
                Ok(texts.iter().map(|t| vec![t.len() as f32, 0.0]).collect())
            }
            fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
                *self.0.lock().unwrap() += 1;
                Ok(vec![text.len() as f32, 0.0])
            }
        }

        let counter = Arc::new(std::sync::Mutex::new(0usize));
        let inner = SharedCounting(counter.clone());
        let cached = CachedEmbedder::new(Box::new(inner), 100);

        let _ = cached.embed_query("你好").unwrap();
        let _ = cached.embed_query("你好").unwrap();
        let _ = cached.embed_query("你好世界").unwrap();

        assert_eq!(*counter.lock().unwrap(), 2, "重复 key 应命中缓存，只算一次");
    }

    #[test]
    fn 并发命中无死锁() {
        struct Dummy;
        impl Embedder for Dummy {
            fn dim(&self) -> usize {
                1
            }
            fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
                Ok(texts.iter().map(|t| vec![t.len() as f32]).collect())
            }
            fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
                Ok(vec![text.len() as f32])
            }
        }

        let cached = Arc::new(CachedEmbedder::new(Box::new(Dummy), 100));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let c = cached.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    let _ = c.embed_query("并发测试").unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }
}
