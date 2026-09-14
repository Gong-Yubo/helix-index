//! 精排的**纯策略层**：分数变换、按输入下标回填、排序、截断、身份字符串。
//!
//! # 为什么单独成一个模块（V2 Step 7 / S7-01 的「注入接缝」）
//!
//! `LocalReranker` 里**真正不可测**的部分只有「一次 ONNX 前向」——它要 2.19GB 权重
//! （设计 §3.5 / R44）⇒ 不能进 CI（`#[ignore]`）。把「拿到一批 `(下标, 原始分数)`
//! 之后怎么做」抽成**不依赖 fastembed** 的纯函数之后，策略就能在 CI 里用**可控的
//! 打分序列**秒级钉住——与 Step 6 的 `EmbedderCtor`（`search/config.rs`）同一个动机：
//! **把「构造/推理」与「策略」分开，策略才可测**。
//!
//! ⚠️ 本模块**不关心分数是谁算的**，也不知道 `TextRerank` 的存在。
//!
//! # 三条行为（与设计附录 A 的不变式一一对应）
//!
//! 1. `hit.score` **按 `ScoredCandidate::index` 回填**为 `σ(logit)`；
//! 2. 排序 = `score` **严格不增**，并列时 **`chunk_id` 升序**（D-S7-07，与 NFR-06 一致）；
//! 3. 截断到 `top_n`。

use crate::query::response::Hit;
use crate::types::Score;

/// 精排器的模型身份（`id()` 的前半段，D-S7-08 / D-S7-10）。
///
/// ⚠️ `rozgo/bge-reranker-v2-m3` —— **不是** BAAI 官方库（官方库没有 ONNX）；
/// `sha256:84b66c78…8945` / 取用日期 2026-09-14（`#23` 前置 ②，已实下载校验）。
pub(crate) const RERANKER_MODEL_ID: &str = "bge-reranker-v2-m3@rozgo";

/// 一次精排打分的**一项**：`index` = 输入切片中的下标，`logit` = 模型原始输出。
///
/// ⚠️ `index` 是**回填的唯一依据**：`fastembed` 的 `RerankResult` 是**按分数降序**
/// 返回的（`fastembed-6.0.2/src/reranking/impl.rs:215-224`，稳定排序），**不是**输入
/// 顺序 ⇒ 按位置 zip 会把分数配到**错误的 chunk** 上（静默错排，不报错）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScoredCandidate {
    /// 对应输入 `Vec<Hit>` 的下标（来自 `RerankResult::index`）
    pub(crate) index: usize,
    /// 模型原始输出：**无界、可负**（设计 §4.4.3）
    pub(crate) logit: f32,
}

/// 模型原始 logit → **`[0, 1]`** 的**单调**变换（D-S7-05）。
///
/// `σ(x) = 1 / (1 + e^(−x))`。单调 ⇒ **排序信息零损失**；原始 logit 不丢
/// （进 `Explain.rerank_score`）。
///
/// 为什么不直接把 logit 当 `score`：`Hit.score` 在三种 mode 下已经是**模式相关**的
/// 量（bm25 分 / 余弦 ∈ [−1,1] / RRF ∈ (0, 0.04]），塞进无界、可负、量级 ±10 的
/// logit 会让下游「用 score 设阈值」的代码以完全不同的量纲工作（设计 §4.4.3）。
///
/// # ⚠️ 值域是**闭**区间：f32 下 σ 会**精确饱和**到端点
///
/// 两端都**不产生 NaN**，但**会取到端点本身**（两点均已实测，bits 见下）：
///
/// | 端 | 机制 | 实测 |
/// | --- | --- | --- |
/// | `logit ≳ 16.7 ⇒ σ == 1.0` | `1.0 + e^(−16.7)`（≈`5.59e-8`）**被舍入回 `1.0`**（f32 在 1.0 处的半个 ULP ≈ `5.96e-8`，余量仅 6%） | `σ(16.7).to_bits() == 0x3f800000` |
/// | `logit ≲ −89.0 ⇒ σ == 0.0` | `e^89`（≈`4.5e38`）**上溢到 `inf`** ⇒ `1 / (1 + inf) == 0` | `σ(−89.0).to_bits() == 0x00000000` |
///
/// ⇒ 饱和处**不再严格单调**（`σ(16.7) == σ(1000.0)`），但**序不增/不减**仍成立 ——
/// D-S7-05 依赖的是**单调**、不是单射，故「排序信息零损失」**不受影响**。
/// ⚠️ 由此推出一条实现纪律：**`0.0` / `1.0` 是可达的真值** ⇒ 任何「用 `0.0` 当哨兵
/// 表示『没打分』」的方案都会与真值**不可区分**（`apply_scores` 的「未覆盖项」因此
/// 保持输入分而非取最低分，见该函数 rustdoc）。
pub(crate) fn sigmoid(logit: f32) -> Score {
    1.0 / (1.0 + (-logit).exp())
}

