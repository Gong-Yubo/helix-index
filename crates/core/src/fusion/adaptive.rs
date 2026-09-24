//! 自适应融合：查询侧上下文（`FusionCtx`）与 tier 规则（V2 Step 9 / T7-14 / FR-35）。
//!
//! # 防泄漏边界（I9-1 / S9-3，本模块最重要的约束）
//!
//! 自适应信号**只允许**「检索自身可观测的量」：召回结果的统计、query 的形态、
//! 索引词典统计。**永不**读相关性判定文件、标注、桶标签——「桶」含标注信息
//! （`exact`/`paraphrase` 的划分用到 `grade ≥ 1` 的段落，`t2_prep.rs`），把桶标签
//! 喂进调权等价于把答案喂进排序（设计 §2.4 N1 / 风险 R55）。
//! 源码级的反扫判据见本模块 `S9_T3` ②。
//!
//! # 字段白名单（设计 §4.3 的显式规则，第 1 轮评审 P4-1）
//!
//! `FusionCtx` / `LaneStats` 的**全部字段**就是白名单。本模块底部的 `S9_T3`
//! 用「结构体字面量逐字段构造」把白名单钉成**编译期事实**：新增字段而不改用例
//! ⇒ 缺字段编译失败；删字段 ⇒ 多字段编译失败 ⇒ 「临时加字段」不可能静默违约。
//!
//! # tier 规则（设计 §4.2，两级）
//!
//! | 级 | 触发条件 | 动作 | 参数量 |
//! | --- | --- | --- | --- |
//! | **tier 1** | 某 lane 为空（`len == 0`） | 该路权重置 **0**（不删槽） | 零参数 |
//! | **tier 2** | 信号 `s < θ` | **BM25 路（lane 0）**权重降到 `0` | 单阈值 θ |
//!
//! ⚠️ tier 1 **单独不改变排序**（设计 §4.2 点破的「行为空转」）：RRF 只用名次，
//! 空 lane 在 `fuse` 内本来就零贡献 ⇒ 它的真实价值是 `Metrics.fusion_weights`
//! 的可观测语义（`Some([0.0, w])` vs `None`）与 I9-2「置 0 不删槽」的实现模板。

use std::collections::HashSet;

use crate::types::{ChunkId, Score};

/// 单路候选的统计摘要（**零拷贝**：`entries` 直接借用 lane 本体）。
///
/// 空态（设计 §4.1 / 第 1 轮评审 P4-1）：`len == 0` ⇒ `top_score == None` /
/// `last_score == None`——**不用 `0.0` 作哨兵**：BM25 无上界、`0.0` 是合法分值，
/// 哨兵会让「空 lane」与「非空但全零分 lane」不可区分。
///
/// ⚠️ `entries` 是**整段 lane 的借用**而非 `&[ChunkId]` 切片：`LaneResults` 的
/// 元素是 `(ChunkId, Score)` 元组，从元组切片投影不出独立的 `&[ChunkId]`（除非
/// 拷贝）⇒ 重合率信号通过 `entries.iter().map(|(id, _)| *id)` 消费（PR9-1 定形，
/// 设计原文写的是「chunk_id 切片」——投影不可达，取语义等价的零拷贝借用）。
#[derive(Debug, Clone, Copy)]
pub struct LaneStats<'a> {
    /// 该路候选条数（== `entries.len()`；单列是为让「空态」有显式字段可断言）
    pub len: usize,
    /// 最高分（tier 2 候选信号 s2 的分布形态用；RRF 本身不用分数）
    pub top_score: Option<Score>,
    /// 最低分（同上）
    pub last_score: Option<Score>,
    /// 该路候选本体（按该路名次排序的 `(chunk_id, score)` 序列）
    pub entries: &'a [(ChunkId, Score)],
}

