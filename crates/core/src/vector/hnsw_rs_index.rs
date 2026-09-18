//! `hnsw_rs` 向量索引（P4 A/B 定案：原生增量 insert 胜出，生产路径）。
//!
//! # 距离换算
//!
//! `DistDot` 返回 `1 − cos`；本实现的 `search` 输出**统一换算为平方欧氏距离**
//! `d² = 2 − 2·cos = 2·(1 − cos)`，与 `VectorIndex` trait 的约定（d²）对齐，
//! 这样 `VectorRetriever` 的 `score = 1 − d²/2 = cos` 在两种实现下都成立。

use hnsw_rs::hnsw::Hnsw;
use hnsw_rs::prelude::Distance;

use crate::error::Result;
use crate::predicate::{CandidateFilter, FilterKind};
use crate::types::ChunkId;

use super::{NormalizedVector, VectorIndex};

/// HNSW 图构建/检索参数（P4 定稿；P5 在真实语料上校准 ef_search，见 p5-design 8.6）。
const MAX_NB_CONNECTION: usize = 32; // M
const MAX_LAYER: usize = 16;
const EF_CONSTRUCTION: usize = 300;
const EF_SEARCH: usize = 200;

/// 带过滤时的过采样倍率（`ef = k * FACTOR`，纯搜索宽度，见 §5.7 路径 B）。
///
/// ⚠️ 这不是"目标收集数"——`hnsw_rs` 的 `search_filter` 在结果堆**未满**时会
/// 关闭距离剪枝（`hnsw.rs:1019`），`ef` 只是"堆满即开剪枝"的阈值。
const EF_FILTER_FACTOR: usize = 4;

/// `ef` 的硬上限，防止低选择度下延迟失控（低选择度时本来就是整图遍历）。
const EF_FILTER_MAX: usize = 256;

/// 路径 A 的候选数上限（防止重度删除场景过采样爆炸）。
const PHYSICAL_OVERSAMPLE_CAP: usize = 1024;

/// `allowed` 低于此值时**绕开 ANN、走精确扫描**（路径 C）的默认阈值
/// （V2 Step 5 / D-S5-02，**S5-04 标定定稿**，2026-09-10）。
///
/// # 为什么是"选择度阈值"而不是"ef 调参"
///
/// 带 filter 时 `hnsw_rs` **没有 fast-return**，且堆未满（`return_points.len() < ef`）
/// 时距离剪枝全程关闭（`hnsw.rs:983-992` / `:1019`）⇒ `allowed < ef` 就是**整图遍历**。
/// 默认 `k=10 ⇒ candidate_k=30 ⇒ ef=120`，而 sel-1% 档 `allowed≈1000`、
/// sel-0.1% 档 `allowed≈100`——两者都远小于整图规模，路径 B 在那里每个点都要算一次
/// 512 维距离；路径 C 每个点只做一次位图判定（常数差 5~20×）。
///
/// # 取值来自 10 万级 A/B 标定（S5-04）
///
/// 同一快照 + 同一 query 集（100 query × 10 次），逐档位跑
/// `--brute-fallback off`（纯 ANN）与 `--brute-fallback 1000000`（强制精确），
/// 比 `mean_vector_ms`：
///
/// | 档位 | `allowed` | ANN(ms) | 精确(ms) | 加速 |
/// | --- | --- | --- | --- | --- |
/// | sel-0.1% | 100 | 95.6 | 2.84 | **33.7×** |
/// | sel-1% | 1000 | 30.1 | 3.71 | **8.1×** |
/// | tier-5% | 5043 | 6.93 | 4.64 | **1.49×** |
/// | tenant-10% | 9946 | 4.62 | 6.64 | **0.70×（反转）** |
///
/// （上表用**未归一**的原始值——两轮非交错执行，控制组 `none` 档同路径在第二轮
/// 整体偏慢 ×1.382（`mean_vector_ms` 口径），归一后 sel-0.1% / sel-1% / tier-5% 分别为
/// 46.6× / 11.2× / 2.06×，反转结论不变。）交叉点**插值** ≈ 9700——按**归一后**加速
/// （2.06× / 0.96×）线性插值（跨轮幅值比较必须按控制组归一，见 `eval-report.md` §8.9）；
/// 若按未归一原始值插值则为 ≈7.2K~8.1K，只说明插值对两轮漂移敏感。
///
/// 取 **8192**（= 2¹³）：低于归一插值交叉点（~15% 余量）、更低于首个实测反转档位
/// `9946`——R31 指出路径 C 的 `O(N)` 访问项随规模线性增长（而高选择度下路径 B 由 `ef` 封顶），
/// 交叉点会**随规模下移** ⇒ **宁低勿高**。
///
/// ⚠️ **这不是正确性边界**：任何阈值下结果都正确（精确路径只会更准），阈值只决定
/// 「哪条路径更快」；`None` = **关闭**（供 A/B 回归对照，见 `with_brute_fallback`）。
pub const BRUTE_FALLBACK_MAX_ALLOWED: usize = 8192;

/// 自定义点积距离：`1 − cos`（要求向量已 L2 归一化）。
///
/// **不用库里的 `DistDot`**：它在 aarch64 的标量实现里有 `assert!(dot <= 1.)`，
/// 自匹配等边界场景会因浮点舍入（dot = 1.0000002）直接 panic。
/// 我们把点积 clamp 到 [−1, 1] 再相减，语义相同但数值安全。
///
/// ⚠️ **类型短名被烧进图文件**（C7）：`distname = type_name::<D>()` 写进
/// `.hnsw.graph`，`load_hnsw` 按短名比对。**改名 = 旧图全部失效**（走降级
/// 重建，不丢功能）；语义变更必须升 manifest 的 `dist_id`（R20）。
#[derive(Default, Copy, Clone)]
pub(crate) struct DistDotClamped;

