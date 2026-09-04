//! 写端门面：`SearchIndex`（拥有型，持有全部装配与状态）。
//!
//! # 可见性语义（p6-design 6.3）
//!
//! `add` 之后必须 `commit()` 才对检索可见（对齐 Lucene / tantivy）。
//! `into_searcher()` 隐含 flush（把 `pending` 刷进 `Inner`，p6-design 6.4 评审 P2）。

use std::sync::Arc;

use crate::document::{content_hash, DocRecord, Document};
use crate::error::{Error, Result};
use crate::index::Index;
use crate::types::{ChunkId, DocId};
use crate::vector::{BruteForceIndex, HnswRsIndex, NormalizedVector, VectorIndex};

use super::config::{Config, SearchIndexBuilder, VectorBackend};

/// 已提交状态（`Arc` 共享：`into_searcher` 零拷贝移交给读端）。
pub(crate) struct Inner {
    /// 倒排 + 正排 + 统计量
    pub index: Index,
    /// 向量索引（`None` = 纯 BM25）
    pub vector_index: Option<Box<dyn VectorIndex>>,
    /// 原始向量（快照策略 D1：存原始向量不存 HNSW 图；`None` = 未启用 keep_raw）
    pub raw_vectors: Option<Vec<(ChunkId, Vec<f32>)>>,
}

/// 待 embed 的缓冲项（写缓冲，p6-design 6.2）。
pub(crate) struct PendingChunk {
    /// 分片 ID（`Index::add` 已分配）
    pub chunk_id: ChunkId,
    /// 分片正文（embed 输入）
    pub text: String,
}

/// `add(doc)` 的结果（p6-design 4.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddOutcome {
    /// 分配的文档 ID（幂等命中时为既有 ID）
    pub doc_id: DocId,
    /// 新增的分片 ID（幂等命中时为 `[]`）
    pub chunk_ids: Vec<ChunkId>,
    /// 是否命中幂等去重（FR-15）
    pub deduped: bool,
}

/// 写端门面（拥有型）。持有 analyzer / chunker / embedder / fusion / reranker /
/// 倒排 / 向量索引 / 写缓冲，暴露 `add(doc)` / `commit()` / `into_searcher()`。
///
/// 注意：`SearchIndex` 是 **`!Sync`**（含可变 `pending`，`add(&mut self)`）。
/// 它可 `Send`（能整体 move 到别的线程），但不可跨线程共享（p6-design 5.3）。
///
/// # 所有权切换（p6-design 6.4 方案 A 的实现细化）
///
/// `inner` 直接持有 `Inner`（**非** `Arc`）。`into_searcher()` 时包成 `Arc` 移交；
/// `Searcher::into_index()` 用 `Arc::try_unwrap` 解包——refcount==1 零拷贝取出，
/// refcount>1（有 `Searcher` clone 残留）时**明确报错**而非静默深拷贝。
/// （设计 6.4 的 P3 修正假设 `Arc::make_mut` 静默深拷贝，但 `Box<dyn VectorIndex>`
/// 不可 `Clone` 使该路径无法编译；`try_unwrap` 是更诚实的等价实现。）
pub struct SearchIndex {
    pub(crate) cfg: Arc<Config>,
    pub(crate) inner: Inner,
    pub(crate) pending: Vec<PendingChunk>,
}

impl SearchIndex {
    /// 配置入口：全部组件可选，不调即用默认值（零配置可用）。
    pub fn builder() -> SearchIndexBuilder {
        SearchIndexBuilder::default()
    }

    /// 从已构建的 `Config` 与后端选择组装空索引。
    pub(crate) fn from_config(cfg: Config, backend: VectorBackend) -> Self {
        let vector_index = match cfg.embedder.as_ref() {
            Some(_) => Some(match backend {
                VectorBackend::Brute => Box::new(BruteForceIndex::new()) as Box<dyn VectorIndex>,
                VectorBackend::Hnsw => {
                    Box::new(HnswRsIndex::with_capacity(1024)) as Box<dyn VectorIndex>
                }
            }),
            None => None,
        };
        let inner = Inner {
            index: Index::new(),
            vector_index,
            raw_vectors: Some(Vec::new()),
        };
        Self {
            cfg: Arc::new(cfg),
            inner,
            pending: Vec::new(),
        }
    }

