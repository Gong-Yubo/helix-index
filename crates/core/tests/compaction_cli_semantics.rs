//! V2 Step 4 / S4-08 的 **CLI `helix compact` 命令语义**集成测试（门面层等效复刻）。
//!
//! CLI `compact` 本体是薄门面：`SearchIndex::load`（默认装配）→ 读 `tombstone_stats`
//! （`--dry-run`）→ `compact_and_save`（原地或 `--output`）→ 打印报告。其全部决策都落在
//! core 门面层；真实二进制的自动化测试受两个硬约束无法可靠跑：① CLI 无 `remove` 子命令，
//! 造不出带墓碑的库（compact 只能 no-op）；② `--index` 走默认装配会实例化 `LocalEmbedder`
//!（下载 bge 约 49s，不适合无网络单测）。
//!
//! 因此本文件用**确定性 TestEmbedder**（`tests/common`）+ core 门面层 API，按 CLI `compact`
//! 完全相同的调用序列复刻并锁住其**行为契约**：
//!
//! - CLI-1 `--dry-run` 只读：`tombstone_stats()` 后磁盘字节/时间戳**分毫不变**。
//! - CLI-2 无墓碑 no-op：`remapped == false`，回收统计全 0，reload `Loaded` 且可检索。
//! - CLI-3 原地 `compact_and_save`：源三体积收缩（`bytes_before/bytes_after` 均 `Some`），
//!   reload `Loaded` + 检索不含墓碑。
//! - CLI-4 `--output` 另存（A/B）：源 `--index` 文件**不被覆盖**（体积保持墓碑态），
//!   `bytes_before == None`（目标原不存在）而 `bytes_after` 反映回收后体积；回收判据
//!   落在「源三体积 vs 结果三体积」（`bytes_source` 语义）。
//!   🔴 **2026-09-19（PR4 第 2 轮）更正**：原写「回收判据落在**源 vs 结果三体积**」——
//!   `D-S8-01` 之后**源与结果都是 4 点图**（第二次 `save` 已物理化墓碑并重建图），
//!   该比较只剩 `OsRng` 抖动（实测 ≈1/8 假红）⇒ 参照系改为**同一装配的 8 点基线**，
//!   与 CLI-3 同法。注意「源不被覆盖」那条断言**保留**（它比的是源与自身，无噪声）。
//! - CLI-5 纯 BM25 快照在带 embedder 装配下 load 静默（不打印降级警告），见
//!   `step4_compaction.rs::R_建议6`；这里补一条「纯 BM25 no-op compact 后 reload 可检索」。

#![allow(non_snake_case)]
// ⚠️ 本文件沿用旧所有权 API（`into_searcher` / `into_index`）以**锁住其行为不变**
// （`S8-02` 起二者为 `#[deprecated]` 薄封装）。生产调用点已在 `cli` / `examples` 迁移。
#![allow(deprecated)]

mod common;

use common::builder_hnsw;
use helix_core::search::{GraphStatus, SearchIndexBuilder, VectorBackend};

/// 读快照 `.idx` / `.hnsw.graph` / `.hnsw.data` 三文件字节数（CLI `on_disk_sizes` 等价）。
fn on_disk_sizes(path: &std::path::Path) -> (u64, u64, u64) {
    let paths = helix_core::storage::graph_paths(path);
    let snap = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let graph = std::fs::metadata(&paths.graph)
        .map(|m| m.len())
        .unwrap_or(0);
    let data = std::fs::metadata(&paths.data).map(|m| m.len()).unwrap_or(0);
    (snap, graph, data)
}

/// 三文件字节数之和（回收判据用「总体积」）。
fn total_bytes(path: &std::path::Path) -> u64 {
    let (s, g, d) = on_disk_sizes(path);
    s + g + d
}

/// 目录里除 snapshot 三件套外的其他文件数（断言 compact 不残留中间产物）。
fn sidecar_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .count()
}

// 装配与 CLI `helix build --single-chunk`（Hnsw 后端 + 强制单 chunk）一致——区别仅在
// embedder 用确定性 TestEmbedder（规避 CLI 默认装配触发的模型下载）。指纹含
// embedder id + dim，故 build 与 load 必须同装配。
fn cli_style_builder() -> SearchIndexBuilder {
    builder_hnsw()
}