impl Distance<f32> for DistDotClamped {
    fn eval(&self, va: &[f32], vb: &[f32]) -> f32 {
        let dot: f32 = va.iter().zip(vb.iter()).map(|(a, b)| a * b).sum();
        1.0 - dot.clamp(-1.0, 1.0)
    }
}

/// 并行建图的最小批量（D-S2-05）：低于此值回落串行——任务拆分开销大于收益，
/// 且小批量下「插入顺序不确定」换不来可观加速。
const PARALLEL_INSERT_THRESHOLD: usize = 1000;

/// hnsw_rs 实现的向量索引：支持原生增量插入。
pub struct HnswRsIndex {
    hnsw: Hnsw<'static, f32, DistDotClamped>,
    /// 查询时动态候选列表宽度（P5 前硬编码 200 从未校准；8.6 最小校准注入口）
    ef_search: usize,
    /// 批量插入是否走 `parallel_insert_slice`（D-S2-05：默认关，保确定性）
    parallel_build: bool,
    /// 实际走 `parallel_insert_slice` 的次数（**可观测性**）。
    ///
    /// ⚠️ 光把 `parallel_build` 打开**不代表走了并行**：`add_batch` 每次最多收到
    /// `batch_size` 条（默认 64），而并行阈值是 1000，两者耦合。没有这个计数，
    /// 「并行 vs 串行质量等价」的测试会退化成「串行 vs 串行」还全绿
    /// （评审 #13 发现 1）。
    parallel_inserts: usize,
    /// 精确兜底阈值（V2 Step 5 / D-S5-02）：`allowed ≤ 阈值` 时走路径 C。
    ///
    /// `None` = **关闭**兜底（回归对照 / bench A/B 用 `--brute-fallback off`）。
    brute_fallback: Option<usize>,
}

impl HnswRsIndex {
    /// `max_capacity` 为容量提示（分配优化，非硬上限）。
    pub fn with_capacity(max_capacity: usize) -> Self {
        let hnsw = Hnsw::new(
            MAX_NB_CONNECTION,
            max_capacity,
            MAX_LAYER,
            EF_CONSTRUCTION,
            DistDotClamped,
        );
        Self {
            hnsw,
            ef_search: EF_SEARCH,
            parallel_build: false,
            parallel_inserts: 0,
            brute_fallback: Some(BRUTE_FALLBACK_MAX_ALLOWED),
        }
    }

    /// 内核默认 ef_search（P0-5：图加载后未显式指定时用它回填）。
    pub fn default_ef_search() -> usize {
        EF_SEARCH
    }

    /// 覆盖 ef_search（8.6 向量路诊断：ef_search ∈ {100, 200, 400} 最小校准）。
    pub fn with_ef_search(mut self, ef_search: usize) -> Self {
        self.ef_search = ef_search;
        self
    }

    /// 从持久化加载的 `Hnsw` 构造（V2 Step 2 / P0-5）。
    ///
    /// 字段对兄弟模块（`vector/persist.rs`）不可见，字面量构造编译不过——
    /// 本构造器是**唯一**通道。`ef_search` 必须由调用方带入：全 crate 无
    /// `set_ef*`，本字段是 ef 的唯一载体。
    ///
    /// # 为什么 `brute_fallback` 不加形参（V2 Step 5 / 附录 A 的第二个选项）
    ///
    /// 它**不是图的属性**，也**不是加载路径的配置**——「低选择度走不走精确扫描」
    /// 是**读端策略**。加形参会把 `VectorGraphPersist::load_graph` 的签名一起改掉
    /// （公开 trait，波及 `load_graph_checked` 与全部调用点），而唯一的非默认需求
    /// 来自 bench 的 A/B 对照——调用方拿到具体的 `HnswRsIndex` 后调
    /// [`Self::with_brute_fallback`] 即可（`load_graph_checked` 返回的就是具体类型）。
    /// 故这里只写**产品默认值**，把"可关"留在构建器上。
    pub(crate) fn from_loaded(
        hnsw: Hnsw<'static, f32, DistDotClamped>,
        ef_search: usize,
        parallel_build: bool,
    ) -> Self {
        Self {
            hnsw,
            ef_search,
            parallel_build,
            parallel_inserts: 0,
            brute_fallback: Some(BRUTE_FALLBACK_MAX_ALLOWED),
        }
    }

    /// 覆盖精确兜底阈值（V2 Step 5 / D-S5-02）。
    ///
    /// - `Some(n)` ⇒ `allowed ≤ n` 的 `FilterKind::Filtered` 查询走精确扫描；
    /// - `None` ⇒ **关闭兜底**，行为逐位回到 Step 5 之前（`--brute-fallback off`
    ///   的 A/B 对照与 `S5-T7` 的热路径零回归护栏都靠它）。
    pub fn with_brute_fallback(mut self, max_allowed: Option<usize>) -> Self {
        self.brute_fallback = max_allowed;
        self
    }

    /// 当前精确兜底阈值（`None` = 已关闭；供 bench 打印配置与测试断言）。
    pub fn brute_fallback(&self) -> Option<usize> {
        self.brute_fallback
    }

    /// 打开并行建图（D-S2-05，**默认关**）。
    ///
    /// ⚠️ 打开后**建图拓扑不可复现**：`parallel_insert_slice` 走 rayon，插入顺序
    /// 不确定（C8）。图一旦落盘即被冻结，故 NFR-06「同快照两次加载逐位一致」
    /// 不受影响；但「同一批向量两次建库结果一致」不再成立。
    pub fn with_parallel_build(mut self, parallel_build: bool) -> Self {
        self.parallel_build = parallel_build;
        self
    }

    /// 实际走 `parallel_insert_slice` 的次数（可观测性：验证并行真的生效）。
    pub fn parallel_inserts(&self) -> usize {
        self.parallel_inserts
    }