    /// 摄入一篇文档：查重 → 分块 → 倒排 → 写缓冲（p6-design 6.1）。
    ///
    /// - **幂等 upsert（FR-15）**：`content_hash = xxh64(dedup_key.unwrap_or(text))`
    ///   命中已存在文档时直接返回 `deduped = true`，不重复分块/插入。
    /// - 向量化**延后到 `flush`**（批量 embed，NFR-03）；此刻只进倒排与写缓冲。
    /// - 缓冲满（`batch_size`）时自动同步 flush。
    pub fn add(&mut self, doc: impl Into<Document>) -> Result<AddOutcome> {
        let doc = doc.into();
        let text = doc.text.clone();
        let hash = content_hash(doc.dedup_key.as_deref().unwrap_or(&text));

        // 短路查重（与 Index::add 内部去重语义一致）
        if let Some(existing) = self.inner.index.doc_id_by_hash(hash) {
            return Ok(AddOutcome {
                doc_id: existing,
                chunk_ids: Vec::new(),
                deduped: true,
            });
        }

        let record = DocRecord {
            doc_id: 0,
            source: doc.source.clone(),
            metadata: doc.metadata.clone(),
            content_hash: hash,
        };

        // 分块 → 倒排（立即完成）
        let chunks = self.cfg.chunker.chunk(0, &text);
        let chunk_texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let (doc_id, chunk_ids) =
            self.inner
                .index
                .add(record, chunks, self.cfg.analyzer.as_ref())?;

        // 进写缓冲（有向量侧时才需要 embed）
        if self.cfg.embedder.is_some() {
            for (chunk_id, text) in chunk_ids.iter().zip(chunk_texts) {
                self.pending.push(PendingChunk {
                    chunk_id: *chunk_id,
                    text,
                });
            }
            if self.pending.len() >= self.cfg.batch_size {
                self.flush()?;
            }
        }

        Ok(AddOutcome {
            doc_id,
            chunk_ids,
            deduped: false,
        })
    }

    /// 批量摄入（吞吐最优路径，`helix build` 走这条）。
    ///
    /// 与逐条 `add` 的区别：一次性把全部文档 chunk 完、分批 embed，
    /// 避免逐条的缓冲检查与 Arc::make_mut 抖动。
    pub fn add_documents<I>(&mut self, docs: I) -> Result<Vec<AddOutcome>>
    where
        I: IntoIterator<Item = Document>,
    {
        docs.into_iter().map(|d| self.add(d)).collect()
    }

    /// 强制冲刷写缓冲：批量 embed + L2 归一化 + 灌向量索引。
    ///
    /// - **L2 归一化在此内部调用 `NormalizedVector::new` 保证**（消除静默陷阱 ⑥）
    /// - 纯 BM25（无 embedder）时为 no-op
    pub fn flush(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let Some(embedder) = self.cfg.embedder.as_ref() else {
            // 无向量侧：缓冲不应存在（add 里已短路）；防御性清空
            self.pending.clear();
            return Ok(());
        };

        let texts: Vec<String> = self.pending.iter().map(|p| p.text.clone()).collect();
        let vecs = embedder.embed_documents(&texts)?;
        debug_assert_eq!(vecs.len(), texts.len(), "embed 输出条数应与输入一致");

        // 归一化 + 灌向量索引 + 记录原始向量（快照用）
        let vi = self.inner.vector_index.as_mut().ok_or(Error::NoEmbedder)?;
        for (pending, v) in self.pending.iter().zip(vecs) {
            let nv = NormalizedVector::new(v);
            if let Some(raw) = self.inner.raw_vectors.as_mut() {
                raw.push((pending.chunk_id, nv.as_slice().to_vec()));
            }
            vi.add(pending.chunk_id, nv)?;
        }
        self.pending.clear();
        Ok(())
    }

    /// 使已摄入的文档对检索可见（对齐 Lucene `commit`，p6-design 6.3）。
    ///
    /// 本轮实现中 `commit()` 与 `flush()` 等价（都是刷写缓冲）；真正的
    /// delta 分段（`commit` = 刷成新 segment）留 P7，届时 API 形态不变。
    pub fn commit(&mut self) -> Result<()> {
        self.flush()
    }

    /// 当前已提交的分片数（不含 pending）。
    pub fn num_chunks(&self) -> u32 {
        self.inner.index.num_chunks()
    }

