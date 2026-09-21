//! 本地 Embedder：fastembed + `Xenova/bge-small-zh-v1.5`（512 维）。
//!
//! - 查询侧加 BGE instruction 前缀，入库侧不加（风险 R2）
//! - 输出已 L2 归一化（fastembed 源码 `output.rs:49` 的 `.map(normalize)` 佐证）
//! - 缓存目录指到仓库外，避免误提交模型（p0-design.md 12.4 的遗留 TODO）

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use crate::error::{Error, Result};

use super::{normalize_sessions, Embedder};

/// BGE 中文查询侧的官方 instruction 前缀。
pub const BGE_ZH_QUERY_PREFIX: &str = "为这个句子生成表示以用于检索相关文章：";

/// 默认缓存目录（仓库外）。
///
/// ⚠️ `pub(crate)` 是给 **`rerank::local`** 复用的（设计 §4.4.1）：精排与向量化必须
/// 用**同一个**缓存根，否则两处路径漂移会让「模型下到哪去了」变成谜。
pub(crate) fn default_cache_dir() -> std::path::PathBuf {
    dirs_home()
        .join(".cache")
        .join("helix-index")
        .join("models")
}

/// 解析用户主目录（无 `dirs` 依赖，读环境变量兜底）。
fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// 轮转取槽的下标（**纯函数**，便于在无模型环境下单测）。
///
/// # 为什么是「轮转」而不是「随机 / 最短队列」
///
/// - **随机**需要 RNG ⇒ 引入依赖与不确定性（NFR-06）；
/// - **最短队列**要读所有槽的状态 ⇒ 又变回「全局共享状态」；
/// - **轮转**只读一个 `AtomicUsize`（无锁）、代价为常数，且**必然**把并发请求摊到不同槽上
///   —— 唯一要防的是「总撞同一把锁」，轮转即可。
///
/// ⚠️ 取模前先把计数器收敛（`% len` 后再加）⇒ **不会**因 `usize` 溢出而回绕到负/异常值
/// （`fetch_add` 的返回值先 `% len`，理论上的溢出只是回绕到 0，最多让一次调度「不轮转」）。
#[inline]
fn slot_index(next: &AtomicUsize, len: usize) -> usize {
    debug_assert!(len > 0, "池至少有一个槽（构造时已规范化）");
    next.fetch_add(1, Ordering::Relaxed) % len
}

/// 本地 embedding 实现：**会话池**（`Vec<Mutex<TextEmbedding>>` + 轮转取槽）。
///
/// # 为什么是「池」（`S8-08` / `D-S8-11`）
///
/// `TextEmbedding::embed` 要 `&mut self` ⇒ 单实例必须用一把 `Mutex` 串起来：
/// **查询侧编码在并发下被完全串行化**（架构 **R43**；Step 6 实测 hybrid 4 线程只有
/// **1.63×**）。池化让不同线程各锁**各自**的槽 ⇒ 编码可并行。
///
/// # 默认 1 ⇒ **零行为变化**
///
/// `new()` 就是 `new_with_sessions(1)`：池里只有一个槽，取槽恒返回 0
/// ⇒ 与「单个 `Mutex<TextEmbedding>`」**逐位等价**（只是外面多包了一层 `Vec`）。
/// `Config::embed_sessions` 的默认值同样是 1（`Q4`：**库侧默认不动**）。
///
/// # 代价（如实登记）
///
/// 每个槽都是**独立的 ONNX `Session` + `Tokenizer`** ⇒ 构造耗时 **N ×**、
/// 常驻内存 **N ×**。模型权重文件走**同一个** `default_cache_dir()`（不重复下载），
/// 但会话各自的运行时缓冲（激活张量）会叠加 ⇒ 内存门槛由 spike **S8-S2** 实测
/// （判据③：相对 `sessions = 1` 的峰值 RSS 增量 ≤ 20%）。
///
/// # ⚠️ 槽之间必须**同构**
///
/// 所有槽用**同一份** `InitOptions`（含 `intra_threads` 的默认值）—— 这是
/// **Q3′ 的前置**：若给不同槽设不同 `intra_threads`，同一 query 在不同并发度下可能
/// 拿到**不同的向量**，那会破坏 `NFR-10` ① 的「逐位一致」前置条件。
/// ⇒ **实现上刻意不给每槽分化参数**；`intra_threads` 是否影响数值由 spike S8-S2 实测
/// （`spike_s8s2.rs` 的判据①c，走了**非生产路径**的对照）。
pub struct LocalEmbedder {
    pool: Vec<Mutex<TextEmbedding>>,
    /// 轮转游标（写端唯一共享状态；读端永不触碰索引数据）
    next: AtomicUsize,
    dim: usize,
}