    /// 内部 `Hnsw` 的只读访问（`vector/persist.rs` dump 统计用）。
    pub(crate) fn hnsw(&self) -> &Hnsw<'static, f32, DistDotClamped> {
        &self.hnsw
    }
}

impl Default for HnswRsIndex {
    fn default() -> Self {
        Self::with_capacity(1024)
    }
}

impl VectorIndex for HnswRsIndex {
    fn add(&mut self, id: ChunkId, vec: NormalizedVector) -> Result<()> {
        // hnsw_rs 原生增量插入（insert 只需 &self）
        self.hnsw.insert((vec.as_slice(), id as usize));
        Ok(())
    }

    /// 批量插入（D-S2-05 / S2-11）：达阈值且**并行建图开关打开**时走
    /// `parallel_insert_slice`，否则回落串行（默认）。
    ///
    /// # 为什么默认串行
    ///
    /// `parallel_insert` 走 `rayon::par_iter` ⇒ 插入顺序不确定 ⇒ **拓扑不可复现**
    /// （C8）。P5/P6 的全部基线都在串行建图下测出，Step 6 精排的对比实验需要
    /// 可比基线 ⇒ 先出实测（S2-11 的 T13）再决定是否翻默认值。
    ///
    /// # 为什么用 `parallel_insert_slice`
    ///
    /// 签名是 `&Vec<(&[T], usize)>`，`NormalizedVector::as_slice()` 可直接组装，
    /// 不必给 `NormalizedVector` 加 `as_vec()`（其内部字段对兄弟模块不可见）。
    fn add_batch(&mut self, items: &[(ChunkId, NormalizedVector)]) -> Result<()> {
        if !self.parallel_build || items.len() < PARALLEL_INSERT_THRESHOLD {
            // 小批量或开关关闭：串行（保确定性，且并行的任务拆分开销不划算）
            for (id, v) in items {
                self.hnsw.insert((v.as_slice(), *id as usize));
            }
            return Ok(());
        }
        let refs: Vec<(&[f32], usize)> = items
            .iter()
            .map(|(id, v)| (v.as_slice(), *id as usize))
            .collect();
        self.hnsw.parallel_insert_slice(&refs);
        self.parallel_inserts += 1;
        Ok(())
    }

    /// 向量近邻检索（过滤下推，见设计文档 §5.7 的两条路径）。
    ///
    /// # 路径 A：`None` 或 `FilterKind::Alive`（热路径）
    ///
    /// 走**普通 `search()`**，保留 fast-return（`hnsw.rs:983-984`），成本结构与
    /// 无过滤检索完全一致——这是所有无过滤查询的必经之路，不能为了过滤而拖慢它。
    /// 已软删除的向量无法从图中摘除，因此按存活比例**过采样 `knbn`**，返回后再过滤。
    ///
    /// # 路径 B：`FilterKind::Filtered`
    ///
    /// 走 `search_filter`。代价是 **fast-return 被禁用**（`hnsw.rs:985-992`：堆满后
    /// 只 `retain` 不 return），且堆未满时距离剪枝全程关闭 ⇒ 低选择度下退化为整图遍历。
    /// 这是 `hnsw_rs` 的结构性行为，调参治不了；我们用 `EF_FILTER_MAX` 限制最坏延迟，
    /// 召回缺口由 `Metrics::vector_shortfall` 暴露。
    fn search_filtered(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        if k == 0 {
            return Ok(Vec::new());
        }

        // 路径分派用 `match` 而非「bool 判据 + Option 形参」：路径 B 只在**确实存在
        // 用户过滤谓词**时被选中，写成 match 后「走了下推却没谓词」在类型层面即不可达，
        // 不必再留一条永远进不去的 `None` 分支（评审 nit-9；注释会过期，类型不会）。
        let mut out: Vec<(ChunkId, f32)> = match filter {
            Some(f) if f.kind() == FilterKind::Filtered => self.search_path_b(query, k, f),
            other => self.search_path_a(query, k, other),
        };

        // 统一稳定排序（距离升序 → chunk_id 升序），保 NFR-06 确定性
        out.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        Ok(out)
    }

    fn len(&self) -> usize {
        self.hnsw.get_nb_point()
    }

