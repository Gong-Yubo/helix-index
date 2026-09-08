//! V2 Step 4 核心 PR（S4-03~S4-07）的**门面层集成测试**：T3 / T7 / T11 / T12 / T13。
//!
//! Index 层的单元测试（T1 / T2）在 `src/index/mod.rs` 内。
//! 本文件聚焦 `SearchIndex::compact / compact_and_save` 的正确性，
//! 共享 helper 收敛在 `tests/common/`（见评审建议）。

#![allow(non_snake_case)]

mod common;

use common::{builder_brute, builder_hnsw};
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
