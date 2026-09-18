//! 读端门面：owned `Searcher`（`'static + Clone + Send + Sync`）。
//!
//! # 与 `QueryExecutor` 的关系（p6-design 5.2）
//!
//! `Searcher` 与借用型 [`crate::query::QueryExecutor`] 共用同一个编排内核
//! [`crate::query::searcher::search_parts`]——每次检索把自己的状态投影成
//! `SearchParts` 再调用，**零逻辑重复**。

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::query::searcher::{search_parts, SearchParts};
use crate::query::{SearchMode, SearchResponse};
use crate::schema::Filter;

use super::index::SearchIndex;
use super::view::{Shared, View};

/// 只读检索器（owned）。持有与写端**共享**的 `Shared`（`S8-02` 起）。
///
/// - `'static + Clone + Send + Sync`（G3）：可直接放进 axum `AppState`、
///   可 `Arc<Searcher>` 分发、可 `move` 进 rayon 任务。
/// - `search(query)` 只有 query 必选；`search_with(query)` 承载可选参数。
///
/// # 可见性（`S8-02` / NFR-11）
///
/// 本类型**不隐含 `flush()`**：它读的是 `shared.view` 的**当前快照**，
/// 因此「能查到什么」完全由写端是否 `commit()` 决定（`commit()` 是唯一的可见性边界）。
/// 旧的所有权型 API `SearchIndex::into_searcher()`（隐含 flush）已 `#[deprecated]`。
#[derive(Clone)]
pub struct Searcher {
    /// 与写端共享的视图 + 装配
    pub(crate) shared: Arc<Shared>,
    /// 最近一次图 sidecar 状态（NFR-07 可观测性，读端也能查；快照语义）
    pub(crate) graph_status: crate::search::index::GraphStatus,
}

impl Searcher {
    /// 检索默认形态：**只有 query 必选**。
    ///
    /// - `mode` 自动推断：有向量侧 → `Hybrid`，否则 → `Bm25`（p6-design 7.1）
    /// - `top_n` 默认 10
    pub fn search(&self, query: &str) -> Result<SearchResponse> {
        self.search_with(query).exec()
    }

    /// 检索 builder 形态：承载可选参数（`mode` / `top_n` / `filter`）。
    pub fn search_with<'a>(&'a self, query: &'a str) -> SearchRequest<'a> {
        SearchRequest {
            searcher: self,
            query,
            mode: None,
            top_n: 10,
            filter: None,
        }
    }

    /// 把 `view` 投影成借用型 `SearchParts`（编排内核的输入）。
    ///
    /// ⚠️ `S8-02` 期 `deltas` 恒空 ⇒ 只用 `view.main`；跨段在 `S8-04` / `S8-05`。
    fn parts<'a>(&'a self, view: &'a View) -> SearchParts<'a> {
        SearchParts {
            index: &view.main.index,
            analyzer: self.shared.cfg.analyzer.as_ref(),
            embedder: self.shared.cfg.embedder.as_ref().map(|e| e.as_ref()),
            vector_index: view.main.vector_index.as_ref().map(|vi| vi.as_ref()),
            fusion: self.shared.cfg.fusion.as_ref(),
            reranker: self.shared.cfg.reranker.as_ref(),
            bm25_params: self.shared.cfg.bm25_params,
        }
    }

    /// 是否有向量侧（决定默认 mode）。
    fn has_vector(&self, view: &View) -> bool {
        self.shared.cfg.embedder.is_some() && view.main.vector_index.is_some()
    }

    /// 按装配推断默认 mode：有向量 → `Hybrid`，否则 → `Bm25`。
    fn infer_mode(&self, view: &View) -> SearchMode {
        if self.has_vector(view) {
            SearchMode::Hybrid
        } else {
            SearchMode::Bm25
        }
    }

    /// 执行检索（内部实现，供 `SearchRequest::exec` 调用）。
    ///
    /// ⚠️ `view` 由调用方**取一次**并传入（`I8-3`：一次检索只用一个 `View`）。
    pub(crate) fn run(
        &self,
        view: &View,
        query: &str,
        mode: SearchMode,
        top_n: usize,
        filter: Option<&Filter>,
    ) -> Result<SearchResponse> {
        let parts = self.parts(view);
        search_parts(&parts, query, mode, top_n, filter)
    }

