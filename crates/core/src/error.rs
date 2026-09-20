//! 错误类型。
//!
//! 设计原则：**配置错误要在最早时刻暴露**，而不是让错误数据静默进入索引。
//! `NoEmbedder` 与 `DimensionMismatch` 就是为此存在的——前者表现为"跑不起来"，
//! 后者若不拦截则表现为"检索效果莫名其妙地差"，排查成本极高。

use thiserror::Error;

/// index-core 的统一错误类型
#[derive(Debug, Error)]
pub enum Error {
    /// 快照文件读写失败
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    /// 快照编码失败
    #[error("序列化错误: {0}")]
    Codec(#[from] bincode::error::EncodeError),

    /// 快照解码失败（版本不符或内容损坏会先被前两级拦下）
    #[error("反序列化错误: {0}")]
    Decode(#[from] bincode::error::DecodeError),

    /// 快照由更高/更低版本的内核写出，无法安全读取
    #[error("快照版本不兼容: 文件版本 {found}, 支持版本 {expected}")]
    SnapshotVersionMismatch {
        /// 文件中的版本号
        found: u32,
        /// 当前内核支持的版本号
        expected: u32,
    },

    /// 快照 CRC 校验失败，文件已损坏
    #[error("快照校验失败，文件可能损坏")]
    SnapshotCorrupted,

    /// 入库向量与索引既有向量维度不一致
    #[error("向量维度不匹配: 期望 {expected}, 实际 {found}")]
    DimensionMismatch {
        /// 索引期望的维度
        expected: usize,
        /// 实际传入的维度
        found: usize,
    },

    /// 嵌入模型初始化或推理失败
    #[error("嵌入模型错误: {0}")]
    Embedding(String),

    /// 未启用任何 Embedder 实现，请开启 local-embed 或 remote-embed feature
    #[error("未启用任何 Embedder 实现，请开启 local-embed 或 remote-embed feature")]
    NoEmbedder,

    /// 精排模型初始化或推理失败（V2 Step 7 / S7-01）
    ///
    /// ⚠️ 与 [`Self::Embedding`] 分开是刻意的：两者的**落点与代价**完全不同
    /// （embedder 96MB / 精排 2.19GB），混在一起会让「哪一步炸了」只能靠读字符串猜。
    #[error("精排模型错误: {0}")]
    Rerank(String),

    /// 分词器内部错误
    #[error("分词错误: {0}")]
    Analyze(String),

    /// 引用的分片 ID 不存在（已删除或越界）
    #[error("分片不存在: {0}")]
    ChunkNotFound(u32),

    /// 调用方传入的参数不合法（如 judgments 格式错误、权重数量不对）
    #[error("非法输入: {0}")]
    InvalidInput(String),

    /// 资源正忙（**暂态、可重试**）：当前有并发读者持有视图快照，写入需要独占。
    ///
    /// 🔑 与 [`Self::InvalidInput`] 分开是刻意的（`S8-02` 评审 P4-5a，同 [`Self::Embedding`]
    /// 与 [`Self::Rerank`] 分开的理由）：这是**瞬态并发状态**（调用方**稍后重试即可**），
    /// 不是参数错误（重试无用）。混在一起 ⇒ 调用方无法在「重试」与「报错退出」之间决策。
    ///
    /// # 产生点与寿命（2026-09-19 按 `S8-03` 批次的**第 2 轮**评审同步，P3-4.1 + P2-1 + P1-2）
    ///
    /// ⚠️ 原文写「`S8-02` 的过渡约束是唯一产生点，`S8-03` 后内核不再产生该错误」——
    /// **该说法已作废**：过渡约束（`with_main_mut`）确实已随 `S8-03` 删除，但同一批次引入了
    /// **三条新的产生点**（同一 `Arc<Shared>` 上可有多个写端 —— `Searcher::into_index()` 总是成功）：
    ///
    /// | 产生点 | 触发条件 | 语义 | 丢失的是 |
    /// | --- | --- | --- | --- |
    /// | **基址对账**（`commit()`） | 另一个写端在本写端 `add` 之后 `commit()` 过 | 段内 ID 与全局 ID 已不对应 | 本写端 builder 里**未提交的内容 + 墓碑** |
    /// | **世代对账**（`commit()` **或** `fold_deltas()`） | 另一个写端 `compact()` 过（重编号 ⇒ 旧 ID 空间作废） | 基址**等值也拦不住**（ABA），故按世代判 | **视调用方而定**：`commit()` ⇒ 同上；`fold_deltas()` ⇒ **只有本次合并的计算**（内容仍在视图里） |
    /// | **快照前缀身份**（`fold_deltas()`，P1-2） | 另一个写端 `fold` 过（增量段列表被**替换**，长度可能相同） | 世代**不变**（`fold` 不换代）⇒ 世代对账拦不住 | **只有本次合并的计算**（内容仍在视图里） |
    ///
    /// 🔴 **只有 `commit()` 的两条不满足「重试即可」的承诺**：本写端 builder 里**未提交的内容
    /// 已被丢弃**（`commit()` 末尾无条件换新 builder）⇒ 重试**不会**恢复它，必须**重新执行未成功的
    /// `add` 与 `remove`**（`remove` 也要重做 —— 分支②的跨段墓碑同样在 builder 上，P3-1）。
    /// ⚠️ **凡 `fold` 走的路径都不同**（`fold_deltas` 自己的**前缀身份**拒绝，**以及**它调
    /// `commit_view` 时的**世代**拒绝）：`fold` 是**维护性**操作，被判拒时增量段**仍在视图里、
    /// 内容没丢**（且它**不碰** `builder` ⇒ builder 会保持过期，直到下一次 `commit()` 顺手刷新），
    /// 恢复方式就是**重试本次 `save()` / `compact()`**。
    /// 🔑 **世代对账那一行曾经把两个调用方并成一条**（写「丢失的是 = 同上」），而这对 `fold` 侧
    /// **三条声明全不成立**（`S8-03` **第 3 轮评审 P4**：① 「未提交内容已被丢弃」在 fold 时刻
    /// 早已被 `save()` 开头的 `commit()` 按设计提交；② `fold_deltas` 不碰 `builder`，它**保持过期**、
    /// 并非「已换成新 builder」；③ 正解是重试 `save()`/`compact()`）⇒ 现由 `PublishCaller`
    /// （`search/view.rs`）**按调用方分家**，判据 = `S8_03_epoch拒绝的恢复指引按调用方分家`。
    /// 真正的「无损重基」（保留 builder、把 `base_*` 对齐到锁内真实值后重新发布）仍未实现。
    #[error("资源忙（暂态，可重试）: {0}")]
    Busy(String),

    /// 快照加载时装配与建库时不一致（p6-design 8.2，修 B1/B2）
    #[error("配置不匹配: 快照期望 {expected}，当前装配 {actual}")]
    ConfigMismatch {
        /// 快照记录的配置指纹（建库时）
        expected: String,
        /// 当前装配的配置指纹
        actual: String,
    },

    /// 向量图 sidecar 读写失败（hnsw_rs 落盘/加载、manifest 编解码）
    #[error("向量图读写失败: {0}")]
    VectorGraph(String),

    /// 图 sidecar 与快照不匹配（缺失/损坏/版本不符）。图是派生缓存，
    /// 默认降级重建（警告输出）；仅 strict 模式（D-S2-04）作为 Err 返回。
    #[error("向量图不可用（{reason}），已降级为加载后重建")]
    GraphStale {
        /// 降级原因（可观测，NFR-07：降级不能静默）
        reason: String,
    },
}

/// 统一结果类型
pub type Result<T> = std::result::Result<T, Error>;