    /// 交出所有权，产出只读 `Searcher`（`'static + Clone + Send + Sync`）。
    ///
    /// **隐含 flush**（p6-design 6.4 评审 P2）：把 `pending` 刷进 `Inner` 后再移交，
    /// 否则未 embed 的分片在检索侧不可见。`Inner` 包成 `Arc` 移交给读端，零拷贝。
    pub fn into_searcher(mut self) -> Result<crate::search::Searcher> {
        self.flush()?;
        Ok(crate::search::Searcher {
            cfg: self.cfg,
            inner: Arc::new(self.inner),
        })
    }

    /// 删除一个文档（及其全部分片），含统计量回滚（复用 `Index::remove`）。
    ///
    /// 注意：墓碑删除只影响倒排/正排；向量索引里的向量条目随 chunk_id 失效
    /// （检索时回捞会跳过墓碑 chunk，见 `search_parts`）。若之后 `save`，
    /// `raw_vectors` 中对应的旧向量条目会被下次 flush/重建丢弃。
    pub fn remove(&mut self, doc_id: DocId) -> Result<()> {
        self.inner
            .index
            .remove(doc_id, self.cfg.analyzer.as_ref())
    }

    /// 落盘快照（**隐含 commit**，p6-design 6.2：不允许带未刷缓冲落盘）。
    ///
    /// 快照写入当前装配的配置指纹（p6-design 8.2），供 `load` 校验。
    pub fn save(&mut self, path: &std::path::Path) -> Result<()> {
        self.commit()?;
        let vectors: Vec<(ChunkId, Vec<f32>)> = self
            .inner
            .raw_vectors
            .as_deref()
            .unwrap_or(&[])
            .to_vec();
        crate::storage::save(path, &self.inner.index, &vectors, &self.cfg.fingerprint())
    }

