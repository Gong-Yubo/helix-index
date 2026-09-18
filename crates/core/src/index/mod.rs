//! 倒排索引、正排存储与统计量维护。
//!
//! # 边界（架构文档 4.2）
//!
//! 本模块**不碰融合与排序**，只负责"把词项映射到分片"这件事。
//! 统计量（`total_len` / `num_chunks`）必须增量维护，且与全量重建一致（风险 R5）。
//!
//! # 设计取舍
//!
//! - `Index` **不持有 `Analyzer`**：它是纯数据结构；`add` / `remove` 由调用方
//!   传入 `&dyn Analyzer`，分词策略与索引解耦。
//! - `df` 不单独存，由 `postings.len()` 推导（见 `inverted.rs`），杜绝漂移。

mod field_index;
mod forward;
mod inverted;
mod posting;
mod stats;

pub use field_index::{json_to_string, FieldIndex, DEFAULT_MAX_VALUES_PER_FIELD};
pub use forward::ForwardStore;
pub use inverted::InvertedIndex;
pub use posting::Posting;
pub use stats::Stats;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::analyze::{Analyzer, Token};
use crate::document::{Chunk, DocRecord};
use crate::error::Result;
use crate::types::{ChunkId, DocId, TermId};

/// 内存索引：倒排 + 正排 + 统计量。
#[derive(Debug, Clone, Default)]
pub struct Index {
    inverted: InvertedIndex,
    forward: ForwardStore,
    stats: Stats,
    /// 每个分片的 term 数（dl），按 `ChunkId` 索引；删除后保留（ID 不复用）
    chunk_lens: Vec<u32>,
    /// content_hash → doc_id，幂等 upsert 去重用（FR-15）
    content_hashes: HashMap<u64, DocId>,
    /// 字段索引：field → value → doc 位图（过滤下推，FR-27）
    ///
    /// **不入快照**：可由 `docs` 的 metadata 全量重建，因此 `FORMAT_VERSION` 无需升版。
    /// 副作用是**基数阈值也不随快照持久化**——`import` 会用默认阈值重建（见
    /// [`Index::with_max_values_per_field`]）。
    field_index: FieldIndex,
}

/// compaction 用的 ID 重映射表（D-S4-01：重编号）。
///
/// - `chunk[old_id] = Some(new_id)`：该 chunk 存活且被重编号；`None` = 墓碑。
/// - `doc[old_id] = Some(new_id)`：同上（doc 级）。
///
/// 由 [`Index::build_remap`] 从存活位图生成，**按旧 id 升序遍历分配 new id** ⇒ 相对顺序
/// 保持（这是 §4.9 中「BM25 逐位一致 I3」的基石：postings 链保序 + remap 单调）。
pub(crate) struct IdRemap {
    pub chunk: Vec<Option<ChunkId>>,
    pub doc: Vec<Option<DocId>>,
}

/// 快照的索引部分（storage 序列化的数据源）。
///
/// `term_dict` **按 TermId 升序**、`content_hashes` **按 hash 升序**导出，
/// 保证快照字节流确定（NFR-06）且 round-trip 后 TermId / ID 映射稳定。
///
/// ⚠️ `DocRecord.metadata` 是 `serde_json::Value`，其序列化走 `serialize_any`，
/// **bincode 2 不支持**（非自描述格式）——因此快照里用 `DocumentDto`
/// 把 metadata 存成 JSON 字符串，导入时再解析回来。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotSections {
    /// 词项 → TermId 映射（导出时按 TermId 升序，防跨进程漂移）
    pub term_dict: Vec<(String, TermId)>,
    /// 每个 TermId 对应的倒排链
    pub postings: Vec<Vec<Posting>>,
    /// 正排文档表（`None` = 已删除的墓碑位）
    pub docs: Vec<Option<DocumentDto>>,
    /// 正排分片表（`None` = 已删除的墓碑位）
    pub chunks: Vec<Option<Chunk>>,
    /// 每个分片的分词数，供 avgdl 与 BM25 长度归一化
    pub chunk_lens: Vec<u32>,
    /// 语料级统计量（文档数、分片数、词项总数、avgdl）
    pub stats: Stats,
    /// content_hash → DocId（幂等 upsert 用，FR-15；按 hash 升序导出）
    pub content_hashes: Vec<(u64, DocId)>,
}

/// 快照专用的文档 DTO（metadata 序列化为 JSON 字符串）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentDto {
    /// 文档 ID
    pub doc_id: DocId,
    /// 出处（溯源用，FR-12）
    pub source: String,
    /// 业务元数据的 JSON 文本（`serde_json::Value` 无法直接进 bincode 2）
    pub metadata_json: String,
    /// 内容哈希（幂等 upsert，FR-15）
    pub content_hash: u64,
}

impl From<&DocRecord> for DocumentDto {
    fn from(d: &DocRecord) -> Self {
        Self {
            doc_id: d.doc_id,
            source: d.source.clone(),
            metadata_json: d.metadata.to_string(),
            content_hash: d.content_hash,
        }
    }
}

impl From<DocumentDto> for DocRecord {
    fn from(d: DocumentDto) -> Self {
        Self {
            doc_id: d.doc_id,
            source: d.source,
            metadata: serde_json::from_str(&d.metadata_json).unwrap_or(serde_json::json!({})),
            content_hash: d.content_hash,
        }
    }
}

impl Index {
    /// 创建空索引。
    pub fn new() -> Self {
        Self::default()
    }

