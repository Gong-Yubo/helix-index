# P1 BM25 链路 — 设计说明

> ⚠️ **历史文档，正文保留原名未改写**：项目已于 2026-09-03 正式定名 **HelixIndex**
> （库 crate `index-core` → `helix-core`，CLI 命令 `idx` → `helix`）。
> 文中出现的旧名均指改名前的同一项目，未逐处改写以保留历史原貌（D-E4）。

| 项目    | 内容                                                                                        |
| ----- | ----------------------------------------------------------------------------------------- |
| 版本    | **v1.1（已执行并回写）**                                                                          |
| 日期    | 2026-09-02                                                                                |
| 状态    | **已执行完成** —— 全部单测 + tantivy 对照通过，见第 14 章执行记录                                 |
| 上游    | `docs/devel/plan.md` 第 6 章（P1，T1-01 ~ T1-15）、`architecture-design.md` 第 5 / 6 / 7.1 / 7.2 节 |
| 关联    | ADR-001（手写 BM25）、ADR-006（TAAT）、ADR-007（tantivy 作 dev 基线）                                  |
| 环境    | Rust 1.90.0 / Apple M5 / P0 已完成（`dd7589c`）                                                |

---

## 1. P1 要达成什么

**一句话**：从零写出一条完整可用的**关键词检索链路**——分词 → 倒排 → 统计量 → BM25 打分 → Top-K，并让它在真实语料上跑出可解释的结果，同时证明自己算对了。

**判定标准（Gate）**

1. `idx build` + `idx search --mode bm25` 在示例语料上返回合理排序
2. BM25 分值与**手算值**逐条一致（第 8.1 节的真实数字）
3. 索引侧与查询侧分词**完全一致**
4. **与 `tantivy` 的 Top-K 排序对照通过**，差异 case 全部归因（ADR-007）
5. `make fmt && make lint && make test` 全绿，无 `clippy` 警告

> P1 是**关键路径的第一段**（P1 → P2 → P3）。它同时也是风险最集中的一段：BM25 是本项目唯一"自己实现的核心算法"，而**实现偏差不会报错，只会让效果慢慢变差**。所以 P1 的一半工作量在验证（T1-11 手算值 + T1-15 对照测试）。

---

## 2. 现状

| 项         | 状态                                                                       |
| --------- | ------------------------------------------------------------------------ |
| P0 产出     | workspace、依赖、`rust-toolchain.toml`(1.90.0)、Makefile、模块骨架（11 个空模块）        |
| `types.rs` / `document.rs` / `schema.rs` | ✅ 已在 P0 落地（T1-01 / T1-02 的数据模型部分）               |
| 未完成草稿     | `analyze/segment.rs`（中英分段）、`analyze/stopwords.rs`（停用词表）已写好含单测，但**未接入 `mod.rs`、未编译验证**，存于 `git stash@{0}` |
| 示例语料      | ❌ 无（T1-14 需要）                                                            |

---

## 3. 关键事实：tantivy 实现核对（源码级）

`/Users/gongyubo/Code/third/tantivy` 有完整源码，以下是**从源码读出**的结论（不是推断）：

### 3.1 公式与我们完全一致

`src/query/bm25.rs`：

```rust
const K1: Score = 1.2;
const B:  Score = 0.75;

pub(crate) fn idf(doc_freq: u64, doc_count: u64) -> Score {
    let x = ((doc_count - doc_freq) as Score + 0.5) / (doc_freq as Score + 0.5);
    (1.0 + x).ln()
}

weight = idf * (1.0 + K1);
norm   = K1 * (1.0 - B + B * dl / avgdl);
score  = weight * tf / (tf + norm);
```

与架构文档 7.2 定下的公式**数学等价**（我们写成 `idf * tf*(k1+1)/(tf + norm)`，只是因式分解的位置不同）。`avgdl = total_num_tokens / total_num_docs`，同样一致。

### 3.2 唯一系统性差异：tantivy 把 `dl` 量化成 8 bit

