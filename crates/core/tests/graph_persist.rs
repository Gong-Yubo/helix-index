//! V2 Step 2 图持久化的端到端测试（S2-T1 ~ S2-T17，设计文档 §8）。
//!
//! # 为什么必须断言 `GraphStatus::Loaded`（P0-1）
//!
//! basename 拼错（写成 `foo.idx.hnsw`）会让 `hnsw_rs` 去找
//! `foo.idx.hnsw.hnsw.graph`——**降级路径一切正常，所有「能加载」断言照样绿**，
//! 只有 NFR-04 静默失效。因此每个「应当走快路径」的测试都必须先断言
//! `GraphStatus::Loaded`，否则测试等于没写。

#![allow(non_snake_case)]

use std::sync::Arc;

use helix_core::document::Document;
use helix_core::embed::Embedder;
use helix_core::error::Result;
use helix_core::query::{SearchMode, VectorRoute};
use helix_core::schema::Filter;
use helix_core::search::{
    GraphPersistMode, GraphStatus, SearchIndex, SearchIndexBuilder, Searcher, VectorBackend,
};
use helix_core::vector::{BruteForceIndex, HnswRsIndex, NormalizedVector, VectorIndex};

// ---------------------------------------------------------------------------
// 测试用确定性 Embedder（不依赖真实模型，保证跨机器可复现）
// ---------------------------------------------------------------------------

/// 确定性伪随机 embedder：把文本 hash 成 seed，生成定长向量（**未归一化**，
/// 与 LocalEmbedder 的 `is_normalized() = true` 不同——`NormalizedVector::new`
/// 会在入库前归一化，这里故意走未归一化路径验证归一化发生在门面层）。
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

    fn is_normalized(&self) -> bool {
        false
    }

    fn id(&self) -> &'static str {
        "test-embedder-v1"
    }
}

/// 文本 → 确定性向量（LCG，跨进程可复现）。
fn hash_vec(text: &str, dim: usize) -> Vec<f32> {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV offset basis
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

/// T13 的 **ANN 名次门**：ANN 返回的每一条都必须落在**本图精确序**的 top-N 内。
///
/// 为什么是 20：实测带宽 `worst exact rank ∈ [10, 11]`（80 次独立建图；min 10 /
/// p50 10 / p95 11 / **max 11**；N=1200、rayon 10、debug profile），余量 ≈ 1.8×。
/// 该量是**同一张图**上的比较（非跨图）⇒ 对给定图逐位可复现，不受建图 RNG 影响。
const T13_EXACT_RANK_GATE: usize = 20;

/// 装配：测试 embedder + Hnsw 后端（默认图持久化开）。
fn builder() -> SearchIndexBuilder {
    SearchIndexBuilder::default()
        .embedder(Some(Arc::new(TestEmbedder { dim: DIM })))
        .vector_backend(VectorBackend::Hnsw)
}

/// 建一个 `n` 篇文档的库并落盘，返回快照路径（临时目录由调用方持有）。
fn build_and_save(dir: &std::path::Path, n: usize) -> std::path::PathBuf {
    let path = dir.join("t.idx");
    let mut idx = builder().build();
    for i in 0..n {
        idx.add(format!("文档 {i} 的内容：检索与向量")).unwrap();
    }
    idx.save(&path).unwrap();
    path
}

/// 取 Top-K（(chunk_id, score) 序列，用于逐位比对）。
///
/// `SearchIndex` 不可 Clone，故**消费**索引转 `Searcher`（与真实用法一致）。
fn topk(idx: SearchIndex, q: &str, k: usize) -> Vec<(u32, f32)> {
    topk_searcher(&idx.into_searcher().unwrap(), q, k)
}

/// 从既有 `Searcher` 取 Top-K（`Searcher` 是 `Arc` 共享，可反复检索，
/// 避免每条 query 都重新加载整张图——T2 有 200 条 query）。
fn topk_searcher(s: &helix_core::search::Searcher, q: &str, k: usize) -> Vec<(u32, f32)> {
    s.search_with(q)
        .mode(helix_core::query::SearchMode::Vector)
        .top_n(k)
        .exec()
        .unwrap()
        .hits
        .iter()
        .map(|h| (h.chunk_id, h.score))
        .collect()
}

/// 用**精确线性扫描**（`BruteForceIndex`）建 oracle 索引（一次建好，可反复查询）。
///
/// 测试 embedder 是确定性的，因此可以脱离索引重建同一批向量；HNSW 是近似索引，
/// 与它比对才有意义（设计文档 §8 对 T13 要求的「重合率」口径）。
///
/// 单独暴露「建索引」这一步是为了**复用**：早期实现每条 query 都重建一遍整库
/// （T6 是 20 次 × 200 篇、T13 是 20 次 × 1200 篇），白算。
fn oracle_index(texts: &[String]) -> BruteForceIndex {
    let mut b = BruteForceIndex::new();
    for (i, t) in texts.iter().enumerate() {
        b.add(i as u32, NormalizedVector::new(hash_vec(t, DIM)))
            .unwrap();
    }
    b
}

/// 在既有的 oracle 索引上取 Top-K 的 chunk_id，**按引擎的输出约定排序**：
/// `score = 1 − d²/2` 降序、同分按 `chunk_id` 升序 —— 与 `VectorRetriever::to_scored`
/// 同一约定（评审 #39 意见 1）。
///
/// 为什么不直接用 `BruteForceIndex` 的原始距离序 `(d² 升序, id 升序)`：它与 score 序
/// `(score 降序, id 升序)` 只在 `1 − d²/2` 于 f32 下**单射**时才逐位等价。一旦两个不同
/// 的 d² 舍入到同一 score，tie-break 会让两侧的**序列**翻脸 —— 那不是缺陷，是一次良性
/// 并列，却会挂在 T13 G2 的逐位比对上、且挂因无法归因（两侧都没错）。套同一变换 ⇒
/// 两侧语义（含并列）结构性相同。
///
/// ⚠️ 这是与 `VectorRetriever::to_scored` **同一口径的第二处实现**：那边改公式（例如换
/// 成 f64 或 `mul_add`），这里要同步。**故意不隐藏这处耦合**：同步失败会以 G2 变红的
/// 形式暴露（本函数也顺带把 score 公式钉住了），而不是静默放宽。
///
/// 注：T6 也调用本函数，但那里只经 `overlap()` 用**集合**语义，排序不影响结果。
fn oracle_topk(b: &BruteForceIndex, q: &str, k: usize) -> Vec<u32> {
    let qv = NormalizedVector::new(hash_vec(q, DIM));
    let mut scored: Vec<(u32, f32)> = b
        .search(&qv, k)
        .unwrap()
        .into_iter()
        .map(|(id, d)| (id, 1.0 - d / 2.0))
        .collect();
    // score 降序；同分按 chunk_id 升序（与 `to_scored` 逐字同约定，含 `unwrap_or` 口径）
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    scored.into_iter().map(|(id, _)| id).collect()
}

/// Top-K 集合重合率（|A∩B| / k），用于与 oracle 比对。
fn overlap(a: &[u32], b: &[u32]) -> f64 {
    let sb: std::collections::HashSet<u32> = b.iter().copied().collect();
    a.iter().filter(|x| sb.contains(x)).count() as f64 / a.len().max(1) as f64
}

// ---------------------------------------------------------------------------
// S2-T1 ~ S2-T17
// ---------------------------------------------------------------------------

/// **S2-T1** ⚠️ 图 roundtrip：save 后三件套齐全，manifest 字段与快照一致。
#[test]
fn T1_图roundtrip与manifest字段一致() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 300);

    let paths = helix_core::storage::graph_paths(&path);
    assert!(paths.graph.exists(), "图拓扑文件应存在");
    assert!(paths.data.exists(), "图数据文件应存在");
    assert!(paths.manifest.exists(), "manifest 应存在（唯一发布点）");

    let m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .expect("manifest 应能读回");

    // 与快照实际 CRC / 长度一致
    let (_, _, _, body_crc) = helix_core::storage::load_with_crc(&path).unwrap();
    assert_eq!(
        m.snapshot_crc, body_crc,
        "snapshot_crc 必须等于快照正文 CRC"
    );
    assert_eq!(
        m.snapshot_len,
        std::fs::metadata(&path).unwrap().len(),
        "snapshot_len 必须等于快照文件长度"
    );
    assert_eq!(m.dim as usize, DIM, "dim 必须等于指纹维度");
    assert!(m.nb_point >= 300, "图中点数应 >= 文档（分片）数");

    // 两个图文件的 CRC/长度与实际一致
    let (gc, gl) = helix_core::storage::file_crc32_len(&paths.graph).unwrap();
    let (dc, dl) = helix_core::storage::file_crc32_len(&paths.data).unwrap();
    assert_eq!((m.graph_crc, m.graph_len), (gc, gl));
    assert_eq!((m.data_crc, m.data_len), (dc, dl));
}