/// 精排结果**回填 + 排序 + 截断**（设计 §4.4.2 的第 5~7 步）。
///
/// 语义见模块文档的三条行为。
///
/// # 契约：`scored` 应**恰好覆盖** `hits`
///
/// 即「每一条候选恰好一项、每项 `index` 都在 `hits` 范围内」（`fastembed` 的
/// `rerank` 满足这一点：它对**每个**输入文档返回一条 `RerankResult`，
/// `fastembed-6.0.2/src/reranking/impl.rs:215-224` 由全部 scores 构造）。
/// 违反时**不得静默**（NFR-07）——两条防御路径的处理刻意**不同**：
///
/// | 情形 | dev（`debug_assertions`） | release | 为什么这样分 |
/// | --- | --- | --- | --- |
/// | `index` **越界** | `debug_assert!` 失败（快速失败） | 忽略该项 + `tracing::warn!` | **结构违反**：分数会被写到**错的 hit** 上 ⇒ 没有「可解释的退化」可言（同 `query/searcher.rs` 的 `dispatch_vector` 先例：优雅退化 + `debug_assert` 指出） |
/// | **覆盖不足**（少给打分） | —— | 该项**保持输入（融合）分** + `tracing::warn!` | 有明确可解释的语义（见下），**且要可测** ⇒ 不加 `debug_assert` 才能让单测钉住它 |
///
/// # 为什么「未覆盖项」保持融合分，而**不是**丢弃或取最低分
///
/// - **丢弃** ⇒ 静默改变**条数契约**：本函数的职责是「排序 + 截断」，不是过滤
///   （`hits` 是编排层按窗口 `take_n` 捞来的，丢一条就等于少一条结果）；
/// - **取最低分**（如 `0.0`）⇒ **伪造**一个分数，而 σ 在 f32 下**会精确饱和到端点**
///   （`σ(−89) == 0.0`，见 [`sigmoid`]）⇒ 伪造值与**真值不可区分**；
/// - **保持** ⇒ 不伪造、不丢分，且该条在 S7-02 之后可由
///   `explain.rerank_score == None` **识别出来** —— 而这**正好**就是 D-S7-05 的
///   `is_some()` 信号的定义（「`score` 是不是精排给的」）⇒ 「混了两个量纲」这件事是
///   **可观测**的，而不是静默的。
///
/// ⚠️ **可达性**：当前后端（`LocalReranker` + `fastembed`）下**两种违反都不可达**，
/// 属防御路径；上面两条分支的存在是为了「上游换了 / 将来有人接别的精排器」时不静默。
pub(crate) fn apply_scores(
    mut hits: Vec<Hit>,
    scored: &[ScoredCandidate],
    top_n: usize,
) -> Vec<Hit> {
    if scored.len() != hits.len() {
        tracing::warn!(
            scored = scored.len(),
            candidates = hits.len(),
            "精排覆盖不足：未覆盖的候选保持输入（融合）分数（见 apply_scores 的 rustdoc）"
        );
    }

    for item in scored {
        // REFERENCE ⑥：非有限值在本题材里是**静默**的（`σ(NaN) == NaN`，排序仍有确定序
        // 但结果无意义）⇒ 用 debug_assert 在测试/调试构建里立即可见。
        debug_assert!(
            item.logit.is_finite(),
            "精排 logit 非有限值（index={}）",
            item.index
        );
        // 越界 = 结构违反（会把分数写到错的 hit 上）⇒ dev 快速失败；release 容忍但**可见**。
        debug_assert!(
            item.index < hits.len(),
            "精排返回越界 index：index={}，候选数={}",
            item.index,
            hits.len()
        );
        match hits.get_mut(item.index) {
            Some(hit) => hit.score = sigmoid(item.logit),
            None => tracing::warn!(
                index = item.index,
                candidates = hits.len(),
                "精排返回的 index 越界，本条已忽略（该候选保持输入分数）"
            ),
        }
    }

    // `total_cmp`（与全项目一致，REFERENCE ⑧）：NaN 也有确定序，不像 `partial_cmp`
    // 那样需要 `unwrap_or(Equal)` 而这种兜底会让「不可比」静默变成「相等」。
    // 降序 `score`，并列时**升序 `chunk_id`**（D-S7-07）——不沿用 fastembed 的
    // 「并列保持输入顺序」（`sort_by` 稳定 ⇒ 输入顺序 = 融合顺序，与 NFR-06 的
    // 「tie-break by chunk_id」不一致）。
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.chunk_id.cmp(&b.chunk_id))
    });
    hits.truncate(top_n);
    hits
}

