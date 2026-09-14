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

/// 模型原始 logit → `(0, 1)` 的**单调**变换（D-S7-05）。
///
/// `σ(x) = 1 / (1 + e^(−x))`。单调 ⇒ **排序信息零损失**；原始 logit 不丢
/// （进 `Explain.rerank_score`）。
///
/// 为什么不直接把 logit 当 `score`：`Hit.score` 在三种 mode 下已经是**模式相关**的
/// 量（bm25 分 / 余弦 ∈ [−1,1] / RRF ∈ (0, 0.04]），塞进无界、可负、量级 ±10 的
/// logit 会让下游「用 score 设阈值」的代码以完全不同的量纲工作（设计 §4.4.3）。
///
/// 数值边界：`logit → +∞` 时 `e^(−logit) → 0` ⇒ `σ → 1`；`logit → −∞` 时
/// `e^(−logit) → +∞` ⇒ `σ → 0`。**两端都不产生 NaN**（`1 / (1 + inf) == 0`）。
pub(crate) fn sigmoid(logit: f32) -> Score {
    1.0 / (1.0 + (-logit).exp())
}

/// 精排结果**回填 + 排序 + 截断**（设计 §4.4.2 的第 5~7 步）。
///
/// 语义见模块文档的三条行为。越界的 `index` 只可能来自上游 bug：这里**不 panic**
/// （检索主路径不留 panic 分支）但**也不静默错配**——直接忽略该项（对应 hit 保持
/// 输入分数）。
pub(crate) fn apply_scores(
    mut hits: Vec<Hit>,
    scored: &[ScoredCandidate],
    top_n: usize,
) -> Vec<Hit> {
    for item in scored {
        // REFERENCE ⑥：非有限值在本题材里是**静默**的（`σ(NaN) == NaN`，排序仍有确定序
        // 但结果无意义）⇒ 用 debug_assert 在测试/调试构建里立即可见。
        debug_assert!(
            item.logit.is_finite(),
            "精排 logit 非有限值（index={}）",
            item.index
        );
        if let Some(hit) = hits.get_mut(item.index) {
            hit.score = sigmoid(item.logit);
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

    /// **S7-T12（前半）**：`score == σ(logit)`，且 `σ` 单调、有界、两端不溢出。
    #[test]
    fn 分数等于sigmoid_logit且单调有界() {
        assert_eq!(sigmoid(0.0), 0.5);
        assert!(sigmoid(1.0) > sigmoid(0.0), "单调增");
        assert!(sigmoid(-1.0) < sigmoid(0.0), "单调增");
        // D-S7-05 的动机：无界 logit → 有界 score，且**不得**出现 NaN/inf。
        for &l in &[-1000.0f32, -50.0, -1.0, 0.0, 1.0, 50.0, 1000.0] {
            let s = sigmoid(l);
            assert!(s.is_finite() && (0.0..=1.0).contains(&s), "logit={l} ⇒ {s}");
        }

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

    /// 越界 `index` 被忽略：不 panic、不产生多余 hit，**其余项照常生效**
    /// （上游 bug 不得让检索主路径炸掉，也不得让整批回填失效）。
    #[test]
    fn 越界index被忽略而其余项照常生效() {
        let hits = vec![hit(1), hit(2)];
        let scored = [
            ScoredCandidate {
                index: 0,
                logit: 5.0,
            },
            ScoredCandidate {
                index: 1,
                logit: -5.0,
            },
            ScoredCandidate {
                index: 99,
                logit: 999.0,
            },
        ];
        let out = apply_scores(hits, &scored, 10);
        assert_eq!(out.len(), 2, "越界项不产生新 hit");
        assert_eq!(ids(&out), vec![1, 2], "σ(5) > σ(−5)");
        let score_of = |id: u32| out.iter().find(|h| h.chunk_id == id).unwrap().score;
        assert_eq!(
            score_of(1).to_bits(),
            sigmoid(5.0).to_bits(),
            "有效项照常回填"
        );
    }
}