/// **S2-T2** ⚠️ 消解 R-P5-13：**同一快照连续加载两次** Top-10 逐位一致。
///
/// 前置断言 `GraphStatus::Loaded`（P0-1：拼错 basename 时本测试会**假绿**）。
#[test]
fn T2_同快照两次加载逐位一致() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 400);

    let a = builder().load(&path).unwrap();
    let b = builder().load(&path).unwrap();

    // 前置：两次都必须真的走了快路径（图加载成功）
    assert_eq!(a.graph_status(), &GraphStatus::Loaded, "第一次应加载图");
    assert_eq!(b.graph_status(), &GraphStatus::Loaded, "第二次应加载图");

    // 200 条 query 的 Top-10 逐位相等（复用同一次加载的 Searcher，不重复加载图）
    let sa = a.into_searcher().unwrap();
    let sb = b.into_searcher().unwrap();
    for i in 0..200 {
        let q = format!("查询 {i}");
        let ra = topk_searcher(&sa, &q, 10);
        let rb = topk_searcher(&sb, &q, 10);
        assert_eq!(ra, rb, "query {i} 结果应逐位一致");
        assert_eq!(ra.len(), 10, "query {i} 应召回 10 条");
    }
}

/// **S2-T4** ⚠️ 图可丢弃：四种场景均可加载且检索可用。
#[test]
fn T4_图可丢弃四种场景() {
    let base_dir = tempfile::tempdir().unwrap();
    let path = build_and_save(base_dir.path(), 300);
    let paths = helix_core::storage::graph_paths(&path);
    let golden = topk(builder().load(&path).unwrap(), "查询零", 10);

    // ① 删 manifest
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(&path, dir.path().join("t.idx")).unwrap();
    for f in [&paths.graph, &paths.data, &paths.manifest] {
        let _ = std::fs::copy(f, dir.path().join(f.file_name().unwrap()));
    }
    std::fs::remove_file(dir.path().join("t.idx.hnsw.manifest")).unwrap();
    let idx = builder().load(&dir.path().join("t.idx")).unwrap();
    assert!(matches!(idx.graph_status(), GraphStatus::Rebuilt(_)));
    assert!(
        !topk(
            builder().load(&dir.path().join("t.idx")).unwrap(),
            "查询零",
            10
        )
        .is_empty(),
        "降级后检索仍可用"
    );

    // ② 删图文件（data 缺失）
    let dir2 = tempfile::tempdir().unwrap();
    std::fs::copy(&path, dir2.path().join("t.idx")).unwrap();
    for f in [&paths.graph, &paths.data, &paths.manifest] {
        let _ = std::fs::copy(f, dir2.path().join(f.file_name().unwrap()));
    }
    std::fs::remove_file(dir2.path().join("t.idx.hnsw.data")).unwrap();
    let idx2 = builder().load(&dir2.path().join("t.idx")).unwrap();
    assert!(matches!(idx2.graph_status(), GraphStatus::Rebuilt(_)));
    assert!(!topk(
        builder().load(&dir2.path().join("t.idx")).unwrap(),
        "查询零",
        10
    )
    .is_empty());

    // ③ 篡改图文件一字节（CRC 兜底）
    let dir3 = tempfile::tempdir().unwrap();
    std::fs::copy(&path, dir3.path().join("t.idx")).unwrap();
    for f in [&paths.graph, &paths.data, &paths.manifest] {
        let _ = std::fs::copy(f, dir3.path().join(f.file_name().unwrap()));
    }
    let g = dir3.path().join("t.idx.hnsw.graph");
    let mut bytes = std::fs::read(&g).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&g, &bytes).unwrap();
    let idx3 = builder().load(&dir3.path().join("t.idx")).unwrap();
    assert!(matches!(idx3.graph_status(), GraphStatus::Rebuilt(_)));
    assert!(!topk(
        builder().load(&dir3.path().join("t.idx")).unwrap(),
        "查询零",
        10
    )
    .is_empty());

    // ④ 旧快照（无 sidecar）
    let dir4 = tempfile::tempdir().unwrap();
    std::fs::copy(&path, dir4.path().join("t.idx")).unwrap();
    let idx4 = builder().load(&dir4.path().join("t.idx")).unwrap();
    assert!(matches!(idx4.graph_status(), GraphStatus::Rebuilt(_)));
    assert!(!topk(
        builder().load(&dir4.path().join("t.idx")).unwrap(),
        "查询零",
        10
    )
    .is_empty());

    // 四种降级重建的结果都应与 golden 同规模（ANN 重建允许微小差异，不断言逐位）
    assert_eq!(golden.len(), 10);
}

