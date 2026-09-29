//! 字段定义、权重与过滤条件。
//!
//! P1 只用到最小集合：字段权重留位，过滤条件在 P4（T4-07）实现。

use serde::{Deserialize, Serialize};

/// 过滤条件（FR-14，P4 实现）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Filter {
    /// tag 等值匹配
    Eq {
        /// 元数据字段名
        field: String,
        /// 期望的值（字符串等值比较）
        value: String,
    },
    /// 数值范围 [gte, lte)
    Range {
        /// 元数据字段名
        field: String,
        /// 下界（含）
        gte: f64,
        /// 上界（不含）
        lte: f64,
    },
    /// 与
    And(Vec<Filter>),
    /// 或
    Or(Vec<Filter>),
}

impl Filter {
    /// 构造等值过滤
    pub fn eq(field: impl Into<String>, value: impl Into<String>) -> Self {
        Filter::Eq {
            field: field.into(),
            value: value.into(),
        }
    }

    /// 构造**命名空间**过滤（V2 Step 10 / FR-32 / D-S10-04）。
    ///
    /// 语义上**就是** [`Filter::eq`] —— 命名空间是「**检索级**隔离」的约定，
    /// 内核**不加第二套过滤路径**，也**不保留魔法字段名**（保留名会与用户元数据冲突，
    /// 且一旦保留就进了「内核语义」，而 FR-32 明确是检索级）。
    /// 本构造器因此**要求显式传入 `field`**：它只是把「这是命名空间过滤」这个**意图**
    /// 写成可读形式，不引入任何默认字段名。若你的场景有固定字段（如 `ns` / `tenant`），
    /// 在**调用侧**定义常量即可。
    ///
    /// ```no_run
    /// # use helix_core::schema::Filter;
    /// const NS_FIELD: &str = "ns";
    /// let f = Filter::namespace(NS_FIELD, "tenant-a");
    /// assert!(matches!(f, Filter::Eq { .. }));
    /// ```
    ///
    /// ⚠️ **成本与上限**：命名空间字段若**每会话一个**（高基数）⇒ 建库期必然**降级**
    /// （数值字段约 `512` 篇、字符串约 `1024` 篇撞线；降级**粘滞**、除重建索引外**不可逆**）
    /// ⇒ 隔离仍然**正确**，但该字段上的过滤退化为 **O(N) 全扫**、随语料规模线性增长
    /// （架构 §5.5.1 / ADR-010 代价段 / 风险 `R64`）。
    /// ⇒ **观测方式**：[`Metrics::filter_degraded`](crate::query::Metrics::filter_degraded) 直接可读。
    pub fn namespace(field: impl Into<String>, value: impl Into<String>) -> Self {
        Filter::Eq {
            field: field.into(),
            value: value.into(),
        }
    }
}
