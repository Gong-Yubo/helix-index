//! 本地精排器：fastembed `TextRerank` + `rozgo/bge-reranker-v2-m3`（交叉编码器）。
//!
//! - **feature `local-rerank`（非默认）**：模型合计 ≈2.19GB
//!   （`model.onnx.data` 2,271,088,656 B）⇒ 不能跟着默认 feature 进构建 / 下载路径
//!   （设计 §3.6 / R44）。未启用时本模块**不存在**（编译期），而不是「运行时静默退化」。
//! - **模型出处**：`rozgo/bge-reranker-v2-m3`（⚠️ **非** BAAI 官方库——官方库没有 ONNX）
//!   / `sha256:84b66c78…8945` / 取用日期 2026-09-14（`#23` 前置 ②，已实下载校验）。
//! - **不做多 session 池化**（设计 §4.4.2）：理由是与 R43 同源的 `Mutex` 串行瓶颈、
//!   R36 的内存教训（E2 峰值 RSS +96.4%），且精排**默认关**（D-S7-04）⇒ 不是热路径。
//! - `Mutex<TextRerank>`：`TextRerank::rerank` 需 `&mut self`
//!   （`fastembed-6.0.2/src/reranking/impl.rs:126-132`）——与 `LocalEmbedder` 同款先例。
//!
//! # 与设计文档 §4.4.1 的三处偏差（S7-01 实现期实测，PR 正文已报评审）
//!
//! ① `with_max_length(self, …)` **无法**按设计写成「构造后 builder」：`max_length` 在
//!    `TextRerank::try_new` 时就烧进 tokenizer 的 `TruncationParams`
//!    （`common.rs:181-185`）⇒ 构造后再改字段只会得到「断言仍绿但没生效」的假象。
//!    改为**构造期**入口 [`LocalReranker::with_params`]。
//! ② 单条候选**不早退**（设计 §4.4.2 第 1 步写的是「零/单条早退」）：否则
//!    「`score` 是否被精排替换」会依赖候选条数，与 D-S7-05 的
//!    `explain.rerank_score.is_some()` 信号自相矛盾（R46 关注的正是这件事）。
//!    只有**空输入**早退（fastembed 对空输入报 `EmptyTokenizations`）。
//! ③ 回填 / 排序 / 截断 / 身份字符串落在**不依赖 fastembed** 的 [`super::scoring`]，
//!    以便在 CI 里用可控打分序列钉住（设计 §3.5 的「注入接缝」）。

use std::sync::Mutex;

use fastembed::{RerankInitOptions, RerankerModel, TextRerank};

use crate::error::{Error, Result};
use crate::query::response::Hit;

use super::scoring::{apply_scores, reranker_identity, ScoredCandidate};
use super::Reranker;

/// 精排窗口默认值 `R`（**「拟」值**：形态已定、数值由 S7-04 标定、S7-05 回填；D-S7-01）。
///
/// 语义 = 「希望拿到融合结果的前 `max(k, R)` 条」。`20` 只是量级意义上的折中档，
/// **不是**实测最优值（设计 §4.2.2）。
pub const DEFAULT_RERANK_WINDOW: usize = 20;

/// 精排 `max_length` 默认值 = 库默认（`HasMaxLength for RerankerModel`，D-S7-08）。
///
/// ⚠️ 保持 512 是**刻意的**（R48）：T2Ranking 段落最长 76,895 字符 ⇒ 精排只看得到
/// 前 512 token，可能**系统性低估**长段落上的效果；是否调大**由 S7-04 的 `1024`
/// 对照档的数据定**，不预先调大（每对前向成本随长度近似平方增长）。
pub const DEFAULT_RERANK_MAX_LENGTH: usize = 512;