    /// 用**自定义字段索引基数上限**创建空索引（默认 [`DEFAULT_MAX_VALUES_PER_FIELD`]）。
    ///
    /// # 为什么需要这个入口
    ///
    /// 单字段的不同取值超过上限即**永久降级**（`FieldIndex` 的 `degraded` 粘滞），
    /// 该字段上的所有过滤退回 O(N) 全扫。默认 1024 覆盖 tag / 类别 / 状态这类维度，
    /// 但**高基数字段正是最常见的真实过滤场景**——毫秒时间戳、uuid、trace_id 都会
    /// 立刻撞线，于是 Q-I1 的优化对它们整体失效。
    ///
    /// 若业务确需在此类字段上做 Range / Eq 过滤，可用本入口调高上限；代价是
    /// 「每个 value 一张 doc 位图」的内存随基数线性增长（记入 NFR-05）。
    ///
    /// ⚠️ 阈值**不随快照持久化**：字段索引不入快照，`import` 一律用默认阈值重建。
    /// 需要自定义阈值时必须在装载后重新摄入，或接受导入后的默认行为。
    pub fn with_max_values_per_field(max_values_per_field: usize) -> Self {
        Self {
            field_index: FieldIndex::with_max_values(max_values_per_field),
            ..Default::default()
        }
    }

    /// 摄入一个文档及其分片，返回 `(doc_id, chunk_ids)`。
    ///
    /// - **幂等 upsert（FR-15）**：`content_hash != 0` 且已存在同 hash 的存活文档
    ///   时直接跳过（返回既有 doc_id 与空 chunk 列表），不产生重复分片
    /// - 每个分片独立分词、独立维护 postings 与统计量
    pub fn add(
        &mut self,
        doc: DocRecord,
        chunks: Vec<Chunk>,
        analyzer: &dyn Analyzer,
    ) -> Result<(DocId, Vec<ChunkId>)> {
        let content_hash = doc.content_hash;

        // 幂等去重：同 hash 的存活文档 → 跳过
        if content_hash != 0 {
            if let Some(&existing) = self.content_hashes.get(&content_hash) {
                if self.forward.doc(existing).is_some() {
                    return Ok((existing, Vec::new()));
                }
            }
        }

        let doc_id = self.forward.insert_doc(doc);
        if content_hash != 0 {
            self.content_hashes.insert(content_hash, doc_id);
        }
        // 字段索引同步点 1/3：摄入（必须在拿到 doc_id 之后、任何删除动作之前）
        if let Some(d) = self.forward.doc(doc_id) {
            self.field_index.insert(doc_id, &d.metadata);
        }
        let mut chunk_ids = Vec::with_capacity(chunks.len());

        for mut chunk in chunks {
            chunk.doc_id = doc_id;
            let tokens = analyzer.analyze_doc(&chunk.text);

            // 聚合词频 + 位置
            let mut tf: Vec<(&str, u32, Vec<u32>)> = Vec::new();
            for tok in &tokens {
                if let Some(entry) = tf.iter_mut().find(|e| e.0 == tok.term.as_str()) {
                    entry.1 += 1;
                    entry.2.push(tok.position);
                } else {
                    tf.push((tok.term.as_str(), 1, vec![tok.position]));
                }
            }

            let chunk_id = self.forward.insert_chunk(chunk);
            for (term, tfn, positions) in &tf {
                self.inverted.add(term, chunk_id, *tfn, positions);
            }

            self.chunk_lens.push(tokens.len() as u32);
            self.stats.total_len += tokens.len() as u64;
            self.stats.num_chunks += 1;
            chunk_ids.push(chunk_id);
        }

        Ok((doc_id, chunk_ids))
    }

    /// 删除一个文档（及其全部分片），含统计量回滚。
    ///
    /// 回滚方式是**重新分析分片原文**（与全量重建用同一 analyzer），
    /// 而非按存储值反推——这是防漂移的关键（架构文档 12.1 的"最关键测试"）。
    pub fn remove(&mut self, doc_id: DocId, analyzer: &dyn Analyzer) -> Result<()> {
        // 先取走该文档的分片 ID 列表（chunk_ids_of_doc 依赖 forward 中的存活分片）
        let chunk_ids = self.forward.chunk_ids_of_doc(doc_id);

        // 回滚 content_hash 映射（幂等 upsert 的状态）
        if let Some(doc) = self.forward.doc(doc_id) {
            if doc.content_hash != 0 {
                self.content_hashes.remove(&doc.content_hash);
            }
        }
        // 字段索引同步点 2/3：删除。metadata 必须**在 tombstone_doc 之前**取走副本——
        // 墓碑化后 `forward.doc()` 返回 None，字段索引就再也摘不干净了（T15 钉死）。
        let metadata = self.forward.doc(doc_id).map(|d| d.metadata.clone());

        for chunk_id in chunk_ids {
            if let Some(chunk) = self.forward.chunk(chunk_id) {
                // 需要持有 text 的副本，因为随后会墓碑化 forward 里的 chunk
                let text = chunk.text.clone();
                let tokens = analyzer.analyze_doc(&text);

                // 回滚 postings
                let mut tf: Vec<(&str, u32)> = Vec::new();
                for tok in &tokens {
                    if let Some(e) = tf.iter_mut().find(|e| e.0 == tok.term.as_str()) {
                        e.1 += 1;
                    } else {
                        tf.push((tok.term.as_str(), 1));
                    }
                }
                for (term, _) in &tf {
                    self.inverted.remove(term, chunk_id);
                }

                // 回滚统计量
                self.stats.total_len -= tokens.len() as u64;
                self.stats.num_chunks -= 1;
            }
            self.forward.tombstone_chunk(chunk_id);
        }

        if let Some(metadata) = metadata {
            self.field_index.remove(doc_id, &metadata);
        }
        self.forward.tombstone_doc(doc_id);
        Ok(())
    }