impl LocalEmbedder {
    /// 构造**单会话**实例（= `new_with_sessions(1)`）。首次会触发模型下载（约 49s）。
    pub fn new() -> Result<Self> {
        Self::new_with_sessions(1)
    }

    /// 构造 **N 会话**实例（`N = Config::embed_sessions`）。
    ///
    /// `sessions == 0` 归一为 **1**（见 `super::normalize_sessions`）。
    /// ⚠️ 构造 N 份 ONNX 会话 ⇒ 耗时与常驻内存均 **N ×**（模型权重共享缓存目录、不重复下载）。
    pub fn new_with_sessions(sessions: usize) -> Result<Self> {
        let sessions = normalize_sessions(sessions);
        let mut pool = Vec::with_capacity(sessions);
        for _ in 0..sessions {
            // 每份都从**同一** `InitOptions` 出发 ⇒ 槽同构（见类型文档的「槽之间必须同构」）。
            let model = TextEmbedding::try_new(
                InitOptions::new(EmbeddingModel::BGESmallZHV15)
                    .with_cache_dir(default_cache_dir())
                    .with_show_download_progress(false),
            )
            .map_err(|e| Error::Embedding(e.to_string()))?;
            pool.push(Mutex::new(model));
        }

        // 取模型维度（bge-small-zh-v1.5 = 512；`get_model_info` 是关联函数）
        let dim = TextEmbedding::get_model_info(&EmbeddingModel::BGESmallZHV15)
            .map(|info| info.dim)
            .unwrap_or(512);

        Ok(Self {
            pool,
            next: AtomicUsize::new(0),
            dim,
        })
    }

    /// 池长（= 生效的 `embed_sessions`）。供装配层与测试观测「规范化真的生效了」。
    pub fn sessions(&self) -> usize {
        self.pool.len()
    }

    /// 轮转取一个槽（只锁那一个）。
    fn slot(&self) -> std::sync::MutexGuard<'_, TextEmbedding> {
        let i = slot_index(&self.next, self.pool.len());
        self.pool[i].lock().expect("embedder 锁已中毒")
    }
}

impl Embedder for LocalEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut model = self.slot();
        model
            .embed(texts, None)
            .map_err(|e| Error::Embedding(e.to_string()))
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let prefixed = format!("{BGE_ZH_QUERY_PREFIX}{text}");
        let mut model = self.slot();
        let mut out = model
            .embed(vec![prefixed], None)
            .map_err(|e| Error::Embedding(e.to_string()))?;
        Ok(out.pop().expect("单条查询应返回一个向量"))
    }

    fn is_normalized(&self) -> bool {
        true
    }

    fn id(&self) -> &'static str {
        "bge-small-zh-v1.5"
    }
}

#[cfg(test)]
mod tests {
    // 与 `search/config.rs` 的 `mod tests` 及 `tests/*.rs` 一致：用例名用 `S8_08_…` / `S8_T16_…`
    // 的编号前缀（本仓惯例），故显式关掉 snake_case 检查。
    #![allow(non_snake_case)]
    use super::*;