/// **S2-T5** ⚠️ 损坏输入不 panic：图文件截断到 1% / 50% / 99%，以及 magic 篡改。
#[test]
fn T5_截断图文件不panic() {
    let base_dir = tempfile::tempdir().unwrap();
    let path = build_and_save(base_dir.path(), 300);
    let paths = helix_core::storage::graph_paths(&path);

    for (name, ratio) in [("1%", 0.01f64), ("50%", 0.5), ("99%", 0.99)] {
        let dir = tempfile::tempdir().unwrap();
        let snap = dir.path().join("t.idx");
        std::fs::copy(&path, &snap).unwrap();
        for f in [&paths.graph, &paths.data, &paths.manifest] {
            let _ = std::fs::copy(f, dir.path().join(f.file_name().unwrap()));
        }
        let g = dir.path().join("t.idx.hnsw.graph");
        let bytes = std::fs::read(&g).unwrap();
        let keep = ((bytes.len() as f64 * ratio) as usize).max(1);
        std::fs::write(&g, &bytes[..keep]).unwrap();

        // 必须返回可用索引（降级）或 Err，**不得 panic**
        let idx = builder().load(&snap).unwrap();
        assert!(
            matches!(idx.graph_status(), GraphStatus::Rebuilt(_)),
            "截断 {name} 应降级重建"
        );
        assert_eq!(
            topk(builder().load(&snap).unwrap(), "查询零", 10).len(),
            10,
            "截断 {name} 后检索仍应可用"
        );
    }

    // magic 篡改（graph 文件头前 4 字节）
    let dir = tempfile::tempdir().unwrap();
    let snap = dir.path().join("t.idx");
    std::fs::copy(&path, &snap).unwrap();
    for f in [&paths.graph, &paths.data, &paths.manifest] {
        let _ = std::fs::copy(f, dir.path().join(f.file_name().unwrap()));
    }
    let g = dir.path().join("t.idx.hnsw.graph");
    let mut bytes = std::fs::read(&g).unwrap();
    bytes[0] ^= 0xFF;
    std::fs::write(&g, &bytes).unwrap();
    let idx = builder().load(&snap).unwrap();
    assert!(matches!(idx.graph_status(), GraphStatus::Rebuilt(_)));
}

/// **S2-T6** 旧快照兼容（无 sidecar 的 FORMAT_VERSION=2 快照）。
///
/// # 质量门为什么是两条**不变式**，而不是「与 oracle 的重合率」数值阈值
///
/// 旧门是「20 个 query 的 Top-10 与 `BruteForceIndex` oracle 的重合率**均值 ≥ 0.90**」。
/// CI 上出现过一次 `均值 0.545 / 最差 0.200`（近 25 次 run 出现 1 次，**重跑同 commit 即绿**）。
/// 该值**落在抖动分布之外**：本地 64 次（50 单测 + 6 次整二进制并行）落在 0.990~1.000，
/// 评审方 30 轮**每轮重新建图**（=30 个不同拓扑）实测单轮均值最低 0.995、单 query 最低 0.900。
/// ⇒ 它是一次**尚未归因的异常**：既不能读成「阈值太紧」，也不能读成「绝对阈值不可用」。
/// 唯一的定量解释 `0.545 ≈ k²/N = 10²/200` 是**有条件**的：它只说明「**若**检索退化为任意
/// 返回 10 条，重合率会是这个量级」，**并不蕴含**拓扑抖动可以产生这个值
/// （前者是后者的必要条件、不是充分条件——初版把这条推反了）。
///
/// 所以本测试的立场是：**把不可归因的数值门换成可归因的不变式**——同样会红，但红得能指出
/// 是「内容缺失」「向量 / ID 灌错」还是「图不可导航」。**issue #34 的根因仍未定位**；
/// 重合率降级为诊断打印（且**先打印、后断言**，免得失败时反而看不到量级）。
///
/// # 两条不变式（都与图拓扑无关，故不 flaky）
///
/// 1. **内容覆盖（结构层）**：`graph_points == raw_vectors == N`。
/// 2. **自匹配（检索层，覆盖全部 N 篇）**：查询文本与文档 `i` 完全相同 ⇒ 向量逐位相同 ⇒
///    相似度全局最大 ⇒ 文档 `i` 必须排第一、相似度≈1。
///    ⚠️ 前提（写死在此，勿当普适结论）：① `ef_search ≥ 语料规模`（本测试 200/200；
///    §8.6 的 ef 校准范围是 100/200/400）；② doc `i` 的 chunk 文本 == query 文本。
///
/// ⚠️ 两条**都要有**，且**覆盖必须到全部 N 篇**：只查 20 篇时，20 条 query 恰好覆盖文档
/// 0..19，于是「重建只灌了前 30 篇」这类**内容缺失**（重合率已掉到 0.22）也能三查全绿。
///
/// ⚠️ 曾考虑但**不采用**的第三条（评审建议的「全量检索 id 集合 == 全量 id 集合」）：
/// **它本身会 flaky**。实测 `top_n(N)` 的返回值随建图拓扑在 198~200 之间跳（7 次里
/// 4 次 200 条、2 次缺 1 个、1 次缺 2 个：缺 `{190}` / `{199}` / `{144,198}` / `{174,198}`）。
/// 机理是 `level ≥ 1` 的点**不在 layer 0**（`hnsw.rs:500-511`：`generate_new_point` 只把新点
/// 推进**它自己那一层**，不回填低层），只能靠上层下降时被访问到，而访问得没访问到取决于拓扑
/// ⇒ 「取不回全部点」是**固有现象、不是缺陷**，不能当断言。
#[test]
fn T6_旧快照无图可加载() {
    const N: usize = 200;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.idx");
    {
        let mut idx = builder().build();
        for i in 0..N {
            idx.add(format!("旧文档 {i}")).unwrap();
        }
        idx.save(&path).unwrap();
    }
    // 手工删掉全部 sidecar，模拟「改造前产出的旧快照」
    helix_core::storage::remove_sidecars(&path).unwrap();

    let loaded = builder().load(&path).unwrap();
    assert!(
        matches!(loaded.graph_status(), GraphStatus::Rebuilt(_)),
        "旧快照应降级重建"
    );

    // ---- 不变式 1：内容覆盖（结构层，与检索行为 / 拓扑都无关） ----
    let st = loaded.tombstone_stats();
    eprintln!(
        "[T6] 重建后：图点数 {} / 快照向量条数 {} / 期望 {N}",
        st.graph_points, st.raw_vectors
    );
    assert_eq!(st.raw_vectors, N, "前提自检：快照里应有 {N} 条向量");
    assert_eq!(
        st.graph_points, st.raw_vectors,
        "重建后的图点数应等于快照向量条数（{N}）：少一个就说明重建**丢了内容**\
         ——这与拓扑抖动无关，是一类独立的退化"
    );

    let texts: Vec<String> = (0..N).map(|i| format!("旧文档 {i}")).collect();
    let s = loaded.into_searcher().unwrap();
    // oracle 只建一次再复用（早先实现每条 query 重建一遍整库，白算）。
    let oracle = oracle_index(&texts);

    // ---- 不变式 2：自匹配，**全部 N 篇**逐篇 ----
    // 先算完、先打印诊断，再断言：断言一失败就不该再执行诊断，
    // 否则最需要的量级线索（「均值 + 最差」正是 #34 的原始线索）反而看不到。
    // 一条 query 的原始结果：先全部算完 → 打印诊断 → 最后才断言。
    struct Row {
        i: usize,
        got: Vec<(u32, f32)>,
        oracle: Vec<u32>,
    }
    let rows: Vec<Row> = (0..N)
        .map(|i| {
            let q = format!("旧文档 {i}");
            Row {
                i,
                got: topk_searcher(&s, &q, 10),
                oracle: oracle_topk(&oracle, &q, 10),
            }
        })
        .collect();

    let mut overlap_sum = 0.0f64;
    let mut overlap_worst = 1.0f64;
    let mut self_fail = 0usize;
    for row in &rows {
        let ids: Vec<u32> = row.got.iter().map(|(id, _)| *id).collect();
        // 1-based 排名；None = 未进 Top-10。
        let rank = ids.iter().position(|id| *id == row.i as u32).map(|r| r + 1);
        let ov = overlap(&ids, &row.oracle);
        overlap_sum += ov;
        overlap_worst = overlap_worst.min(ov);
        if rank != Some(1) {
            self_fail += 1;
        }
        eprintln!(
            "[T6] query {}: 自身文档排名 {rank:?} / top1 相似度 {:.6} / 与 oracle 重合率 {ov:.3}",
            row.i,
            row.got.first().map_or(f32::NAN, |(_, sc)| *sc)
        );
    }
    eprintln!(
        "[T6] 降级重建：{N} 篇自匹配失败 {self_fail} 篇；与 oracle 的 Top-10 重合率 \
         均值 {:.3} / 最差 {:.3}（**仅诊断，不断言**）",
        overlap_sum / N as f64,
        overlap_worst
    );

    for row in &rows {
        // ① 自匹配：**必须排第一**。排第一 ⇒ 必然在 Top-K 内，故不再单列「在 Top-K 内」——
        //    报错信息里已列出实得 Top-10，「找不到」一眼可读（两条分工合并的理由）。
        //    找不到 = **图不可导航** 或 **向量 / ID 灌错**，正是「只断言非空」漏掉的真退化。
        assert_eq!(
            row.got.first().map(|(id, _)| *id),
            Some(row.i as u32),
            "query {} 的自身文档应排第一；实得 Top-10 = {:?}（若无 {}，说明降级重建后的图 \
             不可导航，或灌入的向量 / ID 有误，issue #34）",
            row.i,
            row.got.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            row.i
        );
        // ② 且相似度≈1：`hits.score` 是**余弦相似度**而非距离——`hnsw_rs_index.rs:5-7` 约定
        //    实现统一输出 `d² = 2−2cos`，retriever 再换算 `score = 1 − d²/2 = cos`
        //    ⇒ 自匹配（d² = 0）的得分是 **1**，不是 0。不同文本的向量互不相同
        //    （确定性 embedder）⇒ 最大值唯一。容差 1e-3：clamp 与求和舍入让它略小于 1。
        let top_score = row.got[0].1;
        assert!(
            (top_score - 1.0).abs() < 1e-3,
            "query {} 的自身文档余弦相似度应≈1，实测 {top_score}（向量与查询不同源？）",
            row.i
        );
        // ③ oracle 侧同一不变式（精确扫描，确定性），顺带钉住本测试依赖的前提：
        //    第 i 篇文档的 `chunk_id == i`。
        assert_eq!(
            row.oracle.first().copied(),
            Some(row.i as u32),
            "oracle 应把自身文档排第一（同时钉住「第 i 篇 chunk_id == i」这一前提）"
        );
    }
}