    /// **路径 C：精确扫描**（V2 Step 5 / D-S5-03 方案 A）。
    ///
    /// 逐点遍历**向量存储本身**，对通过谓词的点算距离，取最近 k 条。
    /// 代价 `O(N)` 次谓词判定 + `O(allowed)` 次距离——与路径 B 同为 `O(N)` 遍历，
    /// 差别在每个点做什么：路径 B 每点一次 512 维距离（≈100ns），
    /// 本路径每点一次位图判定（≈5~20ns）。
    ///
    /// # ⚠️ 全量遍历必须**逐层**——**不可**用 `&PointIndexation` 的 `IntoIterator`（R34 递归读）
    ///
    /// 生产写法（**层号上界钉死**，与下面循环体一致）：
    ///
    /// ```text
    /// let pi = self.hnsw.get_point_indexation();
    /// for layer in 0..=pi.get_max_level_observed() as usize {
    ///     for point in pi.get_layer_iterator(layer) { /* … */ }
    /// }
    /// ```
    ///
    /// ① **为什么不能用 `IntoIterator`**：`IterPoint::new` 构造即取
    /// `points_by_layer.read()`（`hnsw.rs:633`）并**全程持有**，而 `IterPoint::next` 在
    /// **层切换**时**再取一次同一把锁**（`hnsw.rs:661`；相邻的 `:660` 取的是另一把
    /// `entry_point` 锁，**不构成**递归）⇒ 同一把 `std::sync::RwLock` 被**递归读**。
    /// `std` 不保证递归读可重入（其 futex 实现要求 `!has_writers_waiting`，**写者优先**）
    /// ⇒ 并发检索之间互不阻塞，但**若恰在层切换那一刻有写者在等待，第二次读可能永久阻塞
    /// ⇒ 挂死**（架构 §14.3 **R34**）。逐层写法每层一个独立 `IterPointLayer`
    /// （`:701` 只取一次锁，`:715-723` 的 `next` **只索引** `pi_guard[self.layer]`、
    /// **不再取锁**）⇒ **层间 guard 不重叠 ⇒ 无递归读**。
    /// 并发判据见 `tests/step8_rw_concurrency.rs`（std-only 复刻体；真实 `Hnsw` 上的
    /// 读写并发在安全 Rust 下不可达 —— `insert` 与 `add` 都要求 `&mut self`）。
    ///
    /// ② **为什么层号必须从 0 起、到 `get_max_level_observed()` 止**：
    /// `generate_new_point` 只把新点推入**它自己那一层**（`hnsw.rs:505` 构造 `p_id`，
    /// `:511` 是全文件唯一一处 push，**无回填低层**）⇒ **每点恰在一层** ⇒ 逐层**并集**
    /// = 全部点、每点恰一次。而单层 `get_layer_iterator(0)` **不是**全量：
    /// `level = floor(-ln u · scale)`、`scale = 1/ln(M)` ⇒ `P(level ≥ 1) = 1/M`，
    /// 本项目 M=32 ⇒ 约 3.1% 的点**不在 layer 0**（实测 N=5000 漏 3.74%）。
    /// 用它做精确扫描会**静默漏掉**这 3%——不是变慢、不是少召回，是**答错**
    /// （拿被漏点自己的向量去查，它会消失）。护栏见测试
    /// `全量遍历基数等于点数且零重复`（含**层号范围**断言）与
    /// `精确扫描覆盖高层点_定向用例`（定向：`level ≥ 1` 的点自查询必排第一）。
    ///
    /// ③ **不可用 `get_max_level()`**（`hnsw.rs:809-812`）：它返回**构造期授权的**
    /// `max_layer`，**大于等于**实际观测层 ⇒ 会白跑空层；两者仅在「已建满」时相等。
    ///
    /// ④ **空图边界**：`get_max_level_observed()` 在 `entry_point == None` 时返回 **0**
    /// （`hnsw.rs:469-475`），而 `points_by_layer` 由 `PointIndexation::new` 按
    /// `max_layer` 预分配（`:448-456`）⇒ `get_layer_iterator(0)` 落在合法下标上、
    /// 返回空迭代器 ⇒ **不 panic**。库自身的 `debug_dump`（`:485-490`）用的就是同一个
    /// `0..=max_level_observed` 模式，可作先例。
    ///
    /// ⑤ **成本（顺带收益）**：把 1 次「长持锁」换成 `L+1` 次「短持锁」
    /// （`L` = 最大层；M=32 / N=12K 下期望 `L ≈ log₁/₃₂(12000) ≈ 2.4`）⇒ 总持锁量同量级，
    /// 但**单次持锁时长从 `O(N)` 降到 `O(该层点数)`** ⇒ 写端的**单次**阻塞窗口显著变短
    /// （R34 的原文只说了「消除死锁」，没说这条）。
    ///
    ///
    /// # 与 `BruteForceIndex` 的逐位一致性（I4）
    ///
    /// 距离走 [`NormalizedVector::distance_to_slice`]（与 `distance_sq` 同一份求和
    /// 实现），排序比较子与 `brute.rs` 同为 `total_cmp` ⇒ 两侧**同源**，
    /// "逐位一致"是结构而非巧合。`(距离, chunk_id)` 在全量互异 ID 上是全序
    /// ⇒ 结果与遍历顺序无关。
    fn search_exact_filtered(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let mut out: Vec<(ChunkId, f32)> = Vec::new();
        // ⚠️ 逐层遍历——**不可**改回 `for point in self.hnsw.get_point_indexation()`：
        //    那条路径的 `IterPoint::next` 会在层切换时**递归读**同一把 `points_by_layer`
        //    （`hnsw.rs:633` 取、`:661` 再取）⇒ 有写者等待时**永久阻塞**（架构 §14.3 R34）。
        //    ⚠️ 上界必须是 `get_max_level_observed()`（**不是** `get_max_level()`），
        //    且必须从 **0** 起（只写 0 会漏掉约 `1/M` 的点）。详见本函数的 rustdoc。
        let pi = self.hnsw.get_point_indexation();
        for layer in 0..=pi.get_max_level_observed() as usize {
            for point in pi.get_layer_iterator(layer) {
                let id = point.get_origin_id() as ChunkId;
                if filter.is_none_or(|f| f.contains(id)) {
                    let d = query.distance_to_slice(point.get_v());
                    // 精确路径把"依赖 embedder 输出有限"从**声明**变成**断言**：
                    // `NormalizedVector::new`（`point.rs`）的 `norm > 0.0` 在 NaN 下为
                    // false（IEEE-754：NaN 的一切比较皆 false）⇒ NaN 会被**静默存下**，
                    // 不 panic / 不报错；全 crate 无 `is_finite` / `is_nan`。
                    debug_assert!(
                        d.is_finite(),
                        "embedder 输出含 NaN/Inf（精确路径契约外输入）：chunk {id}"
                    );
                    out.push((id, d));
                }
            }
        }
        out.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        out.truncate(k);
        Ok(out)
    }

    /// 低选择度用户过滤 ⇒ 声明"该走精确路径"（策略归后端，分派与记账归编排层）。
    fn prefers_exact(&self, filter: &dyn CandidateFilter) -> bool {
        // 只看 `Filtered`：热路径（`None` / `Alive`）必须保住 fast-return，
        // 而 R15 是 Step 1 明确的设计不变量，本 Step 一行不碰（D-S5-02）。
        filter.kind() == FilterKind::Filtered
            && self
                .brute_fallback
                .is_some_and(|max| filter.allowed_count() <= max)
    }

