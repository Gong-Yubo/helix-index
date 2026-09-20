//! V2 Step 8（视图骨架 / delta 写入 / 跨段 BM25）的**门面层集成测试**。
//!
//! # 本文件锁的是什么
//!
//! `S8-02` 把「内容容器」从「写端私有的 `Inner`」换成**共享视图**（`Shared.view: RwLock<Arc<View>>`），
//! 并让 `searcher(&self)` 成为新的读入口（旧 `into_searcher()` / `into_index()` 转 `#[deprecated]`
//! 薄封装）。因此本文件的判据分五类：
//!
//! 1. **读写可以并存**（`D-S8-05` / `D-S8-06`）—— 这是本次重构**唯一**的对外价值；
//! 2. **旧 API 行为逐位不变**（`S8-T9` 的「同视图内确定性」+ 与旧路径的逐位对照）；
//! 3. **`S8-03` 起可见性 = `commit()` 后**（§1.3 第 7 条 / NFR-11）—— `add` / `flush` 写的是
//!    **写端私有的 `SegmentBuilder`** ⇒ 未 `commit` 对读端**不可见**；
//! 4. **`S8-T4` / `S8-T6` / `S8-T8` 三条设计钦定的门测试**（§4 起）—— 跨段 BM25 逐位一致（含
//!    「带跨段删除」的双参照系变体）、跨段去重、合并/落盘**保 ID**；
//! 5. **`compact()` × 多写端的 ID 空间换代**（§7）—— P2-4 的端到端回归锁（含 ABA 窗口构造）。
//!
//! # ⚠️ 可见性语义（`S8-03` 已按计划收紧）
//!
//! `S8-02`（视图骨架）期 `deltas` 恒空、`add` 直写 `main` ⇒ 倒排**随 `add` 立即可见**
//! （与重构前逐位一致，当时由 `S8_02_可见性语义与重构前一致` 显式钉住，并注明「`S8-03`
//! 落地时必须改口径」）。本 PR（`S8-03` delta 写入）交付终态之后，该用例已**改名 + 反转**为
//! `S8_03_未commit不可见_commit后立即可见`。**不要**把它改回「永远绿」的模糊断言。
//!
//! # 比对口径
//!
//! 「逐位一致」用 `format!("{:?}", hits)` 比较：Rust 的浮点 `Debug` 是**最短 round-trip 表示**
//! ⇒ 逐位相同的值必然得到相同字符串（反向亦成立，`-0.0` / `0.0` 也会被区分）。
//! 不逐字段比是为了同时覆盖 `(chunk_id, score, explain)` 三者的整体一致性。
//! ⚠️ **唯一例外**是「带跨段删除」变体的**合并前**一侧：那里必须剔除位置量 `bm25_rank`
//! （理由与鉴别力论证见 `frame1_view` 的文档）。

#![allow(non_snake_case)]
// ⚠️ 本文件**刻意**覆盖旧所有权 API（`into_searcher` / `into_index`）以锁住其行为不变
// （`S8-02` 起二者为 `#[deprecated]` 薄封装）。生产调用点已在 `cli` / `examples` 迁移。
#![allow(deprecated)]

use std::sync::Arc;

use helix_core::chunk::Chunker;
use helix_core::document::Document;
use helix_core::embed::Embedder;
use helix_core::error::{Error, Result};
use helix_core::query::{Hit, SearchMode, SearchResponse, VectorRoute};
use helix_core::retriever::union_route;
use helix_core::schema::Filter;
use helix_core::search::{
    MergeReport, SearchIndex, SearchIndexBuilder, Searcher, VectorBackend, VectorMergeStrategy,
};
use helix_core::types::{ChunkId, DocId, Score};

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

/// 一次 **BM25** 检索的完整响应。
///
/// ⚠️ `mode` 显式钉成 `Bm25`：本节的判据比的是**分数逐位一致**，而 `S8-05`（PR5）之前
/// 向量路只覆盖 `main`（见 `Searcher::parts`），默认 `Hybrid` 会把「两路融合」与
/// 「向量路半盲」两件事混进同一次比较里。
fn resp(searcher: &Searcher, q: &str) -> SearchResponse {
    searcher
        .search_with(q)
        .mode(SearchMode::Bm25)
        .top_n(10)
        .exec()
        .unwrap()
}