/// **S2-T7** 逃生舱不破：Brute 后端加载含图快照 → 忽略图。
#[test]
fn T7_brute后端忽略图() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 200);

    let idx = SearchIndexBuilder::default()
        .embedder(Some(Arc::new(TestEmbedder { dim: DIM })))
        .vector_backend(VectorBackend::Brute)
        .load(&path)
        .unwrap();
    assert_eq!(
        idx.graph_status(),
        &GraphStatus::NotApplicable,
        "Brute 无图是类型事实"
    );
    assert!(!topk(idx, "文档 3", 10).is_empty());
}

/// **S2-T8** 纯 BM25 不写图，且会删掉已存在的旧 sidecar。
#[test]
fn T8_纯BM25不写图并清理旧sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bm25.idx");

    // 先在同一路径建一个含图的库（前置：sidecar 存在）
    build_and_save(dir.path(), 100);
    std::fs::rename(dir.path().join("t.idx"), &path).unwrap();
    for suffix in ["hnsw.graph", "hnsw.data", "hnsw.manifest"] {
        let _ = std::fs::rename(
            dir.path().join(format!("t.idx.{suffix}")),
            dir.path().join(format!("bm25.idx.{suffix}")),
        );
    }
    let paths = helix_core::storage::graph_paths(&path);
    assert!(paths.manifest.exists(), "前置：应已有 sidecar");

    // 再用纯 BM25 装配存到**同一路径**
    let mut bm25 = SearchIndexBuilder::default()
        .embedder(None)
        .vector_backend(VectorBackend::Hnsw)
        .build();
    bm25.add("纯文本没有向量").unwrap();
    bm25.save(&path).unwrap();

    assert!(!paths.manifest.exists(), "纯 BM25 应删掉僵尸 sidecar");
    assert!(!paths.graph.exists());
    assert!(!paths.data.exists());
}

/// **S2-T9** ⚠️ 删除 + 图持久化：结果不含已删 doc，且 nb_point >= 向量条数（墓碑）。
#[test]
fn T9_删除后跨快照不复活() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("del.idx");

    let target = {
        let mut idx = builder().build();
        for i in 0..100 {
            idx.add(format!("待删文档 {i}")).unwrap();
        }
        let out = idx.add("这是要被删除的独特文档 XYZQQQ").unwrap();
        idx.save(&path).unwrap();
        out.doc_id
    };

    // 删除 → 再存
    {
        let mut idx = builder().load(&path).unwrap();
        idx.remove(target).unwrap();
        idx.save(&path).unwrap();
    }

    let loaded = builder().load(&path).unwrap();
    let hits = loaded
        .into_searcher()
        .unwrap()
        .search("这是要被删除的独特文档 XYZQQQ")
        .unwrap();
    for h in &hits.hits {
        assert!(
            !h.text.contains("XYZQQQ"),
            "已删文档不应被召回（跨快照复活）: {}",
            h.text
        );
    }

    // 墓碑留在图里：nb_point >= 快照 vectors 条数
    let paths = helix_core::storage::graph_paths(&path);
    let m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();
    let (_, vectors, _, _) = helix_core::storage::load_with_crc(&path).unwrap();
    assert!(
        m.nb_point >= vectors.len() as u64,
        "图中点数 {} 应 >= 向量条数 {}（墓碑摘不掉）",
        m.nb_point,
        vectors.len()
    );
}

/// **S2-T10** 加载后继续写入：新 chunk 可检索，且第二次加载仍是快路径。
///
/// ⚠️ 这条同时覆盖 N2 防回归：重载图再 dump 若不删旧文件，图会写到随机
/// 后缀文件名，manifest 绑定的路径上仍是旧图 ⇒ 这里会**假绿**吗？不会——
/// 断言 `Loaded` 时 CRC 校验会失败而降级，故本测试能抓到该 bug。
#[test]
fn T10_加载后继续写入仍走快路径() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("grow.idx");

    let mut idx = builder().build();
    for i in 0..200 {
        idx.add(format!("初始文档 {i}")).unwrap();
    }
    idx.save(&path).unwrap();

    // load → add → save
    {
        let mut idx = builder().load(&path).unwrap();
        assert_eq!(idx.graph_status(), &GraphStatus::Loaded);
        idx.add("全新文档 ZZZNEW").unwrap();
        idx.save(&path).unwrap();
    }

    // 第二次加载：新 chunk 可检索，且仍是快路径
    let loaded = builder().load(&path).unwrap();
    assert_eq!(
        loaded.graph_status(),
        &GraphStatus::Loaded,
        "增量保存后仍需走快路径（N2：dump 前删旧文件）"
    );
    let hits = loaded
        .into_searcher()
        .unwrap()
        .search("全新文档 ZZZNEW")
        .unwrap();
    assert!(
        hits.hits.iter().any(|h| h.text.contains("ZZZNEW")),
        "新增 chunk 应可检索"
    );

    // nb_point 应增长
    let paths = helix_core::storage::graph_paths(&path);
    let m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();
    assert!(
        m.nb_point >= 201,
        "图中点数应含新增条目，实际 {}",
        m.nb_point
    );
}

