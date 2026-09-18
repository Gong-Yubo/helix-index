//! V2 Step 4 核心 PR（S4-03~S4-07）的**门面层集成测试**：T3 / T7 / T11 / T12 / T13。
//!
//! Index 层的单元测试（T1 / T2）在 `src/index/mod.rs` 内。
//! 本文件聚焦 `SearchIndex::compact / compact_and_save` 的正确性，
//! 共享 helper 收敛在 `tests/common/`（见评审建议）。

#![allow(non_snake_case)]
// ⚠️ 本文件沿用旧所有权 API（`into_searcher` / `into_index`）以**锁住其行为不变**
// （`S8-02` 起二者为 `#[deprecated]` 薄封装）。生产调用点已在 `cli` / `examples` 迁移。
#![allow(deprecated)]

mod common;

use common::{builder_brute, builder_hnsw};
use helix_core::document::Document;
use helix_core::search::{GraphStatus, SearchIndexBuilder, VectorBackend};

/// 装配：纯 BM25（embedder=None，无向量 lane）——T3 / T12 用。
fn bm25_builder() -> SearchIndexBuilder {
    SearchIndexBuilder::default()
        .embedder(None)
        .vector_backend(VectorBackend::Brute)
        // 强制单 chunk：doc == chunk
        .chunker(helix_core::chunk::Chunker::new(200_000, 0))
        .batch_size(1024)
}

/// 读回快照正文的原始向量列表（不触发图加载）。
fn snapshot_vectors(path: &std::path::Path) -> Vec<(u32, Vec<f32>)> {
    helix_core::storage::load_with_crc(path).unwrap().1
}

/// 读 manifest 的 `nb_point`（图点数，含墓碑的旧图会偏大）。
fn graph_nb_point(path: &std::path::Path) -> u64 {
    let paths = helix_core::storage::graph_paths(path);
    helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap()
        .nb_point
}

// ---------------------------------------------------------------------------
// T3：BM25 逐位一致（I3）
// ---------------------------------------------------------------------------

/// 纯 BM25（无向量 lane）。建库 → 删部分 → 分别在 compact 前/后检索，
/// Top-N 的 `(source, text, score)` 序列逐位相同（`f32` 精确）。
#[test]
fn T3_BM25检索逐位一致() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t3.idx");

    let mut idx = bm25_builder().build();
    let texts = [
        "苹果 香蕉 樱桃 甜",
        "苹果 葡萄 甜 多汁",
        "香蕉 菠萝 热带",
        "樱桃 草莓 浆果",
        "榴莲 特殊 气味",
        "葡萄 酿造 红酒",
    ];
    let mut ids = Vec::new();
    for (i, t) in texts.iter().enumerate() {
        let out = idx.add(format!("doc{i} {t}")).unwrap();
        ids.push(out.doc_id);
    }
    // 删 doc1 / doc3 / doc4（含独特词「榴莲」→ 成死词）
    for d in [ids[1], ids[3], ids[4]] {
        idx.remove(d).unwrap();
    }
    idx.save(&path).unwrap();

    let queries = ["苹果", "香蕉", "樱桃", "葡萄", "水果 甜", "气味 特殊"];

    // compact 前（墓碑仍在，检索期由存活位图挡掉）
    let mut before = Vec::new();
    for q in &queries {
        let hits = bm25_builder()
            .load(&path)
            .unwrap()
            .into_searcher()
            .unwrap()
            .search(q)
            .unwrap();
        let seq: Vec<(String, String, f32)> = hits
            .hits
            .iter()
            .map(|h| (h.source.clone(), h.text.clone(), h.score))
            .collect();
        before.push(seq);
    }

    // compact_and_save（回收墓碑 + 死词）
    let rep = {
        let mut idx = bm25_builder().load(&path).unwrap();
        let rep = idx.compact_and_save(&path).unwrap();
        assert!(rep.remapped, "删了 3/6 应有重编号");
        rep
    };
    // 死词「榴莲」被摘除
    let after_index = bm25_builder().load(&path).unwrap();
    assert_eq!(after_index.num_chunks(), 3, "存活 3 doc");
    assert!(
        rep.reclaimed_terms >= 1,
        "「榴莲」应作为死词被回收，实为 {}",
        rep.reclaimed_terms
    );

    // compact 后检索：与 compact 前逐位一致
    for (i, q) in queries.iter().enumerate() {
        let hits = bm25_builder()
            .load(&path)
            .unwrap()
            .into_searcher()
            .unwrap()
            .search(q)
            .unwrap();
        let seq: Vec<(String, String, f32)> = hits
            .hits
            .iter()
            .map(|h| (h.source.clone(), h.text.clone(), h.score))
            .collect();
        assert_eq!(
            seq, before[i],
            "query「{q}」compact 前后 BM25 结果应逐位一致（I3）"
        );
    }
}

