//! 向量单路召回。
//!
//! 流程：`embed_query`（**加前缀**）→ 向量索引检索 → 距离转相似度 → Top-K。
//!
//! # 边界（架构文档 4.3 硬约束）
//!
//! 本文件**不得** import `super::bm25`，反之亦然——两条 lane 互不引用。
//!
//! # V2 Step 5：向量路的两条形态
//!
//! `Retriever::search_filtered` 仍是 **ANN 路径**（语义一字未改）；
//! 新增的 inherent [`VectorRetriever::search_exact_filtered`] 是**精确路径**，
//! [`VectorRetriever::plan`] 回答"该谓词下走哪条"。
//! **分派与记账归编排层**（`query::searcher`）——它才是"这次检索有没有向量路"
//! 的信息持有者，也是 `Metrics::vector_route` 的写入者。

use std::cmp::Ordering;

use crate::embed::Embedder;
use crate::error::Result;
use crate::predicate::CandidateFilter;
use crate::types::ChunkId;
use crate::vector::{NormalizedVector, VectorIndex, VectorRoute};

use super::{Retriever, Scored};

/// **一个段的**向量检索视图（`S8-05`）：段自己的向量索引 + 它在全局 ID 空间里的 `chunk_id` 基址。
///
/// ⚠️ 与 `SegmentSet` / `SegmentRef`（`retriever::bm25`）**刻意不共用**：本模块的硬约束是
/// 「向量路不得 import BM25 路，反之亦然」（见文件头）—— 两条 lane 只共享 `SearchParts`
/// 这一层**载具**，彼此不引用对方的类型。
///
/// ⚠️ **不含 `base_doc`**：向量路只按 `chunk_id` 取点（HNSW 的 `origin_id` 就是建索引时的
/// `chunk_id`），`doc` 维度的折算归谓词（`SegmentFilter` 自己持有 `base_doc`）⇒ 这里放它
/// 只会是一个**没有任何消费者**的字段。
#[derive(Clone, Copy)]
pub struct VectorSegmentRef<'a> {
    /// 段自己的向量索引。
    ///
    /// `None` = 该段没有向量索引（纯 BM25 装配 / 该段无向量能力）⇒ 本段对向量路的贡献是
    /// **空集**（不是错误：内容确实没有向量可召回）。
    pub index: Option<&'a dyn VectorIndex>,
    /// 该段的全局 `chunk_id` 基址（**本地 `chunk_id` + 它 = 全局**）
    pub base_chunk: ChunkId,
}

/// **逐段 route 的并集分类**（设计 §4.6.1 / Q5）—— `S8-05` 的 `Metrics.vector_route` 取形。
///
/// - 全 `Ann` ⇒ `Ann`；全 `Exact` ⇒ `Exact`；**两者混合 ⇒ [`VectorRoute::Mixed`]**；
/// - `None` 项（未参与的段）**被忽略**；一个有效项都没有 ⇒ `None`。
///
/// ⚠️ 不能「取最保守者」（会丢掉「有一部分段其实走了精确」这个事实）也不能「只报主段的」
/// （那正是 `S8-05` 要修的半盲）—— 并集分类是唯一不说谎的取形。
pub fn union_route(routes: &[VectorRoute]) -> VectorRoute {
    let mut it = routes
        .iter()
        .copied()
        .filter(|r| !matches!(r, VectorRoute::None));
    let Some(first) = it.next() else {
        return VectorRoute::None;
    };
    debug_assert!(
        matches!(first, VectorRoute::Ann | VectorRoute::Exact),
        "逐段 route 只应是 Ann / Exact（None 表示未参与，已被过滤）"
    );
    if it.all(|r| r == first) {
        first
    } else {
        VectorRoute::Mixed
    }
}