/// **S2-T11** 维度 / 距离 / 平台不匹配 → 降级重建，不 panic。
#[test]
fn T11_manifest字段不匹配降级() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 200);
    let paths = helix_core::storage::graph_paths(&path);
    let m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();

    // 三种不匹配各来一遍（dim / dist_id / platform）
    for (name, mutate) in [("dim", 0usize), ("dist_id", 1), ("platform", 2)] {
        let mut m = m.clone();
        match mutate {
            0 => m.dim = 999,
            1 => m.dist_id = "other-v9".into(),
            _ => m.platform = 0x99,
        }
        helix_core::storage::write_manifest_atomic(&paths.manifest, &m).unwrap();

        let idx = builder().load(&path).unwrap();
        assert!(
            matches!(idx.graph_status(), GraphStatus::Rebuilt(_)),
            "{name} 不匹配应降级重建"
        );
        assert_eq!(topk(builder().load(&path).unwrap(), "文档 5", 10).len(), 10);
    }
}

/// **S2-T12** 连续加载 50 次不崩（R23 Box::leak 的回归护栏）。
#[test]
fn T12_连续加载50次() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 60);

    let first = topk(builder().load(&path).unwrap(), "文档 3", 10);
    for i in 0..50 {
        let idx = builder().load(&path).unwrap();
        assert_eq!(
            idx.graph_status(),
            &GraphStatus::Loaded,
            "第 {i} 次应加载图"
        );
        assert_eq!(topk(idx, "文档 3", 10), first, "第 {i} 次结果应一致");
    }
}

/// **S2-T15** manifest 版本未知（未来版本写的）→ 降级重建。
#[test]
fn T15_manifest版本未知降级() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 150);
    let paths = helix_core::storage::graph_paths(&path);
    let mut m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();
    m.manifest_version = 999;
    helix_core::storage::write_manifest_atomic(&paths.manifest, &m).unwrap();

    let idx = builder().load(&path).unwrap();
    assert!(matches!(idx.graph_status(), GraphStatus::Rebuilt(_)));
    assert_eq!(topk(builder().load(&path).unwrap(), "文档 2", 10).len(), 10);
}

/// **S2-T16** 快照已更新、图未更新（改 snapshot_crc）→ 降级重建。
#[test]
fn T16_图与快照版本错配降级() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 150);
    let paths = helix_core::storage::graph_paths(&path);
    let mut m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();
    m.snapshot_crc ^= 0xFFFF_FFFF; // 模拟「快照重写但 manifest 还是旧的」
    helix_core::storage::write_manifest_atomic(&paths.manifest, &m).unwrap();

    let idx = builder().load(&path).unwrap();
    assert!(
        matches!(idx.graph_status(), GraphStatus::Rebuilt(_)),
        "snapshot_crc 不匹配必须降级（多文件原子性的核心防线）"
    );
    assert_eq!(topk(builder().load(&path).unwrap(), "文档 2", 10).len(), 10);
}