impl<'a> LaneStats<'a> {
    /// 从 lane 本体构造统计摘要（编排层的唯一构造入口，保证三字段口径一致）。
    pub fn new(entries: &'a [(ChunkId, Score)]) -> Self {
        Self {
            len: entries.len(),
            top_score: entries.first().map(|(_, s)| *s),
            last_score: entries.last().map(|(_, s)| *s),
            entries,
        }
    }
}

/// 融合上下文：**只含检索自身可观测的量**（I9-1 / S9-3）。
///
/// 由编排层在**融合之前**构造；`query_df` 走一次词典探针（O(query 词项数)，
/// 只查词典、不取正文——与「融合不回捞正文」的硬约束不冲突）。
#[derive(Debug, Clone, Copy)]
pub struct FusionCtx<'a> {
    /// 各路候选的统计摘要（与召回结果**一一对应**，含**空 lane** 的槽位——I9-2）
    pub lanes: &'a [LaneStats<'a>],
    /// query 分词后的词项数（**未去重**，与 BM25 累加循环同口径）
    pub query_terms: usize,
    /// query 字符数（`chars().count()`，与评测分桶的 `NATURAL_MIN_CHARS` 同口径）
    pub query_chars: usize,
    /// query 是否含 ASCII 字母数字（与评测分桶的 mixed 判据同源，`t2_prep.rs`）
    pub has_ascii: bool,
    /// query 词项的**索引侧 df 覆盖率** ∈ [0, 1]：去重词项中 `df > 0` 的占比
    /// （跨段时对每段各查一次，任一段有即算覆盖）。空 query ⇒ `0.0`。
    pub query_df: Score,
    /// 本次判据的 `k`（重合率信号的 top-k 口径须与判据的 `k` 钉死，设计 §4.3 s1）
    pub top_k: usize,
}

/// tier 2 的候选信号（设计 §4.3；**选型留给 spike S9-S1**，不自选——
/// 本枚举把三个候选都实现，spike 跑完灵敏度曲线后按决策门 G1~G5 定夺）。
///
/// 共同语义：值域 `[0, 1]`，**值越低 ⇒ BM25 越可能是弱路** ⇒ 越该触发降权。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptiveSignal {
    /// **s1** 两路 top-k 重合率：`|BM25 前 k ∩ Vector 前 k| / k`——两路分歧越大
    /// （重合率越低）⇒ 越可能是「词面 vs 语义」的分歧 ⇒ BM25 越弱。
    Overlap,
    /// **s3** query 词项的索引侧 df 覆盖率（即 `FusionCtx::query_df`）——
    /// query 词在语料里越查不到，BM25 越无米下锅。
    DfCoverage,
    /// **s4** query 侧字符特征（`has_ascii` 的 0/1 形态）——与评测分桶的
    /// mixed/natural **纯 query 部分**同源；对 exact/paraphrase 无区分力
    /// （设计 §4.3 已声明）。⚠️ 二值信号 ⇒ θ 网格上只有两个不同行为档。
    QueryShape,
}

impl AdaptiveSignal {
    /// 信号名（CLI `--adaptive-fusion-signal` 的取值；`AdaptiveSignal::from_name` 逆解析）。
    pub fn name(&self) -> &'static str {
        match self {
            Self::Overlap => "overlap",
            Self::DfCoverage => "df",
            Self::QueryShape => "shape",
        }
    }

    /// 按名字解析信号（CLI 解析入口；未知名字报回原名便于对账）。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "overlap" => Some(Self::Overlap),
            "df" => Some(Self::DfCoverage),
            "shape" => Some(Self::QueryShape),
            _ => None,
        }
    }

    /// 按本信号计算归一化值 ∈ [0, 1]。
    ///
    /// 返回 `None` = 该信号在当前 ctx 下**无定义**（如 s1 需要**恰好两路**候选与
    /// `k > 0`）⇒ tier 2 不触发（只剩 tier 1），且 `Metrics.fusion_signal` 报 `None`。
    pub(crate) fn value(&self, ctx: &FusionCtx<'_>) -> Option<Score> {
        match self {
            Self::Overlap => overlap_topk(ctx),
            Self::DfCoverage => Some(ctx.query_df),
            Self::QueryShape => Some(if ctx.has_ascii { 1.0 } else { 0.0 }),
        }
    }
}

