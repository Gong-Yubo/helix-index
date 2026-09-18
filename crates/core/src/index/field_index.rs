//! 字段索引：field → value → 命中的 doc 位图（过滤下推的求值数据源）。
//!
//! # 为什么需要
//!
//! 现状的过滤求值（[`crate::query::filter::allowed_chunks`]）每次检索都要
//! **全表扫描**所有活文档、逐个取 JSON 字段做比较（`query/filter.rs:39-49`）——
//! O(N) 次 JSON 取值，10 万级语料下直接威胁 NFR-02 的 P99 预算（Q-I1）。
//!
//! 本模块在**摄入期**维护反向映射，把每次检索的 O(N) 次 JSON 求值降为
//! O(query 词数) 次哈希查找 + 位图运算。
//!
//! # 粒度：为什么只维护 doc 级
//!
//! metadata 挂在 **doc** 上，chunk 继承其判定结果。若维护 chunk 级位图，
//! 一个 3 分片的文档要在每个 value 键下重复 3 位，且文档增删时要同步
//! 3 个位置（漂移面变 3 倍）。doc 级 + 检索期 `chunk → doc` 的 O(1) 映射
//! （见 `predicate.rs` 的惰性判定）更省且更不易错。
//!
//! # 类型覆盖：全类型登记（评审 P1-2）
//!
//! `Eq` 的匹配语义是 `json_to_string(metadata[field]) == value`
//! （`query/filter.rs:22-25`），而 [`json_to_string`] 对**所有** JSON 类型
//! 都返回字符串。因此本索引对 **bool / null / array / object 也一律登记**，
//! 否则会出现「`matches()` 判为真、字段索引返回空集」的**静默假阴性**。
//!
//! 数值额外登记进 `numbers`，供 `Range` 使用。
//!
//! > ⚠️ 一个反直觉但**必须保留**的既有行为：`year: 2021.0` 用 `Eq("year", "2021")`
//! > 匹配不上（`"2021.0"` ≠ `"2021"`）。本索引原样复刻，不做"顺手修正"——
//! > 那会破坏与 `matches()` 的等价性。
//!
//! # 基数保护（内存护栏）
//!
//! 高基数字段（uuid、毫秒时间戳）会让「每 value 一张位图」爆炸。
//! 超过 [`FieldIndex::max_values_per_field`] 后**不再为该字段新增 value 键**，
//! 并置 `degraded` 标记；求值器读到该标记即退化为全表扫描
//! （正确性不受影响，只是慢——见 `query/filter.rs` 的 `doc_bits_scan`）。
//!
//! 保护对象是 **terms + numbers 的合计键数**，避免出现"terms 受保护但
//! numbers 爆炸"的缝（评审 P2-4）。
//!
//! ## ⚠️ 已知短板：高基数 Range 过滤会整体退化
//!
//! **毫秒时间戳、雪花 ID 这类字段正是最常见的真实过滤场景**（"最近 7 天"、
//! "某个时间窗内的日志"），但它们的基数天然超过任何合理上限，于是：
//!
//! - 该字段**永久降级**（`degraded` 粘滞，见 `FieldValues` 的字段文档）
//! - 该字段上的**所有**过滤（含 Range）退回 O(N) 全扫
//! - 即 **Q-I1 的优化对这类场景收益为 0**——本模块要治的病，恰好没治到
//!
//! 撞线比直觉更快：数值字段**同时**登记 terms 键与 numbers 键、两者合计计入限额，
//! 所以毫秒时间戳约 **512 篇**文档就降级（不是 1024）。
//!
//! 缓解手段是调高上限（见 [`crate::index::Index::with_max_values_per_field`]），代价是内存随基数线性增长。
//! 真正的解法（prefilter / 排序列）属 V2.1 议题，**讨论时应以高基数 Range 为第一用例**，
//! 而不是泛泛的 tag 等值过滤；S1-10 的 bench 需补「降级字段 Range 过滤」档位量化该退化。

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use crate::bitmap::DocBits;
use crate::document::DocRecord;
use crate::types::DocId;