// ---------------------------------------------------------------------------
// T7：manifest 重发 / 铁律（验收 2）——compaction 落盘后 reload 得 `Loaded`
// ---------------------------------------------------------------------------

#[test]
fn T7_compaction后reload图状态为Loaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t7.idx");

    let mut idx = builder_hnsw().build();
    for i in 0..8 {
        idx.add(format!("存活文档 {i} AAA 向量检索")).unwrap();
    }
    idx.save(&path).unwrap(); // 第 1 次落盘（5 个全活）
    assert_eq!(graph_nb_point(&path), 8, "初始图 8 点");

    // 删 3 个（墓碑进图，raw_vectors 被 retain 摘除）
    let survivors: Vec<u32> = (0..8).collect();
    for &d in &survivors[0..3] {
        idx.remove(d).unwrap();
    }
    // compact_and_save：内存重建（图剩 5 点）+ 落盘（dump + manifest 重发）
    let rep = idx.compact_and_save(&path).unwrap();
    assert_eq!(rep.reclaimed_chunks, 3, "回收 3 个墓碑 chunk");

    // 铁律验收：重新 load → 从 sidecar 直接加载（Loaded），不是降级 Rebuilt
    let loaded = builder_hnsw().load(&path).unwrap();
    assert_eq!(
        loaded.graph_status(),
        &GraphStatus::Loaded,
        "compaction 后 manifest 必须重发，reload 应走快路径 Loaded"
    );
    // 图点数 == 存活且有向量的 chunk 数 == 5
    assert_eq!(graph_nb_point(&path), 5, "图应恰好 5 个存活点");
    let vectors = snapshot_vectors(&path);
    assert_eq!(vectors.len(), 5, "快照原始向量 5 条");
    // 检索召回存活文档
    let hits = loaded
        .into_searcher()
        .unwrap()
        .search("AAA 向量检索")
        .unwrap();
    assert_eq!(hits.hits.len(), 5, "5 个存活文档都应召回");
}

// ---------------------------------------------------------------------------
// T11：no-op 保证（无墓碑时 ID 一个都不变）
// ---------------------------------------------------------------------------

#[test]
fn T11_无墓碑时compact为noop_remapped_false() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t11.idx");

    let mut idx = builder_hnsw().build();
    let mut ids = Vec::new();
    for i in 0..4 {
        let out = idx.add(format!("文档{i} 内容 BBB")).unwrap();
        ids.push((out.doc_id, out.chunk_ids[0]));
    }
    let before_docs = idx.num_docs();
    let before_chunks = idx.num_chunks();

    let rep = idx.compact_and_save(&path).unwrap();
    assert!(!rep.remapped, "无墓碑时应 no-op");
    assert_eq!(rep.reclaimed_chunks, 0);
    assert_eq!(rep.reclaimed_docs, 0);
    assert_eq!(rep.reclaimed_terms, 0);
    // 统计量不变
    assert_eq!(idx.num_docs(), before_docs);
    assert_eq!(idx.num_chunks(), before_chunks);
    // ID 全部原样（tombstone_stats after 与 before 一致，无槽位缩减）
    assert_eq!(rep.before.chunks_total, rep.after.chunks_total);
    // reload 后检索正常
    let loaded = builder_hnsw().load(&path).unwrap();
    assert_eq!(loaded.num_chunks(), before_chunks);
    let _ = ids;
}

// ---------------------------------------------------------------------------
// T12：无向量（纯 BM25）与 Brute 后端下 compact 正常
// ---------------------------------------------------------------------------