    /// 换回写端（用同一 `Arc<Shared>` 造一个新写端）。
    ///
    /// ⚠️ **已废弃**：`S8-02` 起写端不必「换回」——`SearchIndex::searcher(&self)` 让读写
    /// **并存**（`D-S8-06` 的动机消失）。本方法保留只是为了让既有调用点零改动。
    ///
    /// # ⚠️ 与重构前的**行为差异**（`S8-02`，需评审确认）
    ///
    /// 重构前用 `Arc::try_unwrap`：refcount > 1（有 `Searcher` clone 残留）时**报错**。
    /// 新模型下 `shared` 是**共享**的 ——「`Searcher` clone 残留」与「写端仍然存活」
    /// 在 `Arc<Shared>` 的视角下**不可区分**，而后者在 `S8-02` 是**合法状态**
    /// （`searcher(&self)` 的整个意义）。
    /// ⇒ 取形 = **总是成功**：返回一个共享同一视图的新写端（返回类型保持 `Result` 不变）。
    /// ⚠️ **这正是「双写端」成为可能的入口** —— 也是 `commit()` 全程持 `Shared::ids`
    /// 锁的**前置理由**（设计 §4.4.3 / 评审 P3-4）。
    #[deprecated(note = "写端不必再「换回」：用 SearchIndex::searcher(&self) 让读写并存")]
    pub fn into_index(self) -> Result<SearchIndex> {
        Ok(SearchIndex {
            // ⚠️ 克隆 `Arc`（不是 `try_unwrap`）：写端与读端**共享**同一视图，
            //    因此换回写端**不**要求「无其他持有者」。
            shared: self.shared,
            pending: Vec::new(),
            embed_elapsed: std::time::Duration::ZERO,
            // 读端从未 embed 过（换回写端后重新计数）
            embed_count: 0,
            graph_status: self.graph_status,
            // 读端从未执行过 save，图 dump 耗时无意义（不是 0，是「未发生」）
            graph_dump_elapsed: None,
        })
    }
}

/// `search_with(query)` 的 builder：承载可选参数，`exec()` 收口（p6-design 7.1）。
pub struct SearchRequest<'a> {
    searcher: &'a Searcher,
    query: &'a str,
    mode: Option<ModeArg>,
    top_n: usize,
    filter: Option<&'a Filter>,
}

/// `mode` 参数的两种来源：enum（零开销）或字符串（LLM 输出 / 配置文件）。
///
/// 实现细节（供 `SearchRequest::mode` 的 `Into` 约束），不对外文档化。
#[doc(hidden)]
#[derive(Debug, Clone)]
pub enum ModeArg {
    /// 已解析的 `SearchMode`
    Resolved(SearchMode),
    /// 待解析的字符串（延迟到 `exec`，p6-design 7.2）
    Raw(String),
}

impl<'a> SearchRequest<'a> {
    /// 检索模式：接受 `SearchMode`、`&str` 或 `String`。
    ///
    /// 字符串解析延迟到 [`Self::exec`]，失败统一返回 `Error::InvalidInput`
    /// （builder 阶段无法 `?`，故不提前报错——p6-design 7.2 / R4）。
    pub fn mode<M: Into<ModeArg>>(mut self, mode: M) -> Self {
        self.mode = Some(mode.into());
        self
    }

    /// 返回条数（默认 10）。
    pub fn top_n(mut self, top_n: usize) -> Self {
        self.top_n = top_n;
        self
    }