// ---------------------------------------------------------------------------
// CLI-1：`--dry-run` 只读语义——tombstone_stats() 不得写盘
// ---------------------------------------------------------------------------

/// CLI `helix compact --dry-run` 只读预览：load 后读 `tombstone_stats()`，**不调**
/// `compact_and_save`。本测试断言：含墓碑快照经 dry-run 读路径后，磁盘三体积字节数与
/// 目录文件清单（无残留 sidecar / 无新产物）**完全不变**——防止将来有人把 dry-run
/// 实现成「先 compact 再回滚」这类隐式写盘。
#[test]
fn CLI1_dry_run只读不写盘() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli1.idx");

    // 建 6 篇 → 删 3 篇（制造墓碑）→ 落盘
    let mut idx = cli_style_builder().build();
    let mut ids = Vec::new();
    for i in 0..6 {
        let out = idx.add(format!("文档 {i} 检索主题")).unwrap();
        ids.push(out.doc_id);
    }
    idx.save(&path).unwrap();
    for d in &ids[1..4] {
        idx.remove(*d).unwrap();
    }
    idx.save(&path).unwrap();

    let (snap0, graph0, data0) = on_disk_sizes(&path);
    let files0 = sidecar_count(dir.path());

    // —— dry-run：等价 CLI 的 load + tombstone_stats（只读）——
    let index = cli_style_builder().load(&path).unwrap();
    let stats = index.tombstone_stats();
    assert_eq!(stats.chunks_total, 6, "墓碑槽位仍在（未物理回收）");
    assert_eq!(stats.chunks_alive, 3, "3 篇存活");
    assert_eq!(stats.docs_total - stats.docs_alive, 3, "3 个墓碑 doc");
    assert!(
        (stats.tombstone_ratio - 0.5).abs() < f64::EPSILON,
        "墓碑占比 1 - 3/6 = 0.5"
    );
    drop(index); // dry-run 的 index 在此结束生命周期（等价 CLI return，不落盘）

    // dry-run 后磁盘分毫不变
    assert_eq!(
        on_disk_sizes(&path),
        (snap0, graph0, data0),
        "dry-run 不得改字节"
    );
    assert_eq!(
        sidecar_count(dir.path()),
        files0,
        "dry-run 不得新增/删除任何文件"
    );
}

// ---------------------------------------------------------------------------
// CLI-2：无墓碑 → no-op（remapped=false），reload Loaded 且可检索
// ---------------------------------------------------------------------------

/// CLI `helix compact` 对一个无墓碑快照：`remapped=false`、三回收统计全 0、ID 不变，
/// 落盘后 reload 走 sidecar（`Loaded`），检索正常召回全部存活。
#[test]
fn CLI2_无墓碑compact为noop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli2.idx");

    let mut idx = cli_style_builder().build();
    for i in 0..4 {
        idx.add(format!("内容 {i} 主题")).unwrap();
    }
    idx.save(&path).unwrap();
    let before = idx.num_chunks();

    let rep = idx.compact_and_save(&path).unwrap();
    assert!(!rep.remapped, "无墓碑必须 no-op");
    assert_eq!(rep.reclaimed_chunks, 0);
    assert_eq!(rep.reclaimed_docs, 0);
    assert_eq!(rep.reclaimed_terms, 0);
    // bytes_before/after 均 Some（原地目标本就存在）
    assert!(
        rep.bytes_before.is_some(),
        "原地模式 bytes_before 应可 stat"
    );
    assert!(
        rep.bytes_after.is_some(),
        "compact_and_save 落盘后 bytes_after 应为 Some"
    );

    // reload Loaded + 检索
    let loaded = cli_style_builder().load(&path).unwrap();
    assert_eq!(loaded.num_chunks(), before);
    assert_eq!(loaded.graph_status(), &GraphStatus::Loaded);
    let hits = loaded.into_searcher().unwrap().search("主题").unwrap();
    assert_eq!(hits.hits.len(), before as usize, "全部存活可召回");
}

// ---------------------------------------------------------------------------
// CLI-3：原地 compact 回收——源三体积收缩 + reload 检索不含墓碑
// ---------------------------------------------------------------------------