`src/fieldnorm/code.rs` 有一张 256 项的 `FIELD_NORMS_TABLE`，`fieldnorm_to_id()` 用二分查找映射，**且注释明说"not injective"（非单射）**。也就是说：

- tantivy 存的不是真实 `dl`，而是量化后的 `fieldnorm_id`
- 打分时再 `id_to_fieldnorm(id)` 还原，**不同的 `dl` 可能还原成同一个值**
- 因此 **分值不可能严格相等**，量化还会在 tantivy 侧**制造我们这边不存在的同分**

**这条结论直接决定了对照测试怎么设计（见第 8.3 节）：只能比排序，不能比分值。**

### 3.3 对设计的影响

| 发现                  | 影响                                                     |
| ------------------- | ------------------------------------------------------ |
| K1/B/idf 公式一致        | 排序应当高度一致；若差异大，说明**我们的实现有 bug**，这是 ADR-007 的价值所在     |
| `dl` 被量化且非单射        | 分值不可比；量化造成的同分属于**可解释差异**，不算实现问题                      |
| tantivy 默认不停用词      | 必须让两侧分词规则一致，否则差异无法归因（见 D5）                            |
| 多段（segment）会引入统计差异  | 对照测试中 tantivy 侧强制单线程单段写入，保证确定性                        |

---

## 4. 数据流

```
原始文档 (Document)
  │
  ├─ Chunker ──────────────► Vec<Chunk>（char 长度切分，byte 偏移记录）
  │
  └─ Analyzer.analyze_doc ─► Vec<Token>
                                 │
                                 ▼
                        InvertedIndex（term dict + postings）
                        ForwardStore（chunks + docs + 墓碑）
                        Stats（df / total_len / num_chunks）
                                 │
查询文本 ─► Analyzer.analyze_query ─► Vec<Token>
                                 │
                                 ▼
                     Bm25Retriever（TAAT 累加 → Top-K）
                                 │
                                 ▼
                        Vec<Scored{chunk_id, score}>
```

---

## 5. 数据结构选型

| 结构            | 选型                                            | 理由                                                                               |
| -------------- | --------------------------------------------- | -------------------------------------------------------------------------------- |
| 词项字典           | `HashMap<SmolStr, TermId>`                    | 查询时按词查 ID，必须 O(1)                                                                 |
| postings       | **`Vec<Vec<Posting>>`**（按 `TermId` 索引）         | ① **遍历顺序天然确定**，不依赖 HashMap 迭代序（NFR-06）；② 比 `HashMap` 省内存；③ 缓存友好            |
| `df`           | **`Vec<u32>`**（按 `TermId` 索引）                 | 同上                                                                               |
| 正排             | `Vec<Option<Chunk>>` + `Vec<Option<Document>>` | `None` 即墓碑；按 `ChunkId` O(1) 随机访问                                                  |
| 统计量            | `total_len: u64`、`num_chunks: u32`             | `avgdl` 现算                                                                       |
| `Token.term`   | `SmolStr`                                     | 短字符串 inline 优化，减少堆分配                                                             |

**关于确定性（NFR-06）**：凡是进入排序的路径，全部基于 `Vec` 顺序或 `chunk_id` 排序，**不允许直接依赖 HashMap 迭代顺序**。TAAT 累加时用 `Vec<f32>`（按 `ChunkId` 索引的分数数组）还是 `HashMap`？—— 用 **`HashMap` 累加 + 收集后按 `chunk_id` 排序**（因为只有 query term 命中的 chunk 才有分，稀疏），但排序时必须显式按 `chunk_id` 兜底。

---

## 6. 接口设计