    // ---- 查询接口（供 Retriever / 测试）----

    /// 存活分片数（检索的最小单位数量）。
    pub fn num_chunks(&self) -> u32 {
        self.stats.num_chunks
    }

    /// 全语料词项总数（所有分片的分词数之和）。
    pub fn total_len(&self) -> u64 {
        self.stats.total_len
    }

    /// 平均分片长度（BM25 长度归一化用）。
    pub fn avgdl(&self) -> f32 {
        self.stats.avgdl()
    }

    /// 词项的文档频率 df（BM25 的 IDF 计算用）。
    pub fn doc_freq(&self, term: &str) -> u32 {
        self.inverted.doc_freq(term)
    }

    /// 词项对应的内部 TermId；未收录则该词。
    pub fn term_id(&self, term: &str) -> Option<crate::types::TermId> {
        self.inverted.term_id(term)
    }

    /// 取某 TermId 的倒排链（供 retriever 遍历）。
    pub fn postings_by_id(&self, id: crate::types::TermId) -> &[Posting] {
        self.inverted.postings_by_id(id)
    }

    /// 某分片是否存活（未被墓碑化）。
    ///
    /// 转发到正排的存活位图（O(1) 位测试，与 `chunks` 的 `Some`/`None` 同源）。
    pub fn is_live_chunk(&self, chunk_id: ChunkId) -> bool {
        self.forward.alive_chunks().contains(chunk_id)
    }

    /// 存活分片位图（过滤下推的谓词数据源）。
    ///
    /// 这是 **Q-C1（删除后向量残留）修复的基础**：已删 chunk 的向量仍留在 HNSW 图里，
    /// 检索期必须用这张位图把幽灵候选挡掉。
    pub fn alive_chunks(&self) -> &crate::bitmap::ChunkBits {
        self.forward.alive_chunks()
    }

    /// 存活分片总数（O(1)）。
    pub fn alive_count(&self) -> usize {
        self.forward.alive_count()
    }

    /// 分片所属文档的 ID（O(1)；墓碑位返回 `None`）。
    pub fn doc_of(&self, chunk_id: ChunkId) -> Option<DocId> {
        self.forward.doc_of(chunk_id)
    }

    /// 字段索引（过滤下推的求值数据源，FR-27）。
    pub fn field_index(&self) -> &FieldIndex {
        &self.field_index
    }

    /// 某文档的存活分片数（O(1)）。
    pub fn chunk_count_of_doc(&self, doc_id: DocId) -> u32 {
        self.forward.chunk_count_of_doc(doc_id)
    }

    /// 取分片内容（墓碑位返回 `None`）。
    pub fn chunk(&self, chunk_id: ChunkId) -> Option<&Chunk> {
        self.forward.chunk(chunk_id)
    }

    /// 取文档（墓碑位返回 `None`）。
    pub fn doc(&self, doc_id: DocId) -> Option<&DocRecord> {
        self.forward.doc(doc_id)
    }

    /// 按 content_hash 查存活文档（幂等 upsert 查询，FR-15）。
    /// 供门面层 `add` 短路查重：命中则直接返回 `deduped`，不重复分块/插入。
    pub fn doc_id_by_hash(&self, hash: u64) -> Option<DocId> {
        if hash == 0 {
            return None;
        }
        self.content_hashes
            .get(&hash)
            .copied()
            .filter(|&id| self.forward.doc(id).is_some())
    }

    /// 某分片的 term 数（dl）。已删除分片返回 0（但调用方通常先判活）。
    pub fn chunk_len(&self, chunk_id: ChunkId) -> u32 {
        self.chunk_lens.get(chunk_id as usize).copied().unwrap_or(0)
    }

    /// 迭代所有活分片（确定性顺序，用于"全量重建"对照测试与向量化）。
    pub fn live_chunks(&self) -> impl Iterator<Item = &Chunk> {
        self.forward.iter_live_chunks()
    }

    /// 迭代所有存活文档 `(doc_id, record)`（确定性顺序）。
    ///
    /// 过滤的全扫兜底（`doc_bits_scan`）以 doc 为遍历单位，与 doc 级字段索引同粒度。
    pub fn iter_live_docs(&self) -> impl Iterator<Item = (DocId, &DocRecord)> {
        self.forward.iter_live_docs()
    }

    /// 活文档数。
    pub fn num_docs(&self) -> usize {
        self.forward.live_docs()
    }

    /// 正排分片槽位总数（含墓碑 `None` 槽）。compaction 墓碑统计用。
    pub fn total_chunks(&self) -> usize {
        self.forward.chunks_slots()
    }

    /// 正排文档槽位总数（含墓碑 `None` 槽）。compaction 墓碑统计用。
    pub fn total_docs(&self) -> usize {
        self.forward.docs_slots()
    }

    // ---- compaction（V2 Step 4 / D-S4-01，pub(crate)：结构重建，门面层编排）----