/// **跨段**向量检索（`S8-05`）：对每段各查一次（段内本地 id），再**全局归并**。
///
/// # 为什么是「逐段查 + 全局归并」而不是「拼一张大图」
///
/// 段一旦发布就**不可变**（`I8-2`），`Box<dyn VectorIndex>` 也**不是 `Clone`**，而
/// `hnsw_rs` **没有图合并 API**（设计 §3.2）⇒ 只能逐段查、在结果层归并。
///
/// # 归并口径（设计 §4.6.1）
///
/// `(距离 asc, 全局 chunk_id asc)`。距离升序**等价于** `VectorRetriever::to_scored` 的
/// 相似度降序（`score = 1 − d²/2` 单调）⇒ 归并**复用同一个 `to_scored`**，不另写一份
/// 排序口径（否则「两条路径输出一致」会退化成两份代码的巧合）。
///
/// # 为什么 `Brute` 下「跨段 == 单段」逐位一致（承诺，§4.6.2）
///
/// 每段取自己的 top-`k`。全局 top-`k` 的**任一**成员，在它自己那一段里的排名必然 `< k`
/// （比它更优的**同段**候选 ≤ 比它更优的**全局**候选数 `< k`）⇒ 它**不会**落在段内截断
/// 之外 ⇒ 归并是**无损**的：并集的首 `k` 个 = 全局首 `k` 个，且两侧用同一个排序键。
/// ⚠️ `Hnsw` 下**不承诺**逐位一致（两张图 ≠ 一张图）——那是 ANN 的定义域，不是缺陷。
pub struct SegmentedVectorRetriever<'a> {
    embedder: &'a dyn Embedder,
    segments: &'a [VectorSegmentRef<'a>],
}

impl<'a> SegmentedVectorRetriever<'a> {
    /// 构造：`segments` 必须是 **FIFO 顺序**（与 `View::segments_in_order()` 一致）。
    ///
    /// ⚠️ 顺序影响**同分时的稳定性**（`chunk_id` 已经是全局全序键，所以结果不变），
    /// 但保留 FIFO 让「逐段」这件事在日志与调试里可读。
    pub fn new(embedder: &'a dyn Embedder, segments: &'a [VectorSegmentRef<'a>]) -> Self {
        Self { embedder, segments }
    }

    /// **是否有可查的段**（有向量索引且非空）—— 用于判定「本段是否参与归并」。
    fn participates(seg: &VectorSegmentRef<'_>) -> bool {
        seg.index.is_some_and(|vi| !vi.is_empty())
    }

    /// 逐段检索 + 全局归并。
    ///
    /// - `filters[i]` 是**第 `i` 段的段内谓词**（`CandidateFilter` 的 `contains` 收**本地**
    ///   `chunk_id`）—— 跨段谓词的折算见 `SegmentPredicate`（`search::view`）；
    /// - `route_of(i)` 回答「第 `i` 段走精确还是 ANN」（逐段判定，`S8-05`）；
    /// - 查询向量**只编码一次**（逐段编码会让成本随段数线性放大：NFR-10 / `R43` 的教训）。
    ///
    /// 返回**全局** `chunk_id` 的 `Scored`，已排序并截断到 `k`。
    pub fn search_segmented(
        &self,
        query: &str,
        k: usize,
        filters: &[Option<&dyn CandidateFilter>],
        route_of: impl Fn(usize) -> VectorRoute,
    ) -> Result<Vec<Scored>> {
        debug_assert_eq!(
            filters.len(),
            self.segments.len(),
            "逐段谓词必须与段列表一一对应（含顺序）"
        );
        if k == 0 || query.trim().is_empty() {
            return Ok(Vec::new());
        }
        // 查询侧编码**一次**（与单段路径同口径：`embed_query` 失败即上抛，不吞错）
        let q = NormalizedVector::new(self.embedder.embed_query(query)?);

        // `(全局 chunk_id, 平方欧氏距离)` 的并集；段之间 ID 区间不重叠（`I8-7`）⇒ 无重复项
        let mut all: Vec<(ChunkId, f32)> = Vec::new();
        for (i, seg) in self.segments.iter().enumerate() {
            let Some(vi) = seg.index else { continue };
            if vi.is_empty() {
                continue; // 该段没有向量 ⇒ 贡献空集
            }
            let filter = filters[i];
            // ⚠️ 每段都取 `k` 条（不是 `k / 段数`）：归并需要**每段自己的前 k** 才无损
            //   （证明见类型文档）；真正的全局截断在下面的 `truncate(k)`。
            let raw = match route_of(i) {
                VectorRoute::Exact => vi.search_exact_filtered(&q, k, filter)?,
                // `Ann` 与（不可达的）`None` / `Mixed` 都退化为 ANN：
                // `Mixed` 是**上报口径**，逐段的分派已由 `route_of(i)` 各自决定。
                _ => vi.search_filtered(&q, k, filter)?,
            };
            all.extend(
                raw.into_iter()
                    .map(|(local, d)| (seg.base_chunk + local, d)),
            );
        }

        let mut scored = VectorRetriever::to_scored(all);
        scored.truncate(k);
        Ok(scored)
    }

    /// 本次**参与归并**的段数（= 段列表里「有向量索引且非空」的段数）。
    ///
    /// 语义与 `Metrics.vector_segments` 的差别见 `Searcher::parts` 的注释：
    /// 那个字段报的是「**覆盖**了几个段」（含贡献为空集的段），本方法是「**实际查了**几个段」。
    pub fn participating_segments(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| Self::participates(s))
            .count()
    }
}

