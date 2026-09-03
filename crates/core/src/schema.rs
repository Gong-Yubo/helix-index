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
}