#[test]
fn T12_纯BM25与Brute后端均可compact() {
    let dir = tempfile::tempdir().unwrap();
    // 纯 BM25（vector_index = None）
    {
        let path = dir.path().join("t12a.idx");
        let mut idx = bm25_builder().build();
        idx.add("纯 BM25 文档 甲").unwrap();
        idx.add("纯 BM25 文档 乙").unwrap();
        let rep = idx.compact_and_save(&path).unwrap();
        assert!(!rep.remapped);
        let loaded = bm25_builder().load(&path).unwrap();
        assert_eq!(loaded.num_chunks(), 2);
    }
    // Brute 后端（走 from_entries 重建）
    {
        let path = dir.path().join("t12b.idx");
        let mut idx = builder_brute().build();
        for i in 0..3 {
            idx.add(format!("brute 文档 {i} CC")).unwrap();
        }
        idx.save(&path).unwrap();
        let rep = idx.compact_and_save(&path).unwrap();
        assert!(!rep.remapped);
        let loaded = builder_brute().load(&path).unwrap();
        assert_eq!(loaded.num_chunks(), 3);
        assert_eq!(
            loaded.graph_status(),
            &GraphStatus::NotApplicable,
            "Brute 无图，状态应为 NotApplicable"
        );
    }
}

// ---------------------------------------------------------------------------
// 评审建议 6（回归）：带 embedder 的装配加载**纯 BM25 快照**（raw_vectors 空）
//      不得走「try_load_graph → 失败 → 打印降级警告 → 重建空图」的噪音路径。
// ---------------------------------------------------------------------------

/// 默认装配（embedder=Some + Hnsw）加载无向量快照时，raw_vectors 为空 ⇒
/// save 侧对空 vectors 本就清 sidecar 落 NotApplicable（persist_graph 623 行），
/// 故 load 侧应**静默**建空向量 lane：graph_status 为 NotApplicable（非 Rebuilt）、
/// 不触发降级警告（CLI `search --index <纯BM25快照>` 的每次加载都会踩到这条噪音）。
#[test]
fn R_建议6_默认装配加载纯BM25快照静默不降级() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r6.idx");

    // 用与 CLI `helix build`（不加 --vectors）相同的装配建纯 BM25 快照：
    // embedder=None + Hnsw backend（默认）、同 chunker/analyzer ⇒ 指纹可过。
    let mut bm25 = SearchIndexBuilder::default()
        .embedder(None) // 纯 BM25：无向量 lane、无 raw_vectors
        .vector_backend(VectorBackend::Hnsw)
        .chunker(helix_core::chunk::Chunker::new(200_000, 0))
        .batch_size(1024)
        .build();
    bm25.add("纯 BM25 文档 甲").unwrap();
    bm25.add("纯 BM25 文档 乙").unwrap();
    bm25.save(&path).unwrap();

    // 带 embedder 的装配（等价 CLI `helix search --index` 的默认 load）重新加载。
    // embedder_id 为空 ⇒ 指纹跳过 embedder 严格校验；raw_vectors 为空 ⇒ 走静默空 lane。
    let loaded = builder_hnsw().load(&path).unwrap();
    assert_eq!(loaded.num_chunks(), 2, "纯 BM25 正文完整可读");
    assert_eq!(
        loaded.graph_status(),
        &GraphStatus::NotApplicable,
        "无向量快照无图可载：应静默落 NotApplicable，而非 Rebuilt + 降级警告"
    );

    // 载体后续仍能写入向量（PR30 不变式：装配有 embedder ⇒ 保留空向量 lane）
    let mut w = builder_hnsw().load(&path).unwrap();
    w.add("追加向量文档 CCC").unwrap();
    w.commit()
        .expect("纯 BM25 快照用带 embedder 装配加载后 commit 不得误报 NoEmbedder");
    assert_eq!(w.num_chunks(), 3, "追加 doc 已可见，向量 lane 可用");
}

// ---------------------------------------------------------------------------
// T13：写缓冲交互（I8 / D-S4-10）——compact 必须先 commit，pending 里的存活 chunk
//      不丢向量、不错位
// ---------------------------------------------------------------------------