    /// 从存活集生成 ID 重映射表（按旧 id 升序遍历 ⇒ 稠密、保序）。
    ///
    /// 存活真源是 `forward` 的 `Some`/`None`（与 `alive` 位图同源），直接遍历
    /// `export()` 的结果：凡 `Some` 即分配新 id。doc 与 chunk 各自独立编号。
    pub(crate) fn build_remap(&self) -> IdRemap {
        let (docs, chunks) = self.forward.export();
        let mut doc = vec![None; docs.len()];
        let mut doc_cnt = 0usize;
        for (i, d) in docs.iter().enumerate() {
            if d.is_some() {
                doc[i] = Some(doc_cnt as DocId);
                doc_cnt += 1;
            }
        }
        let mut chunk = vec![None; chunks.len()];
        let mut chunk_cnt = 0usize;
        for (i, c) in chunks.iter().enumerate() {
            if c.is_some() {
                chunk[i] = Some(chunk_cnt as ChunkId);
                chunk_cnt += 1;
            }
        }
        IdRemap { chunk, doc }
    }

    /// 按存活集**重新物化**（compaction，D-S4-01 重编号 / §4.3）。
    ///
    /// **不就地改 `self`**（I5）：在局部构造一份新的、已压实的 `Index` 并返回，
    /// 由门面层与新的 `raw_vectors` / 向量索引一起原子替换。返回（新 Index，摘除的死词数）。
    ///
    /// 六步（§4.3）：①正排稠密化 + id 改写 → `ForwardStore::import`（自动 rebuild
    /// `alive`/`doc_chunk_count`，全活）；②倒排 `filter_map` + 摘死词 + TermId 重排；
    /// ③`chunk_lens` 搬移（dl 不变 ⇒ BM25 不变）；④`content_hashes` remap；
    /// ⑤`field_index` 全量 rebuild（保留自定义阈值，与 import 语义一致）；
    /// ⑥`stats` 原样拷贝（本就是活计数 ⇒ `avgdl` 不变，I2）。
    pub(crate) fn compacted(&self, remap: &IdRemap) -> (Index, usize) {
        let (old_docs, old_chunks) = self.forward.export();

        // ① 正排稠密化：只留存活 doc/chunk，改写 id（按旧升序 ⇒ 新升序）
        let mut dense_docs: Vec<Option<DocRecord>> = Vec::with_capacity(remap.doc.len());
        for (old, slot) in old_docs.iter().enumerate() {
            let Some(new_id) = remap.doc.get(old).copied().flatten() else {
                continue;
            };
            let Some(mut rec) = slot.clone() else {
                continue;
            };
            rec.doc_id = new_id;
            dense_docs.push(Some(rec));
        }
        let mut dense_chunks: Vec<Option<Chunk>> = Vec::with_capacity(remap.chunk.len());
        for (old, slot) in old_chunks.iter().enumerate() {
            let Some(new_cid) = remap.chunk.get(old).copied().flatten() else {
                continue;
            };
            let Some(mut c) = slot.clone() else {
                continue;
            };
            c.chunk_id = new_cid;
            c.doc_id = remap
                .doc
                .get(c.doc_id as usize)
                .copied()
                .flatten()
                .expect("存活 chunk 的所属 doc 必存活");
            dense_chunks.push(Some(c));
        }

        // ② 倒排：remap + 摘死词 + TermId 重排
        let (inverted, reclaimed_terms) = self.inverted.compact(&remap.chunk);

        // ③ chunk_lens：按 remap 搬移（值不变）
        let new_chunk_count = dense_chunks.len();
        let mut chunk_lens = vec![0u32; new_chunk_count];
        for (old, map) in remap.chunk.iter().enumerate() {
            if let Some(new) = map {
                chunk_lens[*new as usize] = self.chunk_lens.get(old).copied().unwrap_or(0);
            }
        }

        // ④ content_hashes：hash → new doc_id（墓碑 doc 已被 `remove` 摘除，此处防御性过滤）
        let mut content_hashes: HashMap<u64, DocId> =
            HashMap::with_capacity(self.content_hashes.len());
        for (&h, &old_doc) in &self.content_hashes {
            if let Some(new) = remap.doc.get(old_doc as usize).copied().flatten() {
                content_hashes.insert(h, new);
            }
        }

        // ⑤ field_index：全量 rebuild（保留自定义基数阈值）
        let mut field_index = FieldIndex::with_max_values(self.field_index.max_values_per_field());
        field_index.rebuild(&dense_docs);

        // ForwardStore::import 内部 rebuild（alive 全活 / doc_chunk_count 重算）
        let forward = ForwardStore::import(dense_docs, dense_chunks);

        let new_index = Index {
            inverted,
            forward,
            stats: self.stats, // 活计数 ⇒ 原样保留
            chunk_lens,
            content_hashes,
            field_index,
        };
        (new_index, reclaimed_terms)
    }