/// 单个字段超过此基数后停止新增 value 键（默认；可配置，见 [`FieldIndex::with_max_values`]）。
///
/// 初值 1024：足够覆盖 tag / 类别 / 状态这类真实过滤维度，又能把
/// 「每个 value 一张位图」的内存上界压在可控范围（实测记入 NFR-05）。
pub const DEFAULT_MAX_VALUES_PER_FIELD: usize = 1024;

/// f64 的全序包装（`BTreeMap` 需要 `Ord`）。
///
/// 用 [`f64::total_cmp`] 而非位模式变换：语义与 `total_cmp` 完全一致，
/// 且不必手写「正数翻符号位 / 负数全翻」的位技巧，可读性更好。
/// （设计文档写的是位模式，实现改用等价且更安全的 `total_cmp`。）
///
/// ⚠️ 注意 [`PartialEq`] 用 `==` 而 [`Ord`] 用 `total_cmp`，二者**仅在 NaN 上不一致**
/// （`NaN != NaN`，但 `total_cmp` 给它一个确定的全序位）。这不是疏漏：
/// `serde_json::Number` 不接受 NaN / Infinity（解析与构造都会拒绝），所以键里不可能
/// 出现 NaN，该不一致**不可达**；而 `==` 的语义必须与 `value.as_f64()` 的等值判断对齐。
#[derive(Debug, Clone, Copy)]
struct NumKey(f64);

impl PartialEq for NumKey {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for NumKey {}

impl PartialOrd for NumKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NumKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// 单个字段的取值索引。
#[derive(Debug, Clone, Default)]
struct FieldValues {
    /// 等值匹配：值的 `json_to_string` 形式 → doc 位图
    terms: HashMap<String, DocBits>,
    /// 范围匹配：数值 → doc 位图（仅当 metadata 值是数字时登记）
    numbers: BTreeMap<NumKey, DocBits>,
    /// `terms` 与 `numbers` 的**合计**键数（基数保护判据）
    value_count: usize,
    /// 已达基数上限：新 value 键不再登记，求值退化为全表扫描。
    ///
    /// ⚠️ **粘滞是有意设计，不要当成 bug 修掉**：降级之后被跳过的值**不在索引里**，
    /// 即使后来删除操作把 `value_count` 扣回阈值以下，复位本标记也会让求值器
    /// 拿到一张「看起来完整、实际漏键」的位图——那是**假阴性**，比慢严重得多。
    ///
    /// save/load 后本标记会归零（字段索引不入快照，`import` 用 `rebuild` 完整重灌），
    /// 这与粘滞**并不矛盾**：rebuild 时数据是全量的，不存在「被跳过的窗口」。
    degraded: bool,
}

/// 字段索引：field → value → doc 位图。
///
/// # 维护入口（必须单一真源）
///
/// - `Index::add` → [`FieldIndex::insert`]
/// - `Index::remove` → [`FieldIndex::remove`]，**必须在 `tombstone_doc` 之前调用**
///   （此时 metadata 仍可读）——该时序由 T15 单独钉死
/// - `Index::import` → [`FieldIndex::rebuild`]（从 docs 全量重建）
/// - **不入快照**：可由 docs 的 metadata 重建，因此 `FORMAT_VERSION` 无需升版（D-S1-07）
#[derive(Debug, Clone)]
pub struct FieldIndex {
    fields: HashMap<String, FieldValues>,
    max_values_per_field: usize,
}

impl Default for FieldIndex {
    fn default() -> Self {
        Self {
            fields: HashMap::new(),
            max_values_per_field: DEFAULT_MAX_VALUES_PER_FIELD,
        }
    }
}

impl FieldIndex {
    /// 空索引（使用默认基数上限）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 指定每字段的基数上限（基数保护阈值）。
    pub fn with_max_values(max_values_per_field: usize) -> Self {
        Self {
            fields: HashMap::new(),
            max_values_per_field,
        }
    }