```rust
// ---------- analyze ----------
pub struct Token {
    pub term: SmolStr,
    pub position: u32,      // 词序，供未来短语查询
    pub byte_start: usize,  // 原文字节偏移（O(1) 切片，非 char 索引）
    pub byte_end: usize,
}

pub trait Analyzer: Send + Sync {
    fn analyze_doc(&self, text: &str) -> Vec<Token>;
    /// 默认转发 analyze_doc —— 强制两侧一致（风险 R4）
    fn analyze_query(&self, text: &str) -> Vec<Token> { self.analyze_doc(text) }
}

pub struct MixedAnalyzer { jieba: Jieba, /* 过滤链配置 */ }

impl MixedAnalyzer {
    /// NFKC 归一化 → 中英分段 → 中文 jieba / 拉丁切词 → 过滤链
    fn analyze(&self, text: &str) -> Vec<Token>;
}

// ---------- chunk ----------
pub struct Chunker { chunk_chars: usize, overlap_chars: usize }
impl Chunker {
    pub fn chunk(&self, doc_id: DocId, text: &str) -> Vec<Chunk>;
}

// ---------- index ----------
pub struct Posting { pub chunk_id: ChunkId, pub tf: u32, #[cfg(feature="positions")] pub positions: Vec<u32> }

pub struct Index {
    terms: HashMap<SmolStr, TermId>,
    postings: Vec<Vec<Posting>>,
    chunks: Vec<Option<Chunk>>,
    docs: Vec<Option<Document>>,
    df: Vec<u32>,
    total_len: u64,
    num_chunks: u32,
}

impl Index {
    pub fn add(&mut self, doc: Document, chunks: Vec<Chunk>) -> Result<()>;
    pub fn remove(&mut self, doc_id: DocId) -> Result<()>;   // 见 D3
    pub fn stats(&self) -> Stats;
}

// ---------- retriever ----------
pub struct Scored { pub chunk_id: ChunkId, pub score: Score }

pub trait Retriever: Send + Sync {
    fn search(&self, query: &str, k: usize) -> Result<Vec<Scored>>;
}

pub struct Bm25Retriever<'a> { index: &'a Index, analyzer: &'a dyn Analyzer, k1: f32, b: f32 }
```

---

## 7. 任务设计

### T1-01 / T1-02 基础类型与文档模型
**已完成**（P0 落地）。仅补充 `Chunk.text` 与偏移的校验。

### T1-03 Chunker

- 输入 `doc_id` + 原文，输出 `Vec<Chunk>`
- 默认 **512 字（char）** + **64 字重叠**，优先在段落边界（`\n\n`）断开
- **偏移用 byte offset**：`&text[start..end]` 可 O(1) 切片；长度按 `char` 计数（符合" 512 字"的直觉）
- 单测：① 重叠部分内容一致；② 拼接后不丢字；③ `&text[start..end] == chunk.text`（含中文与 emoji）

### T1-04 ~ T1-07 分词链路

- **T1-04 分段**：`unicode-segmentation` 切中英段（草稿已写，需验证）
- **T1-05 过滤链**：NFKC 归一化 → 小写化 → 停用词 → 长度过滤（≥1 且 ≤ 32 字符）；**英文词干化不做**（架构 7.1）
- **T1-06 停用词**：草稿已在 `stopwords.rs`；保守裁剪，单字术语（云/网/库）必须保留
- **T1-07 `Analyzer` trait**：`analyze_query` 默认转发 `analyze_doc`
- 单测：中英混排、全角符号、emoji、空串、纯符号；**两侧一致性**

### T1-08 ~ T1-10 倒排 / 正排 / 统计量

- **T1-08**：term dict + postings（`Vec<Vec<Posting>>`），`positions` 走 feature
- **T1-09**：正排 + 墓碑
- **T1-10**：`df` / `total_len` / `num_chunks` 增量维护
- **D3：P1 就实现 `remove()`**（含统计量回滚）。理由见第 9 章
- 单测：**删除后统计量与全量重建逐项相等**（防 R5 漂移，架构 12.1 标记为"最关键测试"）

### T1-11 ~ T1-13 BM25 打分与检索

- **T1-11**：`idf` + `tf` 部分，OR 语义（未命中 term 贡献 0）
- **T1-12**：TAAT 累加 + Top-K，**同分按 `chunk_id` 升序 tie-break**
- **T1-13**：`Retriever` trait + `Bm25Retriever`
- 单测：手算值（第 8.1 节）、空 query、全停用词 query、`df=0` 的 term、超长 query

### T1-14 CLI 与示例语料