/// 本地交叉编码器精排器（`bge-reranker-v2-m3`）。
///
/// 构造即触发模型下载（首次 ≈2.19GB）。除索引内容之外的一切都不改：**不进**
/// `ConfigFingerprint`（D-S7-10）。
#[derive(Debug)]
pub struct LocalReranker {
    /// `TextRerank::rerank` 需 `&mut self` ⇒ 同 `LocalEmbedder` 的 `Mutex` 先例。
    inner: Mutex<TextRerank>,
    /// 窗口 `R`（运行期参数，不触碰模型）。
    window: usize,
    /// 与 `inner` 的 tokenizer 截断配置**一致**（构造期烧入，见模块文档偏差 ①）。
    max_length: usize,
}

impl LocalReranker {
    /// 默认参数：`window = DEFAULT_RERANK_WINDOW`、`max_length = DEFAULT_RERANK_MAX_LENGTH`。
    ///
    /// ⚠️ 首次调用会下载 ≈2.19GB 权重；失败**不 panic**，原样交回调用方。
    pub fn new() -> Result<Self> {
        Self::with_params(DEFAULT_RERANK_WINDOW, DEFAULT_RERANK_MAX_LENGTH)
    }

    /// 显式给全参数（**构造期**生效，见模块文档偏差 ①）。
    ///
    /// `max_length` 决定 tokenizer 的截断长度、`window` 决定候选窗口；两者都进
    /// [`Self::id`]（D-S7-08 的可观测要求）。
    pub fn with_params(window: usize, max_length: usize) -> Result<Self> {
        let model = TextRerank::try_new(
            RerankInitOptions::new(RerankerModel::BGERerankerV2M3)
                .with_max_length(max_length)
                // 与 `LocalEmbedder` **复用同一个**缓存目录函数：两处各写一份会让
                // 「模型下到哪去了」变成谜（设计 §4.4.1）。
                .with_cache_dir(crate::embed::default_cache_dir())
                .with_show_download_progress(false),
        )
        .map_err(|e| Error::Rerank(e.to_string()))?;

        Ok(Self {
            inner: Mutex::new(model),
            window,
            max_length,
        })
    }

    /// 覆盖窗口 `R`（**运行期**参数，不重新加载模型）。
    pub fn with_window(mut self, window: usize) -> Self {
        self.window = window;
        self
    }

    /// 精排器身份字符串（模型 + `max_length` + 窗口），见
    /// [`super::scoring::reranker_identity`]。
    pub fn id(&self) -> String {
        reranker_identity(self.window, self.max_length)
    }

    /// 配置的窗口 `R`（= [`Reranker::candidate_window`] 的返回值）。
    pub fn window(&self) -> usize {
        self.window
    }

    /// 配置的 `max_length`（与 tokenizer 的实际截断长度一致）。
    pub fn max_length(&self) -> usize {
        self.max_length
    }
}

impl Reranker for LocalReranker {
    /// 窗口 = 构造时给的 `R`。语义由 trait 定义：返回值 `w` 表示
    /// 「请把融合结果的前 `max(k, w)` 条交给我」。
    ///
    /// ⚠️ 编排层会据此把 `candidate_k` 联动放大（`max(3k, w, 10)`）——**只改截断
    /// 不改候选池**会让窗口被静默封顶（设计 §2.2 的设计期新发现 A）。
    fn candidate_window(&self, _k: usize) -> usize {
        self.window
    }