    /// 从快照加载（默认装配）。
    ///
    /// - 快照存**原始数据 + 原始向量**（D1），加载后重建倒排 + 向量索引
    /// - **配置指纹校验**（p6-design 8.2 / 修 B1/B2）：快照记录的指纹与当前
    ///   默认装配不一致时报 [`Error::ConfigMismatch`]，绝不静默换分词器/模型
    /// - 若快照含向量但当前装配无 embedder，向量不重建（纯 BM25）
    ///
    /// 加载**非默认装配**的快照（如 charabia）请用
    /// `SearchIndexBuilder::load(path)`（先按已知配置装配、再 load）。
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let builder = SearchIndexBuilder::default();
        let cfg = builder.build_config();
        let backend = builder.backend();
        Self::load_with(cfg, backend, path)
    }

    /// 按给定装配从快照加载（`load` 的实现主体）。
    pub(crate) fn load_with(
        cfg: Config,
        backend: VectorBackend,
        path: &std::path::Path,
    ) -> Result<Self> {
        let (index, raw_vectors, fingerprint) = crate::storage::load(path)?;

        // 配置指纹校验：装配不一致显式报错（修 B1/B2）
        let actual = cfg.fingerprint();
        if fingerprint != actual {
            return Err(Error::ConfigMismatch {
                expected: fingerprint.to_string(),
                actual: actual.to_string(),
            });
        }

        // 重建向量索引（D1：不序列化 HNSW 图，用原始向量重建）
        let vector_index = match (cfg.embedder.as_ref(), raw_vectors.is_empty()) {
            (Some(_), false) => Some(match backend {
                VectorBackend::Brute => {
                    let entries: Vec<(ChunkId, NormalizedVector)> = raw_vectors
                        .iter()
                        .map(|(id, v)| (*id, NormalizedVector::new(v.clone())))
                        .collect();
                    Box::new(BruteForceIndex::from_entries(entries)) as Box<dyn VectorIndex>
                }
                VectorBackend::Hnsw => {
                    let mut vi = HnswRsIndex::with_capacity(raw_vectors.len().max(1024));
                    for (id, v) in &raw_vectors {
                        vi.add(*id, NormalizedVector::new(v.clone()))?;
                    }
                    Box::new(vi) as Box<dyn VectorIndex>
                }
            }),
            _ => None,
        };

        Ok(Self {
            cfg: Arc::new(cfg),
            inner: Inner {
                index,
                vector_index,
                raw_vectors: Some(raw_vectors),
            },
            pending: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;

    fn bm25_index() -> SearchIndex {
        let cfg = SearchIndexBuilder::default().embedder(None).build_config();
        SearchIndex::from_config(cfg, VectorBackend::Brute)
    }

    #[test]
    fn 空索引可构造() {
        let idx = bm25_index();
        assert_eq!(idx.num_chunks(), 0);
        assert!(idx.pending.is_empty());
    }

    #[test]
    fn add后未commit也能查到倒排() {
        // 倒排在 add 时立即写入（可见性语义只影响向量侧 flush）
        let mut idx = bm25_index();
        let out = idx.add("BM25 检索算法").unwrap();
        assert!(!out.deduped);
        assert_eq!(out.chunk_ids.len(), 1);
        assert_eq!(idx.num_chunks(), 1);
    }

    #[test]
    fn 幂等upsert返回deduped() {
        let mut idx = bm25_index();
        let a = idx.add("BM25 检索算法").unwrap();
        let b = idx.add("BM25 检索算法").unwrap();
        assert!(!a.deduped);
        assert!(b.deduped, "第二次同内容应 deduped");
        assert_eq!(a.doc_id, b.doc_id);
        assert!(b.chunk_ids.is_empty());
        assert_eq!(idx.num_chunks(), 1, "不应产生重复分片");
    }

    #[test]
    fn dedup_key替代text哈希() {
        let mut idx = bm25_index();
        let a = idx
            .add(Document::new("文本A").with_dedup_key("key-1"))
            .unwrap();
        // 不同 text 但同 dedup_key → 视为同一文档
        let b = idx
            .add(Document::new("文本B").with_dedup_key("key-1"))
            .unwrap();
        assert!(b.deduped);
        assert_eq!(a.doc_id, b.doc_id);
    }

    #[test]
    fn commit与flush均为noop无向量() {
        let mut idx = bm25_index();
        idx.add("文本一").unwrap();
        idx.commit().unwrap();
        idx.flush().unwrap();
        assert_eq!(idx.num_chunks(), 1);
    }

    #[test]
    fn 纯文本String与str均可add() {
        let mut idx = bm25_index();
        idx.add(String::from("字符串")).unwrap();
        idx.add("字符串字面量").unwrap();
        assert_eq!(idx.num_chunks(), 2);
    }

    #[test]
    fn save后load快照roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("search.idx");

        let mut idx = bm25_index();
        idx.add("BM25 是经典关键词检索算法").unwrap();
        idx.add("向量检索计算余弦相似度").unwrap();
        idx.save(&path).unwrap();

        // 纯 BM25 快照：按同装配（embedder=None）load，指纹一致
        let loaded = SearchIndexBuilder::default()
            .embedder(None)
            .load(&path)
            .unwrap();
        assert_eq!(loaded.num_chunks(), idx.num_chunks());

        // 检索能力保留：doc_freq 一致
        assert_eq!(
            loaded.inner.index.doc_freq("检索"),
            idx.inner.index.doc_freq("检索")
        );
    }

    #[test]
    fn remove删除文档并回滚统计量() {
        let mut idx = bm25_index();
        let out = idx.add("BM25 检索算法").unwrap();
        assert_eq!(idx.num_chunks(), 1);
        idx.remove(out.doc_id).unwrap();
        assert_eq!(idx.num_chunks(), 0);
    }

    #[test]
    fn 装配不一致load报ConfigMismatch() {
        use crate::analyze::Analyzer;

        // 用 id 非默认的自定义 analyzer 建库（模拟 charabia 等非默认装配）
        struct CustomAnalyzer;
        impl Analyzer for CustomAnalyzer {
            fn analyze_doc(&self, text: &str) -> Vec<crate::analyze::Token> {
                crate::analyze::MixedAnalyzer::new().analyze_doc(text)
            }
            fn id(&self) -> &'static str {
                "custom-analyzer"
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mismatch.idx");

        // 用 custom analyzer 建库 + 落盘
        let mut idx = SearchIndex::from_config(
            SearchIndexBuilder::default()
                .embedder(None)
                .analyzer(Arc::new(CustomAnalyzer))
                .build_config(),
            VectorBackend::Brute,
        );
        idx.add("BM25 检索算法").unwrap();
        idx.save(&path).unwrap();

        // 用默认（mixed）装配 load → 必须报 ConfigMismatch（修 B1）
        let err = match SearchIndex::load(&path) {
            Ok(_) => panic!("装配不一致应报 ConfigMismatch"),
            Err(e) => e,
        };
        assert!(
            matches!(err, Error::ConfigMismatch { .. }),
            "应为 ConfigMismatch，得到 {err:?}"
        );
    }
}