    /// P0-4：门面层从 `Box<dyn VectorIndex>` 触达图持久化能力的唯一通道。
    /// 覆盖为 `Some(self)`；`BruteForceIndex` 用默认 `None`——「Brute 无图」
    /// 由此成为类型事实而非运行时 if。
    fn as_graph_persist(&self) -> Option<&dyn super::VectorGraphPersist> {
        Some(self)
    }
}

/// 两条检索路径的实现细节（放在 inherent impl，避免污染 trait 契约）。
impl HnswRsIndex {
    /// 路径 A：普通 `search()` + 按存活比例过采样 + 后置过滤（保 fast-return）。
    fn search_path_a(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: Option<&dyn CandidateFilter>,
    ) -> Vec<(ChunkId, f32)> {
        let alive_ratio = match filter {
            None => 1.0,
            Some(f) => f.allowed_count() as f64 / self.len().max(1) as f64,
        };
        // 期望需要 k/ratio 个候选才能凑出 k 个存活项，再加 k 的余量吸收方差
        let knbn = ((k as f64 / alive_ratio.max(0.01)).ceil() as usize + k)
            .min(self.len())
            .min(PHYSICAL_OVERSAMPLE_CAP)
            .max(k.min(self.len()));

        let neighbours = self.hnsw.search(query.as_slice(), knbn, self.ef_search);
        let mut out: Vec<(ChunkId, f32)> = neighbours
            .into_iter()
            .map(|n| (n.d_id as ChunkId, 2.0 * n.distance))
            .collect();

        if let Some(f) = filter {
            out.retain(|(id, _)| f.contains(*id));
        }
        out.truncate(k);
        out
    }