    /// 当前基数上限。
    pub fn max_values_per_field(&self) -> usize {
        self.max_values_per_field
    }

    /// 已登记的字段数（诊断用）。
    pub fn field_count(&self) -> usize {
        self.fields.len()
    }

    /// 某字段是否已因基数超限而降级（降级后求值必须走全表扫描）。
    pub fn is_degraded(&self, field: &str) -> bool {
        self.fields.get(field).is_some_and(|f| f.degraded)
    }

    /// 登记一个文档的 metadata。
    pub fn insert(&mut self, doc_id: DocId, metadata: &serde_json::Value) {
        let Some(obj) = metadata.as_object() else {
            return; // 非 object metadata 无顶层字段
        };
        let max = self.max_values_per_field;
        for (field, value) in obj {
            let entry = self.fields.entry(field.clone()).or_default();

            // ---- 等值键：所有类型一律登记（P1-2）----
            let key = json_to_string(value);
            if !entry.terms.contains_key(&key) {
                if entry.value_count >= max {
                    entry.degraded = true;
                } else {
                    entry.terms.insert(key.clone(), DocBits::new());
                    entry.value_count += 1;
                }
            }
            if let Some(bits) = entry.terms.get_mut(&key) {
                bits.set(doc_id);
            }

            // ---- 数值键：仅数字登记，供 Range ----
            if let Some(n) = value.as_f64() {
                let nk = NumKey(n);
                if !entry.numbers.contains_key(&nk) {
                    if entry.value_count >= max {
                        entry.degraded = true;
                    } else {
                        entry.numbers.insert(nk, DocBits::new());
                        entry.value_count += 1;
                    }
                }
                if let Some(bits) = entry.numbers.get_mut(&nk) {
                    bits.set(doc_id);
                }
            }
        }
    }

    /// 摘除一个文档的 metadata（`Index::remove` 用，**必须在 `tombstone_doc` 之前**）。
    ///
    /// 位图为空的 value 键会被删除并回退 `value_count`，避免高基数字段
    /// 「删完后仍占着额度」导致后续插入被误判为超限。
    pub fn remove(&mut self, doc_id: DocId, metadata: &serde_json::Value) {
        let Some(obj) = metadata.as_object() else {
            return;
        };
        for (field, value) in obj {
            let Some(entry) = self.fields.get_mut(field) else {
                continue;
            };

            let key = json_to_string(value);
            let mut drop_term = false;
            if let Some(bits) = entry.terms.get_mut(&key) {
                bits.clear(doc_id);
                drop_term = bits.is_empty();
            }
            if drop_term {
                entry.terms.remove(&key);
                entry.value_count = entry.value_count.saturating_sub(1);
            }

            if let Some(n) = value.as_f64() {
                let nk = NumKey(n);
                let mut drop_num = false;
                if let Some(bits) = entry.numbers.get_mut(&nk) {
                    bits.clear(doc_id);
                    drop_num = bits.is_empty();
                }
                if drop_num {
                    entry.numbers.remove(&nk);
                    entry.value_count = entry.value_count.saturating_sub(1);
                }
            }
        }
    }

    /// 从 docs 全量重建（快照导入用；O(N) 一次）。
    pub fn rebuild(&mut self, docs: &[Option<DocRecord>]) {
        let max = self.max_values_per_field;
        *self = Self {
            fields: HashMap::new(),
            max_values_per_field: max,
        };
        for (i, slot) in docs.iter().enumerate() {
            if let Some(doc) = slot {
                self.insert(i as DocId, &doc.metadata);
            }
        }
    }

    // ---- 求值（供 query/filter.rs 的 doc_bits 使用）----