    /// 元数据过滤（FR-14）。
    pub fn filter(mut self, filter: &'a Filter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// 收口执行检索。
    pub fn exec(self) -> Result<SearchResponse> {
        // ⚠️ I8-3：**一次检索只取一次视图快照**，之后全程无锁。
        // 若中途重新取，可能出现「BM25 用 v1 的段、向量用 v2 的段」⇒ 跨 lane 语义不一致。
        let view = self.searcher.shared.snapshot();
        let mode = match self.mode {
            None => self.searcher.infer_mode(&view),
            Some(ModeArg::Resolved(m)) => m,
            Some(ModeArg::Raw(s)) => SearchMode::parse(&s).map_err(Error::InvalidInput)?,
        };
        self.searcher
            .run(&view, self.query, mode, self.top_n, self.filter)
    }
}

impl From<SearchMode> for ModeArg {
    fn from(m: SearchMode) -> Self {
        ModeArg::Resolved(m)
    }
}

impl From<&str> for ModeArg {
    fn from(s: &str) -> Self {
        ModeArg::Raw(s.to_string())
    }
}

impl From<String> for ModeArg {
    fn from(s: String) -> Self {
        ModeArg::Raw(s)
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    // ⚠️ 本模块刻意覆盖旧所有权 API（`into_searcher` / `into_index`）的**行为不变**，
    // 故显式允许 `deprecated` 警告（不影响生产调用点的迁移）。
    #![allow(deprecated)]
    use super::*;
    use crate::embed::Embedder;
    use crate::error::Result;

    /// 确定性假 Embedder：FNV-1a 哈希 → 8 维归一化向量。
    struct FakeEmbedder;
    impl Embedder for FakeEmbedder {
        fn dim(&self) -> usize {
            8
        }
        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| fake_vec(t)).collect())
        }
        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            Ok(fake_vec(text))
        }
    }

    fn fake_vec(text: &str) -> Vec<f32> {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in text.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let mut v: Vec<f32> = (0..8)
            .map(|i| {
                let byte = (h >> (i * 8)) as u8;
                byte as f32 / 255.0 * 2.0 - 1.0
            })
            .collect();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }

    fn build_hybrid() -> Searcher {
        let cfg = super::super::config::SearchIndexBuilder::default()
            .embedder(Some(Arc::new(FakeEmbedder)))
            .vector_backend(crate::search::VectorBackend::Brute)
            .build_config();
        let mut idx =
            SearchIndex::from_config(cfg, crate::search::VectorBackend::Brute, Default::default());
        idx.add("BM25 是经典关键词检索算法").unwrap();
        idx.add("向量检索把文本编码成向量").unwrap();
        idx.add("混合检索融合两路结果").unwrap();
        idx.commit().unwrap();
        idx.into_searcher().unwrap()
    }

    #[test]
    fn searcher满足static_clone_send_sync() {
        fn assert_impl<T: Clone + Send + Sync + 'static>() {}
        assert_impl::<Searcher>();
    }

    #[test]
    fn search单参数默认hybrid() {
        let s = build_hybrid();
        let resp = s.search("检索").unwrap();
        assert!(!resp.hits.is_empty());
    }

    #[test]
    fn search_with_builder定制mode与top_n() {
        let s = build_hybrid();
        let resp = s
            .search_with("检索")
            .mode(SearchMode::Bm25)
            .top_n(1)
            .exec()
            .unwrap();
        assert_eq!(resp.hits.len(), 1);
        // BM25 模式 explain 应有 matched_terms 或 bm25 分
        assert!(resp.hits[0].explain.bm25_score.is_some());
    }

    #[test]
    fn mode字符串解析大小写不敏感() {
        let s = build_hybrid();
        let r1 = s.search_with("检索").mode("hybrid").exec().unwrap();
        let r2 = s.search_with("检索").mode("HyBrId").exec().unwrap();
        assert_eq!(r1.hits.len(), r2.hits.len());
    }

    #[test]
    fn mode非法字符串报InvalidInput() {
        let s = build_hybrid();
        let err = s.search_with("检索").mode("nonsense").exec().unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[test]
    fn into_index换回写端后继续add() {
        let s = build_hybrid();
        let mut idx = s.into_index().unwrap();
        let out = idx.add("新文档继续写入").unwrap();
        assert!(!out.deduped);
        assert!(idx.num_chunks() >= 4);
    }

    /// **`S8-02` 口径变更**（原用例名：`clone残留时into_index报错`）：
    /// 新模型下 `into_index()` **不再**因「clone 残留」失败 —— `shared` 是共享的，
    /// 「clone 残留」与「写端存活」不可区分，而后者是合法状态。
    /// ⇒ 新语义 = **总是成功**，且新旧句柄**共享同一视图**。
    ///
    /// 🔑 这条同时覆盖「双写端」的核心语义（设计 §4.4.3 的前提）：
    /// 老 clone 必须能看到新写端提交的内容。
    #[test]
    fn S8_02_into_index总是成功且与旧clone共享视图() {
        let s = build_hybrid();
        let s2 = s.clone();
        let mut idx = s.into_index().expect("S8-02 起 into_index 不再报错");
        // 旧 clone 仍可检索（共享视图，未被消耗）
        assert!(!s2.search("检索").unwrap().hits.is_empty());
        // 新写端提交的内容，旧 clone 也必须能看到
        idx.add("换成新写端之后写入的一篇文档").unwrap();
        idx.commit().unwrap();
        assert!(
            !s2.search("换成新写端之后写入").unwrap().hits.is_empty(),
            "旧 clone 必须看到新写端提交的内容（共享同一 Arc<Shared>）"
        );
    }
    #[test]
    fn 纯bm25装配默认mode为bm25() {
        let cfg = super::super::config::SearchIndexBuilder::default()
            .embedder(None)
            .build_config();
        let mut idx =
            SearchIndex::from_config(cfg, crate::search::VectorBackend::Brute, Default::default());
        idx.add("BM25 检索算法").unwrap();
        let s = idx.into_searcher().unwrap();
        // 无向量侧：默认 mode 应为 Bm25（而非 Hybrid 报 NoEmbedder）
        let resp = s.search("BM25").unwrap();
        assert!(!resp.hits.is_empty());
    }
}