    fn rerank(&self, query: &str, mut hits: Vec<Hit>, top_n: usize) -> Result<Vec<Hit>> {
        // 空输入早退：fastembed 对空 `documents` 报 `EmptyTokenizations`。
        // ⚠️ **单条不早退**（模块文档偏差 ②）：否则「score 是否被替换」会依赖候选条数。
        if hits.is_empty() {
            return Ok(hits);
        }

        // 正文即 `documents`；`return_documents = false` 免得再从 fastembed 把同一段
        // 文本克隆回来（`RerankResult.document` 会是 `None`）。
        let documents: Vec<&str> = hits.iter().map(|h| h.text.as_str()).collect();

        let started = std::time::Instant::now();
        let scored = {
            let mut model = self.inner.lock().expect("精排器锁已中毒");
            // `batch_size = None` ⇒ 库默认 256（`reranking/mod.rs:2`）。
            // ⚠️ 批内 padding 到该批最长（`PaddingStrategy::BatchLongest`）⇒ **不同 `R`
            //    之间的 `score` 不可逐位横比**（R47 / 设计 §2.5）。
            model
                .rerank(query, &documents, false, None)
                .map_err(|e| Error::Rerank(e.to_string()))?
        };
        let elapsed = started.elapsed();

        // ⚠️ 必须按 `RerankResult::index` 回填（设计 §4.4.2）：返回的是**全量降序排序**，
        //    不是输入顺序 ⇒ 按位置 zip 会静默错排。
        let scored: Vec<ScoredCandidate> = scored
            .into_iter()
            .map(|r| ScoredCandidate {
                index: r.index,
                logit: r.score,
            })
            .collect();

        tracing::info!(
            reranker = %self.id(),
            window = self.window,
            candidates = hits.len(),
            elapsed_ms = elapsed.as_secs_f64() * 1000.0,
            "rerank"
        );

        hits = apply_scores(hits, &scored, top_n);
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;
    use crate::query::response::Explain;

    fn hit(id: u32, text: &str) -> Hit {
        Hit {
            chunk_id: id,
            doc_id: id,
            score: 1.0,
            text: text.into(),
            source: "s".into(),
            metadata: serde_json::json!({}),
            explain: Explain::default(),
        }
    }

    /// **S7-T9（P3，批组成敏感性）**：同一 `(query, doc)` **单条**打分 vs **混在批里**
    /// 打分 ⇒ 原始 logit 是否逐位相同。
    ///
    /// ⚠️ **本条只能在模块内部测**：公开 API 只暴露 `σ(logit)`，而 σ 是**多对一**的
    /// 浮点映射（相邻 logit 可能被舍入到同一 f32）⇒ 用 `score.to_bits()` 相等**不能**
    /// 证明 logit 逐位相同。这里直接读模型原始输出。
    ///
    /// ⚠️ **结论不预设**（设计 §4.7 P3「记录结论，不预设」）：`PaddingStrategy::BatchLongest`
    /// 让批内 padding 到该批最长 ⇒ 矩阵形状不同 ⇒ 理论上有差异空间。本用例把两种情形
    /// 的读数**都打印出来**，硬断言的是「同一组成可复现」；「两种组成是否相等」是**待观测**
    /// 的事实，读数落 `eval-report.md` §8.14。
    #[test]
    #[ignore = "需下载 ≈2.19GB 模型（local-rerank）"]
    fn p3_批组成敏感性读数() {
        const TARGET: &str = "BM25 是一种基于词频的检索算法，中文需要先分词";
        let r = LocalReranker::new().unwrap();
        let query = "如何实现支持中文的 BM25 检索";

        // 直接读**原始 logit**（绕开 σ 的多对一舍入）。
        let read_logit = |hits: Vec<Hit>| -> f32 {
            let docs: Vec<&str> = hits.iter().map(|h| h.text.as_str()).collect();
            let mut m = r.inner.lock().unwrap();
            let out = m.rerank(query, &docs, false, None).unwrap();
            out.iter()
                .find(|x| x.index == 0)
                .expect("必须含 index 0")
                .score
        };

        let single = read_logit(vec![hit(1, TARGET)]);
        let mut mixed = vec![hit(1, TARGET)];
        mixed.extend(
            (10..30).map(|i| hit(i, "完全无关的另一段文本，用于把批次撑到与单条不同的组成")),
        );
        let in_batch = read_logit(mixed);

        println!("P3 单条 logit = {single:?} / 批内 logit = {in_batch:?}");
        // 自证「两条路都真的跑到了」：单条也必须真的过模型（模块文档偏差 ② 的护栏）。
        assert!(
            single.is_finite() && in_batch.is_finite(),
            "读数必须是真值而非默认"
        );
        // 硬断言：同一组成可复现（P1 的最小内核）。
        let single2 = read_logit(vec![hit(1, TARGET)]);
        assert_eq!(
            single.to_bits(),
            single2.to_bits(),
            "同一批次组成必须逐位可复现（P1）"
        );
    }
}
