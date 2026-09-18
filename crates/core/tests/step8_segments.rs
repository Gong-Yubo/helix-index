//! V2 Step 8 / `S8-02`（视图骨架）的**门面层集成测试**。
//!
//! # 本文件锁的是什么
//!
//! `S8-02` 把「内容容器」从「写端私有的 `Inner`」换成**共享视图**（`Shared.view: RwLock<Arc<View>>`），
//! 并让 `searcher(&self)` 成为新的读入口（旧 `into_searcher()` / `into_index()` 转 `#[deprecated]`
//! 薄封装）。因此本文件的判据分三类：
//!
//! 1. **读写可以并存**（`D-S8-05` / `D-S8-06`）—— 这是本次重构**唯一**的对外价值；
//! 2. **旧 API 行为逐位不变**（`S8-T9` 的「同视图内确定性」+ 与旧路径的逐位对照）；
//! 3. **`S8-02` 期可见性语义与重构前一致** —— ⚠️ 见下。
//!
//! # ⚠️ 可见性语义：本阶段**刻意保留**「`add` 后未 `commit` 也能查到倒排」
//!
//! 设计 §4.8.3 的终态是「`add` 写进 delta ⇒ 未 `commit` 不可见」，但那条语义**由 `S8-03`
//! （delta 写入）交付**：`S8-02` 期 `deltas` 恒空、内容只有一段，`add` 直接写进 `main`
//! ⇒ 立即对读端可见。**这是刻意的**（§1.3 第 2 条要求「行为与重构前逐位一致」），
//! 由 `S8_02_可见性语义与重构前一致` 显式钉住；`S8-03` 落地时该用例**必须改口径**。
//!
//! # 比对口径
//!
//! 「逐位一致」用 `format!("{:?}", hits)` 比较：Rust 的浮点 `Debug` 是**最短 round-trip 表示**
//! ⇒ 逐位相同的值必然得到相同字符串（反向亦成立，`-0.0` / `0.0` 也会被区分）。
//! 不逐字段比是为了同时覆盖 `(chunk_id, score, explain)` 三者的整体一致性。

#![allow(non_snake_case)]
// ⚠️ 本文件**刻意**覆盖旧所有权 API（`into_searcher` / `into_index`）以锁住其行为不变
// （`S8-02` 起二者为 `#[deprecated]` 薄封装）。生产调用点已在 `cli` / `examples` 迁移。
#![allow(deprecated)]

use std::sync::Arc;

use helix_core::chunk::Chunker;
use helix_core::document::Document;
use helix_core::embed::Embedder;
use helix_core::error::Result;
use helix_core::query::SearchMode;
use helix_core::search::{SearchIndex, SearchIndexBuilder, Searcher, VectorBackend};

const DOCS: [&str; 4] = [
    "BM25 是经典的关键词检索算法",
    "向量检索把文本编码成稠密向量",
    "混合检索融合关键词与向量两路结果",
    "倒排索引是全文检索的基石",
];

/// 纯 BM25 装配（无 embedder）⇒ 无 `pending`、无模型依赖、可秒级跑完。
fn bm25_builder() -> SearchIndexBuilder {
    SearchIndexBuilder::default()
        .embedder(None)
        .vector_backend(VectorBackend::Brute)
        // 强制单 chunk：doc == chunk，计数与 ID 断言才直观
        .chunker(Chunker::new(200_000, 0))
        .batch_size(1024)
}

/// **带假 embedder**的装配（有向量 lane）⇒ 让「`flush` 有没有发生」变得**可观测**
/// （纯 BM25 下 `pending` 恒空 ⇒ `flush` 是 no-op ⇒ 一切相关退化都测不出来）。
fn hybrid_builder() -> SearchIndexBuilder {
    SearchIndexBuilder::default()
        .embedder(Some(Arc::new(FakeEmbedder)))
        .vector_backend(VectorBackend::Brute)
        .chunker(Chunker::new(200_000, 0))
        .batch_size(1024)
}

fn bm25_index() -> SearchIndex {
    let mut idx = bm25_builder().build();
    for t in DOCS {
        idx.add(Document::new(t)).unwrap();
    }
    idx.commit().unwrap();
    idx
}

/// 确定性假 Embedder（FNV-1a → 8 维归一化向量）：无模型、可复现。
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

/// 把一次响应的 `hits` 投影成可逐位比较的字符串。
fn dump(searcher: &Searcher, query: &str) -> String {
    let resp = searcher.search_with(query).top_n(10).exec().unwrap();
    format!("{:?}", resp.hits)
}

// ══════════════════════════════════════════════════════════════════════════
// 1) 读写并存（`S8-02` 的对外价值）
// ══════════════════════════════════════════════════════════════════════════