/// 向量检索器。持有 embedder 与向量索引的引用，不拥有它们。
pub struct VectorRetriever<'a> {
    embedder: &'a dyn Embedder,
    index: &'a dyn VectorIndex,
}

impl<'a> VectorRetriever<'a> {
    /// 构造向量检索器：embedder 做查询侧向量化，index 做近邻检索。
    pub fn new(embedder: &'a dyn Embedder, index: &'a dyn VectorIndex) -> Self {
        Self { embedder, index }
    }

    /// 该谓词下向量路将走哪条（供编排层**分派**与**记账**，D-S5-01 / D-S5-07）。
    ///
    /// **策略归后端**：`prefers_exact` 由 `VectorIndex` 实现自己回答（它最清楚
    /// 这个谓词下 ANN 会不会退化成整图遍历），编排层不复制阈值规则。
    /// `O(1)`，可在并行 `rayon::join` 之前先算好。
    ///
    /// `filter = None`（无用户过滤）恒为 [`VectorRoute::Ann`]——热路径不走精确扫描
    /// （D-S5-02）。`VectorRoute::None`（未走向量路）**不由本函数产生**：那是
    /// 编排层在 `SearchMode::Bm25` 下写入的取值。
    pub fn plan(&self, filter: Option<&dyn CandidateFilter>) -> VectorRoute {
        match filter {
            Some(f) if self.index.prefers_exact(f) => VectorRoute::Exact,
            _ => VectorRoute::Ann,
        }
    }