```bash
idx build  --input data/corpus.jsonl --output /tmp/index.idx
idx search --index /tmp/index.idx --mode bm25 -k 5 "查询文本"
```

- `build`：读 JSONL（`{"source","text","metadata"}`）→ Chunker → Analyzer → Index → **暂不落盘**（快照是 P4 的 T4-01，P1 阶段内存索引即可）
  - ⚠️ 这意味着 P1 的 `search` 必须与 `build` 同进程，否则索引丢了。方案：`idx search` 在 P1 **重新 build 再搜**（慢但可用），或加 `--rebuild` 语义。**P4 有快照后自然解决。**
- 示例语料：手写 **~30 篇**中文短文档（Rust / 检索 / BM25 / 向量 / 分词主题），每篇 300~800 字
  - ⚠️ 这是**冒烟语料不是评测语料**。P5 需要的 50~200 篇真实语料另需准备（T5-01）→ **已被 2026-09-03 决策取代**：评测数据改用开源 T2Ranking 转换（见 `p5-design.md` 第 3~5 章），30 篇冒烟语料定稿为 demo 语料

### T1-15 与 tantivy 对照测试

见 8.3 节（独立设计）。

---

## 8. 单测设计

### 8.1 BM25 手算值（**已用 Python 精确计算，非估测**）

构造 5 篇文档、4 个词项（**直接构造 postings，绕过分词**，纯算法验证）：

| chunk | 词项序列                      | dl |
| ----- | ------------------------- | -- |
| 0     | rust, rust, bm25          | 3  |
| 1     | rust, bm25, vector        | 3  |
| 2     | vector, vector, index     | 3  |
| 3     | index, rust               | 2  |
| 4     | bm25, index               | 2  |

`N = 5`，`total_len = 13`，`avgdl = 2.6`，`k1 = 1.2`，`b = 0.75`

```
df: rust=3, bm25=3, vector=2, index=3
idf: rust=0.538997, bm25=0.538997, vector=0.875469, index=0.538997
```

**期望分值**

| query            | chunk 0  | chunk 1  | chunk 2  | chunk 3  | chunk 4  | 期望排序（含 tie-break）   |
| ---------------- | -------- | -------- | -------- | -------- | -------- | ------------------- |
| `rust bm25`      | 1.217465 | 1.014164 | 0.000000 | 0.595185 | 0.595185 | **0, 1, 3, 4, 2**   |
| `vector`         | 0.000000 | 0.823632 | 1.153844 | 0.000000 | 0.000000 | **2, 1, 0, 3, 4**   |
| `index nonexist` | 0.000000 | 0.000000 | 0.507082 | 0.595185 | 0.595185 | **3, 4, 2, 0, 1**   |

> 三个 query 各有覆盖点：① `rust bm25` 中 **chunk 3 与 4 同分（0.595185）**，正好验证 `chunk_id` 升序的 tie-break；② `vector` 验证 OR 语义下未命中项为 0；③ `index nonexist` 验证 **`df=0` 的 term 不 panic 且不影响其余 term**（OR 语义）。

### 8.2 边界单测

| case        | 期望                                     |
| ----------- | -------------------------------------- |
| 空 query     | 返回空结果，不 panic                          |
| 全停用词 query  | 返回空结果（分词后无 token）                      |
| 超长 query    | 正常返回（不设长度上限）                           |
| 空索引         | 返回空结果                                  |
| 全部文档被删除     | 返回空结果，`avgdl` 不出现除零（`num_chunks == 0` 时直接返回空） |

### 8.3 T1-15：与 tantivy 对照测试的设计

**目标**：用工业实现当 oracle，验证"我们的 BM25 是对的"（ADR-007）。

**做法**