/// **`S8-02` 核心**：`searcher(&self)` **不消耗写端** ⇒ 拿到读端后写端仍可继续 `add` / `commit`，
/// 且**同一个读端**能看到后续提交的内容（二者共享 `Arc<Shared>`）。
///
/// 🔑 这条在重构前**编译期就不可能**（`into_searcher(mut self)` 消耗自身）——
/// 它是 `D-S8-05`（保持 `!Sync`）+ `D-S8-06`（保留 `into_index()`）的落地判据。
#[test]
fn S8_02_searcher不消耗写端且读端能看到后续提交() {
    let mut idx = bm25_builder().build();
    idx.add(Document::new(DOCS[0])).unwrap();
    idx.commit().unwrap();

    // 取读端 **但不消耗** 写端
    let searcher = idx.searcher();
    assert!(
        !searcher.search("BM25").unwrap().hits.is_empty(),
        "读端应能查到已提交内容"
    );

    // ⚠️ 若 `searcher(&self)` 退化成消耗式接口，下面两行**根本无法编译** ⇒ 本用例即失败。
    idx.add(Document::new("新增的一篇关于雷阵雨与气压梯度"))
        .unwrap();
    idx.commit().unwrap();

    // 同一个读端必须看到新提交（共享 `Arc<Shared>`，而非各自持有的旧快照）
    let hits = searcher.search("气压梯度").unwrap().hits;
    assert!(
        !hits.is_empty(),
        "同一个 Searcher 必须看到后续 commit 的内容（共享视图，非快照分叉）"
    );
    // 写端自身可见（本用例共 add 2 条）
    assert_eq!(idx.num_docs(), 2);
}

/// **`S8-02`**：`Searcher` 仍是 `'static + Clone + Send + Sync`（G3 不破）——
/// 新结构（`Arc<Shared>`）不得引入非 `Send`/`Sync` 的字段。
#[test]
fn S8_02_searcher仍满足static_clone_send_sync() {
    fn assert_impl<T: Clone + Send + Sync + 'static>() {}
    assert_impl::<Searcher>();
    // ⚠️ `SearchIndex` 刻意**不是** `Clone`、也**不是** `Sync`（`D-S8-05`：写端独占）；
    //    它只需 `Send`（可整体 move 到别的线程）—— 这是「零所有权破坏」的另一半。
    fn assert_send<T: Send + 'static>() {}
    assert_send::<SearchIndex>();
}