/// **S2-T13** 并行建图（D-S2-05）质量等价 —— **可归因不变式口径**（2026-09-11 重写）。
///
/// # 为什么不再比「与 oracle 的平均 Top-10 重合率」
///
/// 旧口径断言两条数值门：① 并行与串行的重合率差 `< 0.05`；② 各自 `>= 0.90`。
/// 它是**跨两张独立建图**的比较，而 `hnsw_rs` 每次建图都用 `StdRng::from_os_rng()`
/// 分配层高、并行插入顺序本身也不确定（C8）⇒ 两张图的拓扑互不相同，该数值带一条
/// **无上界**的随机尾巴：
///
/// | 观测（**运行范围**见右列） | 值 |
/// | --- | --- |
/// | CI run `34562148819` charabia job | 并行 **0.6400** / 串行 1.0000 ⇒ 挂 |
/// | 本机 320 次独立建图（10 物理核；rayon 2/10；含 6 路 CPU 负载） | min **0.9650**，无一 < 0.95 |
/// | 本机整套 `--test graph_persist` 22 次（5 次无负载 + 17 次 4 路负载） | min **0.9850**，全绿 |
///
/// 即「0.64 / 0.86」这类离群在本机**复现不出**，也**归因不到**具体代码路径——
/// 用阈值堵不住（幅度 14~36pp）。故照 T6（#35）的同一味药：把**不可归因的数值门**
/// 换成**可归因的不变式**。原重合率与差值仍打印，但**不参与判定**。
///
/// # 已用探针排除的候选因子（本 PR 的实测结论，勿重复）
///
/// - **capacity**：门面建初始索引用 `HnswRsIndex::with_capacity(1024)`
///   （`search/index.rs:320`），小于 N=1200；但 `hnsw_rs` 的 `max_elements` 只是
///   `Vec::with_capacity` 的**预分配提示**（`hnsw.rs:447-462`），没有越界迁置分支
///   ⇒ 实测 1024 与 1200 **无差异**，也不与并行插入构成竞态。
/// - **save→load roundtrip**：**同一张图**的 pre（内存图）/ post（load 回来的图）
///   逐位一致 —— 50 次同图 A/B，`max|Δ重合率| = 0.0000` ⇒ dump/load 不丢边、不退化。
/// - **图丢点 / 向量错位**：80 次建图的图内点数恒 `== 1200`，且**图内精确** top-10 与
///   oracle 的重合率恒 `== 1.0000`。
/// - **不可达点**（点的自身向量查不到自己）：抽样 ≤ 3/300 = 1%。
/// - **hnsw_rs `entry_point` 竞态**（`hnsw.rs:1085-1100`）：真实存在但量级仅 ~1%，
///   不足以解释 0.64（与 §2.2 的探针结论一致）。
///
/// # 现在的门（全部是**同一张图**上的确定量，对给定图逐位可复现）
///
/// 1. **G1 覆盖**：加载后的图点数 `==` 文档数，且精确路径枚举出的 id 集合
///    `==` 全部 `chunk_id`（不漏 / 不重 / 不多）。并行插入丢点、越界、重复插 → 红。
/// 2. **G2 保真**：`QUERIES` 条 query 的**图内精确** top-10 `id` 序列 `==`
///    `BruteForceIndex` oracle 的 top-10 序列（**两侧同约定**：见 `oracle_topk`）。
///    id↔向量错位、向量被截断、score 公式被改坏 → 红。
/// 3. **G3 ANN 质量（同图）**：ANN 路返回的 10 条**全部**落在**本图精确序**的
///    top-[`T13_EXACT_RANK_GATE`] 内。
///    实测带宽：`worst exact rank ∈ [10, 11]`（80 次建图；min 10 / p50 10 / max 11）
///    ⇒ 余量 ≈ 1.8×。它比「重合率」稳在有界空间里：图/检索一退步，返回项会掉到精确序
///    的远处（名次从十位量级跳到百位量级），而不是只差 3 个百分点。
///    ⚠️ 两条腿各带一条**前置路由断言**（exact 腿 `Route::Exact`、ANN 腿 `Route::Ann`）：
///    任一条静默走到另一条 ⇒ 立刻红，而不是让 G2/G3 悄悄退化成恒真式。
/// 4. **G4 冻结图确定性**：同一 query 在冻结图上共查 `REPEATS` 次，`(chunk_id, score)`
///    序列逐位一致（NFR-06）。
///
/// # ⚠️ 本测试覆盖不到什么（如实声明）
///
/// 它**不再**断言「并行图与串行图的 ANN 召回率在统计意义上等价」——那是一个**跨图
/// 统计命题**，而 `hnsw_rs` 的建图 RNG 无 seed，无法确定性地判定。两次建图的重合率
/// 仍打进 `stderr` 供人工比对趋势；T7-21（翻 `parallel_build` 默认值）若需要这条统计
/// 证据，应走 bench/离线标定，而不是一条会在 CI 里间歇翻脸的断言。
///
/// 「并行分支真的被走到」由 **T22** 钉死（`parallel_inserts() == 1`，**后端层**）——
/// 门面层不暴露该计数器，故本测试把它当前置护栏。
///
/// ⚠️ **T22 钉死的范围比「门面确实走了并行」小一圈**（评审 #39 意见 3）：它直接驱动
/// `HnswRsIndex::with_parallel_build(true)` + `add_batch`，钉的是**后端分派**（开关 +
/// 阈值）；未覆盖门面层透传（`SearchIndexBuilder::parallel_build` → 新建/重建两条路径）
/// 与 `flush` 是否真把 ≥1000 条交付给 `add_batch`（`batch_size` 耦合）。若门面接线回退，
/// 本测试会退化成「串行 vs 串行」，而 G1~G4 **与 T22 双双全绿**（T22 自己的注释也写了
/// 这处盲区）。**T7-21（#24）已评估过**这条「门面级护栏」：补它需要给 `VectorIndex`
/// 加公开的可观测读法，结论是**不加**（公开面加法不值得）⇒ **该盲区保留**，
/// 由 T22 的文档与 `search/config.rs` 中 `parallel_build()` 的文档**双向如实声明**。
#[test]
fn T13_并行建图质量等价() {
    const N: usize = 1200;
    // 本测试同框有三个互不相干的「20」（评审 #39 意见 4），分别起名防将来只改一处：
    // `QUERIES` = 参与比对的 query 数；`REPEATS` = G4 同一 query 的**重复查询总数**；
    // 文件级 `T13_EXACT_RANK_GATE` = ANN 名次门（**语义不同**，不并入这两者）。
    const QUERIES: usize = 20;
    const REPEATS: usize = 20;
    let dir = tempfile::tempdir().unwrap();
    // 每篇文档带同一个 meta 标记：match-all 过滤 ⇒ `FilterKind::Filtered`
    // （**不是**热路径的 `Alive`）⇒ allowed = N ≤ 阈值 ⇒ 门面走**精确路径（C）**。
    // 这是本测试所有「图内精确」断言的可达路径。
    const MARK: &str = "t13-match-all";
    let docs: Vec<Document> = (0..N)
        .map(|i| Document {
            text: format!("并行文档 {i} 的内容"),
            source: String::new(),
            metadata: serde_json::json!({ "t13": MARK }),
            dedup_key: None,
        })
        .collect();
    let texts: Vec<String> = docs.iter().map(|d| d.text.clone()).collect();
    let all = Filter::eq("t13", MARK);

    // ⚠️ **并行生效的前提是 `parallel_build(true)` 与「一次交够 ≥1000 条」同时成立**：
    // `add()` 攒够 `batch_size` 才 flush，而 `flush` 交给 `add_batch` 的是**整个 `pending`**
    // ⇒ 批量 = 存量（≤ batch_size−1）+ 本批各文档的 chunk 数。默认 `batch_size=64` 且文档不大时
    // 批量 ≪ 1000，够不到并行阈值（评审 #13 发现 1；上界的精确口径见评审 #49 P2-1 的勘误）。
    // 本测试用**同一个** batch_size，保证唯一变量是 parallel_build。
    let path_par = dir.path().join("par.idx");
    {
        let mut idx = builder().parallel_build(true).batch_size(N).build();
        for d in &docs {
            idx.add(d.clone()).unwrap();
        }
        idx.save(&path_par).unwrap();
    }
    let par = builder().batch_size(N).load(&path_par).unwrap();
    assert_eq!(
        par.graph_status(),
        &GraphStatus::Loaded,
        "basename 拼错会静默降级（P0-1）——降级后本测试全部断言都失去意义"
    );

    // 串行建图（⚠️ **显式关**：T7-21 / D-J11 之后门面默认已是「开」——若这里仍吃默认，
    // 本测试会退化成「并行 vs 并行」，正是 T22 文档里警告的那种退化）
    let path_seq = dir.path().join("seq.idx");
    {
        let mut idx = builder().parallel_build(false).batch_size(N).build();
        for d in &docs {
            idx.add(d.clone()).unwrap();
        }
        idx.save(&path_seq).unwrap();
    }
    let seq = builder().batch_size(N).load(&path_seq).unwrap();
    assert_eq!(seq.graph_status(), &GraphStatus::Loaded);

    // ---- G1 覆盖：图点数 + raw_vectors 条数（`tombstone_stats` 是唯一公开口径）----
    for (label, idx) in [("并行", &par), ("串行", &seq)] {
        let ts = idx.tombstone_stats();
        assert_eq!(
            ts.graph_points, N,
            "{label}库图内点数应 == 文档数（本语料 chunk 1:1）；并行插入丢点会在此暴露，实测 {}",
            ts.graph_points
        );
        assert_eq!(ts.raw_vectors, N, "{label}库 raw_vectors 条数应 == 文档数");
        assert_eq!(idx.num_chunks(), N as u32, "{label}库 chunk 数应 == 文档数");
    }

    let sp = par.into_searcher().unwrap();
    let ss = seq.into_searcher().unwrap();
    // oracle 只建一次再复用（1200 篇 × 20 条 query，没必要每条重建）。
    let oracle_idx = oracle_index(&texts);

    // 「图内精确」结果：match-all 过滤 ⇒ 路径 C。`route == Exact` 是本口径的**前置断言**——
    // 若哪天兜底阈值被改小到 < N，这里会立刻红，而不是静默退化成 ANN 让断言变弱。
    let exact = |s: &Searcher, q: &str, k: usize| -> Vec<u32> {
        let r = s
            .search_with(q)
            .mode(SearchMode::Vector)
            .top_n(k)
            .filter(&all)
            .exec()
            .unwrap();
        assert_eq!(
            r.metrics.vector_route,
            VectorRoute::Exact,
            "match-all 过滤（allowed = {N} ≤ 兜底阈值）必须走精确路径；\
             若退化为 Ann，本测试的『图内精确』口径即失效"
        );
        r.hits.iter().map(|h| h.chunk_id).collect()
    };

    // ANN 腿：**无用户过滤** ⇒ `try_build_predicate(index, None)` 返回 `AliveOnly`
    // （`query/filter.rs:195`）⇒ `plan()` 恒 `Ann`（`retriever/vector.rs:48-53`；
    // hnsw 的 `prefers_exact` 只认 `FilterKind::Filtered`，`hnsw_rs_index.rs:351-358`）。
    // 与 exact 腿**同构的前置断言**（评审 #39 意见 2）：若哪天热路径分派翻转、这条腿
    // 静默变成 exact 结果，G3 就退化成「exact top-10 ⊆ exact top-20」的**恒真式**——
    // 绿着失效、零红灯。四条门因此全部自证路径。
    let ann = |s: &Searcher, q: &str, k: usize| -> Vec<(u32, f32)> {
        let r = s
            .search_with(q)
            .mode(SearchMode::Vector)
            .top_n(k)
            .exec()
            .unwrap();
        assert_eq!(
            r.metrics.vector_route,
            VectorRoute::Ann,
            "无用户过滤的向量路必须走 ANN；退化为 Exact 会让 G3 的名次门变成恒真式"
        );
        r.hits.iter().map(|h| (h.chunk_id, h.score)).collect()
    };

    // G1（续）：精确路径全量枚举的 id 集合必须恰为 0..N（不漏 / 不重 / 不多）
    let expect: Vec<u32> = (0..N as u32).collect();
    for (label, s) in [("并行", &sp), ("串行", &ss)] {
        let mut got = exact(s, "并行文档 0", N + 1);
        got.sort_unstable();
        assert_eq!(
            got, expect,
            "{label}库：图内全量枚举的 id 集合应恰为 0..{N}（并行插入丢点/重复插在此暴露）"
        );
    }

    // ---- G2 / G3 + 诊断（重合率只打印，不判定）----
    let audit = |s: &Searcher, label: &str| -> (f64, usize) {
        let mut sum = 0.0f64;
        let mut worst = 0usize;
        for i in 0..QUERIES {
            let q = format!("并行文档 {i}");
            let oracle_ids = oracle_topk(&oracle_idx, &q, 10);

            // G2：图内精确 top-10 应与 oracle **逐位一致**（含顺序）
            let ex_ids = exact(s, &q, 10);
            assert_eq!(
                ex_ids, oracle_ids,
                "{label}库 query {i}：图内精确 top-10 应与 BruteForce oracle 逐位一致"
            );

            // G3：ANN 返回的每一条都必须落在本图精确序的 top-T 内
            let top_t = exact(s, &q, T13_EXACT_RANK_GATE);
            let ann_ids = ann(s, &q, 10)
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<u32>>();
            assert_eq!(ann_ids.len(), 10, "{label}库 query {i} 应召回 10 条");
            for id in &ann_ids {
                match top_t.iter().position(|x| x == id) {
                    Some(pos) => worst = worst.max(pos + 1),
                    None => panic!(
                        "{label}库 query {i}：ANN 返回 chunk {id} 落在本图精确序 \
                         top-{T13_EXACT_RANK_GATE} 之外 —— 图/检索已退化（旧口径只把它\
                         记成一个百分数，看不出名次掉到了哪里）"
                    ),
                }
            }

            let set: std::collections::HashSet<u32> = oracle_ids.iter().copied().collect();
            sum += ann_ids.iter().filter(|x| set.contains(x)).count() as f64 / 10.0;
        }
        (sum / QUERIES as f64, worst)
    };
    let (r_par, worst_par) = audit(&sp, "并行");
    let (r_seq, worst_seq) = audit(&ss, "串行");

    // ---- G4 冻结图确定性：同一 query 共查 `REPEATS` 次（1 次基准 + REPEATS-1 次重复），
    // `(chunk_id, score)` 序列逐位一致（NFR-06）----
    // `1..REPEATS` 而非 `0..REPEATS`：第 0 轮是 `first == first` 的恒真式，不构成检验
    // （评审 #39 意见 4）。
    for (label, s) in [("并行", &sp), ("串行", &ss)] {
        let q = "并行文档 7";
        let first = ann(s, q, 10);
        for round in 1..REPEATS {
            assert_eq!(
                ann(s, q, 10),
                first,
                "{label}库第 {round} 次重复查询与首次不逐位一致"
            );
        }
    }

    eprintln!(
        "[T13] 诊断（**不参与判定**，跨图数值）：与 oracle 的平均 Top-10 重合率 并行 \
         {r_par:.4} / 串行 {r_seq:.4}（差 {:.4}）",
        (r_par - r_seq).abs()
    );
    eprintln!(
        "[T13] 判定门：图内精确 == oracle 逐位 ✓ / 图内全量 id 集合 ✓ / ANN 返回项均落在\
         本图精确序 top-{T13_EXACT_RANK_GATE} 内（最差名次 并行 {worst_par} / 串行 {worst_seq}）\
         / 冻结图重复查询逐位一致 ✓"
    );
}