/// 精排器**身份字符串**（D-S7-08 / D-S7-10，设计 §4.4.4）。
///
/// ⚠️ 返回 `String` 而**不是** `&'static str`：`max_length` 与 `window` 是**运行期**
/// 参数，`&'static str` 表达不了（Step 6 的 F3 指出过同类问题的另一种形态）。
///
/// 它**不进** `ConfigFingerprint`（精排不改索引内容 ⇒ 进指纹会把「换个精排器」
/// 误判成 `ConfigMismatch`、把老快照锁死），但**必须可见**（日志 / `search` 输出）：
/// 否则「同一 query 两次结果不同」会变成无法排查的谜（NFR-07）。
pub(crate) fn reranker_identity(window: usize, max_length: usize) -> String {
    format!("{RERANKER_MODEL_ID};max_len={max_length};window={window}")
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;
    use crate::query::response::Explain;

    fn hit(id: u32) -> Hit {
        Hit {
            chunk_id: id,
            doc_id: id,
            score: 1.0,
            text: format!("t{id}"),
            source: "s".into(),
            metadata: serde_json::json!({}),
            explain: Explain::default(),
        }
    }

    fn ids(hits: &[Hit]) -> Vec<u32> {
        hits.iter().map(|h| h.chunk_id).collect()
    }

    /// **S7-T5（前半，错排防护）**：分数必须按 `index` 回填，**不是**按位置 zip。
    ///
    /// 打分序列刻意与输入顺序**反序**（模拟 fastembed 的「按分数降序返回」）：
    /// `index` 2 → 9.0 / `index` 0 → 0.0 / `index` 1 → −9.0。
    #[test]
    fn 分数按index回填而不是按位置zip() {
        let hits = vec![hit(5), hit(1), hit(3)];
        let scored = [
            ScoredCandidate {
                index: 2,
                logit: 9.0,
            },
            ScoredCandidate {
                index: 0,
                logit: 0.0,
            },
            ScoredCandidate {
                index: 1,
                logit: -9.0,
            },
        ];
        let out = apply_scores(hits, &scored, 10);

        // ① 逐条核对「哪个 chunk 拿到哪个分数」——这才是错排的直接判据。
        let score_of = |id: u32| out.iter().find(|h| h.chunk_id == id).unwrap().score;
        assert_eq!(
            score_of(3).to_bits(),
            sigmoid(9.0).to_bits(),
            "index 2 是 chunk 3"
        );
        assert_eq!(
            score_of(5).to_bits(),
            sigmoid(0.0).to_bits(),
            "index 0 是 chunk 5"
        );
        assert_eq!(
            score_of(1).to_bits(),
            sigmoid(-9.0).to_bits(),
            "index 1 是 chunk 1"
        );

        // ② 排序：σ(9) > 0.5 > σ(−9)。
        assert_eq!(ids(&out), vec![3, 5, 1]);

        // ③ 反向自证（断言有牙齿）：按位置 zip 会得到 [5, 1, 3]，与 ② 不同 ⇒
        //    若有人把回填改成 zip，①②都会红，而不是「碰巧一样」。
        assert_ne!(
            ids(&out),
            vec![5, 1, 3],
            "按位置 zip 的顺序必须与正确回填不同"
        );
    }

    /// **S7-T5（后半）+ D-S7-07**：`σ(logit)` 并列时按 `chunk_id` **升序**。
    ///
    /// fastembed 的 `sort_by` 是稳定排序 ⇒ 它并列时保持**输入顺序**（= 融合顺序）。
    /// 这里刻意让输入顺序与 `chunk_id` 升序**不同**，否则断言无鉴别力。
    #[test]
    fn 并列时按chunkId升序而不是保持输入顺序() {
        let hits = vec![hit(7), hit(2), hit(5)];
        let scored = [
            ScoredCandidate {
                index: 0,
                logit: 0.0,
            },
            ScoredCandidate {
                index: 1,
                logit: 0.0,
            },
            ScoredCandidate {
                index: 2,
                logit: 0.0,
            },
        ];
        let out = apply_scores(hits, &scored, 10);
        assert_eq!(ids(&out), vec![2, 5, 7], "并列 ⇒ chunk_id 升序");
        assert_ne!(
            ids(&out),
            vec![7, 2, 5],
            "输入顺序（= 稳定排序的结果）必须与之不同"
        );
    }

    /// 截断到 `top_n`，且截断发生在**排序之后**（取的是分高的那些）。
    #[test]
    fn 截断到top_n且取分高的() {
        let hits = vec![hit(1), hit(2), hit(3)];
        let scored = [
            ScoredCandidate {
                index: 0,
                logit: -5.0,
            },
            ScoredCandidate {
                index: 1,
                logit: 5.0,
            },
            ScoredCandidate {
                index: 2,
                logit: 0.0,
            },
        ];
        let out = apply_scores(hits, &scored, 2);
        assert_eq!(ids(&out), vec![2, 3], "先排序再截断（chunk 2 分最高）");
    }

    /// **S7-T12（前半）**：`score == σ(logit)`，且 `σ` 单调、值域是**闭**区间 `[0,1]`。
    #[test]
    fn 分数等于sigmoid_logit且单调有界() {
        assert_eq!(sigmoid(0.0), 0.5);
        assert!(sigmoid(1.0) > sigmoid(0.0), "单调增");
        assert!(sigmoid(-1.0) < sigmoid(0.0), "单调增");
        // D-S7-05 的动机：无界 logit → **有界** score，且**不得**出现 NaN/inf。
        // ⚠️ 用**闭**区间（PR #52 评审意见 4）：f32 下 σ 会**精确饱和**到端点。
        for &l in &[-1000.0f32, -50.0, -1.0, 0.0, 1.0, 50.0, 1000.0] {
            let s = sigmoid(l);
            assert!(s.is_finite() && (0.0..=1.0).contains(&s), "logit={l} ⇒ {s}");
        }
        // 两个饱和端点（机制见 `sigmoid` 的 rustdoc；bits 已实测：3f800000 / 00000000）
        assert_eq!(
            sigmoid(16.7),
            1.0,
            "正端：1.0 + e^(−16.7) 被舍回 1.0 ⇒ σ 精确 == 1.0"
        );
        assert_eq!(sigmoid(-89.0), 0.0, "负端：e^89 上溢到 inf ⇒ σ 精确 == 0.0");
        assert!(sigmoid(-88.0) > 0.0, "−88 尚未饱和（是次正规数，不是 0）");
        // 饱和 ⇒ **不再严格**单调，但序仍不增/不减 —— D-S7-05 依赖的是**单调**、不是单射
        assert_eq!(
            sigmoid(16.7),
            sigmoid(1000.0),
            "饱和区等值（刻意保留的性质）"
        );

        // 回填后逐条等于 σ(logit)（逐位比较：同一函数、同一输入）。
        let hits = vec![hit(1), hit(2)];
        let scored = [
            ScoredCandidate {
                index: 1,
                logit: 3.0,
            },
            ScoredCandidate {
                index: 0,
                logit: -3.0,
            },
        ];
        let out = apply_scores(hits, &scored, 10);
        assert_eq!(out[0].chunk_id, 2);
        assert_eq!(out[0].score.to_bits(), sigmoid(3.0).to_bits());
        assert_eq!(out[1].score.to_bits(), sigmoid(-3.0).to_bits());
    }

    /// **S7-T12（前半，可观测）**：身份字符串必须**随两个运行期参数变化**。
    ///
    /// 这是 D-S7-08「`max_length` / 窗口进可观测面」的最小保证：若有人把参数写死，
    /// 这条会红（而不是「日志里看着像对的」）。
    #[test]
    fn 身份字符串随窗口与maxLen变化() {
        let a = reranker_identity(20, 512);
        assert!(a.contains("max_len=512"), "{a}");
        assert!(a.contains("window=20"), "{a}");
        assert!(a.contains(RERANKER_MODEL_ID), "{a}");
        assert_ne!(a, reranker_identity(50, 512), "窗口必须体现在身份里");
        assert_ne!(
            a,
            reranker_identity(20, 1024),
            "max_length 必须体现在身份里"
        );
    }

    /// **S7-T5 补充（PR #52 评审意见 2 的落地）**：**部分覆盖**时未覆盖项保持**输入分**。
    ///
    /// 语义见 `apply_scores` 的 rustdoc：「未覆盖」= 精排**没给分**的候选 ⇒ 保持输入
    /// （融合）分 + `warn!`，且该条在 S7-02 之后可由 `explain.rerank_score == None` 识别
    /// （正是 D-S7-05 的 `is_some()` 信号的定义）。
    ///
    /// ⚠️ **鉴别力是刻意设计的**：`scored` 只覆盖**最后**一条（`index = 2`）⇒ 若实现按
    /// **位置** zip（把 `index` 当位置用），被覆盖的会变成**第一条** ⇒ 分数与顺序都会变、
    /// 本用例必红。（原先那条「越界」用例对这一点**没有鉴别力**：它的每条 hit 都被覆盖
    /// 且 `index == 位置` ⇒ 实测在「按位置 zip」变异下**仍然通过**，故已由本用例取代。）
    ///
    /// ⚠️ **越界 `index` 不在这里测**：它是结构违反、被 `apply_scores` 里的
    /// `debug_assert!` 拦住（debug 下直接 panic）⇒ **debug 构建里不存在可测的容忍路径**；
    /// release 下的容忍 + `warn!` 是**盲区**（已在该函数 rustdoc 的分支表里写明）。
    #[test]
    fn 部分覆盖时未覆盖项保持输入分() {
        let mut hits = vec![hit(7), hit(2), hit(5)];
        // 输入（融合）分：刻意选在 σ 的值域内、但与 σ(logit) 的**顺序不同**
        hits[0].score = 0.30;
        hits[1].score = 0.95;
        hits[2].score = 0.10;
        // 只覆盖「第三条」（index 2 = chunk 5）
        let scored = [ScoredCandidate {
            index: 2,
            logit: 6.0,
        }];
        let out = apply_scores(hits, &scored, 10);

        let score_of = |id: u32| out.iter().find(|h| h.chunk_id == id).unwrap().score;
        assert_eq!(
            score_of(5).to_bits(),
            sigmoid(6.0).to_bits(),
            "被覆盖项 → σ(logit)"
        );
        assert_eq!(
            score_of(7),
            0.30,
            "未覆盖项保持输入分（不丢弃、不取最低分）"
        );
        assert_eq!(score_of(2), 0.95, "同上");
        assert_eq!(out.len(), 3, "不丢弃任何条（条数契约不变）");
        assert_eq!(ids(&out), vec![5, 2, 7], "σ(6)≈0.9975 > 0.95 > 0.30");
        // 反向自证：按位置 zip 会得到 [7, 2, 5]
        assert_ne!(ids(&out), vec![7, 2, 5], "按位置 zip 的顺序必须与之不同");
    }
}