/// **`S8-02`**：`searcher(&self)` **不隐含 `flush()`**（语义收紧，对齐 NFR-11）；
/// 旧的 `into_searcher()` 隐含 flush。
///
/// 🔑 用 `embed_count()` 作判据（公开 API，不依赖 explain 字段）：
/// 向量侧内容只有 `flush()` 才会真正 embed ⇒ 「`searcher()` 之后仍是 0」证明它没隐式 flush。
#[test]
fn S8_02_searcher不隐含flush() {
    let cfg = SearchIndexBuilder::default()
        .embedder(Some(Arc::new(FakeEmbedder)))
        .vector_backend(VectorBackend::Brute)
        .chunker(Chunker::new(200_000, 0))
        .batch_size(1024);
    let mut idx = cfg.build();
    idx.add(Document::new(DOCS[0])).unwrap();
    assert_eq!(idx.embed_count(), 0, "前提：`add` 不 embed");

    let _s = idx.searcher();
    assert_eq!(
        idx.embed_count(),
        0,
        "`searcher()` 不得隐含 flush（可见性由 commit() 显式决定）"
    );

    idx.commit().unwrap();
    assert!(
        idx.embed_count() > 0,
        "`commit()` 必须 flush（含显式 embed）"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 2) 与旧路径逐位一致 / 确定性（`S8-T9`）
// ══════════════════════════════════════════════════════════════════════════

/// **`S8-T9`**：同一视图内，连续 N 次检索**逐位一致**（NFR-06 不破）。
///
/// 判据同时覆盖「段未被就地修改」：若 `with_main_mut` 之后忘了重新发布、
/// 或有读者时静默改到共享段上，稳定输入下的重复检索会先出现不一致。
#[test]
fn S8_T9_同视图内检索逐位一致() {
    let idx = bm25_index();
    let searcher = idx.searcher();
    let first = dump(&searcher, "检索");
    assert!(
        !first.is_empty() && first != "[]",
        "前提：该 query 必须有命中"
    );
    for i in 1..=50 {
        assert_eq!(
            dump(&searcher, "检索"),
            first,
            "第 {i} 次检索与首次不一致（同视图内必须确定性）"
        );
    }
}

/// **`S8-02`**：旧 `into_searcher(mut self)` 的**行为与新路径逐位一致**
/// （它是 `#[deprecated]` 薄封装 = `commit()` + `searcher()`）。
///
/// 🔑 这是「41 处既有调用点零改动」的**唯一可信证据**：不是「能编译」，是「结果逐位相同」。
#[test]
fn S8_02_旧API与新API结果逐位一致() {
    // ⚠️ **必须用带向量 lane 的装配**（变异实测，2026-09-18）：纯 BM25 下 `pending` 恒空
    // ⇒ `flush` 是 no-op ⇒「`into_searcher` 的隐含 flush 被删掉」**测不出来**
    //（M6′ 注入后全仓测试**全绿**）。带向量后，两条路径的 `hits`（含 `explain`）
    // 才真正反映「有没有 embed 过」。
    // 路径 A：旧 API（隐含 flush）
    let mut a = hybrid_builder().build();
    for t in DOCS {
        a.add(Document::new(t)).unwrap();
    }
    let old = a.into_searcher().unwrap();

    // 路径 B：新 API（显式 commit）
    let mut b = hybrid_builder().build();
    for t in DOCS {
        b.add(Document::new(t)).unwrap();
    }
    b.commit().unwrap();
    let new = b.searcher();

    for q in ["检索", "向量", "BM25", "倒排索引"] {
        assert_eq!(
            dump(&old, q),
            dump(&new, q),
            "query `{q}`：旧 API 与新 API 结果必须逐位一致"
        );
    }
    // 避免「都是空」的空转通过，并**显式断言向量 lane 真的参与了**
    // （否则两条路径「都没有向量」也会逐位一致 ⇒ 本用例失去鉴别力）
    let probe = old
        .search_with("检索")
        .mode(SearchMode::Hybrid)
        .exec()
        .unwrap();
    assert!(!probe.hits.is_empty(), "hybrid 检索必须有命中");
    // 🔑 判据用 `Explain` 的**结构化字段**，不用 `format!("{:?}")` 的字符串包含
    // （`S8-02` 评审 P4-4）：原写法 `dbg.contains("0.")` 对任何 `0.x` 的分数都成立
    // ⇒ **几乎恒真、零鉴别力** —— 它**并不能**证明向量 lane 参与了（能证明这一点的
    // 是「必须用带向量 lane 的装配」这个**夹具选择**，不是那条断言）。
    let vector_recalled = probe
        .hits
        .iter()
        .filter(|h| h.explain.vector_score.is_some() || h.explain.vector_rank.is_some())
        .count();
    assert!(
        vector_recalled > 0,
        "hybrid 检索下必须至少有一条命中由**向量路**召回\
         （`explain.vector_score` / `vector_rank` 为 `Some`）—— 否则「带向量 lane 的装配」\
         这个前提不成立，「两条路径逐位一致」就成了空转。explain = {:?}",
        probe.hits.iter().map(|h| &h.explain).collect::<Vec<_>>()
    );
}

/// **`S8-02`**：`Searcher::into_index()`（deprecated 薄封装）仍是
/// 「用同一 `Arc<Shared>` 造回写端」且**行为不变**：换回后可继续写入并检索到。
#[test]
fn S8_02_into_index换回写端后继续add() {
    let idx = bm25_index();
    let searcher = idx.searcher();
    let mut back = searcher.into_index().unwrap();
    back.add(Document::new("换回写端之后新增的一篇文档"))
        .unwrap();
    back.commit().unwrap();
    assert!(back.num_docs() > DOCS.len());
    let s2 = back.searcher();
    assert_eq!(s2.search("新增的一篇文档").unwrap().hits.len(), 1);
}

// ══════════════════════════════════════════════════════════════════════════
// 3) `S8-02` 期的可见性语义（⚠️ 刻意保留，`S8-03` 起收紧）
// ══════════════════════════════════════════════════════════════════════════

/// **`S8-02` 期可见性语义**：`add` 之后**未** `commit` 也能查到**倒排**内容
/// （向量侧不在此列，见上条用例）。⚠️ **刻意保留** —— 与重构前一致（§1.3 第 2 条）。
///
/// 🔴 **`S8-03`（delta 写入）落地时本用例的断言必须反转**（改成「未 commit 查不到」，
/// 对齐 §1.3 第 7 条）。**不要**把这条改成「永远绿」的模糊断言。
#[test]
fn S8_02_可见性语义与重构前一致_add未commit亦可查到倒排() {
    let mut idx = bm25_builder().build();
    idx.add(Document::new(DOCS[0])).unwrap();

    // 写端自身
    assert_eq!(idx.num_chunks(), 1);
    // 读端（未 commit：`searcher()` 不隐含 flush，但倒排是 `add` 时立即写入的）
    let s = idx.searcher();
    assert_eq!(
        s.search("BM25").unwrap().hits.len(),
        1,
        "S8-02 期：倒排随 add 立即可见（与重构前一致）"
    );

    idx.commit().unwrap();
    assert_eq!(s.search("BM25").unwrap().hits.len(), 1, "commit 后仍可见");
}

/// **`S8-02`**：`commit()` 之后**立即可查**（正向），且 `commit()` 多次不改变结果（幂等）。
#[test]
fn S8_02_commit后立即可查且幂等() {
    let mut idx = bm25_builder().build();
    idx.add(Document::new(DOCS[2])).unwrap();
    idx.commit().unwrap();
    let s = idx.searcher();
    let once = dump(&s, "混合检索");

    // 无新内容时重复 commit 不得改变可观测结果
    for _ in 0..3 {
        idx.commit().unwrap();
    }
    assert_eq!(dump(&s, "混合检索"), once, "空 commit 必须幂等");

    // 模式切换（`search_with`）在骨架期同样不受影响
    let bm = s
        .search_with("混合检索")
        .mode(SearchMode::Bm25)
        .exec()
        .unwrap();
    assert!(!bm.hits.is_empty());
}