    /// **`S8-08` 的轮转取槽判据**（无模型，进 CI）。
    ///
    /// # 它在钉什么
    ///
    /// 池化的**唯一**目的就是把并发请求摊到不同槽上（不摊开 = 与单槽等价 = 白付 N 份内存）。
    /// ⇒ 「轮转」是这条优化的**全部机制**，必须逐值钉住。
    ///
    /// ⚠️ **期望序列刻意写成「不是全 0」**：若把实现改成恒返回 0（= 退化成单槽），
    /// 本用例必须红 —— 这是「池化还在，但已经不轮转了」的唯一绊线
    /// （其它判据只看「结果对不对」，而恒 0 的结果**照样是对的**）。
    #[test]
    fn S8_08_轮转取槽把并发摊到不同槽且单槽时恒为0() {
        // ① 4 槽 ⇒ 连续取 8 次应是 0,1,2,3,0,1,2,3
        let next = AtomicUsize::new(0);
        let got: Vec<usize> = (0..8).map(|_| slot_index(&next, 4)).collect();
        assert_eq!(got, vec![0, 1, 2, 3, 0, 1, 2, 3], "必须严格轮转");
        assert_ne!(got, vec![0usize; 8], "🔴 恒 0 = 退化成单槽（池化名存实亡）");

        // ② 单槽 ⇒ 恒 0（= 与「单个 Mutex<TextEmbedding>」的行为逐位等价）
        let next = AtomicUsize::new(0);
        let got: Vec<usize> = (0..5).map(|_| slot_index(&next, 1)).collect();
        assert_eq!(got, vec![0; 5], "sessions = 1 ⇒ 恒取 0 号槽（零行为变化）");

        // ③ 并发下每个槽都被用到（这条才是「摊开」的证据：不轮流用就不算摊开）
        let next = AtomicUsize::new(0);
        let mut seen = [0usize; 4];
        for _ in 0..40 {
            seen[slot_index(&next, 4)] += 1;
        }
        assert_eq!(seen, [10, 10, 10, 10], "40 次取槽应均匀落在 4 个槽上");
    }

    /// 会话数规范化：`0` ⇒ `1`（不静默造出空池）。
    #[test]
    fn S8_08_会话数零归一为一() {
        assert_eq!(normalize_sessions(0), 1, "0 是无意义输入 ⇒ 归一为 1");
        assert_eq!(normalize_sessions(1), 1);
        assert_eq!(normalize_sessions(4), 4);
    }

    /// **`S8-T16` 判据①（本地、需真模型）**：`sessions = 1` 与 `sessions = 4`
    /// 对**同一批** query 必须给出**逐位相同**的向量（`to_bits()` 相等）。
    ///
    /// # 为什么这条是「投不投」的前置
    ///
    /// `NFR-10` ① 的「逐位一致」是并发判据的**前置条件**：若同一 query 在不同并发度下
    /// 拿到不同向量，池化会**破坏正确性判据本身**（而不是只影响性能）⇒ 按 `D-S8-11`
    /// 必须判「**不投**」。
    ///
    /// # 与 `spike_s8s2.rs` 的分工
    ///
    /// 计时的部分（判据②③）在 example 里做（要独立进程 + 外置 `time -l` 采 RSS）；
    /// **这一条只判「值」，不需要计时** ⇒ 放在这里当可复跑的判据，
    /// 需要时 `cargo test -p helix-core --lib -- --ignored` 唤醒。
    #[test]
    #[ignore = "需本地模型（bge-small-zh-v1.5）+ 4 份 ONNX 会话"]
    fn S8_T16_判据一_会话数不改变向量数值() {
        let queries = [
            "并发检索的可见性边界",
            "合并期间读端不得被阻塞",
            "倒排索引的统计数据怎么合并",
            "向量检索的召回率",
            "commit 之后立即可查",
        ];

        let one = LocalEmbedder::new_with_sessions(1).unwrap();
        let four = LocalEmbedder::new_with_sessions(4).unwrap();
        assert_eq!((one.sessions(), four.sessions()), (1, 4), "前提：池长生效");
        assert_eq!(one.dim(), four.dim(), "dim 不得随会话数变");

        for q in queries {
            let a = one.embed_query(q).unwrap();
            let b = four.embed_query(q).unwrap();
            assert_eq!(a.len(), b.len());
            // 逐位比较（`f32::to_bits`）：不是「近似相等」——「逐位一致」是本判据的全部内容
            let same = a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits());
            assert!(
                same,
                "🔴 会话数改变了 embed_query 的向量数值（query = {q:?}）⇒ 按 D-S8-11 必须判「不投」"
            );
        }