/// **S2-T22** ⚠️ 并行分支**真的被走到**（T13 的前置护栏）。
///
/// T13 依赖「`batch_size` 足够大」这个隐式耦合；一旦耦合被破坏（改默认值、
/// 改 flush 策略），T13 会退化成「串行 vs 串行」并且**依然全绿**。
/// 本测试直接钉死 `add_batch` 的分派：不加这个护栏，发现 1 无从暴露。
///
/// # T7-21 / D-J11 翻转后的口径（2026-09-14）
///
/// 翻转的是**门面默认**（`SearchIndexBuilder` / `Config`），**不是**本测试直接用的低层
/// 构造子。故本测试两条腿一律**显式传开关**、不依赖任何默认值 ——
/// 低层 `HnswRsIndex::with_capacity` 的默认值属**实现细节、不作契约**
/// （门面默认由 `search/config.rs` 的单测 `并行建图默认开且可显式关闭` 钉住）。
///
/// # ⚠️ 本测试覆盖不到什么（如实声明）
///
/// 它只覆盖**后端分派**（开关 + 阈值）。「门面配置 → 透传到新建 / 重建两条路径 →
/// `flush` 真把 ≥1000 条交给 `add_batch`」这条**端到端链路不可观测**
/// （`parallel_inserts()` 只在低层 `HnswRsIndex` 上，门面不暴露该计数）；
/// T7-21 评估过「为此加公开可观测 API」，结论是**不加**（见 `search/config.rs` 中
/// `parallel_build()` 的文档）。
///
/// ⚠️ 另一条**已知边界**（实测，2026-09-14）：门面默认 `batch_size = 64` < 阈值 1000
/// ⇒ **增量 `flush` 路径永远走不到并行**；能吃到并行的只有 `rebuild_vector_index` 的
/// **单次全量**建图（`compact()` 重建 / 冷启动降级重建）。
#[test]
fn T22_并行分支真的被走到() {
    let items: Vec<(u32, NormalizedVector)> = (0..1200)
        .map(|i| {
            (
                i,
                NormalizedVector::new(hash_vec(&format!("向量 {i}"), DIM)),
            )
        })
        .collect();

    // 显式关闭 / 批量不足 → 串行（⚠️ 开关显式传，不依赖默认值）
    let mut off = HnswRsIndex::with_capacity(1200).with_parallel_build(false);
    off.add_batch(&items).unwrap();
    assert_eq!(
        off.parallel_inserts(),
        0,
        "显式关闭 ⇒ 必须串行（保拓扑可复现）"
    );

    let mut small = HnswRsIndex::with_capacity(1200).with_parallel_build(true);
    small.add_batch(&items[..999]).unwrap();
    assert_eq!(small.parallel_inserts(), 0, "低于阈值 1000 应回落串行");

    // 开关开 + 批量足够 → 并行
    let mut on = HnswRsIndex::with_capacity(1200).with_parallel_build(true);
    on.add_batch(&items).unwrap();
    assert_eq!(on.parallel_inserts(), 1, "1200 条 + 开关开 ⇒ 应走并行");
    assert_eq!(on.len(), 1200);
}

