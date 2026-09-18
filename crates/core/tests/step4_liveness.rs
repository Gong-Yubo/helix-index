//! V2 Step 4 的资源回收测试（S4 系列）。
//!
//! 当前只含 **S4-01 / T5**（flush 侧幽灵向量防线，D-S4-05 / §2.3）。
//! 后续核心 PR（S4-03~S4-07）的 T1~T4 / T7 / T11 / T12 / T13 追加在
//! `step4_compaction.rs`（门面层）与 `src/index/mod.rs`（Index 层单元）。
//! 共享 helper（确定性 Embedder）收敛在 `tests/common/`（评审建议）。

#![allow(non_snake_case)]
// ⚠️ 本文件沿用旧所有权 API（`into_searcher` / `into_index`）以**锁住其行为不变**
// （`S8-02` 起二者为 `#[deprecated]` 薄封装）。生产调用点已在 `cli` / `examples` 迁移。
#![allow(deprecated)]

mod common;

use common::builder_hnsw;

/// 读回快照正文里的原始向量列表（不触发图加载，纯粹读快照正文）。
fn snapshot_vectors(path: &std::path::Path) -> Vec<(u32, Vec<f32>)> {
    helix_core::storage::load_with_crc(path).unwrap().1
}

// ---------------------------------------------------------------------------
// S4-01 / T5：幽灵向量防线（D-S4-05 / 设计 §2.3 / §7 T5）
// ---------------------------------------------------------------------------

/// `add(d)` → `remove(d)`（**未 flush**，pending 非空）→ `save`（隐含 commit → flush）
/// → `load`：已删 chunk 的向量**不**进入快照 `vectors`（S4-01 修复前会残留并跨快照永续）。
///
/// 这是设计 §2.3 的必现时序：`add` 在入 `pending` 前已分配真实 chunk_id，
/// `remove` 只墓碑化正排/倒排、不摘 pending；若 `flush` 不按 liveness 过滤，
/// 紧随 `save()` 的隐式 `commit()` → `flush()` 会把已删 chunk 灌进 `raw_vectors` 与图。
#[test]
fn T5_remove早于flush的向量不残留() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ghost.idx");

    // 构造：2 个存活 doc + 1 个将被删除的 doc。强制单 chunk ⇒ 每 doc 恰 1 个 chunk_id。
    let (target_chunk, target_doc) = {
        let mut idx = builder_hnsw().build();
        idx.add("存活文档 AAAAA").unwrap();
        idx.add("存活文档 BBBBB").unwrap();
        let out = idx.add("将被删除的独特文档 ZZZDELETE").unwrap();
        // pending 现有 3 条、远未到 batch_size=1024 ⇒ 尚未自动 flush；
        // 若断言失败说明构造失效，需重看 chunker/batch_size 装配。
        assert_eq!(
            out.chunk_ids.len(),
            1,
            "强制单 chunk 应给每 doc 1 个 chunk_id"
        );
        (out.chunk_ids[0], out.doc_id)
    };

    {
        let mut idx = builder_hnsw().build();
        // 重新灌入同样的 3 个 doc（本测试不跨空索引持久化状态，直接内存构造）
        idx.add("存活文档 AAAAA").unwrap();
        idx.add("存活文档 BBBBB").unwrap();
        let out = idx.add("将被删除的独特文档 ZZZDELETE").unwrap();
        assert_eq!(out.chunk_ids.len(), 1);
        assert_eq!(
            out.chunk_ids[0], target_chunk,
            "chunk_id 分配应确定（同序 add）"
        );
        assert_eq!(out.doc_id, target_doc, "doc_id 分配应确定（同序 add）");

        // remove：墓碑化该 doc 的 chunk；pending 里仍留着它的向量待 flush。
        idx.remove(out.doc_id).unwrap();

        // save 隐含 commit() → flush()：修复前已删 chunk 在此被灌进 raw_vectors/图；
        // 修复后被 liveness 过滤掉。
        idx.save(&path).unwrap();
    }

    // 断言快照正文的原始向量**不含**待删 doc 的 chunk。
    let vectors = snapshot_vectors(&path);
    let chunks_in_snapshot: Vec<u32> = vectors.iter().map(|(id, _)| *id).collect();
    assert!(
        !chunks_in_snapshot.contains(&target_chunk),
        "已删 chunk {target_chunk} 的向量不应残留在快照中（S4-01 未生效？）: {chunks_in_snapshot:?}"
    );
    assert_eq!(
        vectors.len(),
        2,
        "快照应只含 2 个存活 chunk 的向量，实为 {}",
        vectors.len()
    );

    // 检索确认删除语义未被破坏（FR-26：删除的永远不回来）。
    let loaded = builder_hnsw().load(&path).unwrap();
    let hits = loaded
        .into_searcher()
        .unwrap()
        .search("将被删除的独特文档 ZZZDELETE")
        .unwrap();
    for h in &hits.hits {
        assert!(
            !h.text.contains("ZZZDELETE"),
            "已删文档不应被召回: {}",
            h.text
        );
    }

    // 图侧断言：本场景幽灵向量**从未进图**（remove 先于 flush，被 S4-01 过滤），
    // 故 `nb_point == 快照向量条数 == 2` 恒成立——与设计 T5 的「快照 vectors **与图中**
    // 不含该 chunk」口径完全对齐。（对比 T5b：那里 remove 后于 flush，墓碑已进图，
    // compaction 前 `nb_point` 不归位，故只断 `>=`；两条路径互补。）
    let paths = helix_core::storage::graph_paths(&path);
    let m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();
    assert_eq!(
        m.nb_point, 2,
        "被删 chunk 的向量未进图，图中应恰好 2 个存活点"
    );
}