1. dev-dependencies 增加 **`tantivy-jieba = "0.20"`**（MIT）。已核对兼容性：`tantivy-jieba 0.20.0` 依赖 `tantivy-tokenizer-api ^0.7` 与 `jieba-rs ^0.10`，与我们的 `tantivy 0.26` / `jieba-rs 0.10` **完全同版本**，无冲突。
2. **让两侧分词规则一致**：tantivy 侧用 `tantivy-jieba` 分词，再挂一个**自定义 `TokenFilter`，复用我们自己的停用词表与过滤链**。这样变量收敛到只剩一个：**fieldnorm 量化**。
3. tantivy 侧强制**单线程单段**写入（避免多段统计差异），我们侧同时保证不依赖 HashMap 迭代序。
4. 对每条 query 各取 Top-10，计算：
   - **集合重叠率** = `|A ∩ B| / 10`（主指标）
   - **排序一致率** = 相同顺序的相邻对占比（次指标， Kendall tau 亦可）
5. **差异 case 必须逐条打印并归因**，归因分四类：
   - `fieldnorm-quantization`：tantivy 的 `dl` 量化导致（**预期内，非 bug**）
   - `tokenizer-diff`：分词不一致（应修正到一致）
   - `stopword-diff`：停用词不一致（应修正到一致）
   - `unexplained`：**必须人工查清，这类是我们的实现 bug 候选**

**阈值**：首次运行后按实测校准并写回本文档。**不得以"跑不过就放宽"的方式处理**——若 `unexplained` 类差异存在，先查实现。

> plan.md T1-15 原写"排序一致率 ≥ 95%"，考虑到 fieldnorm 量化必然引入噪声，实际阈值以首次实测校准为准；但**`unexplained` 类必须恒为 0**，这才是硬指标。

---

## 9. 需要你确认的决策点

| #   | 决策点                       | 我的建议                                                                                                                                      | 影响                                       |
| --- | ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------- |
| D1  | **偏移量用 byte 还是 char**     | **byte offset**（`&text[s..e]` O(1) 切片），长度按 char 计数                                                                                        | char 索引切片是 O(n)，且易与 byte 混用出错            |
| D2  | **postings 用 Vec 还是 HashMap** | **`Vec<Vec<Posting>>`**（按 TermId 索引）                                                                                                   | 确定性（NFR-06）+ 省内存；HashMap 迭代序不确定          |
| D3  | **`remove()` 是否提前到 P1**   | **提前**。P1 就实现删除 + 统计量回滚（原属 T4-05）                                                                                                        | ① 防漂移的关键单测能尽早跑起来；② 否则 P1 的索引长期"只增不删"，残缺 |
| D4  | **P1 是否落盘**               | **不落盘**。快照是 P4（T4-01）；P1 的 `idx search` 先重新 build 再搜                                                                                     | 避免重复实现；代价是 P1 的 CLI 检索要重建索引（30 篇语料 <1s）  |
| D5  | **对照测试的分词对齐**             | 加 dev 依赖 `tantivy-jieba`，并写自定义 TokenFilter 复用我们的停用词表                                                                                   | 多一个 dev 依赖（MIT）；换来差异可归因                  |
| D6  | **示例语料规模**                | P1 手写 **~30 篇**短文档作为冒烟语料；P5 的评测语料（50~200 篇真实文档）另行准备 → **已被 2026-09-03 决策取代**：改用 T2Ranking 转换，见 `p5-design.md`                                              | 30 篇只够冒烟，不足以得出任何效果结论                     |
| D7  | **对照阈值**                  | 首次实测后校准并写回本文档；硬指标是 **`unexplained` 差异 = 0**                                                                                             | 避免"数字不好看就放宽阈值"                           |

---

## 10. 风险与兜底