/// CLI `helix compact --index X`（原地）：墓碑多时 `graph+data` 三体积显著回落，
/// 回收统计准确；reload `Loaded`，检索命中的文本不含任何被删内容。
#[test]
fn CLI3_原地compact回收墓碑并收缩体积() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli3.idx");

    let mut idx = cli_style_builder().build();
    let mut ids = Vec::new();
    for i in 0..8 {
        // 一半文本带「墓碑专用词」便于断言不可召回
        let tag = if i % 2 == 0 { "存活" } else { "删除目标" };
        let out = idx.add(format!("文档 {i} 主题 {tag}")).unwrap();
        ids.push(out.doc_id);
    }
    idx.save(&path).unwrap();
    // 🔑 **参照基线（8 点、无墓碑）**：`D-S8-01`（`S8-03`/PR4 起 `save` 第一步就 `fold_deltas`）
    //    之前，它与下面的 `graph_tomb` 是同一个量；**之后不再是** —— 见下。
    let graph_baseline = on_disk_sizes(&path).1;
    let total_baseline = total_bytes(&path);

    // 删 4 篇（全「删除目标」）→ 墓碑落盘。hnsw 图无 remove API ⇒ 墓碑点留在图 sidecar，
    // 故此刻 graph 体积 = 8 点（含 4 墓碑）；raw_vectors/data 已被 remove 的 retain 摘除
    // 4 条，故只看 total 不一定变大。以 **graph sidecar 体积**作为 compaction 回收的
    // 判别信号：compact 把图重建为 4 存活点 ⇒ graph 必须显著回落。
    // ⚠️ **上面这句（`PR4` 之前在 `main` 上成立的版本）在本 PR 之后已作废** ——
    //    `D-S8-01` 让第二次 `save` 就把墓碑物理化并重建图 ⇒ 「墓碑态」已是 4 点图。
    //    保留原文是为了留痕（判据为什么必须换参照系），**不要按它理解当前状态**，见下。
    //
    // 🔴 **2026-09-19（`S8-03` / PR4）前提更正** —— 本节原本断言
    // `graph_after < graph_tomb`，其中 `graph_tomb` 被当作「8 点含墓碑」的图。PR4 起不再是：
    // 第二次 `save()`（`D-S8-01`：第一步 `commit()`、第二步 `fold_deltas()`）在**落盘前**就
    // 把 4 条跨段墓碑**物理化**、并用 `retain` 后的 4 条原始向量**重建了图**
    // ⇒ 磁盘上的「墓碑态」已经是一张 **4 点图** ⇒ 两侧同量级。
    // 实测：`graph_tomb = 1097`、`graph_after = 1097`（**相等**）—— 这不是「图没收缩」，
    // 而是**已经收缩过了**；旧断言据此必红（且 `hnsw_rs` 的层级分配用无种子 `OsRng`
    // ⇒ 两棵同点数的图的字节数会上下浮动 ⇒ 旧断言**偶发通过** = flaky）。
    // ⇒ 参照系改为**同一装配的 8 点基线**（`graph_baseline`）：这个判据与「回收发生在
    // `fold` 还是 `compact`」**无关**（`S8-06` 若改成增量合并，结论不变），仍然钉住
    // 「墓碑态 + 重编号 + 重建之后，图与总体积都必须回落到存活规模」。
    for d in ids[1..8].iter().step_by(2) {
        idx.remove(*d).unwrap();
    }
    idx.save(&path).unwrap();
    let graph_tomb = on_disk_sizes(&path).1;
    let total_tomb = total_bytes(&path);

    // 原地 compact_and_save
    let rep = idx.compact_and_save(&path).unwrap();
    assert!(rep.remapped, "删了 4/8 应重编号");
    assert_eq!(rep.reclaimed_chunks, 4);
    let total_after = total_bytes(&path);
    let graph_after = on_disk_sizes(&path).1;
    assert!(
        graph_after < graph_baseline,
        "图 sidecar 必须相对 **8 点基线** {graph_baseline} 字节收缩（compact 后 {graph_after} 字节 / \
         4 存活点；墓碑态 {graph_tomb} 已经是 4 点图，见上方前提更正）"
    );
    assert!(
        total_after < total_baseline,
        "compact 后总字节 {total_after} 应小于 **8 点基线** {total_baseline}（墓碑态 {total_tomb}）"
    );
    // ⚠️ 原第三条断言 `total_after < total_tomb`（「compact 后应小于墓碑态」）**已删除**
    //    （2026-09-19 补）：它与上面两条同源 —— 墓碑态**已经是 4 点状态**，两者的差距只剩
    //    `hnsw_rs` 用无种子 `OsRng` 带来的**几字节抖动**。实测两侧读数：墓碑态 **2619**、
    //    compact 后 **2624**（`--features local-rerank` 下）⇒ 该断言是**纯噪声判据**
    //    （默认 feature 下侥幸通过、换一个 feature 组合就红，与实现无关）。
    //    「体积必须回落」的语义**已被 `graph_baseline` / `total_baseline` 两条完整覆盖**，
    //    且它们以「8 点无墓碑状态」为参照 ⇒ 与「回收发生在 `fold` 还是 `compact`」无关。

    // reload Loaded + 检索命中的正文不含任何「删除目标」
    let loaded = cli_style_builder().load(&path).unwrap();
    assert_eq!(loaded.graph_status(), &GraphStatus::Loaded);
    let hits = loaded.into_searcher().unwrap().search("主题").unwrap();
    assert_eq!(hits.hits.len(), 4, "4 篇存活全召回");
    for h in &hits.hits {
        assert!(
            !h.text.contains("删除目标"),
            "被删内容不得被召回: {}",
            h.text
        );
    }
}