/// 造「`main` 已承载内容」的布局：建库 → `save`（`D-S8-01`：落盘第一步就是 `fold_deltas`）
/// → `load` ⇒ `deltas` 为空、`main` 持全部内容。
///
/// ⚠️ `PR4`（`S8-04`）**没有**「只合并不落盘」的公开入口（`merge_all()` 属 `S8-06`）
/// ⇒ 本节用 `save()` 作为合并的触发点，顺带覆盖「落盘往返」。
fn build_and_fold(docs: &[&str], path: &std::path::Path) -> SearchIndex {
    let mut idx = bm25_builder().build();
    for d in docs {
        idx.add(Document::new(*d)).unwrap();
    }
    idx.commit().unwrap();
    idx.save(path).unwrap();
    bm25_builder().load(path).unwrap()
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
/// 判据同时覆盖「段未被就地修改」：若某条写路径忘了重新发布（而是就地改已发布的段）、
/// 或把半修改状态暴露给读者，稳定输入下的重复检索会先出现不一致。
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
    // ✅ **`S8-05` 已反转（2026-09-20，PR5）**：`S8-03` 起内容落在 `deltas`，当时**向量路只覆盖
    //    `main`** ⇒ 本夹具下 hybrid 的命中全部来自 BM25 lane，故 PR4 把断言从「必须有向量路
    //    召回」**反转**为「当前不应有」，并在注释里写明「**PR5 落地后必须再反转回 `> 0`**」。
    //    `S8-05`（逐段向量检索）落地后，`deltas` 里的向量**会被召回** ⇒ 本断言按承诺反转回
    //    `> 0`。这一条现在是**跨段向量真的通了**的最低成本信号（真正的门是 `S8_T5_*`）。
    //    ⚠️ 判据同时保留「命中非空」（上面那条）⇒ 不会退化成「空 == 空」。
    assert!(
        vector_recalled > 0,
        "S8-05 起向量路覆盖全部段 ⇒ 本夹具（内容全在 deltas）必须出现向量路召回；\
         实得 0 ⇒ 逐段向量检索没有接上。explain = {:?}",
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

/// **`S8-03` 可见性语义**：`add` / `flush` 写的是**写端私有的 `SegmentBuilder`**
/// ⇒ **未 `commit` 对读端不可见**；`commit()` 后**立即可见**（§1.3 第 7 条 / NFR-11）。
///
/// 🔑 **本用例是 §1.3 第 2 条 → 第 7 条的分界线**：`S8-02` 期它的断言是**反的**
/// （「未 commit 也能查到倒排」= 与重构前逐位一致），本 PR 按模块文档里的承诺
/// **改名 + 反转**。**不要**把它改成「永远绿」的模糊断言。
#[test]
fn S8_03_未commit不可见_commit后立即可见() {
    let mut idx = bm25_builder().build();
    idx.add(Document::new(DOCS[0])).unwrap();

    // 写端自身：内容尚未发布 ⇒ 对外统计量仍为 0
    assert_eq!(
        idx.num_chunks(),
        0,
        "S8-03 起：未 commit ⇒ 不计入对外统计量（§1.3 第 7 条）"
    );

    // 即使先 `flush`（内容进了**写端自己**的 builder 索引），只要没 `commit` 就仍不可见
    // ⇒ 可见性边界**只有** `commit()` 一个
    idx.flush().unwrap();
    assert_eq!(idx.num_chunks(), 0, "flush 不发布（可见性只在 commit）");

    // 读端（未 commit）：`searcher()` 不隐含 flush ⇒ 什么都没发布 ⇒ 零命中
    let s = idx.searcher();
    assert_eq!(
        s.search("BM25").unwrap().hits.len(),
        0,
        "S8-03 起：未 commit 的内容不得对读端可见"
    );

    idx.commit().unwrap();
    assert_eq!(idx.num_chunks(), 1, "commit 后才计入对外统计量");
    // ⚠️ 同一个 `Searcher` 句柄（commit **之前**取的）也必须看到新视图：它持 `Arc<Shared>`，
    //    读的是 `shared.view` 的**当前快照**（`I8-3` 只约束「一次检索内不换快照」）。
    assert_eq!(
        s.search("BM25").unwrap().hits.len(),
        1,
        "commit 后立即可见（NFR-11），且 commit 前取得的句柄同样可见"
    );
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

// ══════════════════════════════════════════════════════════════════════════
// 6) `content_hash` 与「跨段墓碑 + 重新 upsert」的交互（评审 P1-2 的回归锁）
// ══════════════════════════════════════════════════════════════════════════

/// **`S8-03` 新增（评审 P1-2 的回归锁）**：跨段墓碑 + 同内容重加 + `save()` **不得 panic**，
/// 且替换文档的去重条目**必须保留**（FR-15 幂等 upsert）。
///
/// 🔴 修前形态（**已在评审批次内实测复现**）：`remove` 分支② 只记墓碑 ⇒ 同 hash 的旧 doc X
/// 仍在已发布的段里 ⇒ `doc_id_by_hash_global` 被墓碑挡住 ⇒ 允许插入同 hash 的新 doc Y ⇒
/// `fold_deltas` 的 `merge_from` 撞上 `content_hashes` 的 `debug_assert!`（**debug 必 panic**）；
/// release 下继续走，但物理化 X 时 `Index::remove` **无条件**摘 hash 条目 ⇒ 把 **Y 的去重条目**
/// 一并抹掉 ⇒ 同一内容可被重复 upsert（**静默失效**）。
///
/// 修法（同批次落地）：① `Index::remove` 的 hash 摘除改**条件式**（只摘指向本 doc 的条目）；
/// ② `merge_from` 的碰撞断言**删除**（碰撞是合法状态，语义 = 后写入者为准）。
#[test]
fn S8_T6_跨段墓碑下同内容重加经save后仍可去重() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t6.idx");
    let text = "跨段墓碑下的去重条目 苹果";

    let mut idx = bm25_builder().build();
    let out = idx.add(Document::new(text)).unwrap();
    idx.commit().unwrap(); // 内容进 delta（已发布）

    // ── 设计 §7 的 `S8-T6` **上半句**：既往段已有同 `content_hash` ⇒ 再 `add` 必须**去重**
    //    （FR-15 幂等 upsert；`doc_id_by_hash_global` 的 ② 分支：`deltas` 逆序 → `main`）
    let dup = idx.add(Document::new(text)).unwrap();
    assert!(dup.deduped, "同 hash 命中既往段 ⇒ 必须去重（FR-15）");
    assert_eq!(dup.doc_id, out.doc_id, "去重命中必须返回既有 `doc_id`");
    assert!(dup.chunk_ids.is_empty(), "去重命中不得分配新分片");

    idx.remove(out.doc_id).unwrap(); // 分支② 记跨段墓碑（**在 builder 上**）
                                     // ⚠️ 墓碑与内容走**同一条可见性边界**：要 `commit()` 才进 `View.tombstones`
                                     //    ⇒ 不 commit 就重加，查重根本看不到墓碑，会直接去重（本用例第一版就是这么错的）。
    idx.commit().unwrap();

    // 同内容重加：查重被墓碑挡住 ⇒ 视为未命中 ⇒ 允许插入（§4.8.3 钦定语义）
    let again = idx.add(Document::new(text)).unwrap();
    assert!(!again.deduped, "被墓碑挡住 ⇒ 应重新 upsert（不算命中）");
    idx.commit().unwrap();

    // ① 合并 + 落盘：修前 debug 构建必在 `merge_from` 的碰撞断言上 panic
    idx.save(&path).unwrap();

    // ② 落盘往返之后，同内容**仍然**可去重（替换文档的条目没被旧 doc 的物理化抹掉）
    let mut loaded = bm25_builder().load(&path).unwrap();
    let third = loaded.add(Document::new(text)).unwrap();
    assert!(
        third.deduped,
        "物理化旧 doc 不得摘掉替换文档的去重条目（FR-15 幂等 upsert）"
    );
}

/// **`S8-03` 第 2 轮评审 P1-1 的回归锁**：`remove(已发布 doc)` 后**不 `commit()`** 就重加同
/// `content_hash` 的内容 ⇒ 必须**不被去重吞掉**（`!deduped` 且 `doc_id != X`）。
///
/// 🔴 修前形态（第 2 轮评审指出，本轮已复现）：`doc_id_by_hash_global` 的 ② 循环只查
/// `view.tombstones`，而 `remove` **分支②** 的墓碑**先落在 `self.builder.tombstones`**
/// （要 `commit()` 才发布）⇒ 查重看不到那条墓碑 ⇒ 命中**刚被删除的** X
/// ⇒ 返回 `deduped = true` + `doc_id = X` ⇒ **替换 / 重加静默丢失**
/// （`commit()` 后 X 被墓碑挡住，而新内容**根本没被创建**，调用方却拿到一个已死 doc 的 ID）。
///
/// 🔑 **这是缺口不是设计**（判据 = 同序列在**分支①**下的行为）：目标**未发布**时
/// `remove` 就地物理删 + 条件式摘 hash 条目 ⇒ 重加正常工作；同一个逻辑操作只因「目标 doc
/// 是否已发布」而结果相反 —— 而「`remove` 后用同 `dedup_key` 换正文重新 upsert」（FR-15 的
/// 替换文档流程）恰恰**总是**落在分支②（目标必然已发布才谈得上"替换"）。
#[test]
fn S8_T6_remove未commit即重加不得被去重吞掉() {
    let text = "未发布墓碑下的重加 香蕉";
    let mut idx = bm25_builder().build();
    let out = idx.add(Document::new(text)).unwrap();
    idx.commit().unwrap(); // X 已发布（进 delta）

    // 目标在既往段 ⇒ `remove` 分支②：墓碑记在 **builder** 上（尚未发布）
    idx.remove(out.doc_id).unwrap();

    // 同 hash 重加：墓碑还在 builder 上 ⇒ 修前这里看不到墓碑 ⇒ 被误判为「已存在」
    let again = idx.add(Document::new(text)).unwrap();
    assert!(
        !again.deduped,
        "刚被删除的 doc 不算命中 ⇒ 必须重新 upsert\
         （修前：查重只认 `view.tombstones`，看不到 builder 上未发布的墓碑）"
    );
    assert_ne!(
        again.doc_id, out.doc_id,
        "不得把新内容记到刚被删除的 doc_id 上（那等于静默丢弃）"
    );

    idx.commit().unwrap();

    // 终态：新内容**可被检索**（修前它压根没被创建），且旧 doc 不复活
    let seg = idx.searcher();
    let hits = resp(&seg, "香蕉").hits;
    assert_eq!(
        hits.len(),
        1,
        "重加的内容必须在检索里出现（修前 `hits` 为空）"
    );
    assert_eq!(hits[0].doc_id, again.doc_id, "命中的必须是**新** doc");
}

// ══════════════════════════════════════════════════════════════════════════
// 4) `S8-T4`：跨段 BM25 逐位一致（**本 PR 的门测试**）
// ══════════════════════════════════════════════════════════════════════════

/// 门测试语料：主段 4 篇 ／ 增量① 2 篇 ／ 增量② 2 篇（共 8 篇）。
const T4_MAIN: [&str; 4] = [
    "BM25 是经典的关键词检索算法",
    "向量检索把文本编码成稠密向量",
    "混合检索融合关键词与向量两路结果",
    "倒排索引是全文检索的基石",
];
// ⚠️ 第 2 篇**刻意**含一个在主段也出现的词（「检索」）⇒ 「带跨段删除」变体才真的覆盖
// `I8-6`（跨段共享词项的累加）。变异实测（2026-09-19）：改成不含跨段共享词的版本时，
// `MUT-I8-6`（各段各算 `df`）**只被主用例命中**，该变体**不敏感** —— 那是覆盖边界，不是「已守住」。
const T4_D1: [&str; 2] = [
    "分段写入把新内容追加成独立的段",
    "后台合并把增量段折回主段后仍可检索",
];
const T4_D2: [&str; 2] = [
    "墓碑挡住已删除文档的召回",
    "发号器保证段之间的 ID 空间不重叠",
];
/// 覆盖「只在主段出现 / 只在增量段出现 / 跨段共同出现」三类词项。
const T4_QUERIES: [&str; 6] = ["检索", "向量", "BM25", "段", "合并", "墓碑"];

/// 全语料（`main` + 两个增量段拼接后的**同一份内容**，8 篇）。
fn t4_all() -> Vec<&'static str> {
    T4_MAIN
        .iter()
        .chain(T4_D1.iter())
        .chain(T4_D2.iter())
        .copied()
        .collect()
}

/// 「参照系① 比较视图」的一个元素：
/// `(chunk_id, doc_id, score, matched_terms, bm25_score, fused_score)`。
///
/// 抽成别名有两个理由：① `clippy::type_complexity` 在 `-D warnings` 下会拦长元组返回类型；
/// ② 这个元组就是「参照系① 比什么」的定义（**刻意不含 `bm25_rank`**，理由见 [`frame1_view`]）。
type Frame1Item = (ChunkId, DocId, Score, Vec<String>, Option<Score>, Score);

/// 「参照系①」的比较视图：**刻意剔除位置量 `bm25_rank`**。
///
/// 🔑 为什么必须剔除（**本轮变异/落地实测的发现**，2026-09-19）：`bm25_rank` 是「**在候选集里的
/// 位置**」（1 起）。参照系① 的构造方式是「单段建库**保留**被删 doc，再在**命中集**里剔除它」，
/// 而**事后剔除不会让后面条目的 rank 前移** ⇒ 参照侧那条被删 doc 之后的条目 rank 比被测侧**大 1**。
/// 这**不是缺陷**：两侧的候选集本就不是同一个（被测侧的墓碑 doc 从未进入 lane）。
/// ⇒ 参照系① 只比**非位置量**（`chunk_id` / `doc_id` / `score` / `matched_terms` /
/// `bm25_score` / `fused_score`）；**参照系②**（合并后）两侧候选集**相同** ⇒ 仍比全量 `Hit`（含 rank）。
///
/// ⚠️ 剔除 rank **不降低鉴别力**：墓碑**有没有真的被挡住**由「`got` 里不得出现 `dead`」+ 与
/// 剔除后的参照集**逐元素相等**共同钉住（若墓碑没被挡住，`got` 会多出一条 ⇒ 立刻红）。
fn frame1_view(hits: &[Hit]) -> Vec<Frame1Item> {
    hits.iter()
        .map(|h| {
            (
                h.chunk_id,
                h.doc_id,
                h.score,
                h.explain.matched_terms.clone(),
                h.explain.bm25_score,
                h.explain.fused_score,
            )
        })
        .collect()
}

/// **`S8-T4`（设计 §7 的门测试）**：同一份内容，`[单段]` vs `[主段 + 2 个增量段]`
/// ⇒ `hits`（含 `chunk_id` / `score` / `explain`）必须**逐位相同**。
///
/// 🔴 这条是设计 §8 写明的「**PR4 的门**」：它同时钉住三条不变式
/// —— `I8-5`（全局精确整数统计量：`N` / `total_len` 必须跨段求和后**一次**算 `avgdl`）、
/// `I8-6`（`term` 外层累加：跨段 TAAT 的累加顺序）、
/// `I8-7`（段 ID 空间不重叠：增量段的全局 ID 必须从主段长度起步）。
/// 任一被破坏 ⇒ 分数漂移（「结果看起来对、分数悄悄变了」）⇒ 本用例红。
///
/// 🔑 **夹具要点**：两个索引的**全局 ID 空间必须相同**（都是 0..8，按同一个顺序分配），
/// 否则比较会退化成「在比 ID」而不是「在比打分」。⇒ 参照系的 `main` 与被测侧的 `main`
/// 都由「同样 4 篇、同样顺序」建出来。
#[test]
fn S8_T4_跨段BM25与单段逐位一致() {
    let dir = tempfile::tempdir().unwrap();

    // ── 参照系 `[单段]`：8 篇一次性建库 → 折叠 → load（`deltas` 空、`main` 持全部）
    let sref = build_and_fold(&t4_all(), &dir.path().join("t4-ref.idx")).searcher();

    // ── 被测 `[主段 + 2 个增量段]`：同内容、同 ID 空间，只是分三次发布
    let mut idx = build_and_fold(&T4_MAIN, &dir.path().join("t4-main.idx"));
    for d in T4_D1 {
        idx.add(Document::new(d)).unwrap();
    }
    idx.commit().unwrap();
    for d in T4_D2 {
        idx.add(Document::new(d)).unwrap();
    }
    idx.commit().unwrap();
    let sseg = idx.searcher();

    // 前提自证（缺了它们，这条门测试可能什么都没测到）：
    // ① 被测侧**真的**是 3 段、参照侧**真的**是单段；② 两边内容量一致；
    // ③ 该 query 有命中（否则「空 == 空」是空转通过）。
    let probe_ref = resp(&sref, T4_QUERIES[0]);
    let probe_seg = resp(&sseg, T4_QUERIES[0]);
    assert_eq!(
        probe_ref.metrics.segments, 1,
        "参照系必须是单段（`main` 持全部）"
    );
    assert_eq!(
        probe_seg.metrics.segments, 3,
        "被测侧必须是 `main` + 2 个 delta（否则本用例没走到跨段累积）"
    );
    assert_eq!(probe_seg.metrics.tombstoned, 0, "本用例无跨段墓碑");
    assert!(!probe_seg.hits.is_empty(), "前提：该 query 必须有命中");
    assert_eq!(
        idx.num_chunks(),
        8,
        "两边内容量必须一致（主段 4 + 增量 2 + 2）"
    );

    for q in T4_QUERIES {
        let want = resp(&sref, q);
        let got = resp(&sseg, q);
        assert!(
            !got.hits.is_empty(),
            "query `{q}`：前提是必须有命中，否则本断言退化成「空 == 空」"
        );
        assert_eq!(
            format!("{:?}", got.hits),
            format!("{:?}", want.hits),
            "query `{q}`：跨段布局的 `hits`（含 explain / chunk_id）必须与单段布局**逐位一致**\
             （`I8-5` / `I8-6` / `I8-7` 之一被破坏）"
        );
    }
}

/// **`S8-T4` 的「带跨段删除」变体**（设计 §7，2026-09-19 按 PR4 评审 **P2-5** 更正后的
/// **双参照系**）。
///
/// # 为什么必须是两个参照系
///
/// 墓碑态在**段内仍然存活**（这正是墓碑存在的理由）⇒ `View::bm25_totals()` 与 `df` 都
/// **不扣墓碑** ⇒ 「合并前」若拿「单段建库 + `remove`」（统计量已**不含**它）当参照物，
/// `idf` / `avgdl` 必然不同 ⇒ **逐位一致结构上不可能成立**（照原设计写会永远红，
/// 并误导成「实现错误」）。
///
/// | 阶段 | 正确参照系 |
/// | --- | --- |
/// | **① 合并前**（墓碑未物理化） | 单段建库 **不删**（`N` / `total_len` / `df` 一律**含**被删 doc）+ **命中集剔除**它 |
/// | **② 合并后**（墓碑已物理化） | 单段建库 **+ 同序 `remove`**（统计量也不含它） |
///
/// ② 同时锁住 §4.9.2 的「物理化」—— 若 `fold_deltas` 忘了把墓碑落成物理删除，
/// ② 会因为 `N` 偏大而红。
#[test]
fn S8_T4_带跨段删除_合并前后双参照系() {
    let dir = tempfile::tempdir().unwrap();
    let all = t4_all();

    // ── 被测：`main` = 4 篇；随后 `remove` 主段第 3 篇（落进**跨段墓碑**）
    //           + 追加 4 篇 ⇒ 一次 `commit` 同时发布「墓碑」与「新 delta」
    let mut idx = bm25_builder().build();
    let mut ids: Vec<DocId> = Vec::new();
    for t in T4_MAIN {
        ids.push(idx.add(Document::new(t)).unwrap().doc_id);
    }
    idx.commit().unwrap();
    let p_main = dir.path().join("t4d-main.idx");
    idx.save(&p_main).unwrap();
    let mut idx = bm25_builder().load(&p_main).unwrap();

    let dead = ids[2];
    idx.remove(dead).unwrap(); // 分支②：目标在既往段 ⇒ 记墓碑（**在 builder 上**）
    let mut appended: Vec<DocId> = Vec::new();
    for t in T4_D1.iter().chain(T4_D2.iter()) {
        appended.push(idx.add(Document::new(*t)).unwrap().doc_id);
    }
    assert_eq!(
        appended[0] as usize,
        T4_MAIN.len(),
        "前提：追加段的基址 = 主段已用长度（`I8-7`；跨段墓碑**不释放槽位**）"
    );
    idx.commit().unwrap(); // 墓碑与新 delta 走**同一条**可见性边界

    let sseg = idx.searcher();
    let p0 = resp(&sseg, T4_QUERIES[0]);
    assert_eq!(p0.metrics.segments, 2, "被测侧 = `main` + 1 个 delta");
    assert!(
        p0.metrics.tombstoned > 0,
        "被测侧必须有跨段墓碑，否则本变体根本没走到删除路径"
    );
    assert!(
        !p0.hits.is_empty(),
        "前提：该 query 必须有命中（否则本变体什么都没比）"
    );
    assert!(
        !p0.hits.iter().any(|h| h.doc_id == dead),
        "墓碑必须**真的挡住**那一条（不是「只在统计量里扣」）；hits = {:?}",
        p0.hits
            .iter()
            .map(|h| (h.doc_id, h.chunk_id))
            .collect::<Vec<_>>()
    );

    // ── 参照系①（**合并前**）：单段建库**不删**，只在命中集里剔除它
    let sref1 = build_and_fold(&all, &dir.path().join("t4d-ref1.idx")).searcher();
    let pr1 = resp(&sref1, T4_QUERIES[0]);
    assert_eq!(pr1.metrics.segments, 1, "参照系① 必须是单段");
    assert_eq!(pr1.metrics.tombstoned, 0);
    assert_eq!(
        idx.num_chunks(),
        pr1.metrics.allowed as u32,
        "★ 双参照系的关键：**合并前**两侧的 `N`（= 命中集上界 allowed）必须**都含**被删 doc"
    );

    let mut excluded = 0usize;
    for q in T4_QUERIES {
        let got = resp(&sseg, q);
        let want = resp(&sref1, q);
        let want_hits: Vec<Hit> = want
            .hits
            .iter()
            .filter(|h| h.doc_id != dead)
            .cloned()
            .collect();
        excluded += want.hits.len() - want_hits.len();
        assert!(
            !got.hits.is_empty(),
            "query `{q}`：前提是必须有命中（否则本断言退化成「空 == 空」）"
        );
        // ⚠️ 比的是 `frame1_view`（**剔除位置量 `bm25_rank`**），理由见该函数文档：
        //    参照系① 的「命中集剔除」**不会**让后续条目的 rank 前移。
        assert_eq!(
            frame1_view(&got.hits),
            frame1_view(&want_hits),
            "query `{q}`【合并前】跨段 + 墓碑布局必须与「单段不删 + 命中集剔除」一致\
             （`N` / `total_len` / `df` 在两侧都含被删 doc）"
        );
    }
    assert!(
        excluded > 0,
        "前提：至少有一个 query 命中被删文档 ⇒「命中集剔除」这一步才真的被验到"
    );

    // ── 合并（`save` 第一步 = `fold_deltas` ⇒ 墓碑**物理化**、`deltas` 清空）
    let p_folded = dir.path().join("t4d-folded.idx");
    idx.save(&p_folded).unwrap();
    let pfolded = resp(&idx.searcher(), T4_QUERIES[0]);
    assert_eq!(pfolded.metrics.segments, 1, "合并后必须回到单段");
    assert_eq!(
        pfolded.metrics.tombstoned, 0,
        "合并后墓碑必须已物理化（热路径回到零谓词，R52 的回归可逆）"
    );

    // ── 参照系②（**合并后**）：单段建库 + **同序 `remove`**
    //    ⚠️ `remove` 必须在**内容还躺在 builder 里**时调用 ⇒ 走分支①「就地物理删」，
    //    这样参照系才真的是「一个不含该 doc 的单段」，而不是又记了一条墓碑。
    let mut ref2 = bm25_builder().build();
    let mut ids2: Vec<DocId> = Vec::new();
    for t in &all {
        ids2.push(ref2.add(Document::new(*t)).unwrap().doc_id);
    }
    assert_eq!(
        ids2[2], dead,
        "前提：同序构建 ⇒ 同一逻辑文档拿到同一 `doc_id`"
    );
    ref2.remove(dead).unwrap();
    ref2.commit().unwrap();
    let p_ref2 = dir.path().join("t4d-ref2.idx");
    ref2.save(&p_ref2).unwrap();
    let ref2 = bm25_builder().load(&p_ref2).unwrap();
    let sref2 = ref2.searcher();

    let pr2 = resp(&sref2, T4_QUERIES[0]);
    assert_eq!(pr2.metrics.segments, 1, "参照系② 必须是单段");
    assert_eq!(
        resp(&idx.searcher(), T4_QUERIES[0]).metrics.allowed,
        pr2.metrics.allowed,
        "★ 双参照系的另一半：**合并后**两侧的 `N` 必须**都不含**被删 doc"
    );

    for q in T4_QUERIES {
        let got = resp(&idx.searcher(), q);
        let want = resp(&sref2, q);
        assert!(
            !got.hits.is_empty(),
            "query `{q}`：前提是必须有命中（否则本断言退化成「空 == 空」）"
        );
        assert_eq!(
            format!("{:?}", got.hits),
            format!("{:?}", want.hits),
            "query `{q}`【合并后】墓碑物理化之后必须与「单段建库 + 同序 remove」逐位一致"
        );
    }
}

// ══════════════════════════════════════════════════════════════════════════
// 5) `S8-T8`：合并 / 落盘往返**保 ID**（`D-S8-04`）
// ══════════════════════════════════════════════════════════════════════════

/// 只在追加段出现、且词面唯一的文档（用于按 `chunk_id` 精确认领命中）。
const T8_APPENDED: &str = "增量段里唯一的文档 蓝莓派";

/// **`S8-T8`**：合并（`fold_deltas`）与 `save`/`load` 往返都**不得改 ID**。
///
/// 🔑 为什么这条是门的一部分：`compact()` 会重编号（`D-S4-01`，ID 会变），而合并**不会**
/// （`D-S8-04`：`merge_from` 按本地顺序 append、基址不变）——两者语义必须显式分离
/// （`D-S8-12`），否则用户侧的 `chunk_id` 会**静默失效**（Agent 记忆里的 ID 指到别的 chunk）。
///
/// ⚠️ 判据用「搜出来那条命中的 `(doc_id, chunk_id)`」而不是内部字段：这才是调用方
/// 真正持有的东西，也是 `R50` 描述的静默风险面。
#[test]
fn S8_T8_合并与落盘往返保ID() {
    let dir = tempfile::tempdir().unwrap();
    let mut idx = build_and_fold(&T4_MAIN, &dir.path().join("t8-main.idx"));
    assert_eq!(idx.num_chunks(), T4_MAIN.len() as u32, "前提：主段 4 篇");

    // 追加一篇并捕获它拿到的**全局** ID
    let out = idx.add(Document::new(T8_APPENDED)).unwrap();
    idx.commit().unwrap();
    let (g_doc, g_chunk) = (out.doc_id, out.chunk_ids[0]);
    assert_eq!(
        g_doc as usize,
        T4_MAIN.len(),
        "`I8-7`：新段的全局 ID 必须从「主段已用长度」起步（重叠即分数被合并）"
    );

    // 认领函数：按词面唯一的 query 找到那条命中，返回它的 `(doc_id, chunk_id)`
    let claimed = |s: &Searcher| -> Vec<(DocId, ChunkId)> {
        resp(s, "蓝莓派")
            .hits
            .iter()
            .map(|h| (h.doc_id, h.chunk_id))
            .collect()
    };

    assert_eq!(
        resp(&idx.searcher(), T4_QUERIES[0]).metrics.segments,
        2,
        "前提：合并前是「主段 + 1 个 delta」"
    );
    assert_eq!(
        claimed(&idx.searcher()),
        vec![(g_doc, g_chunk)],
        "前提：合并前该文档恰好命中一条，且 ID 就是 `add` 返回的那个"
    );

    // ① 合并（`save` 第一步 = `fold_deltas`）之后 ID 不变
    let p_folded = dir.path().join("t8-folded.idx");
    idx.save(&p_folded).unwrap();
    assert_eq!(
        resp(&idx.searcher(), T4_QUERIES[0]).metrics.segments,
        1,
        "前提：合并必须真的发生（`deltas` 清空 ⇒ 回到单段）"
    );
    assert_eq!(
        claimed(&idx.searcher()),
        vec![(g_doc, g_chunk)],
        "合并**不得**改 ID（`D-S8-04`；改 ID 的是 `compact()`，两者不可混用）"
    );

    // ② `load` 往返之后 ID 仍不变（快照里的全局 ID 语义正确 ⇒ 不是「本地 ID 当全局写盘」）
    let reloaded = bm25_builder().load(&p_folded).unwrap();
    assert_eq!(reloaded.num_chunks(), T4_MAIN.len() as u32 + 1);
    assert_eq!(
        claimed(&reloaded.searcher()),
        vec![(g_doc, g_chunk)],
        "`save`/`load` 往返必须保 ID（否则 Agent 记住的 `chunk_id` 在重启后指向别的分片）"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 7) `compact()` × 多写端：ID 空间**换代**（评审 P2-4 的端到端回归锁）
// ══════════════════════════════════════════════════════════════════════════

/// **`S8-03` 评审 P2-4 的端到端回归锁**：`compact()` 重编号之后，**另一个写端**持旧世代的
/// builder 再 `commit()` 必须被拒 —— 本用例刻意构造**基址等值**的 **ABA 窗口**，
/// 使得「只有基址对账」的实现在这里**必然放行**（= 静默错删）。
///
/// # ABA 窗口的构造（逐步骤）
///
/// ```text
/// ① A: add(甲) → commit      ⇒ ids.next = (1, 1)；视图中 doc 0 = 甲
/// ② A: remove(0) → commit    ⇒ 发布跨段墓碑 {0}（`compact` 需要墓碑才不早退）
/// ③ B = searcher.into_index() ⇒ B.builder.base = (1, 1)、epoch = 0
/// ④ B: remove(0) + add(乙)    ⇒ B 的 builder 里有一条**指向旧 ID 空间**的墓碑
/// ⑤ A: compact()              ⇒ 重编号 ⇒ epoch 0 → 1、ids 归零（旧 ID 空间作废）
/// ⑥ A: add(丙) → commit       ⇒ ids.next 回到 (1, 1) —— **恰好等于 B 记录的基址**
/// ⑦ B: commit()               ⇒ 若只比基址 ⇒ 通过 ⇒ B 的墓碑 {0} 被并进视图
///                               ⇒ 它指向的是**重编号后的另一个文档**（= 丙）
/// ```
///
/// ⇒ 判据 = ⑦ 必须报 `Busy` 且点名换代；反向自证 = 丙**仍然可召回**（若放行，
/// 丙会被那条过期墓碑挡住 —— 这正是「静默错删」在检索层的表现）。
#[test]
fn S8_03_compact换代后另一写端的提交被拒() {
    let mut a = bm25_builder().build();
    let first = a.add(Document::new("甲 云端同步")).unwrap();
    a.commit().unwrap();
    // `remove` 目标在**既往段** ⇒ 记跨段墓碑（在 A 的 builder 上）
    a.remove(first.doc_id).unwrap();
    a.commit().unwrap();

    // 第二个写端：与 A 共享同一个 `Arc<Shared>`（`into_index()` 总是成功 ⇒ 双写端入口）。
    // ⚠️ 在**克隆**上调用：`into_index(self)` 消耗读端，而 `Searcher::clone` 共享同一个
    //    `Arc<Shared>` —— 「clone 残留」与「写端存活」不可区分正是双写端存在的理由。
    let searcher = a.searcher();
    let mut b = searcher.clone().into_index().unwrap();
    b.remove(first.doc_id).unwrap(); // B 也记一条指向**旧 ID 空间**的墓碑
    b.add(Document::new("乙 边车代理")).unwrap();

    // A 侧 compact：有墓碑（`chunks_total > chunks_alive`）⇒ 真的重编号
    a.compact().unwrap();
    // 重编号后 doc 0 重新可用 ⇒ A 再写一篇就让 `ids.next_doc` 回到 B 记录的基址
    let reused = a.add(Document::new("丙 熔断降级")).unwrap();
    a.commit().unwrap();
    assert_eq!(
        reused.doc_id, first.doc_id,
        "前提：重编号 + 新写入把 `ids.next_*` 送回 B 记录的基址（ABA 窗口成立）"
    );

    // ★ 旧世代的提交必须被 **epoch** 拦下（基址等值检查在这个窗口里失效）
    let err = b.commit().unwrap_err();
    assert!(
        matches!(err, Error::Busy(_)),
        "换代后旧世代的提交必须报 Busy，实际 = {err:?}"
    );
    assert!(
        err.to_string().contains("epoch 0 → 1"),
        "错误消息必须点名换代（可诊断，NFR-07），实际 = {err}"
    );
    // 🔑 **接线判据**（第 3 轮评审 **P4**）：这条拒绝来自 `commit()` ⇒ 恢复指引必须是 **commit 版**。
    //    世代对账被 `commit()` 与 `fold_deltas()` **共用**（`PublishCaller`）⇒ 若有人把两个调用方
    //    的身份**误传**（例如 `commit()` 传了 `Fold`），「两条文案不同」那条单测是**抓不到**的。
    //    这里把**生产接线**钉在端到端路径上。
    //    ⚠️ `fold` 侧的接线仍无确定性判据（要构造「另一写端在 `fold` 窗口内 `compact()`」的交错）
    //    ⇒ 见 CHANGELOG 的「覆盖边界」。
    assert!(
        err.to_string().contains("已被丢弃"),
        "`commit()` 这条拒绝必须带 **commit 版**恢复指引（`fold` 版的文案里**没有**「已被丢弃」）；\
         实际 = {err}"
    );

    // 反向自证：A 的文档不得被 B 的过期墓碑挡住（放行的后果）
    assert_eq!(
        searcher.search("熔断降级").unwrap().hits.len(),
        1,
        "A 的文档必须仍可召回 —— 若 B 的提交被放行，它的墓碑 {{doc 0}} 会挡住本条"
    );
    // B 侧的内容没有进视图（被拒 = 什么都没发布）
    assert_eq!(
        searcher.search("边车代理").unwrap().hits.len(),
        0,
        "被拒的提交不得留下任何已发布内容"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 8) `S8-T5`：跨段向量检索（`S8-05` 的**门测试**，设计 §7 明写）
// ══════════════════════════════════════════════════════════════════════════

/// `S8-T5` 语料（6 篇）。⚠️ 顺序是判据的一部分：跨段侧按 `T5_SPLIT` 切成两段。
const T5_CORPUS: [&str; 6] = [
    "向量检索把文本编码成稠密向量",
    "分段写入把新内容追加成独立的段",
    "跨段向量检索必须对每段各查一次",
    "全局归并按距离与全局分片编号排序",
    "批量写入先落在写端私有的增量段",
    "查询向量只编码一次以免成本翻倍",
];
const T5_SPLIT: usize = 2;
const T5_QUERIES: [&str; 3] = ["向量检索", "分段写入", "全局归并"];

/// 造「**单段视图**」的参照系：内容全在 `main` 里（`deltas` 空）⇒ 本地 `chunk_id` == 全局。
///
/// 🔑 用 `save`/`load` 而不是裸 `commit`：`commit` 会把内容放进 **delta**（那样两侧都是跨段
/// 路径，「跨段 vs 单段」就比不出来了）。`D-S8-01` 的 `save` 第一步就是 `fold_deltas`
/// ⇒ 折进 `main`；`Brute` 后端下 `load` 重建的向量与原件**同值同 id** ⇒ 逐位比较仍然严格。
fn t5_single_segment(corpus: &[&str], path: &std::path::Path) -> Searcher {
    let mut idx = hybrid_builder().build();
    for t in corpus {
        idx.add(Document::new(*t)).unwrap();
    }
    idx.commit().unwrap();
    idx.save(path).unwrap();
    hybrid_builder().load(path).unwrap().searcher()
}

/// 造**跨段**布局：前 `T5_SPLIT` 篇一个 delta、其余一个 delta（`main` 为空）。
fn t5_two_segments(corpus: &[&str], split: usize) -> Searcher {
    let mut idx = hybrid_builder().build();
    for t in &corpus[..split] {
        idx.add(Document::new(*t)).unwrap();
    }
    idx.commit().unwrap();
    for t in &corpus[split..] {
        idx.add(Document::new(*t)).unwrap();
    }
    idx.commit().unwrap();
    idx.searcher()
}

/// 造**「`main` 非空 + 两个 delta」**布局：`main(2) + delta(2) + delta(2)`。
///
/// # 为什么必须有这个夹具（`S8-05` 第 1 轮评审 **P3-1**，**已实证的覆盖盲区**）
///
/// 其余跨段向量用例的 `main` **恒为空**（`t5_two_segments` / 逐段谓词用例 / `S8_02_旧API`
/// 的 `into_searcher` 路径）⇒ 「向量路静默漏查 `main`」（= 库里**最大、最先建**的那一段）
/// 这一类变异**全仓不可检测**：实测在 `search_segmented` 的循环里注入
/// `if i == 0 { continue; }`（向量路**永不**查 `main`）之后，那三条用例**全部照常绿**。
///
/// ⚠️ 而 `helix serve` 的常态恰恰是「`main` 有存量 + `delta` 有增量」（`save`/`compact`
/// 之后又 `commit`）⇒ 该变异若真实发生（例如有人把段来源换成 `deltas.iter()`、
/// 或归并循环的起点写错），**存量内容的向量召回会静默消失**。
/// 本夹具让 `main` 非空 ⇒ 漏查 `main` 立刻表现为**结果少 / 错**，判据才有牙齿。
///
/// 造法：`add ×2 → commit → save`（`D-S8-01`：`save` 第一步 `fold_deltas` ⇒ 折进 `main`）
/// `→ add ×2 → commit → add ×2 → commit`。
/// 🔑 它同时锁住**两个分支**：「空段参与归并只贡献空集」与「非空段正常并入」。
fn t5_main_and_two_deltas(corpus: &[&str], split: usize, path: &std::path::Path) -> Searcher {
    let mut idx = hybrid_builder().build();
    for t in &corpus[..split] {
        idx.add(Document::new(*t)).unwrap();
    }
    idx.commit().unwrap();
    idx.save(path).unwrap(); // `save` 的第一步就是 `fold_deltas` ⇒ 此后 `main` 非空
    for t in &corpus[split..split * 2] {
        idx.add(Document::new(*t)).unwrap();
    }
    idx.commit().unwrap();
    for t in &corpus[split * 2..] {
        idx.add(Document::new(*t)).unwrap();
    }
    idx.commit().unwrap();
    idx.searcher()
}

/// 「跨段视图」与「**同一份内容**的单段参照系」在 `vector` 模式下必须**逐位一致**。
///
/// 🔑 抽成助手（`S8-05` 评审 P3-1 的配套）：两个布局变体（`main` 空 / `main` 非空）
/// 用**同一套判据** —— 否则将来改判据会漏掉一个，正是「一类缺陷只修被点到的那一处」的同族错误。
/// `label` 只进断言消息，让变异下能一眼看出**是哪个布局**红的。
/// `segments` 是被测侧应有的段数（两个布局都是 3，但显式传入以免助手隐含假设）。
fn assert_vector_cross_eq_single(one: &Searcher, many: &Searcher, segments: usize, label: &str) {
    for q in T5_QUERIES {
        let a = one
            .search_with(q)
            .mode(SearchMode::Vector)
            .top_n(6)
            .exec()
            .unwrap();
        let b = many
            .search_with(q)
            .mode(SearchMode::Vector)
            .top_n(6)
            .exec()
            .unwrap();

        assert!(
            !a.hits.is_empty(),
            "[{label}] query `{q}`：参照系必须有命中（否则下面的逐位比较是空转）"
        );
        assert_eq!(
            (a.metrics.segments, a.metrics.vector_segments),
            (1, 1),
            "[{label}] 参照系必须是**单段**视图（`deltas` 空 ⇒ 走单索引路径）"
        );
        assert_eq!(
            (b.metrics.segments, b.metrics.vector_segments),
            (segments, segments),
            "[{label}] 被测侧必须**真的跨段**（{segments} 段）；\
             ⚠️ 这也是 `S8-05` 的验收项：`vector_segments` 恒等于 `segments`"
        );
        assert_eq!(
            a.metrics.vector_route, b.metrics.vector_route,
            "[{label}] `Brute` 恒精确 ⇒ 两侧的逐段并集分类都应是 `Exact`（不能是 `Mixed`）"
        );
        assert_eq!(
            format!("{:?}", a.hits),
            format!("{:?}", b.hits),
            "[{label}] query `{q}`：`Brute` 后端下跨段必须与单段**逐位一致**（含 score / explain）"
        );
    }
}

/// **`S8-T5`（本 PR 的门）**：`Brute` 后端下 `vector` 模式**跨段 == 单段**、**逐位一致**。
///
/// # 为什么只承诺 `Brute`（设计 §4.6.2）
///
/// 每段的线性扫描是**精确**的，归并又复用同一个 `to_scored`（`score = 1 − d²/2` 单调
/// ⇒ 距离升序 ≡ 相似度降序）⇒ 「每段各自前 `k`」的并集截断**就是**全局前 `k`
/// （无损证明见 `SegmentedVectorRetriever` 的类型文档）。
/// ⚠️ `Hnsw` 下**不承诺**逐位一致（两张图 ≠ 一张图）—— 本用例**刻意只用 `Brute`**，
/// 把「归并写对了」与「ANN 本身的近似性」两件事**分开**：否则一条红无法归因。
///
/// # 判据里的三处前提自证（否则本用例会退化成「空 == 空」）
///
/// ① 参照系 `vector_segments == segments == 1`（真的走了单段路径）；
/// ② 被测侧 `vector_segments == segments == 3`（`main` + 两个 delta，**真的跨段**）；
/// ③ 命中非空。
///
/// ⚠️ **本变体的 `main` 是空的**（布局 `main(0) + delta(2) + delta(4)`）——
/// 单靠它**抓不住「漏查 `main`」**（评审 P3-1 已实证）。那一类由**同批的**
/// `S8_T5_main非空与delta并存时跨段仍逐位一致` 钉住，两者必须**都在**。
#[test]
fn S8_T5_Brute后端vector模式跨段逐位一致() {
    let dir = tempfile::tempdir().unwrap();
    let one = t5_single_segment(&T5_CORPUS, &dir.path().join("t5-one.idx"));
    let many = t5_two_segments(&T5_CORPUS, T5_SPLIT);
    assert_vector_cross_eq_single(&one, &many, 3, "main 空");
}

/// **`S8-T5` 的第二布局变体（`S8-05` 第 1 轮评审 P3-1 的回归锁）**：
/// `main` **非空** + 两个 delta 时，跨段仍必须与单段**逐位一致**。
///
/// # 它堵的是什么
///
/// 上一条（以及逐段谓词 / `S8_02_旧API`）的夹具里 `main` **恒为空** ⇒
/// 「向量路**漏查 `main`**」这一类变异在那些用例上**全部照常绿**（**修前已实证**：
/// 在 `search_segmented` 的循环注入 `if i == 0 { continue; }`，三条用例 17/17 绿）。
/// 本变体的 `main` 里有 2 篇（且是**全局 `chunk_id` 最小**的那两篇）⇒
/// 漏查它必然让结果**少条目 / 错排序** ⇒ 逐位断言当场红。
///
/// 🔑 这也正是「**夹具的要害**」在**段维度**上的同一教训（对照：逐段谓词用例最初的
/// tag 图样与分段点周期对齐 ⇒ 变异零命中）：**判据的鉴别力取决于夹具覆盖到哪些分支**，
/// 而「`main` 是否非空」是归并路径上的一条**真实分叉**，不是可有可无的装饰。
#[test]
fn S8_T5_main非空与delta并存时跨段仍逐位一致() {
    let dir = tempfile::tempdir().unwrap();
    // 参照系与上一条**共用同一份 6 篇内容与同一顺序** ⇒ 两次比较的期望值同一个
    // （全局 `chunk_id` 按插入顺序分配 ⇒ 两套布局的 ID 空间一致）。
    let one = t5_single_segment(&T5_CORPUS, &dir.path().join("t5-one.idx"));
    let many = t5_main_and_two_deltas(&T5_CORPUS, T5_SPLIT, &dir.path().join("t5-main.idx"));
    assert_vector_cross_eq_single(&one, &many, 3, "main 非空");
}

/// **`S8-05`：逐段谓词必须折算成「段内本地」语义**（`SegmentPredicate`）。
///
/// # 这条抓的是什么
///
/// `VectorIndex::search_filtered` 的谓词收的是**段内本地 `chunk_id`**，而跨段谓词
/// （`ViewFilter`）收的是**全局 `chunk_id`**。若逐段下推时把**全局**谓词直接交下去：
/// 主段（`base_chunk == 0`）**看不出问题**，而 delta 段会把「本地 id」当成「全局 id」去判
/// ⇒ 过滤决策**来自别的段**⇒ 结果错（不是少召回，是**答错**）。
///
/// 判据：带**用户过滤**（`FilterKind::Filtered`）的跨段结果必须与**单段参照系**逐位一致。
#[test]
fn S8_05_逐段谓词的语义折算_带过滤的跨段向量与单段一致() {
    // 🔴 **tag 图样必须与分段点「错位」**（两条 delta 的 tag 序列互为反相）——
    //    这是本条判据的**夹具要害**：若两段的 tag 序列相同（周期 == 分段长度），
    //    那么「把本地 id 当成全局 id 查」会**恰好得到相同结论** ⇒ 变异下断言**照样绿**
    //    （实测：M-S8-05-b 在「周期对齐」的语料上零命中 ⇒ 换成本图样后才命中）。
    //    本图样下 A=[keep,drop]、B=[drop,keep] ⇒ 错位查表**必然**既漏又错。
    let docs = [
        ("向量检索把文本编码成稠密向量", "keep"),
        ("分段写入把新内容追加成独立的段", "drop"),
        ("跨段向量检索必须对每段各查一次", "drop"),
        ("全局归并按距离与全局分片编号排序", "keep"),
    ];
    let make = |idx: &mut SearchIndex| {
        for (t, tag) in docs {
            let mut d = Document::new(t);
            d.metadata = serde_json::json!({ "tag": tag });
            idx.add(d).unwrap();
        }
    };

    // 参照系：单段视图
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t5-filt.idx");
    let mut one = hybrid_builder().build();
    make(&mut one);
    one.commit().unwrap();
    one.save(&path).unwrap();
    let one = hybrid_builder().load(&path).unwrap().searcher();

    // 被测：**两段**布局（「前 2 篇 / 后 2 篇」两个 delta，`tag=keep` 恰好横跨两段）。
    // ⚠️ 单 delta 时「本地 `chunk_id` == 全局」⇒ 该布局**测不出**折算错误 ⇒ 必须真的两段。
    let mut split = hybrid_builder().build();
    for (t, tag) in &docs[..2] {
        let mut d = Document::new(*t);
        d.metadata = serde_json::json!({ "tag": tag });
        split.add(d).unwrap();
    }
    split.commit().unwrap();
    for (t, tag) in &docs[2..] {
        let mut d = Document::new(*t);
        d.metadata = serde_json::json!({ "tag": tag });
        split.add(d).unwrap();
    }
    split.commit().unwrap();
    let split = split.searcher();

    let keep = Filter::eq("tag", "keep");
    let q = "向量";
    let a = one
        .search_with(q)
        .mode(SearchMode::Vector)
        .filter(&keep)
        .top_n(10)
        .exec()
        .unwrap();
    let b = split
        .search_with(q)
        .mode(SearchMode::Vector)
        .filter(&keep)
        .top_n(10)
        .exec()
        .unwrap();

    assert_eq!(a.metrics.segments, 1, "参照系是单段");
    assert_eq!(b.metrics.segments, 3, "被测侧是 main + 两个 delta");
    assert_eq!(
        b.metrics.allowed, a.metrics.allowed,
        "前提：两侧的**全局** allowed 必须相同（同一个过滤条件）"
    );
    assert!(!a.hits.is_empty(), "前提：必须有命中（否则退化成空 == 空）");
    assert!(
        a.hits.iter().all(|h| {
            matches!(
                &h.metadata,
                serde_json::Value::Object(m) if m.get("tag") == Some(&serde_json::json!("keep"))
            )
        }),
        "前提：过滤真的生效（参照系结果里不得出现 tag=drop）"
    );
    assert_eq!(
        format!("{:?}", a.hits),
        format!("{:?}", b.hits),
        "带过滤的跨段向量必须与单段逐位一致 —— \
         若把**全局**谓词直接下推给 delta 段（本地 id 被当成全局 id 判），本行必红"
    );
}

/// **`S8-05`：`union_route` 的并集分类**（设计 §4.6.1 / Q5）。
///
/// 单测而非端到端：造「一段 `Exact` + 一段 `Ann`」需要 `hnsw` 的 `allowed` 跨过
/// `BRUTE_FALLBACK_MAX_ALLOWED`（8192 个活分片）⇒ 端到端要万级语料。**分类规则**本身是纯函数
/// ⇒ 在这里锁死；「逐段 route 真的被收集并归并」由 `S8_T5` 的 `vector_route` 相等断言锁
/// （`Brute` 两端都 `Exact` ⇒ 若实现写死 `Ann` 就会红）。
#[test]
fn S8_05_union_route的并集分类() {
    use VectorRoute::{Ann, Exact, Mixed, None as RNone};
    assert_eq!(union_route(&[]), RNone, "没有任何参与段 ⇒ 未走向量路");
    assert_eq!(
        union_route(&[RNone, RNone]),
        RNone,
        "`None` 项（未参与的段）必须被忽略"
    );
    assert_eq!(union_route(&[RNone, Ann]), Ann, "全 Ann ⇒ Ann");
    assert_eq!(
        union_route(&[Exact, Exact, RNone]),
        Exact,
        "全 Exact ⇒ Exact（`None` 项忽略）"
    );
    assert_eq!(union_route(&[Ann, Exact]), Mixed, "混合 ⇒ Mixed");
    assert_eq!(
        union_route(&[Exact, Ann, Exact]),
        Mixed,
        "混合 ⇒ Mixed（与顺序无关）"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 10) `S8-06` 合并器（`merge_pending` / `merge_all` / `MergeReport`）
// ══════════════════════════════════════════════════════════════════════════

/// **`S8-06` 原语的契约**（新公开面）：
/// `merge_pending()` 一次只并**队首一段**、无未合并段时返回 `Ok(None)`；
/// `merge_all()` 并掉全部（无增量段时是 no-op）；`MergeReport` 的读数与视图状态一致。
///
/// # 为什么要单钉契约（而不是只靠端到端）
///
/// 「一次一段」是**宿主做后台合并**的唯一抓手（`D-S8-08`：库不 `spawn` 线程），
/// 而它一旦写成「一次全并」，宿主就**无法控制单次合并的开销**（`O(N)` 与 `O(delta)` 差两个量级）。
#[test]
fn S8_06_合并原语的契约_一次一段与空段返回None() {
    // ① 纯 BM25 装配 ⇒ 无向量侧工作 ⇒ `vector_strategy` 应为 `None`
    let mut bm = bm25_builder().build();
    for t in ["甲甲甲", "乙乙乙", "丙丙丙"] {
        bm.add(Document::new(t)).unwrap();
        bm.commit().unwrap();
    }
    let segs = |idx: &SearchIndex| {
        idx.searcher()
            .search_with("甲甲甲")
            .mode(SearchMode::Bm25)
            .top_n(10)
            .exec()
            .unwrap()
            .metrics
            .segments
    };
    assert_eq!(segs(&bm), 4, "前提：main + 3 个 delta = 4 段");

    let r1 = bm
        .merge_pending()
        .unwrap()
        .expect("有未合并段 ⇒ 应返回 Some");
    assert_eq!(
        r1.segments_merged, 1,
        "merge_pending 一次只并 FIFO 队首那一段"
    );
    assert_eq!(r1.chunks_merged, 1, "本次并入 1 个活分片");
    assert_eq!(segs(&bm), 3, "并掉一段后段数应减 1（其余段仍在）");
    assert_eq!(
        r1.vector_strategy,
        VectorMergeStrategy::None,
        "纯 BM25 装配没有向量侧工作 ⇒ 必须是 None（不是 Rebuild —— 别把「无」记成「重建」）"
    );

    let r2 = bm.merge_pending().unwrap().expect("还有两段");
    let r3 = bm.merge_pending().unwrap().expect("还有一段");
    assert_eq!((r2.segments_merged, r3.segments_merged), (1, 1));
    assert_eq!(segs(&bm), 1, "三段并完 ⇒ 只剩主段");
    assert!(
        bm.merge_pending().unwrap().is_none(),
        "无未合并段 ⇒ 必须返回 Ok(None)（而不是 Some(空报告)）—— 宿主靠它判「干完了」"
    );

    // ② 无增量段时 `merge_all` 是 no-op，且**不推进 generation**
    let r4 = bm.merge_all().unwrap();
    assert_eq!(r4.segments_merged, 0, "无增量段 ⇒ no-op");
    assert_eq!(r4.tombstones_applied, 0);
    assert_eq!(r4.vector_strategy, VectorMergeStrategy::None);

    // ③ 带向量装配但后端无图（Brute）⇒ 走兜底 `Rebuild`：
    //    `as_graph_persist()` 对 Brute 返回 `None`（类型事实）⇒ 方案 A 不可用。
    let mut br = hybrid_builder().build();
    for t in ["向量检索甲", "向量检索乙"] {
        br.add(Document::new(t)).unwrap();
        br.commit().unwrap();
    }
    let r5 = br.merge_all().unwrap();
    assert_eq!(r5.segments_merged, 2);
    assert_eq!(
        r5.vector_strategy,
        VectorMergeStrategy::Rebuild,
        "Brute 无图 ⇒ 方案 A 不可用 ⇒ 必须如实记 Rebuild（不是 Incremental）"
    );

    // ④ 合并**不改 ID**（D-S8-04）：把 ID 抓在合并前后比
    let mut idx = bm25_builder().build();
    let mut ids = Vec::new();
    for t in ["保持标识甲", "保持标识乙"] {
        let out = idx.add(Document::new(t)).unwrap();
        idx.commit().unwrap();
        ids.push(out.chunk_ids[0]);
    }
    idx.merge_all().unwrap();
    let hits = idx
        .searcher()
        .search_with("保持标识")
        .mode(SearchMode::Bm25)
        .top_n(10)
        .exec()
        .unwrap();
    let got: Vec<ChunkId> = hits.hits.iter().map(|h| h.chunk_id).collect();
    for want in &ids {
        assert!(
            got.contains(want),
            "合并不得改 chunk_id（D-S8-04）：{want} 未出现在 {got:?}"
        );
    }
}

/// **`S8-T13`**：合并后的**字段索引**求值 == 全量重建（设计 §4.9.2）。
///
/// `Index::merge_from` **不能** `extend` 字段索引（它是 field → value → doc 位图，且带
/// 「基数保护」的降级判定）⇒ 走 `FieldIndex::rebuild` 全量重建。**重建后的降级结论可能与
/// 增量维护不同**，正是这条要钉的。
///
/// 判据：分段写入 + `merge_all()` 的**过滤 battery** 结果 == **单段建库**的同 battery 结果
/// （逐位一致）。语料含两类字段：`grp`（低基数）+ `ser`（**高基数**，200 个唯一值 ⇒
/// 尽量压到基数保护/降级的判定路径上）。
#[test]
fn S8_T13_合并后的字段索引与单段全量重建等价() {
    const N: usize = 200;
    let docs: Vec<(String, String, String)> = (0..N)
        .map(|i| {
            (
                format!("字段索引语料 {i} 检索"),
                format!("g{}", i % 3), // 低基数：3 个值
                format!("s{i}"),       // 高基数：200 个唯一值
            )
        })
        .collect();

    let make = |idx: &mut SearchIndex, slice: &[(String, String, String)]| {
        for (text, grp, ser) in slice {
            let mut d = Document::new(text.clone());
            d.metadata = serde_json::json!({ "grp": grp, "ser": ser });
            idx.add(d).unwrap();
        }
        idx.commit().unwrap();
    };

    // 参照：**单段**建库（一次 add 全部 ⇒ 字段索引由增量维护得出）
    let dir = tempfile::tempdir().unwrap();
    let p_ref = dir.path().join("t13-ref.idx");
    let mut one = bm25_builder().build();
    make(&mut one, &docs);
    one.save(&p_ref).unwrap();
    let one = bm25_builder().load(&p_ref).unwrap().searcher();

    // 被测：**分 4 段**写入（每 50 篇一个 delta）再 `merge_all()`
    let mut many = bm25_builder().build();
    for chunk in docs.chunks(50) {
        make(&mut many, chunk);
    }
    let before = many
        .searcher()
        .search_with("检索")
        .mode(SearchMode::Bm25)
        .top_n(5)
        .exec()
        .unwrap()
        .metrics
        .segments;
    assert_eq!(before, 5, "前提：main + 4 个 delta = 5 段");
    let rep = many.merge_all().unwrap();
    assert_eq!(rep.segments_merged, 4, "应并掉 4 个增量段");
    let many = many.searcher();

    // 过滤 battery：两个字段 × 多个值 ⇒ 覆盖「高基数」「低基数」「无命中」三类
    let mut battery: Vec<Filter> = Vec::new();
    for g in ["g0", "g1", "g2", "g9"] {
        battery.push(Filter::eq("grp", g));
    }
    for i in [0usize, 7, 55, 199, 9999] {
        battery.push(Filter::eq("ser", format!("s{i}")));
    }
    let mut nonempty = 0usize;
    for f in &battery {
        let a = one
            .search_with("检索")
            .mode(SearchMode::Bm25)
            .filter(f)
            .top_n(50)
            .exec()
            .unwrap();
        let b = many
            .search_with("检索")
            .mode(SearchMode::Bm25)
            .filter(f)
            .top_n(50)
            .exec()
            .unwrap();
        if !a.hits.is_empty() {
            nonempty += 1;
        }
        assert_eq!(
            a.metrics.allowed, b.metrics.allowed,
            "allowed 必须一致（字段索引等价的前提量）"
        );
        assert_eq!(
            format!("{:?}", a.hits),
            format!("{:?}", b.hits),
            "合并后的字段索引求值必须与单段建库逐位一致（`FieldIndex::rebuild` 的等价性）"
        );
    }
    assert!(
        nonempty >= 5,
        "前提：battery 里至少有 5 条有命中（否则「空 == 空」是空转），实测 {nonempty}"
    );
}

/// **`S8-T14`**：合并器**向量侧不丢点**（设计 §7 的 `S8-T14`）。
///
/// # ⚠️ 判据口径**与设计原文不同**（已单列报评审）
///
/// 设计 §4.9.3 判据③ 写「合并后的图与全量重建图的 top-10 重合率 **≥ 0.99**」。**spike S8-S1**
/// 实测该阈值**结构性不可达**：`hnsw_rs` 用**无种子 `OsRng`** 分配层级 ⇒ **同一份数据重建
/// 两次**的图（B vs B′）只有 **0.9280** 的重合率 ⇒ 任何实现都到不了 0.99。
/// ⇒ 本条改为以 **`Brute`（精确）为参照**量 `recall@10` —— 它**不受拓扑抖动污染**，
/// 直接回答「合并有没有丢点」这个真问题。
///
/// 参照库与库的被测侧用**同一个 `FakeEmbedder`** ⇒ 向量逐位相同 ⇒ 差异只来自「图」。
#[test]
fn S8_T14_合并后的向量侧不丢点_以精确后端为参照() {
    let texts: Vec<String> = (0..120).map(|i| format!("向量合并语料 {i} 检索")).collect();

    // 参照：`Brute` 后端 = **精确**（线性扫描）⇒ 它的 top-10 就是真值
    let dir = tempfile::tempdir().unwrap();
    let p_ref = dir.path().join("t14-ref.idx");
    let mut brute = hybrid_builder().build(); // hybrid_builder 用 Brute
    for t in &texts {
        brute.add(Document::new(t.clone())).unwrap();
    }
    brute.commit().unwrap();
    brute.save(&p_ref).unwrap();
    let brute = hybrid_builder().load(&p_ref).unwrap().searcher();

    // 被测：**Hnsw** 后端。
    //
    // ⚠️ **场景必须是「主段非空」**（否则测不到方案 A）：首次合并时内容**全在增量段**里，
    //    主段图是**空图** ⇒ `hnsw_rs` 的 `file_dump` 对空图报错 ⇒ A 结构上不可用（走 B）。
    //    真实场景里「`save` 之后又写了增量」才是常态 ⇒ 本用例按它构造。
    let p_h = dir.path().join("t14-hnsw.idx");
    let mut hnsw = hybrid_builder().vector_backend(VectorBackend::Hnsw).build();
    for t in &texts[..40] {
        hnsw.add(Document::new(t.clone())).unwrap();
    }
    hnsw.commit().unwrap();
    hnsw.save(&p_h).unwrap(); // 内容折进 main（main 的图非空）
    let mut hnsw = hybrid_builder()
        .vector_backend(VectorBackend::Hnsw)
        .load(&p_h)
        .unwrap();
    for chunk in texts[40..].chunks(40) {
        for t in chunk {
            hnsw.add(Document::new(t.clone())).unwrap();
        }
        hnsw.commit().unwrap();
    }
    let rep = hnsw.merge_all().unwrap();
    assert_eq!(rep.segments_merged, 2, "应并掉 2 个增量段");
    assert_eq!(
        rep.vector_strategy,
        VectorMergeStrategy::Incremental,
        "Hnsw 有图 ⇒ 必须走方案 A（Incremental）—— 若这里是 Rebuild，说明 A 静默失败了"
    );
    let hnsw = hnsw.searcher();

    let mut sum = 0.0f64;
    let mut queries = 0usize;
    for q in &texts[..20] {
        // 用文档原文当 query：它与自己的向量最接近 ⇒ 真值里必然含它自己
        let t = brute
            .search_with(q)
            .mode(SearchMode::Vector)
            .top_n(10)
            .exec()
            .unwrap();
        let g = hnsw
            .search_with(q)
            .mode(SearchMode::Vector)
            .top_n(10)
            .exec()
            .unwrap();
        assert!(
            !t.hits.is_empty(),
            "前提：精确参照必须有命中（否则 recall 无意义）"
        );
        let ts: std::collections::HashSet<ChunkId> = t.hits.iter().map(|h| h.chunk_id).collect();
        let gs: std::collections::HashSet<ChunkId> = g.hits.iter().map(|h| h.chunk_id).collect();
        sum += ts.intersection(&gs).count() as f64 / ts.len() as f64;
        queries += 1;
    }
    let recall = sum / queries as f64;
    assert!(
        recall >= 0.9,
        "合并后的 Hnsw 图相对精确路径的 recall@10 = {recall:.4}，应 ≥ 0.9（丢点会让它显著下滑）"
    );
}