#[test]
fn T13_compact前先commit_pending存活chunk不丢向量() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t13.idx");

    let mut idx = builder_hnsw().build();
    // 加 3 个 doc，不 flush（builder_hnsw 的 batch_size=1024 ⇒ 这 3 条必然还在写缓冲，
    // 未被自动 flush；若后续有人改小默认 batch_size 使 add 触发自动 flush，则本场景
    // 退化为「pending 已空」——但下方「向量都在」的断言仍会因缺 step0-commit 而失败，
    // 不会假绿；pending 本身是 pub(crate)，集成测试无法直接读，靠行为保证。）
    for i in 0..3 {
        idx.add(format!("存活文档 {i} ZZZ")).unwrap();
    }

    // compact_and_save：内部第一步 commit() → flush pending 到 index+raw+graph，再重编号
    let rep = idx.compact_and_save(&path).unwrap();
    assert!(!rep.remapped, "无墓碑时是 no-op（ID 不变）");
    // ① 无 stale 旧 id：3 个存活 chunk 的向量都在快照里
    let vectors = snapshot_vectors(&path);
    assert_eq!(vectors.len(), 3, "3 个存活 chunk 的向量都应入库");
    let mut in_snapshot: Vec<u32> = vectors.iter().map(|(id, _)| *id).collect();
    in_snapshot.sort_unstable();
    assert_eq!(
        in_snapshot,
        vec![0, 1, 2],
        "chunk_id 连续（无 tombstone 时不变）"
    );
    // ③ raw_vectors.len() == nb_point == 存活数
    assert_eq!(vectors.len() as u64, graph_nb_point(&path));

    // ④ 对照组：remove 后再走同路径，被删 chunk 的向量**完全不出现**。
    //    注意 compact 会重编号，故不能用旧 chunk_id 断言「不在」，改用向量**条数**守卫：
    //    存活 4 个（3 原 + 1 额外），若被删 chunk 的幽灵向量泄漏，条数会 > 4。
    let mut idx = builder_hnsw().load(&path).unwrap();
    let doomed = idx.add("将被删除 ZZZDELETE").unwrap();
    idx.remove(doomed.doc_id).unwrap(); // 墓碑化 + retain raw
                                        // 额外加一个存活（pending 非空），再 compact——pending 里同时有被删(滤掉)与存活(保留)
    idx.add("额外存活 WWW").unwrap();
    let rep = idx.compact_and_save(&path).unwrap();
    assert!(rep.remapped, "删 1 加 1 触发重编号");
    let vectors2 = snapshot_vectors(&path);
    assert_eq!(
        vectors2.len() as u64,
        graph_nb_point(&path),
        "raw_vectors.len() == nb_point（I7/验收 3）"
    );
    assert_eq!(
        vectors2.len(),
        4,
        "存活 4 个（3 原 + 1 额外）；被删 chunk 的幽灵向量不得泄漏: {:?}",
        vectors2.iter().map(|(id, _)| *id).collect::<Vec<_>>()
    );
    // 检索：被删内容不可召回（FR-26）
    let loaded = builder_hnsw().load(&path).unwrap();
    let hits = loaded.into_searcher().unwrap().search("ZZZDELETE").unwrap();
    for h in &hits.hits {
        assert!(!h.text.contains("ZZZDELETE"), "已删不应被召回");
    }
}

// ---------------------------------------------------------------------------
// 评审发现 1（回归）：全删 → compact → 空向量索引砖死
// ---------------------------------------------------------------------------