// ---------------------------------------------------------------------------
// CLI-4：`--output` 另存（A/B）——源不被覆盖、bytes_before=None、回收看源 vs 结果
// ---------------------------------------------------------------------------

/// CLI `helix compact --index SRC --output DST`（A/B）：SRC **不被覆盖**（保持墓碑态
/// 体积），DST 是新建的回收后快照。CLI 的 `bytes_before` 是对目标路径 stat——DST 原本
/// 不存在故为 `None`（诚实，不是 0），回收判据落在「SRC 源三体积 vs DST 结果三体积」。
/// 本测试用 core `compact_and_save(DST)` 复刻该路径。
///
/// 🔴 **2026-09-19 第 2 轮评审期间实测：本用例原是 flaky（≈1/8 假红）** ——
/// 原断言 `dst_total < src_tomb`（「回收后应小于**墓碑态**」）把 `src_tomb` 当作「8 点含墓碑的图」，
/// 而 `D-S8-01`（`save` 第一步 `fold_deltas`）之后**第二次 `save` 就已经把 4 条墓碑物理化、
/// 并用 4 条存活原始向量重建了图** ⇒ `src_tomb` 与 `dst_total` **都是 4 点图**，本就没有「可回收」的
/// 差量可比。实测 8 次：`src_tomb` 恒 **2556**；`dst_total` ∈ {**2528** ×7, **2579** ×1}
/// —— 差别只来自 `hnsw_rs` 层级分配的**无种子 `OsRng`** ⇒ 断言符号随机翻转。
/// ⇒ 参照系改为**同一装配的 8 点基线**（`total_baseline` / `graph_baseline`，在删除**之前**量取）：
/// 该判据与「回收发生在 `fold` 还是 `compact`」**无关**（`S8-06` 若改增量合并，结论不变），
/// 且 8 点 vs 4 点有 **≈2.2×** 裕度。**其余断言一条未放宽**（`remapped` / `reclaimed_docs` /
/// 源不被覆盖 / reload 后 4 存活且图 `Loaded` / 检索 4 条）。
#[test]
fn CLI4_output另存源不变回收看源vs结果() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("cli4-src.idx");
    let dst = dir.path().join("cli4-dst.idx");

    let mut idx = cli_style_builder().build();
    let mut ids = Vec::new();
    for i in 0..8 {
        let out = idx.add(format!("文档 {i} 主题")).unwrap();
        ids.push(out.doc_id);
    }
    idx.save(&src).unwrap();
    // 🔑 回收判据的参照系：**8 点、无墓碑**的同一装配（删除发生前量取）
    let total_baseline = total_bytes(&src);
    let graph_baseline = on_disk_sizes(&src).1;
    for d in &ids[0..4] {
        idx.remove(*d).unwrap();
    }
    idx.save(&src).unwrap(); // SRC = 含 4 墓碑（⚠️ 图在这一步已被重建为 4 点）
    let src_tomb = total_bytes(&src);

    // A/B：compact 到 dst（src 保持只读源）
    let rep = idx.compact_and_save(&dst).unwrap();
    assert!(rep.remapped, "删了 4/8 应重编号");
    assert_eq!(rep.reclaimed_docs, 4);
    // 目标原本不存在 → bytes_before None（诚实语义）；bytes_after Some
    assert!(
        rep.bytes_before.is_none(),
        "--output 目标原本不存在，bytes_before 应为 None"
    );
    assert!(rep.bytes_after.is_some(), "落盘后 bytes_after 应为 Some");

    // 源不被覆盖：体积仍等于墓碑态；目标是新建回收快照
    assert_eq!(
        total_bytes(&src),
        src_tomb,
        "--output 不得改动源 --index 文件"
    );
    let dst_total = total_bytes(&dst);
    let graph_dst = on_disk_sizes(&dst).1;
    assert!(dst_total > 0, "目标快照已生成");
    // 回收的信号用 **graph sidecar**（与 `CLI3` 同法）：它是「图里有几个点」的直接反映，
    // 不会被 `raw_vectors` / postings 的字节量淹没。
    assert!(
        graph_dst < graph_baseline,
        "图 sidecar 应从 8 点基线 {graph_baseline} 字节收缩到 {graph_dst}（4 存活点）"
    );
    assert!(
        dst_total < total_baseline,
        "compact 后的 dst 应为 4 点形态：总字节 {dst_total} 应小于 8 点基线 {total_baseline}"
    );

    // 源仍是可 load 的墓碑快照（含 4 存活）；dst reload 后只有 4 存活且 Loaded
    let src_loaded = cli_style_builder().load(&src).unwrap();
    assert_eq!(src_loaded.num_chunks(), 4, "源未被覆盖，仍 4 存活");
    let dst_loaded = cli_style_builder().load(&dst).unwrap();
    assert_eq!(dst_loaded.graph_status(), &GraphStatus::Loaded);
    assert_eq!(dst_loaded.num_chunks(), 4);
    let hits = dst_loaded.into_searcher().unwrap().search("主题").unwrap();
    assert_eq!(hits.hits.len(), 4);
}