/// T5 的对照：**flush 之后再 remove**（架构 §7.5.2 防线①的路径——`remove` 的
/// `retain` 主动摘 raw_vectors）。即使没有 S4-01 的 flush 过滤，这条也不该残留——
/// 证明「先 flush 后 remove」路径本就干净，S4-01 修的是「remove 先于 flush」的缺口。
///
/// ⚠️ 图点数的归位**不在本测试断言**：`hnsw_rs` 无 remove API，图里被删的点会一直
/// 留到 compaction（S4-03~S4-07 / 验收 3）才清，故 `nb_point ≥ 快照向量条数` 是
/// compaction 前 HNSW 的**正常**状态（与 T9 同口径），此处只验证 raw_vectors（快照）已摘净。
#[test]
fn T5b_remove在flush后raw_vectors被retain摘净() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clean.idx");

    let (target_chunk, target_doc) = {
        let mut idx = builder_hnsw().build();
        for i in 0..20 {
            idx.add(format!("存活文档 {i} AAA")).unwrap();
        }
        // 加第 21 个并立即 save：把它 flush 进 raw_vectors（pending 清空）。
        let out = idx.add("将被删除的独特文档 ZZZDELETE2").unwrap();
        assert_eq!(out.chunk_ids.len(), 1);
        idx.save(&path).unwrap();
        (out.chunk_ids[0], out.doc_id)
    };

    // remove → save：remove 的 retain 把 raw_vectors 里的 target 摘掉。
    {
        let mut idx = builder_hnsw().load(&path).unwrap();
        idx.remove(target_doc).unwrap();
        idx.save(&path).unwrap();
    }

    let vectors = snapshot_vectors(&path);
    assert_eq!(
        vectors.len(),
        20,
        "remove 后快照应只含 20 个存活 doc 的向量"
    );
    assert!(
        !vectors.iter().any(|(id, _)| *id == target_chunk),
        "已删 chunk {target_chunk} 的向量应从快照摘净（remove 的 retain 路径）"
    );

    // 图点数在 compaction 前不归位（hnsw 墓碑留图）——校验这是设计内状态，不是坏档。
    let paths = helix_core::storage::graph_paths(&path);
    let m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();
    assert!(
        m.nb_point >= vectors.len() as u64,
        "compaction 前图点数可 ≥ 快照向量条数（墓碑留图，由存活位图挡掉）"
    );

    // 检索：已删不可召回。
    let loaded = builder_hnsw().load(&path).unwrap();
    let hits = loaded
        .into_searcher()
        .unwrap()
        .search("将被删除的独特文档 ZZZDELETE2")
        .unwrap();
    for h in &hits.hits {
        assert!(!h.text.contains("ZZZDELETE2"), "已删不应被召回");
    }
}