    /// **精确路径**：与 `Retriever::search_filtered` 同转换（`score = 1 − d²/2`）、
    /// 同排序，只有底层调用的方法不同（`search_exact_filtered` vs `search_filtered`）。
    ///
    /// 转换与排序由私有的 `VectorRetriever::to_scored` **单点实现**，两条路径共用——
    /// 否则"两条路径的输出口径一致"会变成两份代码的巧合。
    ///
    /// ⚠️ 代价不保证优于 ANN（精确扫描是 `O(N)`）⇒ **调用方必须先问 [`Self::plan`]**。
    pub fn search_exact_filtered(
        &self,
        query: &str,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<Scored>> {
        // 早退条件与 ANN 路径逐条对齐（空查询 / 空索引 / k=0），
        // 否则两条路径对同一输入会给出不同的空语义
        if k == 0 || query.trim().is_empty() || self.index.is_empty() {
            return Ok(Vec::new());
        }
        let q = NormalizedVector::new(self.embedder.embed_query(query)?);
        let raw = self.index.search_exact_filtered(&q, k, filter)?;
        Ok(Self::to_scored(raw))
    }

    /// `(chunk_id, 平方欧氏距离)` → `Scored`（相似度降序、同分按 `chunk_id` 升序）。
    ///
    /// 两条路径的**唯一**输出转换点（`score = 1 - d²/2 = cos`，单位向量）。
    fn to_scored(raw: Vec<(ChunkId, f32)>) -> Vec<Scored> {
        let mut out: Vec<Scored> = raw
            .into_iter()
            .map(|(chunk_id, d)| Scored {
                chunk_id,
                score: 1.0 - d / 2.0,
            })
            .collect();

        // 相似度降序；同分按 chunk_id 升序（确定性 NFR-06）
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        out
    }
}

impl Retriever for VectorRetriever<'_> {
    /// 向量召回（**ANN 路径**）：谓词**直接下推**到 `VectorIndex`（而非召回后再过滤）。
    ///
    /// 下推是必须的：已软删除的 chunk 其向量仍留在 HNSW 图里（hnsw_rs 无 remove API），
    /// 后置过滤会浪费 Top-K 名额、且低选择度下几乎召不回东西。
    ///
    /// ⚠️ `filter = None` 语义同 [`Retriever::search_filtered`]：不过滤，**含已软删除条目**。
    ///
    /// ⚠️ 本方法**不自己分派**精确路径：低选择度下的兜底由编排层按 [`VectorRetriever::plan`]
    /// 决定并记账（`Metrics::vector_route`）。在这里偷偷分派会让路径变成不可观测的副作用。
    fn search_filtered(
        &self,
        query: &str,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<Scored>> {
        if k == 0 || query.trim().is_empty() || self.index.is_empty() {
            return Ok(Vec::new());
        }

        let q = NormalizedVector::new(self.embedder.embed_query(query)?);
        let raw = self.index.search_filtered(&q, k, filter)?;
        Ok(Self::to_scored(raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::BruteForceIndex;

    /// 测试用假 Embedder：把文本映射到固定向量（不依赖模型）。
    struct FakeEmbedder;
    impl Embedder for FakeEmbedder {
        fn dim(&self) -> usize {
            2
        }
        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| vec![t.len() as f32, 1.0]).collect())
        }
        fn embed_query(&self, _text: &str) -> Result<Vec<f32>> {
            // 查询向量恒为 (1,0) → 最近的是 (1,1) 归一化后的向量
            Ok(vec![1.0, 0.0])
        }
    }

    #[test]
    fn topk按相似度降序() {
        let mut idx = BruteForceIndex::new();
        // 三个向量：0≈(1,0) 1≈(0,1) 2≈(1,1)/√2
        idx.add(0, NormalizedVector::new(vec![1.0, 0.0])).unwrap();
        idx.add(1, NormalizedVector::new(vec![0.0, 1.0])).unwrap();
        idx.add(2, NormalizedVector::new(vec![1.0, 1.0])).unwrap();

        let e = FakeEmbedder;
        let r = VectorRetriever::new(&e, &idx);

        // 查询向量 (1,0)：最近 0（cos=1），其次 2（cos≈0.707），最远 1（cos=0）
        let got = r.search("任意", 10).unwrap();
        let ids: Vec<u32> = got.iter().map(|s| s.chunk_id).collect();
        assert_eq!(ids, vec![0, 2, 1]);
        // 分数 = cos，递减
        assert!(got[0].score > got[1].score && got[1].score > got[2].score);
        assert!((got[0].score - 1.0).abs() < 1e-5);
        assert!(got[1].score < 0.75);
    }

    #[test]
    fn 空查询空索引() {
        let e = FakeEmbedder;
        let idx = BruteForceIndex::new();
        let r = VectorRetriever::new(&e, &idx);
        assert!(r.search("", 10).unwrap().is_empty());
        assert!(r.search("x", 10).unwrap().is_empty());
    }
}
