//! V2 Step 4 的资源回收测试（S4 系列）。
//!
//! 当前只含 **S4-01 / T5**（flush 侧幽灵向量防线，D-S4-05 / §2.3）。
//! 后续核心 PR（S4-03~S4-07）的 T1~T4 / T7 / T11 / T12 / T13 会追加到此文件，
//! 因此这里建立与 `graph_persist.rs` 平行、但独立于 Step 2 主题的 helper 集。

#![allow(non_snake_case)]

use std::sync::Arc;

use helix_core::chunk::Chunker;
use helix_core::embed::Embedder;
use helix_core::error::Result;
use helix_core::search::{SearchIndexBuilder, VectorBackend};

// ---------------------------------------------------------------------------
// 测试用确定性 Embedder（不依赖真实模型，跨机器可复现；复制自 graph_persist.rs，
// 集成测试间无法共享私有 item）
// ---------------------------------------------------------------------------

struct TestEmbedder {
    dim: usize,
}

impl Embedder for TestEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| hash_vec(t, self.dim)).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(hash_vec(text, self.dim))
    }

    fn id(&self) -> &'static str {
        "test-embedder-step4-v1"
    }
}

/// 文本 → 确定性向量（LCG，跨进程可复现）。
fn hash_vec(text: &str, dim: usize) -> Vec<f32> {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    let mut next = || {
        h ^= h << 13;
        h ^= h >> 7;
        h ^= h << 17;
        ((h >> 11) as u32 as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    (0..dim).map(|_| next()).collect()
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

const DIM: usize = 64;

/// 装配：测试 embedder + Hnsw 后端 + **强制单 chunk**（`Chunker::new(200_000, 0)`）
/// ——让「一个 doc == 一个 chunk_id」，便于断言具体 chunk 是否残留。
fn builder() -> SearchIndexBuilder {
    SearchIndexBuilder::default()
        .embedder(Some(Arc::new(TestEmbedder { dim: DIM })))
        .vector_backend(VectorBackend::Hnsw)
        .chunker(Chunker::new(200_000, 0))
        // 显式调大写缓冲阈值：T5 需要在「pending 未满」时保持未 flush 状态，
        // 默认 batch_size=64 时只要 doc 数 < 64 即可，此处放大以免疫未来改默认值。
        .batch_size(1024)
}

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
        let mut idx = builder().build();
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
        let mut idx = builder().build();
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
    let loaded = builder().load(&path).unwrap();
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
        let mut idx = builder().build();
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
        let mut idx = builder().load(&path).unwrap();
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
    let loaded = builder().load(&path).unwrap();
    let hits = loaded
        .into_searcher()
        .unwrap()
        .search("将被删除的独特文档 ZZZDELETE2")
        .unwrap();
    for h in &hits.hits {
        assert!(!h.text.contains("ZZZDELETE2"), "已删不应被召回");
    }
}