    /// 把 `other` 的内容**按本地顺序追加**到 `self` —— **不重编号**（`D-S8-04` / 设计 §4.9.2）。
    ///
    /// # 前提（由调用方保证）
    ///
    /// `other` 是 `self` 之后的**下一个**段（严格 FIFO，见 §4.4.2）⇒ append 之后
    /// `other` 的本地 ID `i` 在全局恰好是 `base + i`，即**不需要任何重编号**。
    /// 这也正是 `compact()`（会重编号）**必须**在 `merge_all()` 之后的原因（`D-S8-12`）。
    ///
    /// # 返回
    ///
    /// 本次追加的**活分片数**（= `other.stats.num_chunks`，即 `other` 的墓碑位不计入）。
    ///
    /// # 🔴 postings 顺序（`S8-T4`「逐位一致」的结构性依据之一）
    ///
    /// [`InvertedIndex::add`] 把 posting **push 到链尾** ⇒ 合并后同一 term 的链内顺序 =
    /// 「主段原序 + 增量段序」，而基址连续 ⇒ **恰为全局 `chunk_id` 升序** = 单段建库的顺序。
    /// ⇒ 与 `retriever::bm25` 的 TAAT 累加顺序一致（`I8-6`）。
    /// **后人在此处「顺便排序」会破坏它** —— 所以这段注释必须留着。
    ///
    /// # 与 `compacted()` 的区别
    ///
    /// `compacted()` **重编号**（并为墓碑腾出的稠密化）；本方法**不动 ID**。
    pub(crate) fn merge_from(&mut self, other: Index) -> Result<usize> {
        let base_doc = self.forward.docs_slots() as u64;
        let base_chunk = self.forward.chunks_slots() as u64;
        let other_docs = other.forward.docs_slots() as u64;

        // 不静默溢出：全局 ID 是 `u32`（`types.rs`），合并本身不重编号 ⇒ 越界就是真错误。
        if base_doc + other_docs > u32::MAX as u64 {
            return Err(crate::error::Error::InvalidInput(format!(
                "合并后文档槽位数 {} 超过 u32 上限（doc_id 是 u32）",
                base_doc + other_docs
            )));
        }
        if base_chunk + other.forward.chunks_slots() as u64 > u32::MAX as u64 {
            return Err(crate::error::Error::InvalidInput(format!(
                "合并后分片槽位数 {} 超过 u32 上限（chunk_id 是 u32）",
                base_chunk + other.forward.chunks_slots() as u64
            )));
        }

        // ① 倒排：必须在 `forward` append **之前**取出（此时 `other` 的 chunk_id 还是本地 ID，
        //    统一加 `base_chunk` 一步到位）。`export()` 返回的 `term_dict` 按 TermId 升序
        //    ⇒ 遍历顺序确定（NFR-06）。
        let (term_dict, postings) = other.inverted.export();
        for (term, tid) in term_dict {
            for p in &postings[tid as usize] {
                #[cfg(feature = "positions")]
                self.inverted.add(
                    &term,
                    p.chunk_id + base_chunk as ChunkId,
                    p.tf,
                    &p.positions,
                );
                #[cfg(not(feature = "positions"))]
                self.inverted
                    .add(&term, p.chunk_id + base_chunk as ChunkId, p.tf, &[]);
            }
        }

        // ② 正排：槽位逐个 push（含 `None` 墓碑位 ⇒ 保槽位对齐）
        let other_chunk_lens = other.chunk_lens.clone();
        debug_assert_eq!(
            other_chunk_lens.len(),
            other.forward.chunks_slots(),
            "`chunk_lens` 必须与 chunks 槽位一一对应（删除后保留 ⇒ 不是活分片数）"
        );
        let other_stats = other.stats;
        let other_hashes = other.content_hashes.clone();
        self.forward.append_from(other.forward);

        // ③ 统计量 / chunk_lens：都是「活量」，且已按各自段维护 ⇒ 直接相加 / 追加
        self.stats.total_len += other_stats.total_len;
        self.stats.num_chunks += other_stats.num_chunks;
        self.chunk_lens.extend(other_chunk_lens);

        // ④ content_hash 表：跨段查重的前提。⚠️ 碰撞**理论上不应发生**（写入时已跨段查重），
        //    但仍要显式处理：debug 下报错，release 下**确定性**地以后写入者为准（`insert` 覆盖）。
        for (h, d) in other_hashes {
            let displaced = self.content_hashes.insert(h, d + base_doc as DocId);
            debug_assert!(
                displaced.is_none(),
                "content_hash 跨段碰撞：{h}（写入路径的跨段查重应已排除）"
            );
        }

        // ⑤ 字段索引：**不能 extend**（它是 field → value → doc 位图，且带基数降级判定）
        //    ⇒ 走既有的全量重建（`import` 用同一函数）。重建后「基数保护」的降级结论
        //    可能与增量维护不同 ⇒ 由 `S8-T13` 的等价用例钉住（属 S8-06）。
        let (docs, _) = self.forward.export();
        self.field_index.rebuild(docs);

        Ok(other_stats.num_chunks as usize)
    }

    // ---- 快照导出 / 导入（T4-02，供 storage 模块序列化）----

    /// 导出为快照所需的结构化分区（供 storage 序列化）。
    pub fn export(&self) -> SnapshotSections {
        let (term_dict, postings) = self.inverted.export();
        let (docs, chunks) = self.forward.export();
        let mut content_hashes: Vec<(u64, DocId)> =
            self.content_hashes.iter().map(|(h, d)| (*h, *d)).collect();
        content_hashes.sort_unstable();
        SnapshotSections {
            term_dict,
            postings,
            docs: docs
                .iter()
                .map(|d| d.as_ref().map(DocumentDto::from))
                .collect(),
            chunks: chunks.to_vec(),
            chunk_lens: self.chunk_lens.clone(),
            stats: self.stats,
            content_hashes,
        }
    }