/// s1：两路 top-k 重合率。
///
/// 无定义（`None`）：路数 ≠ 2（Hybrid 恒为 2；其余形态不适用）或 `k == 0`。
/// lane 长度不足 k 时**仍按 k 归一**（短 lane 本身就是「分歧」的证据，不该把
/// 分母换成实际长度而把分歧藏起来）。
fn overlap_topk(ctx: &FusionCtx<'_>) -> Option<Score> {
    if ctx.top_k == 0 {
        return None;
    }
    let a = ctx.lanes.first()?;
    let b = ctx.lanes.get(1)?;
    let top_ids: HashSet<ChunkId> = a
        .entries
        .iter()
        .take(ctx.top_k)
        .map(|(id, _)| *id)
        .collect();
    let inter = b
        .entries
        .iter()
        .take(ctx.top_k)
        .filter(|(id, _)| top_ids.contains(id))
        .count();
    Some(inter as Score / ctx.top_k as Score)
}

/// tier 2 规则：**单阈值**形态（D-S9-08：不做权重网格搜索，多参数形态一律禁止）。
///
/// 触发：`信号值 s < θ` ⇒ **BM25 路（lane 0）**权重降到 `0`（设计 §4.2「w_bm25
/// 按单参数单调映射降到 0」的最简取形——降到 0 而非缩放，这让 S9-1b 的上界
/// **结构性可达**：θ 触发后该次 hybrid ≡ vector 单路的名次）。
///
/// ⚠️ θ 语义是「弱路判定线」：`s < θ` = 「信号弱于阈值 ⇒ 弃 BM25」。θ 越大越激进
/// （θ = 0 ⇒ 永不触发，退化为纯 tier 1；θ > 1 的网格外取值在构造期被拒）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptiveRule {
    /// 信号（见 [`AdaptiveSignal`]；选型由 spike S9-S1 定夺）
    pub signal: AdaptiveSignal,
    /// 单阈值 θ ∈ [0, 1]（预注册网格 `0.00 → 1.00` 步长 `0.05`，设计 §4.5）
    pub theta: Score,
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;

    /// 构造两路 `LaneStats`（lane 用 `(id, score)` 序列直接给）。
    /// `FusionCtx` 由各用例自己按字段构造（白名单用例必须逐字段写）。
    fn lanes_of<'a>(a: &'a [(ChunkId, Score)], b: &'a [(ChunkId, Score)]) -> Vec<LaneStats<'a>> {
        vec![LaneStats::new(a), LaneStats::new(b)]
    }

    /// 中性默认 ctx（其余字段给可覆盖的默认值；`FusionCtx` 是 `Copy`）。
    fn ctx_with<'a>(lanes: &'a [LaneStats<'a>], top_k: usize) -> FusionCtx<'a> {
        FusionCtx {
            lanes,
            query_terms: 3,
            query_chars: 12,
            has_ascii: false,
            query_df: 0.5,
            top_k,
        }
    }

    /// **S9-T3 ①（白名单）**：`FusionCtx` 的字段集被字面量逐字段钉住。
    ///
    /// 🔑 这是**编译期断言**：新增字段 ⇒ 此处缺字段编译失败；删字段/改名 ⇒
    /// 多余字段编译失败。白名单 = `lanes / query_terms / query_chars / has_ascii /
    /// query_df / top_k`——任何「临时加字段」都必须先改这里（而改这里会被
    /// 评审看见，这就是设计 §4.3「显式规则」的执行形态）。
    #[test]
    fn S9_T3_FusionCtx字段集等于白名单() {
        let lanes: [LaneStats; 0] = [];
        let ctx = FusionCtx {
            lanes: &lanes,
            query_terms: 0,
            query_chars: 0,
            has_ascii: false,
            query_df: 0.0,
            top_k: 0,
        };
        // 运行期自证（字段语义照读）
        assert_eq!(ctx.lanes.len(), 0);
        assert_eq!(ctx.query_terms, 0);
        assert_eq!(ctx.query_chars, 0);
        assert!(!ctx.has_ascii);
        assert_eq!(ctx.query_df, 0.0);
        assert_eq!(ctx.top_k, 0);
    }

    /// **S9-T3 ①（白名单）**：`LaneStats` 的字段集同上钉住
    /// （白名单 = `len / top_score / last_score / entries`）。
    #[test]
    fn S9_T3_LaneStats字段集等于白名单() {
        let entries: [(ChunkId, Score); 0] = [];
        let s = LaneStats {
            len: 0,
            top_score: None,
            last_score: None,
            entries: &entries,
        };
        assert_eq!(s.len, 0);
        assert!(s.top_score.is_none());
        assert!(s.last_score.is_none());
        assert_eq!(s.entries.len(), 0);
    }

    /// **S9-T3 ②（反扫）**：fusion 模块四个源文件**零命中**相关性数据标识。
    ///
    /// 🔴 禁词以**拼接**构造（判据源码里直接写禁词会误伤自扫描——`include_str!`
    /// 把本文件也扫进去）；并先自证检查方法有效（对照样本必须命中——否则
    /// 「零命中」可能是检查本身坏了的假阴性）。
    ///
    /// ⚠️ 被扫的是 `adaptive.rs` / `mod.rs` / `rrf.rs` / `weighted.rs` 的**源码文本**
    /// ⇒ fusion 模块的注释/标识符里**不许**出现这三个英文标识（中文写法不受影响，
    /// 判据只挡「代码里真的 import/读取了这些东西」的形态）。
    #[test]
    fn S9_T3_fusion模块源码反扫相关性标识零命中() {
        // 🔴 拼接构造：源码文本里只有 "qr"+"els" 两半，不会被自己的扫描命中
        let forbidden = [
            "qr".to_owned() + "els",
            "releva".to_owned() + "nce",
            "buck".to_owned() + "et",
        ];

        // ① 对照样本：检查方法自证（能命中已知含禁词的文本）
        let control = format!(
            "load_{} grade {} {}",
            forbidden[0], forbidden[1], forbidden[2]
        );
        for w in &forbidden {
            assert!(
                control.contains(w.as_str()),
                "对照样本必须命中 {w:?}（否则检查方法坏了）"
            );
        }

        // ② 反扫（include_str! 相对本文件 ⇒ 稳定不随 cwd 漂移）
        for (file, src) in [
            ("adaptive.rs", include_str!("adaptive.rs")),
            ("mod.rs", include_str!("mod.rs")),
            ("rrf.rs", include_str!("rrf.rs")),
            ("weighted.rs", include_str!("weighted.rs")),
        ] {
            for w in &forbidden {
                assert!(
                    !src.contains(w.as_str()),
                    "🔴 fusion/{file} 出现相关性标识 {w:?}（I9-1 违约：信号只许检索自身可观测）"
                );
            }
        }
    }

    /// `LaneStats::new` 的空态口径（P4-1：`None` 而非 `0.0` 哨兵）。
    #[test]
    fn LaneStats空态用None不用哨兵() {
        let empty: [(ChunkId, Score); 0] = [];
        let s = LaneStats::new(&empty);
        assert_eq!(s.len, 0);
        assert!(s.top_score.is_none(), "空 lane 的 top_score 必须是 None");
        assert!(s.last_score.is_none(), "空 lane 的 last_score 必须是 None");

        let lane = [(1u32, 3.0f32), (2, 1.0)];
        let s = LaneStats::new(&lane);
        assert_eq!(s.len, 2);
        assert_eq!(s.top_score, Some(3.0));
        assert_eq!(s.last_score, Some(1.0));
    }

    /// s1（重合率）的手算锚点 + 无定义分支。
    #[test]
    fn s1重合率手算与无定义分支() {
        // lane A: [1,2,3]；lane B: [2,4,5]；k=3 ⇒ 交集 {2} ⇒ 1/3
        let a = [(1u32, 9.0f32), (2, 8.0), (3, 7.0)];
        let b = [(2u32, 0.9f32), (4, 0.8), (5, 0.7)];
        let lanes = lanes_of(&a, &b);
        let ctx = ctx_with(&lanes, 3);
        let got = AdaptiveSignal::Overlap.value(&ctx).unwrap();
        assert!((got - 1.0 / 3.0).abs() < 1e-6, "实际 {got}");

        // 完全重合 ⇒ 1.0；完全分歧 ⇒ 0.0
        let lanes = lanes_of(&a, &a);
        assert!((AdaptiveSignal::Overlap.value(&ctx_with(&lanes, 3)).unwrap() - 1.0).abs() < 1e-6);
        let c = [(7u32, 1.0f32)];
        let lanes = lanes_of(&a, &c);
        assert!((AdaptiveSignal::Overlap.value(&ctx_with(&lanes, 3)).unwrap() - 0.0).abs() < 1e-6);

        // 短 lane 仍按 k 归一：A=[1,2,3] vs B=[1]（k=10）⇒ 交集 {1} ⇒ 1/10
        let lanes = lanes_of(&a, &[(1u32, 1.0f32)]);
        let got = AdaptiveSignal::Overlap
            .value(&ctx_with(&lanes, 10))
            .unwrap();
        assert!((got - 0.1).abs() < 1e-6, "短 lane 按 k 归一，实际 {got}");

        // 无定义：单路 / k == 0
        let one = [LaneStats::new(&a)];
        assert!(AdaptiveSignal::Overlap.value(&ctx_with(&one, 10)).is_none());
        let lanes = lanes_of(&a, &b);
        assert!(AdaptiveSignal::Overlap
            .value(&ctx_with(&lanes, 0))
            .is_none());
    }

    /// s3 / s4 的取值与边界。
    #[test]
    fn s3与s4取值() {
        let lanes = lanes_of(&[(1u32, 1.0f32)], &[(2u32, 1.0f32)]);
        let ctx = ctx_with(&lanes, 10);
        // s3 = query_df 原样透传
        assert_eq!(AdaptiveSignal::DfCoverage.value(&ctx), Some(0.5));
        // s4 = has_ascii 的 0/1
        assert_eq!(AdaptiveSignal::QueryShape.value(&ctx), Some(0.0));

        let mut ctx2 = ctx_with(&lanes, 10);
        ctx2.has_ascii = true;
        assert_eq!(AdaptiveSignal::QueryShape.value(&ctx2), Some(1.0));
        assert_eq!(
            AdaptiveSignal::DfCoverage.value(&ctx2),
            Some(0.5),
            "s3 不看 ASCII"
        );
    }

    /// 信号名与解析互逆（CLI 解析入口的锚点）。
    #[test]
    fn 信号名与解析互逆() {
        for s in [
            AdaptiveSignal::Overlap,
            AdaptiveSignal::DfCoverage,
            AdaptiveSignal::QueryShape,
        ] {
            assert_eq!(AdaptiveSignal::from_name(s.name()), Some(s));
        }
        assert_eq!(AdaptiveSignal::from_name("nope"), None);
        // 三个名字互异（防止两个变体共用一个名字导致 CLI 无法区分）
        let names = [
            AdaptiveSignal::Overlap.name(),
            AdaptiveSignal::DfCoverage.name(),
            AdaptiveSignal::QueryShape.name(),
        ];
        for i in 0..names.len() {
            for j in (i + 1)..names.len() {
                assert_ne!(names[i], names[j]);
            }
        }
    }
}
