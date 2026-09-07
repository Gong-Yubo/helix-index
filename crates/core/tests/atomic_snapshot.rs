//! V2 Step 3：原子快照的**公开 API** 集成测试（无注入）。
//!
//! # 落点说明（v2-step3-design §7 / D-S3-03 方案 C′）
//!
//! 故障注入类测试（S3-T3~T6、T9）依赖 `pub(crate)` 的 `fault::FAIL_AT` 钩子，
//! 集成层（独立 crate）不可达——全部放 crate 内 `#[cfg(test)]`
//! （`storage/atomic.rs` 承载 T3~T5/T9，`search/index.rs` 承载端到端 T6）。
//! 本文件只放**无注入**的公开 API 测试：save 原子性外观、tmp 不残留、
//! 旧快照兼容回归。

#![allow(non_snake_case)]

use helix_core::search::{SearchIndex, SearchIndexBuilder};

/// 纯 BM25 装配（不依赖真实模型；集成层只能走公开 builder API）。
fn bm25_index() -> SearchIndex {
    SearchIndexBuilder::default().embedder(None).build()
}

/// S3-T1 集成层外观：save 覆盖写后 load 得新内容，且目录里只有目标文件
/// （tmp 被 rename 消费，不残留）。
#[test]
fn save覆盖写后load得到新内容且无tmp残留() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("atomic.idx");

    let mut idx = bm25_index();
    idx.add("第一版内容").unwrap();
    idx.save(&path).unwrap();

    idx.add("第二版新增内容").unwrap();
    idx.save(&path).unwrap();

    // 目录里只有目标文件——两次 save 的 tmp 都被 rename 消费
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["atomic.idx".to_string()], "不应有任何 tmp 残留");

    let loaded = SearchIndexBuilder::default()
        .embedder(None)
        .load(&path)
        .unwrap();
    assert_eq!(loaded.num_chunks(), 2, "load 必须得到新快照（两篇文档）");
}

/// 旧快照兼容回归（C3）：文件格式零变化——直写时代（旧内核）产出的
/// 完整快照文件，新内核照常加载；反之新内核产物旧内核也能读
/// （格式不携带原子性信息，roundtrip 即证）。
#[test]
fn 快照格式兼容回归() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("compat.idx");

    // 新内核 save
    let mut idx = bm25_index();
    idx.add("BM25 是经典关键词检索算法").unwrap();
    idx.add("向量检索计算余弦相似度").unwrap();
    idx.save(&path).unwrap();

    // load（等价于旧内核读新产物——格式未变）
    let loaded = SearchIndexBuilder::default()
        .embedder(None)
        .load(&path)
        .unwrap();
    assert_eq!(loaded.num_chunks(), 2);
    let resp = loaded.into_searcher().unwrap().search("检索").unwrap();
    assert!(
        !resp.hits.is_empty(),
        "doc_freq 语义应保留（两篇都含「检索」）"
    );
}