| #      | 风险                                     | 影响            | 兜底                                                                      |
| ------ | -------------------------------------- | ------------- | ----------------------------------------------------------------------- |
| R-P1-1 | `jieba_rs::Jieba` 不满足 `Send + Sync`    | `Analyzer` trait 约束不满足 | 用 `Mutex<Jieba>` 包裹（**已验证后再定**，P0 已把 jieba-rs 编入，可先跑一个小测试确认）     |
| R-P1-2 | jieba 默认词典对技术术语切分不理想（如 "BM25" 被拆）     | 召回下降         | ① 拉丁段不走 jieba（设计已隔离）；② 需要时加自定义词典（`jieba.add_word`）                     |
| R-P1-3 | 中文分块时 512 字切在词中间                       | 词被截断，召回下降     | 重叠 64 字可缓解；P5 视评测结果调整                                                    |
| R-P1-4 | 对照测试差异无法归因（分词/停用词不一致）                 | ADR-007 失去意义  | 按 D5 对齐；仍不行则**降低对照目标**为"仅核对 idf/tf 单项数值"，并如实记录                         |
| R-P1-5 | 手算值与实现差一点点（浮点结合律）                      | 单测偶发失败        | 用 `approx` 或自写 `assert!((a-b).abs() < 1e-5)`，不用 `assert_eq!`             |
| R-P1-6 | `remove()` 提前实现引入统计量漂移 bug             | 反噬 P4         | 这正是"删除后统计量与全量重建一致"单测要兜住的；P1 先写测试再写实现                                   |

---

## 11. 验收清单

```bash
cd /Users/gongyubo/Code/mine/index-demo
git stash pop                     # 恢复 P1-A 草稿（segment.rs / stopwords.rs）

cargo test -p index-core          # 全部单测通过，含手算值、分词一致性、统计量
cargo test -p index-core --test bm25_vs_tantivy   # 对照测试通过，无 unexplained 差异

cargo run -p idx -- build  --input data/corpus.jsonl --output /tmp/index.idx
cargo run -p idx -- search --index /tmp/index.idx --mode bm25 -k 5 "BM25 参数怎么调"
# 期望：5 条结果，带 source 与匹配词，分数递减

make fmt && make lint && make test && make deny   # 全绿
```

**并记录到附录**：对照测试的实测阈值、Top-10 重叠率、差异归因统计。

---

## 12. 执行批次

| 批次   | 任务                                        | 说明                    |
| ---- | ----------------------------------------- | --------------------- |
| 批次 1 | 恢复 stash + 验证草稿（T1-04 分段 / T1-06 停用词）      | 先把已写的跑通              |
| 批次 2 | T1-05 过滤链 + T1-07 Analyzer trait（P1-A 完成）  | 含两侧一致性单测             |
| 批次 3 | T1-03 Chunker（P1-B）                        | 偏移单测                 |
| 批次 4 | T1-08 ~ T1-10 倒排 / 正排 / 统计量 + `remove()`（P1-C） | 含删除后统计量单测       |
| 批次 5 | T1-11 ~ T1-13 BM25 + Top-K + Retriever（P1-D） | **手算值单测**          |
| 批次 6 | T1-14 CLI + 示例语料（P1-E）                     | 端到端跑通                |
| 批次 7 | T1-15 tantivy 对照测试（P1-F）                   | 阈值校准 + 差异归因          |

---

## 13. 明确不做（P1 范围外）

- **快照落盘**（P4 / T4-01）—— P1 的 `search` 每次重建索引
- 向量检索、融合、可解释性（P2 / P3）
- 元数据过滤（P4 / T4-07）
- posting 压缩（架构 7.6 明确 MVP 用朴素 `Vec`）
- DAAT / WAND（ADR-006 列为 P4 优化项）
- 真实评测语料与标注（P5 / T5-01、T5-02）
- 任何性能调优 —— P1 只保证正确，不保证快

---

## 14. 执行记录（v1.1 新增）

### 14.1 实测结果

| 验收项 | 结果 |
| --- | --- |
| 单元测试 | **29 个全部通过**（analyze 15 + chunk 6 + index/stats 4 + retriever 4） |
| tantivy 对照 | **7 个 query 的 Top-10 重叠率全部 = 1.00**，排序完全一致 |
| `make fmt` / `lint` / `test` / `deny` | 全绿（clippy `-D warnings` 零告警） |
| CLI 端到端 | `idx build` + `idx search --mode bm25` 在 30 篇语料上返回合理排序 |

### 14.2 与设计的三处细化（均已落地，非方向性调整）