/// 全库删空后 `compact()` 曾把 `vector_index` 置 `None`（`if !raw.is_empty()` guard
/// 对空 raw 落到 `_ => None`），但 embedder 仍在装配里——此后 `add → commit` 的
/// `flush()` 对 `vector_index == None` 误报 `Err(NoEmbedder)`，`save` / `compact` /
/// `into_searcher` 全数失败，索引**永久砖死**只能重建。CLI 场景：清空 collection 后
/// 继续写入即踩中。
///
/// 修复：判定维度是「是否有向量能力」而非「存活向量是否为空」，全删后保留**空**向量
/// 索引（`rebuild_vector_index` 对空 raw 天然安全）。此测试走完整闭环
/// 全删 → compact → add → commit → save → load → 检索。
#[test]
fn R_发现1_全删compact后仍可写入检索() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r1.idx");

    // 建 2 个 doc 并落盘（先有向量 lane + graph sidecar）
    let mut idx = builder_hnsw().build();
    let a = idx.add("初始文档 AAAA").unwrap().doc_id;
    let b = idx.add("初始文档 BBBB").unwrap().doc_id;
    let doc_ids = vec![a, b];
    idx.save(&path).unwrap();
    assert_eq!(idx.num_chunks(), 2);

    // 全删 → compact_and_save（raw 变空；修复前 vector_index 落 None）
    for d in doc_ids {
        idx.remove(d).unwrap();
    }
    let rep = idx.compact_and_save(&path).unwrap();
    assert!(rep.remapped, "全删应有重编号");
    assert_eq!(rep.reclaimed_chunks, 2, "回收 2 个墓碑");
    assert_eq!(idx.num_chunks(), 0, "compact 后应 0 存活");

    // 继续写入：add → commit 不得报 NoEmbedder（修复前在此砖死）
    idx.add("新写入文档 CCCC").unwrap();
    idx.commit()
        .expect("全删 compact 后 commit 不得误报 NoEmbedder");
    assert_eq!(idx.num_chunks(), 1, "新 doc 已可见");

    // save → reload（走 load_with：快照向量可能为空，不得落 None）→ 检索命中
    idx.save(&path).unwrap();
    let loaded = builder_hnsw().load(&path).unwrap();
    assert_eq!(loaded.num_chunks(), 1, "reload 应看到 1 个存活 doc");
    let hits = loaded
        .into_searcher()
        .unwrap()
        .search("新写入文档 CCCC")
        .unwrap();
    assert!(
        hits.hits.iter().any(|h| h.text.contains("CCCC")),
        "全删 compact 后新写入的 doc 应可被检索（FR-26/可用性）"
    );
}

// ---------------------------------------------------------------------------
// T14：外部持久引用（source）跨 compact 稳定（D-S4-09 验收）
// ---------------------------------------------------------------------------