// ---------------------------------------------------------------------------
// CLI-5：纯 BM25 no-op compact（带 embedder 装配 load）后 reload 可检索
// ---------------------------------------------------------------------------

/// CLI 面向纯 BM25 库的 compact：默认（带 embedder）装配加载纯 BM25 快照应**静默**
/// （R_建议6 已锁不打印降级警告），此处补完整 no-op compact → reload `Loaded` 之外，
/// 纯 BM25 快照 reload 后 `graph_status == NotApplicable`（无向量，本就不该有图）。
#[test]
fn CLI5_纯BM25快照compact后reload可检索() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli5.idx");

    // 纯 BM25 快照（embedder=None + Hnsw 后端，等价 CLI `helix build` 不加 --vectors）
    let mut bm25 = SearchIndexBuilder::default()
        .embedder(None)
        .vector_backend(VectorBackend::Hnsw)
        .chunker(helix_core::chunk::Chunker::new(200_000, 0))
        .batch_size(1024)
        .build();
    bm25.add("纯 BM25 文档 甲 主题").unwrap();
    bm25.add("纯 BM25 文档 乙 主题").unwrap();
    bm25.save(&path).unwrap();

    // 带 embedder 装配 load（等价 CLI `helix compact` 的默认装配）→ no-op compact
    let mut idx = builder_hnsw().load(&path).unwrap();
    assert_eq!(
        idx.graph_status(),
        &GraphStatus::NotApplicable,
        "纯 BM25 无图可载"
    );
    let rep = idx.compact_and_save(&path).unwrap();
    assert!(!rep.remapped, "无墓碑 no-op");

    // reload（带 embedder 装配）仍静默 NotApplicable，正文可读、检索命中
    let loaded = builder_hnsw().load(&path).unwrap();
    assert_eq!(loaded.graph_status(), &GraphStatus::NotApplicable);
    assert_eq!(loaded.num_chunks(), 2);
    let hits = loaded.into_searcher().unwrap().search("主题").unwrap();
    assert_eq!(hits.hits.len(), 2);
}