    /// 求值 `Eq`：返回**通过等值匹配的 doc 位图**。
    ///
    /// - `Some(bits)`：可精确回答（字段无文档拥有 / 该值不存在 → 空集，语义正确）
    /// - `None`：**无法回答**，调用方必须退化为全表扫描（字段已因基数超限降级）
    pub fn eq_bits(&self, field: &str, value: &str) -> Option<DocBits> {
        match self.fields.get(field) {
            None => Some(DocBits::new()), // 没有任何文档有这个字段 → 空集
            Some(entry) if entry.degraded => None,
            Some(entry) => Some(entry.terms.get(value).cloned().unwrap_or_else(DocBits::new)),
        }
    }

    /// 求值 `Range`（左闭右开 `[gte, lte)`，与 `matches()` 一致）：返回命中的 doc 位图。
    ///
    /// 返回 `None` 的含义同 [`Self::eq_bits`]。
    pub fn range_bits(&self, field: &str, gte: f64, lte: f64) -> Option<DocBits> {
        let entry = match self.fields.get(field) {
            None => return Some(DocBits::new()),
            Some(e) if e.degraded => return None,
            Some(e) => e,
        };
        if entry.numbers.is_empty() {
            // 该字段没有任何数值 → 不可能有文档落在数值区间内
            return Some(DocBits::new());
        }
        let mut out = DocBits::new();
        for (_, bits) in entry.numbers.range(NumKey(gte)..NumKey(lte)) {
            out.union_with(bits);
        }
        Some(out)
    }
}

/// metadata 值 → `Eq` 比较用的字符串形式。
///
/// **必须与 [`crate::query::filter::matches`] 使用同一口径**：字符串原样返回，
/// 其余类型用 `to_string()`（`true` / `null` / `[1,2]` / `{"a":1}`）。
/// 这是"字段索引 ≡ matches()"等价性的根基（评审 P1-2）。
pub fn json_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;
    use crate::document::DocRecord;

    fn doc_with(meta: serde_json::Value) -> DocRecord {
        DocRecord {
            doc_id: 0,
            source: "s".to_string(),
            metadata: meta,
            content_hash: 0,
        }
    }

    fn build(metas: Vec<serde_json::Value>) -> FieldIndex {
        let mut fi = FieldIndex::new();
        for (i, m) in metas.iter().enumerate() {
            fi.insert(i as DocId, m);
        }
        fi
    }

    // ---- T6：全类型随机等价性（字段索引 ≡ matches()）----