1. **D5 细化：对照测试不引入 `tantivy-jieba`**。最终采用更聚焦的方案——**把我们的分词结果（已 lower、已去停用词）用空格拼接，喂给 tantivy 的 `WhitespaceTokenizer`**，使两侧 token 完全一致，变量收敛到只剩 fieldnorm 量化。分词正确性由 analyze 模块单测独立保证，真实语料端到端对照放到 P5（T5-04）。**好处**：少一个 dev 依赖，且测试目标更精确（只验证打分数学）。
   - 实测 tantivy 0.26 API 与文档假设有两处差异：`TopDocs::with_limit(n)` 需再调 `.order_by_score()` 才是 Collector；`Searcher::doc` 是泛型需显式 `doc::<TantivyDocument>`，取字段要 `use tantivy::schema::Value` 后调 `.as_u64()`。

2. **`df` 不单独存**。设计稿写 `df: Vec<u32>`，实现改为**由 `postings[t].len()` 直接推导**——删除时同步移除 posting，`df` 自动一致，从根本上杜绝"df 与 postings 漂移"这个风险 R5 的隐患。

3. **0 分文档不返回**。手算表把分数 0.000000 的文档也列进了"期望排序"，但实现（正确地）不返回零分文档。测试断言已改为只校验非零结果，并明确了这一点：BM25 中不含任何 query term 的文档得分恒为 0，没有返回的必要。

### 14.3 过程中自行解决的小问题

- `jieba_rs::Jieba::cut()` 返回 `Vec<jieba_rs::Token>` 而非 `Vec<String>`，取词用 `tok.word`
- `Jieba` 满足 `Send + Sync`（R-P1-1 排除，无需 `Mutex`）
- `moka` / `bincode` 已在前一轮 P0 修正，此处无新问题
- 测试断言曾把分词顺序写错（分段保持原文顺序，属正确行为），修正的是断言而非实现

### 14.4 遗留与交接

- `Token.byte_start` / `byte_end` 当前置 0（P1 不消费），P3 实现 FR-12 精确引用时补上"归一化文本 → 原文"的偏移映射
- `Document.content_hash` 当前置 0，P4（T4-04）做幂等 upsert 时补
- `data/corpus.jsonl` 是 30 篇**冒烟语料**，P5 需 50~200 篇真实语料（T5-01）→ **已被 2026-09-03 决策取代**：评测数据改用 T2Ranking 转换（`p5-design.md`），本语料定稿为 demo 语料

---

## 附录 A 手算值复现脚本

```python
import math
docs = {0: ["rust","rust","bm25"], 1: ["rust","bm25","vector"],
        2: ["vector","vector","index"], 3: ["index","rust"], 4: ["bm25","index"]}
K1, B = 1.2, 0.75
dl = {d: len(t) for d, t in docs.items()}
N = len(docs); avgdl = sum(dl.values())/N
from collections import Counter
tf = {d: Counter(t) for d, t in docs.items()}
df = Counter()
for c in tf.values():
    for t in c: df[t] += 1
idf = lambda t: math.log(1 + (N - df[t] + 0.5)/(df[t] + 0.5))
def score(d, q):
    s = 0.0
    for t in set(q):
        if t not in tf[d]: continue          # OR 语义
        norm = K1*(1-B+B*dl[d]/avgdl)
        s += idf(t) * tf[d][t]*(K1+1)/(tf[d][t]+norm)
    return s
for q in (["rust","bm25"], ["vector"], ["index","nonexistent"]):
    print(q, sorted(((d, round(score(d,q),6)) for d in docs), key=lambda x: (-x[1], x[0])))
```

## 附录 B tantivy 源码对照位置

| 内容                          | 文件                                             |
| --------------------------- | ---------------------------------------------- |
| `K1=1.2` / `B=0.75` / `idf` | `src/query/bm25.rs`                            |
| `weight = idf*(1+K1)`        | `src/query/bm25.rs:159`（`Bm25Weight::new`）      |
| `tf_factor`                  | `src/query/bm25.rs:189`（`tf / (tf + norm)`）     |
| fieldnorm 量化表（非单射）          | `src/fieldnorm/code.rs`（`FIELD_NORMS_TABLE`）    |
| `avgdl = total_tokens/total_docs` | `src/query/bm25.rs:111`                    |