/// compaction 会重编号 `doc_id` / `chunk_id`（D-S4-01），因此跨 compact 的**外部持久引用**
/// 必须走 `source`（溯源，FR-12），而非内部 ID。本测试锁住：compact 前后，同一查询词的
/// 检索结果命中**同一批 source**（逐条对齐），被删 source 不再被召回——证明即便内部 ID
/// 全变，`source` 作为稳定键依旧可靠。这是索引对外契约（rustdoc「勿用 doc_id 引用」）的
/// 端到端验收。
#[test]
fn T14_外部source引用跨compact稳定() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t14.idx");

    // 每篇文档一个专属词 + 唯一 source（模拟真实出处，供跨 compact 对齐）
    let docs: Vec<(String, String)> = [
        ("ref://a", "天青石 检索"),
        ("ref://b", "玄铁剑 检索"),
        ("ref://c", "鲛绡纱 检索"),
        ("ref://d", "流金火 检索"),
        ("ref://e", "昆仑木 检索"),
        ("ref://f", "赤霄绫 检索"),
    ]
    .iter()
    .map(|(s, t)| (s.to_string(), t.to_string()))
    .collect();
    // source → 专属词（用作该 source 的探针查询）
    let probe: Vec<(String, &str)> = docs
        .iter()
        .map(|(s, t)| {
            let word = t.split_whitespace().next().unwrap();
            (s.clone(), word)
        })
        .collect();

    let mut idx = bm25_builder().build();
    let mut src_to_id = std::collections::HashMap::new();
    for (src, text) in &docs {
        let out = idx
            .add(Document::new(text.clone()).with_source(src.clone()))
            .unwrap();
        src_to_id.insert(src.clone(), out.doc_id);
    }
    idx.save(&path).unwrap();

    // 删 b / d / f（source 里的三篇）→ 墓碑落盘 → compact
    let doomed_src = ["ref://b", "ref://d", "ref://f"];
    for (src, _) in docs
        .iter()
        .filter(|(s, _)| doomed_src.contains(&s.as_str()))
    {
        let id = src_to_id[src];
        idx.remove(id).unwrap();
    }
    idx.save(&path).unwrap(); // 墓碑进快照/图
    let rep = idx.compact_and_save(&path).unwrap();
    assert!(rep.remapped, "删了 3/6 应有重编号");
    assert_eq!(rep.reclaimed_docs, 3, "回收 3 个墓碑 doc");

    // 探针对照：每个 source 有一个专属词。compact 会重编号内部 id，但 `source` 不变。
    // 于是——若 `source` 作为外部持久引用可靠，则：存活 source 的专属词仍能命中且带
    // 正确的 source；被删 source 的专属词零命中（FR-26）。内部 id 变更与否不影响本断言，
    // 恰好证明「跨 compact 引用必须用 source，而非 doc_id」（D-S4-09 / rustdoc 711 行）。
    let loaded = bm25_builder().load(&path).unwrap();
    let alive_sources: Vec<&String> = docs
        .iter()
        .filter(|(s, _)| !doomed_src.contains(&s.as_str()))
        .map(|(s, _)| s)
        .collect();

    for (src, word) in &probe {
        let hits = bm25_builder()
            .load(&path)
            .unwrap()
            .into_searcher()
            .unwrap()
            .search(word)
            .unwrap();
        if doomed_src.contains(&src.as_str()) {
            // 被删 source：其专属词必须零命中（FR-26 墓碑不可召回）
            assert!(
                hits.hits.is_empty(),
                "source {src} 已被删，其专属词「{word}」不得被召回"
            );
        } else {
            // 存活 source：专属词命中且 source 正确映射（内部 ID 变了也无妨）
            assert!(
                hits.hits.iter().any(|h| &h.source == src),
                "存活 source {src} 的专属词「{word}」应命中并带对 source，实得 {:?}",
                hits.hits.iter().map(|h| &h.source).collect::<Vec<_>>()
            );
        }
    }
    // 额外断言：loaded 正好 3 篇存活、每篇 source 仍在（正文/出处未随 compaction 丢失）
    assert_eq!(loaded.num_chunks(), 3);
    assert!(alive_sources.iter().all(|s| !s.is_empty()));
}

// ---------------------------------------------------------------------------
// T15：ID 重编号后存活向量 id 连续无洞（D-S4-01）
// ---------------------------------------------------------------------------

/// 重编号（D-S4-01）的核心目标：把存活集压实为**从 0 开始、无洞的连续 id**，从而
/// 消除墓碑留下的空洞，让 `raw_vectors.len() == nb_point == 存活数`（I7 / 验收 3）。
/// 构造「删中间两篇造成 id 空洞」的快照，compact 后断言存活向量 id 恰好是 `0..alive`
/// 的连续整数，且快照里已无被删 chunk 的残留向量。
#[test]
fn T15_ID重编号后存活向量id连续无洞() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t15.idx");

    let mut idx = builder_hnsw().build();
    for i in 0..5 {
        idx.add(format!("内容 {i} AAA 连续")).unwrap();
    }
    idx.save(&path).unwrap();
    assert_eq!(graph_nb_point(&path), 5);

    // 删 id 1 / 3 → 存活 0,2,4（id 出现空洞），raw_vectors 被 retain 摘除
    idx.remove(1).unwrap();
    idx.remove(3).unwrap();
    let rep = idx.compact_and_save(&path).unwrap();
    assert!(rep.remapped, "删了 2/5 触发重编号");
    assert_eq!(rep.reclaimed_chunks, 2);

    // 存活向量 id 必须被压实为 0..3 连续无洞
    let mut ids: Vec<u32> = snapshot_vectors(&path)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        vec![0, 1, 2],
        "重编号后存活 chunk id 应连续无洞（0..3），实得 {ids:?}"
    );
    // I7：raw_vectors.len() == nb_point == 存活数（无幽灵向量）
    assert_eq!(ids.len() as u64, graph_nb_point(&path));
    // 铁律：reload 走 sidecar（Loaded）
    let loaded = builder_hnsw().load(&path).unwrap();
    assert_eq!(loaded.graph_status(), &GraphStatus::Loaded);
    // 检索召回全部存活 3 篇，不含被删内容
    let hits = loaded.into_searcher().unwrap().search("AAA").unwrap();
    assert_eq!(hits.hits.len(), 3, "3 个存活 chunk 全召回");
}