/// **S2-T17** 建图参数漂移：`max_nb_connection` / `ef_construction` 与内核常量
/// 不一致 → §4.6 步骤 4.5 的 Description 预校验拦下，降级重建。
#[test]
fn T17_建图参数漂移降级() {
    let dir = tempfile::tempdir().unwrap();
    let path = build_and_save(dir.path(), 150);
    let paths = helix_core::storage::graph_paths(&path);
    let mut m = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();

    // ef_construction 改成与内核常量（300）不一致 → 预校验必须拦下
    m.ef_construction = 999;
    helix_core::storage::write_manifest_atomic(&paths.manifest, &m).unwrap();
    let idx = builder().load(&path).unwrap();
    assert!(
        matches!(idx.graph_status(), GraphStatus::Rebuilt(_)),
        "ef_construction 漂移应被 Description 预校验拦下"
    );
    assert_eq!(topk(builder().load(&path).unwrap(), "文档 2", 10).len(), 10);

    // max_nb_connection 同理（改回 ef、只动 M）
    let mut m2 = helix_core::storage::read_manifest(&paths.manifest)
        .unwrap()
        .unwrap();
    m2.max_nb_connection = 7;
    helix_core::storage::write_manifest_atomic(&paths.manifest, &m2).unwrap();
    let idx = builder().load(&path).unwrap();
    assert!(
        matches!(idx.graph_status(), GraphStatus::Rebuilt(_)),
        "max_nb_connection 漂移应被 Description 预校验拦下"
    );
    assert_eq!(topk(builder().load(&path).unwrap(), "文档 2", 10).len(), 10);
}

// ---------------------------------------------------------------------------
// 开关语义（评审 #12 发现 3：三个新开关此前零测试覆盖）
// ---------------------------------------------------------------------------

/// 让 `.hnsw.graph` 的位置被一个**目录**占据 ⇒ 写图链路必失败。
///
/// 不用 chmod：CI 以 root 运行时权限位无效，`File::create` 撞上同名目录
/// 才是跨 POSIX / Windows 都稳定的失败方式。
fn 让写图必失败(path: &std::path::Path) {
    std::fs::create_dir_all(helix_core::storage::graph_paths(path).graph).unwrap();
}

/// 建一个 n 篇文档的库（不落盘）。
fn 内存库(n: usize) -> SearchIndex {
    let mut idx = builder().build();
    for i in 0..n {
        idx.add(format!("文档 {i} 的内容")).unwrap();
    }
    idx
}

/// **S2-T18** `GraphPersistMode::Strict`：写图失败 → `save` 返回 **Err**（且状态已记录）。
#[test]
fn T18_strict模式下图失败升级为Err() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.idx");
    let mut idx = builder().graph_mode(GraphPersistMode::Strict).build();
    for i in 0..100 {
        idx.add(format!("文档 {i} 的内容")).unwrap();
    }
    让写图必失败(&path);

    let err = idx
        .save(&path)
        .expect_err("Strict 模式下图失败必须升级为 Err");
    assert!(!format!("{err}").is_empty(), "错误信息应可读");
    assert!(
        matches!(idx.graph_status(), GraphStatus::PersistFailed(_)),
        "即便返回 Err 也应记录图状态，实测 {:?}",
        idx.graph_status()
    );
}

/// **S2-T19** 默认（`Lenient`）：写图失败 → **快照照常落盘**，`save` 返回 Ok，
/// 状态为 `GraphStatus::PersistFailed`（P0-3 的核心语义：缓存坏了不能否决主数据）。
#[test]
fn T19_lenient模式下图失败不阻断save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.idx");
    let mut idx = 内存库(100);
    让写图必失败(&path);

    idx.save(&path).expect("图是缓存，写图失败不该让 save 失败");
    assert!(path.exists(), "快照必须完整落盘");
    assert!(
        matches!(idx.graph_status(), GraphStatus::PersistFailed(_)),
        "应如实记录写图失败（而非伪装成 Rebuilt），实测 {:?}",
        idx.graph_status()
    );
    // 快照本身仍可加载（走降级重建）
    let loaded = builder().load(&path).unwrap();
    assert!(matches!(loaded.graph_status(), GraphStatus::Rebuilt(_)));
    assert_eq!(topk(loaded, "文档 2", 10).len(), 10);
}

/// **S2-T20** `without_graph_persist()`：写侧不落图、读侧不碰图。
#[test]
fn T20_without_graph_persist不落图也不读图() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.idx");
    let mut idx = builder().without_graph_persist().build();
    for i in 0..100 {
        idx.add(format!("文档 {i} 的内容")).unwrap();
    }
    idx.save(&path).unwrap();

    let paths = helix_core::storage::graph_paths(&path);
    assert!(!paths.graph.exists(), "不应落图拓扑");
    assert!(!paths.manifest.exists(), "不应发布 manifest");
    assert!(matches!(idx.graph_status(), GraphStatus::NotApplicable));
    assert_eq!(
        idx.graph_dump_elapsed(),
        None,
        "未发生图落盘 ⇒ 应为 None 而不是 0ms"
    );

    // 读侧：即便存在一份合法 sidecar 也不该去读，且必须说出原因
    let with_graph = dir.path().join("g.idx");
    let mut idx2 = 内存库(100);
    idx2.save(&with_graph).unwrap();
    let loaded = builder().without_graph_persist().load(&with_graph).unwrap();
    assert!(
        matches!(loaded.graph_status(), GraphStatus::Rebuilt(r) if r.contains("已关闭")),
        "关闭持久化时读侧应显式说明原因，实测 {:?}",
        loaded.graph_status()
    );
}

/// **S2-T21** `graph_dump_elapsed()`：真的落图时 `Some`，从未落图时 `None`。
#[test]
fn T21_graph_dump_elapsed语义() {
    let dir = tempfile::tempdir().unwrap();
    let mut idx = 内存库(100);
    idx.save(&dir.path().join("u.idx")).unwrap();
    assert!(
        idx.graph_dump_elapsed().is_some(),
        "走了图持久化 ⇒ 应为 Some"
    );
    assert!(matches!(idx.graph_status(), GraphStatus::Loaded));

    // 纯 BM25（无向量）⇒ 从未发生图落盘
    let bm25_path = dir.path().join("bm25.idx");
    let mut b = SearchIndexBuilder::default()
        .embedder(None)
        .vector_backend(VectorBackend::Hnsw)
        .build();
    b.add("纯文本无向量").unwrap();
    b.save(&bm25_path).unwrap();
    assert_eq!(b.graph_dump_elapsed(), None);
    assert!(matches!(b.graph_status(), GraphStatus::NotApplicable));
}