    /// 从快照恢复。**不重建向量索引**——向量由 storage 层按原始向量重建（D1）。
    ///
    /// `ForwardStore::import` 会顺带重建存活位图与每文档分片计数（O(N)），
    /// 因此**跨快照的墓碑状态得到保留**：已删 chunk 在加载后依然是死的，
    /// 这正是 Q-C1「幽灵候选跨快照永续」的修复基础。
    pub fn import(sections: SnapshotSections) -> Self {
        let inverted = InvertedIndex::import(sections.term_dict, sections.postings);
        let docs: Vec<Option<DocRecord>> = sections
            .docs
            .into_iter()
            .map(|d| d.map(DocRecord::from))
            .collect();
        // 字段索引同步点 3/3：全量重建（不入快照，故每次导入重建，O(N)）
        let mut field_index = FieldIndex::new();
        field_index.rebuild(&docs);

        let forward = ForwardStore::import(docs, sections.chunks);
        let content_hashes: HashMap<u64, DocId> = sections.content_hashes.into_iter().collect();
        Self {
            inverted,
            forward,
            stats: sections.stats,
            chunk_lens: sections.chunk_lens,
            content_hashes,
            field_index,
        }
    }
}

/// 把 token 序列聚合为 (term, tf, positions) 序列——供测试与 add 内部复用。
#[allow(dead_code)]
fn aggregate(tokens: &[Token]) -> Vec<(String, u32, Vec<u32>)> {
    let mut out: Vec<(String, u32, Vec<u32>)> = Vec::new();
    for tok in tokens {
        if let Some(e) = out.iter_mut().find(|e| e.0 == tok.term.as_str()) {
            e.1 += 1;
            e.2.push(tok.position);
        } else {
            out.push((tok.term.to_string(), 1, vec![tok.position]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)] // 中文测试名
    use super::*;
    use crate::analyze::MixedAnalyzer;
    use crate::chunk::Chunker;
    use crate::document::content_hash;

    fn make_doc(source: &str, text: &str) -> DocRecord {
        DocRecord {
            doc_id: 0,
            source: source.to_string(),
            metadata: serde_json::json!({}),
            content_hash: content_hash(text),
        }
    }

    fn make_chunks(text: &str) -> Vec<Chunk> {
        Chunker::default().chunk(0, text)
    }

    #[test]
    fn 幂等upsert不产生重复分片() {
        let analyzer = MixedAnalyzer::new();
        let mut index = Index::new();
        let text = "BM25 是一种检索算法";

        let (d1, _) = index
            .add(make_doc("a", text), make_chunks(text), &analyzer)
            .unwrap();
        let n1 = index.num_chunks();

        // 同内容再次 upsert：应跳过，chunk 数不翻倍
        let (d2, c2) = index
            .add(make_doc("b", text), make_chunks(text), &analyzer)
            .unwrap();
        assert_eq!(d1, d2, "重复内容应返回同一 doc_id");
        assert!(c2.is_empty(), "重复内容不应新增分片");
        assert_eq!(index.num_chunks(), n1);
        assert_eq!(index.num_docs(), 1);

        // 不同内容正常入库
        let text2 = "向量检索完全不同";
        index
            .add(make_doc("c", text2), make_chunks(text2), &analyzer)
            .unwrap();
        assert_eq!(index.num_chunks(), n1 + make_chunks(text2).len() as u32);
    }

    #[test]
    fn 删除后可重新upsert() {
        let analyzer = MixedAnalyzer::new();
        let mut index = Index::new();
        let text = "删除后再写入同一内容";

        let (d1, _) = index
            .add(make_doc("a", text), make_chunks(text), &analyzer)
            .unwrap();
        index.remove(d1, &analyzer).unwrap();
        assert_eq!(index.num_chunks(), 0);

        // 删除后同 hash 应能重新入库（hash 映射已回滚）
        let (_, c2) = index
            .add(make_doc("a", text), make_chunks(text), &analyzer)
            .unwrap();
        assert!(!c2.is_empty(), "删除后重新 upsert 应正常入库");
        assert_eq!(index.num_chunks(), c2.len() as u32);
    }

    #[test]
    fn 删除后统计量与全量重建一致() {
        let analyzer = MixedAnalyzer::new();
        let texts = ["文档一内容检索", "文档二向量检索", "文档三倒排索引"];

        // 增量：加三个、删第一个
        let mut incremental = Index::new();
        let mut ids = Vec::new();
        for t in texts {
            let (d, _) = incremental
                .add(make_doc("s", t), make_chunks(t), &analyzer)
                .unwrap();
            ids.push(d);
        }
        incremental.remove(ids[0], &analyzer).unwrap();

        // 全量重建（只含后两个）
        let mut rebuilt = Index::new();
        for t in &texts[1..] {
            rebuilt
                .add(make_doc("s", t), make_chunks(t), &analyzer)
                .unwrap();
        }

        assert_eq!(incremental.num_chunks(), rebuilt.num_chunks());
        assert_eq!(incremental.total_len(), rebuilt.total_len());
        for term in ["检索", "向量", "倒排", "索引"] {
            assert_eq!(
                incremental.doc_freq(term),
                rebuilt.doc_freq(term),
                "term={term} 的 df 不一致"
            );
        }
    }

    // ---- 字段索引的三个同步点（add / remove / import）----

    fn make_doc_meta(source: &str, text: &str, meta: serde_json::Value) -> DocRecord {
        DocRecord {
            doc_id: 0,
            source: source.to_string(),
            metadata: meta,
            content_hash: content_hash(text),
        }
    }

    fn meta_index() -> Index {
        let analyzer = MixedAnalyzer::new();
        let mut index = Index::new();
        for (i, (tag, year)) in [("rust", 2021.0), ("python", 2022.0), ("rust", 2023.0)]
            .iter()
            .enumerate()
        {
            let text = format!("第 {i} 篇关于 {tag} 的检索文档");
            let meta = serde_json::json!({"tag": tag, "year": year});
            index
                .add(
                    make_doc_meta(&format!("s{i}"), &text, meta),
                    Chunker::default().chunk(0, &text),
                    &analyzer,
                )
                .unwrap();
        }
        index
    }

    #[test]
    fn add时字段索引同步登记() {
        let index = meta_index();
        let fi = index.field_index();
        let rust = fi.eq_bits("tag", "rust").expect("未降级");
        assert_eq!(rust.count_ones(), 2, "tag=rust 有两个文档");
        assert!(rust.contains(0) && rust.contains(2));
        assert_eq!(
            fi.range_bits("year", 2022.0, 2024.0).unwrap().count_ones(),
            2
        );
    }

    /// T15：字段索引必须在 `tombstone_doc` **之前**清理——
    /// 墓碑化后 metadata 不可读，索引就再也摘不干净。
    #[test]
    fn remove时字段索引同步摘除() {
        let analyzer = MixedAnalyzer::new();
        let mut index = meta_index();

        // 删掉 doc 0（tag=rust, year=2021）
        index.remove(0, &analyzer).unwrap();

        let fi = index.field_index();
        let rust = fi.eq_bits("tag", "rust").expect("未降级");
        assert!(!rust.contains(0), "已删文档的 doc 位必须被清掉");
        assert!(rust.contains(2));
        assert_eq!(rust.count_ones(), 1);
        assert_eq!(
            fi.range_bits("year", 0.0, 2022.0).unwrap().count_ones(),
            0,
            "year=2021 的文档已删，区间内应为空"
        );
        // 删除不应把 value_count 扣成负数（否则高基数字段会被永久误判降级）
        assert!(!fi.is_degraded("tag"));
        assert!(!fi.is_degraded("year"));
    }

    /// T7（Index 层）：增量维护 ≡ 全量重建 ≡ 快照往返后重建。
    #[test]
    fn 字段索引的增量维护与全量重建一致() {
        let analyzer = MixedAnalyzer::new();
        let mut incremental = meta_index();
        incremental.remove(1, &analyzer).unwrap(); // 删掉 tag=python 那个

        // 全量重建：只加 doc 0 与 doc 2
        let mut rebuilt = Index::new();
        for (i, (tag, year)) in [("rust", 2021.0), ("rust", 2023.0)].iter().enumerate() {
            let text = format!("第 {i} 篇关于 {tag} 的检索文档");
            let meta = serde_json::json!({"tag": tag, "year": year});
            rebuilt
                .add(
                    make_doc_meta(&format!("s{i}"), &text, meta),
                    Chunker::default().chunk(0, &text),
                    &analyzer,
                )
                .unwrap();
        }

        // 快照往返：export → import 走 rebuild 路径
        let roundtrip = Index::import(incremental.export());

        for idx in [&incremental, &rebuilt, &roundtrip] {
            let fi = idx.field_index();
            assert_eq!(
                fi.eq_bits("tag", "rust").unwrap().count_ones(),
                2,
                "三个路径下 tag=rust 都应命中 2 个文档"
            );
            assert!(fi.eq_bits("tag", "python").unwrap().is_empty());
            assert_eq!(fi.range_bits("year", 0.0, 3000.0).unwrap().count_ones(), 2);
        }
    }

    // ---- compaction（V2 Step 4 / T1 / T2 / T11，Index 层）----

    /// 强制单 chunk 的分块器（一个 doc == 一个 chunk_id，便于断言）。
    fn single_chunker() -> Chunker {
        Chunker::new(200_000, 0)
    }

    /// 构造一个「有墓碑 + 有死词」的索引：
    /// - A「苹果 香蕉 樱桃」、B「苹果 榴莲」、C「葡萄 苹果 樱桃」
    /// - 删除 B ⇒ B 的 doc/chunk 变墓碑；「榴莲」只出现在 B ⇒ 成死词（链空、词留字典）
    fn tombstoned_index() -> (Index, DocId, Vec<(DocId, &'static str)>) {
        let analyzer = MixedAnalyzer::new();
        let mut index = Index::new();
        let mut keep = Vec::new();
        let mut b_id = 0;
        for (i, text) in ["苹果 香蕉 樱桃", "苹果 榴莲", "葡萄 苹果 樱桃"]
            .iter()
            .enumerate()
        {
            let doc = DocRecord {
                doc_id: 0,
                source: format!("s{i}"),
                metadata: serde_json::json!({}),
                content_hash: content_hash(text),
            };
            let (d, _) = index
                .add(doc, single_chunker().chunk(0, text), &analyzer)
                .unwrap();
            if i == 1 {
                b_id = d;
            } else {
                keep.push((d, *text));
            }
        }
        index.remove(b_id, &analyzer).unwrap();
        (index, b_id, keep)
    }

    /// T1：compaction 后正排无 `None` 槽、chunk_lens 长度 = 存活数、统计量不变、alive 不变。
    #[test]
    fn T1_compaction稠密化且统计量不变() {
        let (index, _, _) = tombstoned_index();
        assert!(
            index.total_chunks() > index.alive_count(),
            "构造前提：应有墓碑"
        );
        assert!(
            index.total_docs() > index.num_docs(),
            "构造前提：应有墓碑 doc"
        );

        let remap = index.build_remap();
        let (new_idx, _reclaimed) = index.compacted(&remap);

        // 正排无空洞：槽位总数 == 存活数
        assert_eq!(new_idx.total_chunks(), new_idx.alive_count(), "无墓碑槽");
        assert_eq!(new_idx.total_docs(), new_idx.num_docs(), "无墓碑 doc");
        // chunk_lens 长度 == 存活数
        assert_eq!(new_idx.chunk_lens.len(), new_idx.alive_count());
        // 统计量不变（活计数）
        assert_eq!(new_idx.num_chunks(), index.num_chunks());
        assert_eq!(new_idx.total_len(), index.total_len());
        assert_eq!(new_idx.avgdl(), index.avgdl());
        // 存活 chunk 集合不变（I1：按 source+text）
        let collect_docs = |idx: &Index| {
            let mut v: Vec<String> = idx
                .iter_live_docs()
                .map(|(_, r)| r.source.clone())
                .collect();
            v.sort();
            v
        };
        assert_eq!(collect_docs(&new_idx), collect_docs(&index), "存活集不变");
        // 存活 chunk 的 text 集合不变（I1）
        let chunk_texts = |idx: &Index| {
            let mut v: Vec<String> = idx.live_chunks().map(|c| c.text.clone()).collect();
            v.sort();
            v
        };
        assert_eq!(
            chunk_texts(&new_idx),
            chunk_texts(&index),
            "存活 chunk 内容不变"
        );
    }

    /// T2：compaction 摘除死词 + df 不变 + 重建确定性（同一状态两次 compact 字节一致）。
    #[test]
    fn T2_compaction摘死词并保持df与重建确定() {
        let (index, _, _) = tombstoned_index();
        // 构造前提：死词「榴莲」在 compact 前仍在字典（链已空）
        assert!(
            index.term_id("榴莲").is_some(),
            "死词在 compact 前应残留字典"
        );
        assert_eq!(index.doc_freq("榴莲"), 0, "死词 df 应为 0（链已空）");

        // 记录 compact 前存活词的 df
        let df_before: Vec<(&str, u32)> = ["苹果", "香蕉", "樱桃", "葡萄"]
            .iter()
            .map(|t| (*t, index.doc_freq(t)))
            .collect();

        let remap = index.build_remap();
        let (new_a, reclaimed) = index.compacted(&remap);
        // 死词「榴莲」被摘除
        assert_eq!(new_a.term_id("榴莲"), None, "死词应被摘除");
        // 摘除死词数 >= 1
        assert!(reclaimed >= 1, "应回收至少 1 个死词，实为 {reclaimed}");
        // 存活词的 df 不变（I2）
        for (t, df) in &df_before {
            assert_eq!(new_a.doc_freq(t), *df, "词 {t} 的 df 在 compact 后应不变");
        }
        // 词表无空链：每个词都 df>0
        assert!(new_a.doc_freq("榴莲") == 0, "死词已摘");

        // 重建确定性：对同一状态 compact 两次 → export 关键字段一致
        let (new_b, _) = index.compacted(&remap);
        assert_eq!(new_a.export().term_dict, new_b.export().term_dict);
        assert_eq!(new_a.export().postings, new_b.export().postings);
        assert_eq!(new_a.export().chunk_lens, new_b.export().chunk_lens);
        assert_eq!(new_a.export().stats, new_b.export().stats);
        assert_eq!(new_a.export().docs.len(), new_b.export().docs.len());
        assert_eq!(new_a.export().chunks.len(), new_b.export().chunks.len());
    }

    /// T11（Index 层）：无墓碑时 compact 是 no-op——所有存活 id 原样不变、无回收。
    #[test]
    fn T11_无墓碑时compaction为no_op_id不变() {
        let analyzer = MixedAnalyzer::new();
        let mut index = Index::new();
        let mut ids = Vec::new();
        for (i, text) in ["苹果 香蕉", "葡萄 苹果"].iter().enumerate() {
            let doc = DocRecord {
                doc_id: 0,
                source: format!("s{i}"),
                metadata: serde_json::json!({}),
                content_hash: content_hash(text),
            };
            let (d, chunks) = index
                .add(doc, single_chunker().chunk(0, text), &analyzer)
                .unwrap();
            ids.push((d, chunks[0]));
        }

        let remap = index.build_remap();
        let (new_idx, reclaimed) = index.compacted(&remap);
        assert_eq!(reclaimed, 0, "无死词应不回收");
        // ID 一个都不变（T11 的 no-op 保证）：doc/chunk 都保持原值
        for (d, c) in &ids {
            assert_eq!(
                new_idx.doc(*d).map(|r| r.doc_id),
                Some(*d),
                "doc {d} id 应不变"
            );
            assert_eq!(
                new_idx.chunk(*c).map(|ch| ch.chunk_id),
                Some(*c),
                "chunk {c} id 应不变"
            );
            assert!(new_idx.is_live_chunk(*c));
        }
        // 无墓碑槽（构造本就无）
        assert_eq!(new_idx.total_chunks(), new_idx.alive_count());
    }
}
