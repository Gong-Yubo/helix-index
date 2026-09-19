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
use helix_core::query::{Hit, SearchMode, SearchResponse};
use helix_core::search::{SearchIndex, SearchIndexBuilder, Searcher, VectorBackend};
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
    // 🔴 **`S8-03` 起的已知边界（本 PR 显式钉住，不掩盖）**：`S8-03` 后内容落在 `deltas`，
    //    而**向量路只覆盖 `main`**（跨段向量属 `S8-05` / PR5）⇒ 本夹具下 hybrid 的命中
    //    **全部来自 BM25 lane**。⇒ 断言由 `S8-02` 期的「必须有向量路召回」**反转**为
    //    「当前不应有」，**PR5 落地后必须再反转回 `> 0`**。
    //    ⚠️ 被替换掉的那条前提（「必须用带向量 lane 的装配」）在 PR4→PR5 窗口内**结构上
    //    无法成立**；但本用例的鉴别力**并不依赖它**：`S8-03` 后「旧 API（隐含 `commit`）」
    //    与「新 API（显式 `commit`）」的差异在**任何装配**下都可见（不 `commit` 就什么都没
    //    发布 ⇒ 命中数 `0` vs `N`），纯 BM25 也一样。
    assert_eq!(
        vector_recalled,
        0,
        "PR4→PR5 窗口：向量路只覆盖 main，而本夹具内容全在 deltas ⇒ 不应出现向量路召回\
         （PR5 落地后本断言必须反转）。explain = {:?}",
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