        // 入库侧同样要比（池化对两条路径都生效）
        let texts: Vec<String> = queries.iter().map(|q| (*q).to_string()).collect();
        let da = one.embed_documents(&texts).unwrap();
        let db = four.embed_documents(&texts).unwrap();
        for (i, (x, y)) in da.iter().zip(&db).enumerate() {
            assert!(
                x.iter().zip(y).all(|(p, q)| p.to_bits() == q.to_bits()),
                "🔴 会话数改变了 embed_documents 的向量数值（第 {i} 条）"
            );
        }
    }

    /// **`S8-08`**：`new_with_sessions(0)` 必须归一为**单槽**（而不是造出空池再在
    /// 第一次 `embed_query` 时除零 / 越界）。
    ///
    /// ⚠️ 这是**第二道防线**：装配链路（`build_config`）已经会把 `0` 归一 ——
    /// 但 `LocalEmbedder::new_with_sessions` 是**公开构造**，调用方可以直接传 0。
    #[test]
    #[ignore = "需本地模型（bge-small-zh-v1.5）"]
    fn S8_08_零会话构造归一为单槽() {
        assert_eq!(
            LocalEmbedder::new_with_sessions(0).unwrap().sessions(),
            1,
            "0 是无意义输入 ⇒ 归一为 1（公开构造也必须自守，不能只在装配层守）"
        );
    }

    /// ⚠️ 池化后**同一个实例**轮转 4 个槽，也必须给出同一结果（钉「槽同构」）。
    #[test]
    #[ignore = "需本地模型（bge-small-zh-v1.5）+ 4 份 ONNX 会话"]
    fn S8_T16_判据一_同实例内四个槽互相一致() {
        let e = LocalEmbedder::new_with_sessions(4).unwrap();
        // 连取 4 次 ⇒ 必然轮过 4 个槽（见 `S8_08_轮转取槽…`）
        let vs: Vec<Vec<f32>> = (0..4)
            .map(|_| e.embed_query("槽同构检查").unwrap())
            .collect();
        for (i, v) in vs.iter().enumerate().skip(1) {
            assert!(
                vs[0].iter().zip(v).all(|(a, b)| a.to_bits() == b.to_bits()),
                "🔴 第 {i} 个槽与 0 号槽给出不同向量 ⇒ 槽不同构（每槽的 InitOptions 必须一致）"
            );
        }
    }

    /// 依赖模型下载，默认跳过：`cargo test -- --ignored`
    #[test]
    #[ignore]
    fn 查询侧前缀生效() {
        let e = LocalEmbedder::new().unwrap();
        let doc = e.embed_documents(&["检索".to_string()]).unwrap();
        let query = e.embed_query("检索").unwrap();
        // R2 防护：同一文本，查询侧与入库侧向量必须不同（前缀存在）
        assert_ne!(doc[0], query);
        assert_eq!(e.dim(), 512);
        assert!(e.is_normalized());
    }

    /// 依赖模型下载，默认跳过。
    #[test]
    #[ignore]
    fn 输出已归一化() {
        let e = LocalEmbedder::new().unwrap();
        let v = e.embed_documents(&["测试文本".to_string()]).unwrap();
        let norm: f32 = v[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3);
    }
}