    /// 覆盖所有 JSON 顶层类型的取值集合。
    fn all_type_values() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!("rust"), // string
            serde_json::json!(2021.0), // number（浮点形式）
            serde_json::json!(7),      // number（整数形式）
            serde_json::json!(true),   // bool
            serde_json::json!(false),
            serde_json::json!(null),     // null
            serde_json::json!([1, 2]),   // array
            serde_json::json!({"a": 1}), // object
        ]
    }

    #[test]
    fn 全类型取值都能被Eq索引命中() {
        let values = all_type_values();
        let metas: Vec<serde_json::Value> = values
            .iter()
            .map(|v| serde_json::json!({"f": v.clone()}))
            .collect();
        let fi = build(metas.clone());

        for (i, v) in values.iter().enumerate() {
            let expect = json_to_string(v);
            let bits = fi
                .eq_bits("f", &expect)
                .expect("未降级字段应能精确回答")
                .clone();
            assert!(
                bits.contains(i as DocId),
                "值 {v}（字符串形式 {expect:?}）应被索引命中 doc {i}"
            );
            // 与 matches() 逐 doc 对照
            for (j, m) in metas.iter().enumerate() {
                let hit_scan = crate::query::filter::matches(
                    &crate::schema::Filter::eq("f", json_to_string(v)),
                    m,
                );
                assert_eq!(
                    hit_scan,
                    bits.contains(j as DocId),
                    "doc {j} 在字段索引与 matches() 上判定不一致（查询值 {v}）"
                );
            }
        }
    }

    #[test]
    fn Eq对字符串形式严格区分() {
        // 反直觉但必须保留：2021.0 的 json_to_string 是 "2021.0"，与 "2021" 不等
        let fi = build(vec![serde_json::json!({"year": 2021.0})]);
        assert!(fi.eq_bits("year", "2021.0").unwrap().contains(0));
        assert!(
            fi.eq_bits("year", "2021").unwrap().is_empty(),
            "不能'顺手修正'数值字符串化行为，否则与 matches() 不等价"
        );
    }

    #[test]
    fn 字段缺失与值缺失都返回空集() {
        let fi = build(vec![serde_json::json!({"tag": "rust"})]);
        assert!(fi.eq_bits("nosuch", "x").unwrap().is_empty());
        assert!(fi.eq_bits("tag", "python").unwrap().is_empty());
        assert!(fi.eq_bits("tag", "rust").unwrap().contains(0));
    }

    #[test]
    fn Range仅覆盖数值且左闭右开() {
        let fi = build(vec![
            serde_json::json!({"year": 2021.0}),
            serde_json::json!({"year": 2022.0}),
            serde_json::json!({"year": 2023.0}),
            serde_json::json!({"year": "not-a-number"}),
        ]);
        let bits = fi.range_bits("year", 2022.0, 2023.0).unwrap();
        assert_eq!(bits.count_ones(), 1, "[2022, 2023) 只命中 doc 1");
        assert!(bits.contains(1));

        let all = fi.range_bits("year", 0.0, 3000.0).unwrap();
        assert_eq!(all.count_ones(), 3, "非数值的 doc 3 不应进 numbers");

        assert!(fi.range_bits("nosuch", 0.0, 1.0).unwrap().is_empty());
    }

    // ---- T7：增量维护 ≡ 全量重建 ----

    #[test]
    fn 增量维护结果与全量重建一致() {
        let metas = [
            serde_json::json!({"tag": "rust", "year": 2021.0}),
            serde_json::json!({"tag": "python", "year": 2022.0}),
            serde_json::json!({"tag": "rust", "year": 2023.0}),
            serde_json::json!({"tag": "go"}),
            serde_json::json!({"tag": "rust", "year": 2021.0}),
        ];

        // 增量路径
        let mut inc = FieldIndex::new();
        for (i, m) in metas.iter().enumerate() {
            inc.insert(i as DocId, m);
        }
        inc.remove(1, &metas[1]); // 删掉中间一个

        // 全量重建路径（把 doc 1 置为墓碑）
        let docs: Vec<Option<DocRecord>> = metas
            .iter()
            .enumerate()
            .map(|(i, m)| {
                if i == 1 {
                    None
                } else {
                    Some(doc_with(m.clone()))
                }
            })
            .collect();
        let mut full = FieldIndex::new();
        full.rebuild(&docs);

        // 逐 (field, value) 对照
        for field in ["tag", "year", "nosuch"] {
            for value in ["rust", "python", "go", "2021.0", "2022.0", "2023.0"] {
                assert_eq!(
                    inc.eq_bits(field, value),
                    full.eq_bits(field, value),
                    "field={field} value={value} 增量与全量不一致"
                );
            }
        }
        for (gte, lte) in [(0.0, 3000.0), (2021.0, 2023.0), (2023.0, 2024.0)] {
            assert_eq!(
                inc.range_bits("year", gte, lte),
                full.range_bits("year", gte, lte),
                "range [{gte}, {lte}) 增量与全量不一致"
            );
        }
    }

    #[test]
    fn remove后空位图的值键被回收() {
        let m = serde_json::json!({"tag": "rust"});
        let mut fi = FieldIndex::new();
        fi.insert(0, &m);
        assert!(fi.eq_bits("tag", "rust").unwrap().contains(0));
        fi.remove(0, &m);
        assert!(fi.eq_bits("tag", "rust").unwrap().is_empty());
        assert_eq!(
            fi.field_count(),
            1,
            "字段条目保留（低基数），但其下 value 键应被回收"
        );

        // 删完再插同值仍能命中（额度未被占用）
        fi.insert(1, &m);
        assert!(fi.eq_bits("tag", "rust").unwrap().contains(1));
    }

    // ---- 基数保护 ----

    #[test]
    fn 超过基数上限后该字段降级并退化为全扫() {
        let mut fi = FieldIndex::with_max_values(2);
        fi.insert(0, &serde_json::json!({"id": "a"}));
        fi.insert(1, &serde_json::json!({"id": "b"}));
        assert!(!fi.is_degraded("id"));

        // 第三个 value 触发降级
        fi.insert(2, &serde_json::json!({"id": "c"}));
        assert!(fi.is_degraded("id"), "超过上限应置 degraded");

        // 降级后求值返回 None → 调用方全扫（正确性不受影响）
        assert!(
            fi.eq_bits("id", "a").is_none(),
            "降级字段必须返回 None 以触发全扫，不能返回不完整的位图"
        );
        assert!(fi.eq_bits("id", "c").is_none());

        // 其他字段不受牵连
        fi.insert(3, &serde_json::json!({"tag": "rust"}));
        assert!(fi.eq_bits("tag", "rust").unwrap().contains(3));
    }

    #[test]
    fn 基数保护对数值键同样生效() {
        let mut fi = FieldIndex::with_max_values(2);
        fi.insert(0, &serde_json::json!({"ts": 1.0}));
        fi.insert(1, &serde_json::json!({"ts": 2.0}));
        fi.insert(2, &serde_json::json!({"ts": 3.0}));
        assert!(
            fi.is_degraded("ts"),
            "terms + numbers 合计超限才算降级，数值键不能成为漏网之缝"
        );
        assert!(fi.range_bits("ts", 0.0, 10.0).is_none());
    }

    #[test]
    fn 非object的metadata不panic() {
        let mut fi = FieldIndex::new();
        for v in [
            serde_json::json!(null),
            serde_json::json!(42),
            serde_json::json!("str"),
            serde_json::json!([1, 2]),
        ] {
            fi.insert(0, &v);
            fi.remove(0, &v);
        }
        assert_eq!(fi.field_count(), 0);
    }

    // ---- T15：remove 的时序（字段索引必须在 tombstone 前清）----
    //
    // 该时序在 `Index::remove` 里保证，这里只钉死契约：
    // 「先 remove 后 tombstone」与「只 tombstone 不 remove」结果不同。

    #[test]
    fn remove需要metadata且幂等() {
        let m = serde_json::json!({"tag": "rust", "year": 2021.0});
        let mut fi = FieldIndex::new();
        fi.insert(0, &m);
        fi.remove(0, &m);
        fi.remove(0, &m); // 幂等，不应 panic 也不应下溢
        assert!(fi.eq_bits("tag", "rust").unwrap().is_empty());
        assert!(fi.range_bits("year", 0.0, 3000.0).unwrap().is_empty());
        assert!(
            !fi.is_degraded("tag"),
            "重复删除不应把 value_count 扣成负数"
        );
        // 扣成负数会让 value_count 变成 usize::MAX，从而永久误判降级
        fi.insert(1, &m);
        assert!(fi.eq_bits("tag", "rust").unwrap().contains(1));
    }

    #[test]
    fn 全量重建对墓碑位跳过() {
        let docs = vec![
            Some(doc_with(serde_json::json!({"tag": "rust"}))),
            None,
            Some(doc_with(serde_json::json!({"tag": "python"}))),
        ];
        let mut fi = FieldIndex::new();
        fi.rebuild(&docs);
        assert!(fi.eq_bits("tag", "rust").unwrap().contains(0));
        assert!(!fi.eq_bits("tag", "rust").unwrap().contains(1));
        assert!(fi.eq_bits("tag", "python").unwrap().contains(2));
    }
}