    /// 路径 B：`search_filter` + `FilterT` 适配器（真下推，但无 fast-return）。
    ///
    /// 谓词是**非可选**的：本路径只在确实存在用户过滤时被分派（见调用点的 `match`），
    /// 因此不存在「走了下推却没有谓词」的状态需要兜底。
    fn search_path_b(
        &self,
        query: &NormalizedVector,
        k: usize,
        filter: &dyn CandidateFilter,
    ) -> Vec<(ChunkId, f32)> {
        // knbn 是输出条数旋钮，ef 只是搜索宽度（§3.3 结论 4）
        let ef = (k * EF_FILTER_FACTOR).max(k).min(EF_FILTER_MAX);
        // `FilterT` 对 `Fn(&usize) -> bool` 有 blanket impl，闭包即可适配
        let adapt = |id: &usize| filter.contains(*id as ChunkId);
        self.hnsw
            .search_filter(query.as_slice(), k, ef, Some(&adapt))
            .into_iter()
            .map(|n| (n.d_id as ChunkId, 2.0 * n.distance))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;
    use crate::vector::BruteForceIndex;

    fn random_vec(seed: &mut u64) -> Vec<f32> {
        let mut next = || {
            *seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((*seed >> 33) as u32 as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        (0..512).map(|_| next()).collect()
    }

    #[test]
    fn 增量插入立即可检索() {
        let mut idx = HnswRsIndex::with_capacity(100);
        let mut seed = 7u64;

        // 先插 50 条
        let entries: Vec<(u32, NormalizedVector)> = (0..50)
            .map(|i| (i as u32, NormalizedVector::new(random_vec(&mut seed))))
            .collect();
        for (id, v) in &entries {
            idx.add(*id, v.clone()).unwrap();
        }
        assert_eq!(idx.len(), 50);

        // 自匹配应为最近邻（距离 ≈ 0）
        let q = entries[0].1.clone();
        let got = idx.search(&q, 5).unwrap();
        assert!(!got.is_empty());
        assert!(got[0].1 < 1e-3, "最近邻应是自身，实际 d²={}", got[0].1);
    }

    /// 与暴力 Top-10 重合率（1 万条）。debug 下 hnsw_rs 构建慢，标 ignore 用 release 跑。
    #[test]
    #[ignore]
    fn 与暴力万条重合率达标() {
        let n = 10_000usize;
        let mut seed = 7u64;
        let entries: Vec<(u32, NormalizedVector)> = (0..n)
            .map(|i| (i as u32, NormalizedVector::new(random_vec(&mut seed))))
            .collect();

        // hnsw_rs：逐条增量插入（这正是它的卖点）
        let mut h = HnswRsIndex::with_capacity(n);
        for (id, v) in &entries {
            h.add(*id, v.clone()).unwrap();
        }
        let brute = BruteForceIndex::from_entries(entries);

        let mut total = 0.0;
        let mut queries = 0;
        for _ in 0..10 {
            let q = NormalizedVector::new(random_vec(&mut seed));
            let hv: Vec<u32> = h.search(&q, 10).unwrap().iter().map(|(i, _)| *i).collect();
            let bv: Vec<u32> = brute
                .search(&q, 10)
                .unwrap()
                .iter()
                .map(|(i, _)| *i)
                .collect();
            let hs: std::collections::HashSet<u32> = hv.iter().copied().collect();
            total += bv.iter().filter(|x| hs.contains(x)).count() as f32 / 10.0;
            queries += 1;
        }
        let avg = total / queries as f32;
        println!("hnsw_rs vs 暴力 Top-10 平均重合率 = {avg:.3}");
        assert!(avg >= 0.95, "重合率过低: {avg}");
    }

    /// 只允许偶数 chunk_id 通过的谓词（路径 B 测试用）。
    struct EvenChunks {
        count: usize,
    }

    impl CandidateFilter for EvenChunks {
        fn contains(&self, id: ChunkId) -> bool {
            id.is_multiple_of(2)
        }
        fn allowed_count(&self) -> usize {
            self.count
        }
        fn kind(&self) -> FilterKind {
            FilterKind::Filtered
        }
    }

    /// 建一个 `n` 条的图，返回 (index, entries)。
    fn build_index(n: usize, seed: &mut u64) -> (HnswRsIndex, Vec<(u32, NormalizedVector)>) {
        let mut h = HnswRsIndex::with_capacity(n);
        let entries: Vec<(u32, NormalizedVector)> = (0..n)
            .map(|i| (i as u32, NormalizedVector::new(random_vec(seed))))
            .collect();
        for (id, v) in &entries {
            h.add(*id, v.clone()).unwrap();
        }
        (h, entries)
    }

    /// **路径 A**：`AliveOnly` 必须挡掉已软删除的候选（Q-C1 的修复点）。
    #[test]
    fn 路径A存活过滤挡掉幽灵候选() {
        use crate::bitmap::Bitmap;
        use crate::predicate::AliveOnly;

        let mut seed = 11u64;
        let (idx, entries) = build_index(200, &mut seed);

        // 只留偶数 id 存活
        let mut alive = Bitmap::new();
        for i in 0..200u32 {
            if i % 2 == 0 {
                alive.set(i);
            }
        }
        let predicate = AliveOnly::new(&alive);

        let q = entries[7].1.clone();
        let got = idx.search_filtered(&q, 10, Some(&predicate)).unwrap();

        assert!(!got.is_empty(), "过采样后应能凑出足够的存活候选");
        assert_eq!(got.len(), 10, "存活比例 50% 时应能凑满 k=10");
        for (id, _) in &got {
            assert!(
                predicate.contains(*id),
                "路径 A 返回了已软删除的 chunk {id}"
            );
            assert_eq!(id % 2, 0);
        }
        // 自匹配：chunk 7 是死的，最近的存活候选不应是它
        assert!(!got.iter().any(|(id, _)| *id == 7));
    }

    /// **路径 B**：`search_filter` 的返回值**必须全部通过谓词**（soundness）。
    #[test]
    fn 路径B结果全部通过谓词() {
        let mut seed = 23u64;
        let (idx, entries) = build_index(200, &mut seed);
        let predicate = EvenChunks { count: 100 };

        for probe in [3usize, 17, 99, 150] {
            let q = entries[probe].1.clone();
            let got = idx.search_filtered(&q, 5, Some(&predicate)).unwrap();
            assert!(!got.is_empty(), "probe={probe} 应至少召回一条");
            for (id, _) in &got {
                assert!(
                    predicate.contains(*id),
                    "路径 B 返回了未通过谓词的 chunk {id}（probe={probe}）"
                );
            }
            // 距离升序（统一稳定排序的契约）
            for w in got.windows(2) {
                assert!(w[0].1 <= w[1].1 || (w[0].1 - w[1].1).abs() < 1e-6);
            }
        }
    }

    /// 路径 A 与路径 B 的选择：谓词种类决定走哪条（行为等价性由 T12 量化）。
    #[test]
    fn 无过滤时走路径A且与现状一致() {
        let mut seed = 31u64;
        let (idx, entries) = build_index(200, &mut seed);
        let q = entries[5].1.clone();

        let via_trait = idx.search_filtered(&q, 10, None).unwrap();
        let via_search = idx.search(&q, 10).unwrap();
        assert_eq!(via_trait, via_search, "None 谓词应等价于无过滤检索");
        assert_eq!(via_trait[0].0, 5, "自匹配应是最近邻（d²≈0）");
    }

    /// **T12**：「选择度 × 延迟 × 召回」三元数据（V2.1 是否引入 prefilter 结构的依据）。
    ///
    /// ⚠️ 必须 `--release` 运行（debug 下 hnsw_rs 建图极慢）。
    /// 本测试**不做硬断言**（低选择度 + ANN 的延迟与召回无法两全，是 hnsw_rs 的
    /// 结构性行为），只打印三元曲线供人工判读。
    #[test]
    #[ignore]
    fn T12_选择度与延迟与召回三元数据() {
        let n = 5_000usize;
        let mut seed = 7u64;
        let (idx, entries) = build_index(n, &mut seed);
        let brute = BruteForceIndex::from_entries(entries.clone());

        // 选择度档位：100% / 10% / 1% / 0.1%
        for (name, modulus) in [("100%", 1u32), ("10%", 10), ("1%", 100), ("0.1%", 1000)] {
            let mut alive = crate::bitmap::Bitmap::new();
            for i in 0..n as u32 {
                if i % modulus == 0 {
                    alive.set(i);
                }
            }
            let allowed = alive.count_ones();
            let predicate = crate::predicate::AliveOnly::new(&alive);
            let predicate_b = EvenChunks { count: allowed };

            let mut lat_us = Vec::new();
            let mut recall = Vec::new();
            let mut shortfall = Vec::new();

            for probe in (0..20).map(|i| i * 37 % n) {
                let q = entries[probe].1.clone();

                let t0 = std::time::Instant::now();
                let got = idx.search_filtered(&q, 10, Some(&predicate)).unwrap();
                lat_us.push(t0.elapsed().as_micros());
                // 缺口口径与 `Metrics::vector_shortfall` 一致（按 allowed 归一），
                // 否则 bench 打印值与运行时指标对不上
                shortfall.push(10usize.min(allowed).saturating_sub(got.len()));

                // 召回基线：暴力 + 同谓词（精确 oracle）
                let oracle: Vec<u32> = brute
                    .search_filtered(&q, 10, Some(&predicate_b))
                    .unwrap_or_default()
                    .iter()
                    .map(|(i, _)| *i)
                    .collect();
                let hit = std::collections::HashSet::<u32>::from_iter(got.iter().map(|(i, _)| *i));
                let inter = oracle.iter().filter(|x| hit.contains(x)).count();
                recall.push(inter as f32 / oracle.len().max(1) as f32);
            }

            let avg_lat = lat_us.iter().sum::<u128>() as f32 / lat_us.len() as f32;
            let p99_lat = {
                let mut s = lat_us.clone();
                s.sort_unstable();
                s[((s.len() as f32 * 0.99) as usize).min(s.len() - 1)]
            };
            let avg_recall = recall.iter().sum::<f32>() / recall.len() as f32;
            let avg_short = shortfall.iter().sum::<usize>() as f32 / shortfall.len() as f32;

            println!(
                "选择度 {name:>5} (allowed={allowed:>5}) | 平均 {avg_lat:>8.1}µs | \
                 P99 {p99_lat:>8}µs | 召回 {avg_recall:.3} | 平均缺口 {avg_short:.2}"
            );
        }
    }

    // ========================================================================
    // V2 Step 5 / S5-02：精确扫描（路径 C）的正确性护栏
    // ========================================================================

    /// **S5-T2 ①**：全量遍历的**基数断言**——每点恰好 yield 一次、零重复。
    ///
    /// 这条是 v0.1 教训的直接固化：当时把 `get_layer_iterator(0)` 当全量遍历写进
    /// 设计，而它只迭代 layer 0（`hnsw.rs:715-723` 只索引 `pi_guard[self.layer]`），
    /// 因 `generate_new_point` **无回填低层**（`:500-511`）而漏掉约 `1/M` 的点。
    /// 断言"全量 API 的基数 == `get_nb_point()`"能在**有人把遍历写回 layer 0 时立刻红**。
    #[test]
    fn 全量遍历基数等于点数且零重复() {
        let mut seed = 99u64;
        let (idx, entries) = build_index(300, &mut seed);
        let pi = idx.hnsw.get_point_indexation();

        let ids: Vec<ChunkId> = pi
            .into_iter()
            .map(|p| p.get_origin_id() as ChunkId)
            .collect();
        assert_eq!(
            ids.len(),
            idx.hnsw.get_nb_point(),
            "全量遍历必须覆盖每一个点（每点恰一次）"
        );

        let mut uniq = ids.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), ids.len(), "全量遍历出现重复 yield");

        // 与真实 ID 全集相等：既不少（漏点）也不多（幽灵）
        let mut expect: Vec<ChunkId> = entries.iter().map(|(id, _)| *id).collect();
        expect.sort_unstable();
        assert_eq!(uniq, expect, "遍历得到的 ID 集与入库集不相等");
    }

    /// **S5-T2 ②**：**定向用例**——取一个 `level ≥ 1` 的点，用**它自己的向量**查询，
    /// 断言它排第一（距离 0）。
    ///
    /// 为什么必须是定向用例而不能只靠随机 Top-K：`LayerGenerator` 用
    /// `StdRng::from_os_rng()`（`hnsw.rs:328`），**哪 3% 被漏掉每次建图都不同**
    /// （实测漏点率 2.66%~3.74%）⇒ 随机查询恰不含漏点时测试会**时好时坏**。
    /// 本用例同时断言 victim **不在 layer 0**，否则它会退化成"layer 0 也能过"、
    /// 失去鉴别力。
    #[test]
    fn 精确扫描覆盖高层点_定向用例() {
        let mut seed = 1234u64;
        let (idx, _entries) = build_index(512, &mut seed);
        let pi = idx.hnsw.get_point_indexation();

        // M = 32 ⇒ P(level ≥ 1) = 1/32；N = 512 时 P(一个都没有) ≈ 1e-7
        let victim = pi
            .get_layer_iterator(1)
            .next()
            .unwrap_or_else(|| panic!("M=32 / N=512 下应存在 level ≥ 1 的点（P(不存在)≈1e-7）"));
        let victim_id = victim.get_origin_id() as ChunkId;
        let q = NormalizedVector::new(victim.get_v().to_vec());

        assert!(
            pi.get_layer_iterator(0)
                .all(|p| p.get_origin_id() as ChunkId != victim_id),
            "定向用例失效：victim {victim_id} 同时也在 layer 0"
        );

        // 用它自己的向量查：精确路径必须把它排第一且距离为 0
        let got = idx.search_exact_filtered(&q, 3, None).unwrap();
        assert_eq!(
            got[0].0, victim_id,
            "精确路径漏掉了 level ≥ 1 的点 {victim_id}（疑似用了 get_layer_iterator(0)）"
        );
        assert!(got[0].1.abs() < 1e-6, "自匹配距离应为 0，实际 {}", got[0].1);

        // 对照：只扫 layer 0 确实找不到它 —— 把 v0.1 的误判钉成回归
        assert!(
            !pi.get_layer_iterator(0)
                .any(|p| p.get_origin_id() as ChunkId == victim_id),
            "本用例的 victim 在 layer 0 里，无法证明全量遍历的必要性"
        );
    }

    /// **S5-T3 / I4**：精确路径与 `BruteForceIndex` 的 Top-K **逐位一致**
    /// （`(chunk_id, distance)` 序列）。
    ///
    /// 多 query × 多谓词（`None` / `EvenChunks`）覆盖两条不同的谓词路径。
    /// 一致性是**结构性**的：`distance_sq` 转发 `distance_to_slice`（单一求和实现）
    /// + 两侧比较子同为 `total_cmp` + `(距离, chunk_id)` 全序（结果与遍历顺序无关）。
    #[test]
    fn 精确扫描与暴力逐位一致() {
        let mut seed = 77u64;
        let (idx, entries) = build_index(300, &mut seed);
        let brute = BruteForceIndex::from_entries(entries.clone());
        let even = EvenChunks { count: 150 };

        for probe in [0usize, 41, 137, 299] {
            let q = entries[probe].1.clone();
            for (label, filter) in [
                ("None", None),
                ("EvenChunks", Some(&even as &dyn CandidateFilter)),
            ] {
                for k in [1usize, 5, 10, 500] {
                    let exact = idx.search_exact_filtered(&q, k, filter).unwrap();
                    let oracle = brute.search_filtered(&q, k, filter).unwrap();
                    assert_eq!(
                        exact, oracle,
                        "probe={probe} k={k} filter={label}：精确路径与 Brute 不逐位一致"
                    );
                }
            }
        }

        // **I5 / S5-T6**：同一冻结图 + 同一 query 下，精确扫描**连续 100 次**逐位一致
        // （NFR-06 确定性）。这里可以硬断言：图已冻结，遍历与排序都是确定性的，
        // 不受建图期 `StdRng::from_os_rng()` 影响的只有**跨建图**的拓扑，
        // 而本用例全程用同一张图。
        let q = entries[137].1.clone();
        let first = idx.search_exact_filtered(&q, 10, Some(&even)).unwrap();
        for round in 0..100 {
            assert_eq!(
                idx.search_exact_filtered(&q, 10, Some(&even)).unwrap(),
                first,
                "第 {round} 次精确扫描与首次不一致"
            );
        }
    }

    /// **S5-T2（hnsw 侧完整性）**：返回条数 `== min(k, 命中数)`——**不允许少返回**
    /// （这是路径 C 与路径 B 的唯一语义差异，也是低选择度场景的价值所在）。
    #[test]
    fn 精确扫描条数为min_k与allowed() {
        let mut seed = 313u64;
        let (idx, entries) = build_index(200, &mut seed);
        let even = EvenChunks { count: 100 };
        let q = entries[3].1.clone();

        let k4 = idx.search_exact_filtered(&q, 4, Some(&even)).unwrap();
        assert_eq!(k4.len(), 4);
        for (id, _) in &k4 {
            assert!(even.contains(*id), "返回了未通过谓词的 chunk {id}");
        }

        let all = idx.search_exact_filtered(&q, 999, Some(&even)).unwrap();
        assert_eq!(all.len(), 100, "k > allowed 时应返回全部 allowed 条");

        assert!(idx
            .search_exact_filtered(&q, 0, Some(&even))
            .unwrap()
            .is_empty());
        for w in all.windows(2) {
            assert!(
                w[0].1 < w[1].1 || (w[0].1 == w[1].1 && w[0].0 < w[1].0),
                "排序必须是 (距离升序, chunk_id 升序) 的全序"
            );
        }
    }

    /// **S5-T4 / S5-T1**：阈值边界（`≤` 成立）+ 关闭开关 + 默认值 + `Alive` 不兜底。
    #[test]
    fn 阈值边界与关闭开关() {
        assert_eq!(
            HnswRsIndex::with_capacity(8).brute_fallback(),
            Some(BRUTE_FALLBACK_MAX_ALLOWED),
            "默认必须开兜底（否则产品拿不到 Step 5 的收益）"
        );

        let mut seed = 5u64;
        let (idx, _) = build_index(64, &mut seed);
        let idx = idx.with_brute_fallback(Some(32));
        assert_eq!(idx.brute_fallback(), Some(32));

        assert!(
            idx.prefers_exact(&EvenChunks { count: 32 }),
            "allowed == 阈值 ⇒ 走精确（≤ 成立）"
        );
        assert!(
            !idx.prefers_exact(&EvenChunks { count: 33 }),
            "allowed == 阈值 + 1 ⇒ 走 ANN"
        );

        // 热路径（Alive）永不兜底：R15 的 fast-return 是 Step 1 的不变量
        use crate::bitmap::Bitmap;
        use crate::predicate::AliveOnly;
        let mut alive = Bitmap::new();
        alive.set(0);
        let alive_only = AliveOnly::new(&alive);
        assert!(
            !idx.prefers_exact(&alive_only),
            "FilterKind::Alive 是热路径，不得走精确扫描"
        );

        // 关闭兜底（回归对照）
        let (off, _) = build_index(64, &mut seed);
        let off = off.with_brute_fallback(None);
        assert_eq!(off.brute_fallback(), None);
        assert!(
            !off.prefers_exact(&EvenChunks { count: 1 }),
            "--brute-fallback off ⇒ 任何 allowed 都不兜底"
        );
    }

    /// **S5-T7（向量层护栏）**：兜底**不改热路径**。
    ///
    /// 断言的是**策略层**的事实：`None` 与 `FilterKind::Alive` 谓词下
    /// `prefers_exact` 恒 `false`（阈值调到 `usize::MAX` 也否），
    /// 于是编排层**结构上不可能**把热路径分派到精确扫描。
    ///
    /// ⚠️ 为什么"开/关兜底 Top-K 逐位一致"的**行为** A/B 不放在这里：
    /// 向量层的 `search_filtered` **根本不读阈值**——分派在编排层。要在这层比较
    /// 两个阈值，只能构造两个不同的图（`hnsw_rs` 用 OS 熵，拓扑互不相同 ⇒ 比较
    /// 无意义）。行为 A/B 用**同图 + 间谍后端**在 `query::searcher` 的测试里做，
    /// 那里才能既固定拓扑又观测"到底调了哪个方法"。
    #[test]
    fn 热路径在策略层就排除兜底() {
        use crate::bitmap::Bitmap;
        use crate::predicate::AliveOnly;

        let mut seed = 61u64;
        let (idx, entries) = build_index(200, &mut seed);
        // 阈值调到无穷大：任何"看 allowed 就兜底"的错误实现都会在这里暴露
        let always = idx.with_brute_fallback(Some(usize::MAX));

        let mut alive = Bitmap::new();
        for i in 0..200u32 {
            if i % 3 != 0 {
                alive.set(i);
            }
        }
        let alive_only = AliveOnly::new(&alive);

        assert!(
            !always.prefers_exact(&alive_only),
            "FilterKind::Alive 必须永远走路径 A（R15 的 fast-return 是 Step 1 的不变量）"
        );
        for p in [0usize, 88, 190] {
            // 无过滤检索的契约仍是一字未改：search() 等价于 search_filtered(None)
            let q = entries[p].1.clone();
            assert_eq!(
                always.search(&q, 10).unwrap(),
                always.search_filtered(&q, 10, None).unwrap(),
                "None 谓词下 search() 与 search_filtered(None) 必须等价"
            );
        }
    }
}