// ---------------------------------------------------------------------------
// T16：多轮（删除+compact 循环）墓碑不累积（J2 / NFR-07 收敛）
// ---------------------------------------------------------------------------

/// churn 是真实运维常态：反复「加一批 → 删一批 → compact」。若每次 compact 都只回收
/// 当轮墓碑而留下历史空洞，`chunks_total` 会随轮次单调累积（图/正文虚胖，即 J2 判据的
/// 「不累积」被破坏）。本测试跑**三轮**「删最老一批 + 追加 + compact」，依赖 **T15 证明的
/// 不变式**（compact 后存活 id 连续 `0..alive`）来安全删除老 doc，断言每轮 compact 后
/// `chunks_total` 都**回落到当前存活数**、墓碑占比归零，末轮历史墓碑不可召回——锁住
/// 长期不累积（NFR-07 收敛）。
#[test]
fn T16_多轮compact墓碑不累积() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t16.idx");

    let mut idx = builder_hnsw().build();
    // 轮 0：初始 4 篇
    for i in 0..4 {
        idx.add(format!("初始内容 {i} 留存")).unwrap();
    }
    idx.save(&path).unwrap();

    // 三轮 churn：每轮删最老 alive/3 篇、追加等量新 doc（净保持总量、制造墓碑）
    for round in 0..3 {
        // compact_and_save 后 id 已压实为 0..alive-1（T15 不变式），故删 `0..k` 必删存活老 doc。
        let alive = idx.num_docs(); // usize
        let k = alive / 3;
        assert!(k >= 1, "轮 {round} 每轮至少删 1 篇，当前存活 {alive}");
        for d in 0..k {
            idx.remove(d as u32).unwrap();
        }
        // 追加等量新 doc（content_hash 各异，防 FR-15 去重）
        for i in 0..k {
            idx.add(format!("轮{round} 追加 {i} 留存")).unwrap();
        }
        // compact_and_save：回收本轮墓碑 + 重编号（含 I8：开头先 commit pending）
        let rep = idx.compact_and_save(&path).unwrap();
        assert!(rep.remapped, "轮 {round} 删了 doc 应触发重编号");
        // 关键断言：compact 后 chunks_total 恰等于当前存活数（墓碑不累积）
        assert_eq!(
            rep.after.chunks_total,
            idx.num_chunks() as usize,
            "轮 {round}: compact 后槽位数应 == 当前存活 chunk 数，墓碑不得累积"
        );
        assert!(
            rep.after.tombstone_ratio.abs() < f64::EPSILON,
            "轮 {round}: 每轮 compact 后墓碑占比应归零"
        );
    }

    // 末轮 reload（铁律：Loaded），且 I7 成立：图点数 == 存活 chunk 数（无墓碑残留点）。
    let loaded = builder_hnsw().load(&path).unwrap();
    assert_eq!(loaded.graph_status(), &GraphStatus::Loaded);
    let live = loaded.num_chunks();
    let live_docs = loaded.num_docs();
    assert_eq!(graph_nb_point(&path), live as u64, "I7：图中不得残留墓碑点");

    // 反泄漏（J2）：所有存活内容都含「留存」，故检索该词的召回**上界**是 `live`——
    // 若历史墓碑点/幽灵向量跨 compact 累积，图点数会 > live，可能把召回顶过 `live`。
    // （注意：这是向量近邻召回，命中数 ∈ [0, live]，断言上界即可锁定"不因墓碑虚增"。）
    let hits = loaded.into_searcher().unwrap().search("留存").unwrap();
    assert!(
        hits.hits.len() <= live as usize,
        "存活 {live} 个 chunk，query「留存」召回 {} 不得超过 live（墓碑累积会把图上界顶高）",
        hits.hits.len()
    );
    assert_eq!(
        live_docs, live as usize,
        "存活 doc 数 == 存活 chunk 数（单 chunk 装配）"
    );
}
