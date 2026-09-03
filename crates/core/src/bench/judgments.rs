//! 评测标注（judgments）加载与校验。
//!
//! # 格式（p5-design.md 5.2 产出）
//!
//! ```jsonl
//! {"qid":"30321","query":"太阳花怎么养","type":"natural",
//!  "relevance":[{"source":"82391","grade":3},{"source":"9811","grade":0}]}
//! ```
//!
//! `grade 0` = **已判定负例**（信息保留：池内零假负例的关键——
//! 已标注为不相关的段落不会在指标里被误当正例）。
//!
//! # 校验（fail fast）
//!
//! - 坏行带行号报错
//! - `grade ∉ {0..3}` 报错
//! - `source` 在索引中不存在 → 报错（防 pid 映射断裂后指标静默失真）

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::index::Index;

/// 一条评测查询及其 graded 标注。
#[derive(Debug, Clone, Deserialize)]
pub struct Judgment {
    pub qid: String,
    pub query: String,
    /// 分桶：mixed / natural / exact / paraphrase
    #[serde(rename = "type")]
    pub qtype: String,
    /// source → grade（0 = 已判定负例）
    pub relevance: Vec<RelEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RelEntry {
    pub source: String,
    pub grade: u8,
}

/// 加载 `t2-queries.jsonl`。坏行 / 非法 grade 带行号报错。
pub fn load_judgments(path: &Path) -> Result<Vec<Judgment>> {
    let content = std::fs::read_to_string(path).map_err(Error::Io)?;
    let mut out = Vec::new();
    for (lineno, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let j: Judgment = serde_json::from_str(line).map_err(|e| {
            Error::InvalidInput(format!(
                "judgments 第 {} 行解析失败（{}）: {e}",
                lineno + 1,
                path.display()
            ))
        })?;
        for r in &j.relevance {
            if r.grade > 3 {
                return Err(Error::InvalidInput(format!(
                    "judgments 第 {} 行（qid={}）source {} 的 grade {} 超出 0..=3",
                    lineno + 1,
                    j.qid,
                    r.source,
                    r.grade
                )));
            }
        }
        out.push(j);
    }
    if out.is_empty() {
        return Err(Error::InvalidInput(format!(
            "judgments 为空: {}",
            path.display()
        )));
    }
    Ok(out)
}

/// 校验全部标注的 source 都存在于索引中（文档级）。
///
/// 缺失即报错——pid 映射断裂会让该段落的所有指标静默归零，必须 fail fast。
/// 返回每个 source 对应的 chunk 数（诊断用：多 chunk source 的双计风险提示）。
pub fn validate_sources(judgments: &[Judgment], index: &Index) -> Result<HashMap<String, usize>> {
    // 按 chunk 聚合 source -> chunk 数
    let mut per_source: HashMap<String, usize> = HashMap::new();
    for chunk in index.live_chunks() {
        if let Some(doc) = index.doc(chunk.doc_id) {
            *per_source.entry(doc.source.clone()).or_insert(0) += 1;
        }
    }

    let mut multi_chunk = Vec::new();
    for j in judgments {
        for r in &j.relevance {
            match per_source.get(&r.source) {
                Some(n) => {
                    if *n > 1 {
                        multi_chunk.push((r.source.clone(), *n));
                    }
                }
                None => {
                    return Err(Error::InvalidInput(format!(
                        "judgments（qid={}）指向不存在的 source {:?}——pid 映射断裂",
                        j.qid, r.source
                    )));
                }
            }
        }
    }
    if !multi_chunk.is_empty() {
        // 不报错（demo 语料合法多 chunk），但 t2 语料应在数据侧已断言单 chunk
        eprintln!(
            "[warn] {} 个 source 有多 chunk（评测按首次出现计位次）: {:?}…",
            multi_chunk.len(),
            &multi_chunk[..multi_chunk.len().min(3)]
        );
    }
    Ok(per_source)
}

impl Judgment {
    /// grades 视图（metrics::evaluate 的输入）。
    pub fn grades(&self) -> HashMap<String, u8> {
        self.relevance
            .iter()
            .map(|r| (r.source.clone(), r.grade))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::MixedAnalyzer;
    use crate::chunk::Chunker;
    use crate::document::Document;

    fn tmp_judgments(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("idx-bench-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{name}.jsonl"));
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn 加载与校验() {
        let p = tmp_judgments(
            "load",
            "{\"qid\":\"1\",\"query\":\"测试\",\"type\":\"exact\",\"relevance\":[{\"source\":\"a\",\"grade\":3},{\"source\":\"b\",\"grade\":0}]}\n\
             {\"qid\":\"2\",\"query\":\"另一个\",\"type\":\"mixed\",\"relevance\":[{\"source\":\"b\",\"grade\":1}]}\n",
        );
        let js = load_judgments(&p).unwrap();
        assert_eq!(js.len(), 2);
        assert_eq!(js[0].qtype, "exact");
        assert_eq!(js[0].grades().len(), 2);

        let analyzer = MixedAnalyzer::new();
        let chunker = Chunker::default();
        let mut index = Index::new();
        for (i, (src, text)) in [("a", "文本一"), ("b", "文本二")].iter().enumerate() {
            index
                .add(
                    Document {
                        doc_id: 0,
                        source: src.to_string(),
                        metadata: serde_json::json!({}),
                        content_hash: i as u64,
                    },
                    chunker.chunk(0, text),
                    &analyzer,
                )
                .unwrap();
        }
        let per_source = validate_sources(&js, &index).unwrap();
        assert_eq!(per_source["a"], 1);
        assert_eq!(per_source["b"], 1);
    }

    #[test]
    fn 坏行带行号报错() {
        let p = tmp_judgments(
            "badjson",
            "{\"qid\":\"1\",\"query\":\"q\",\"type\":\"exact\",\"relevance\":[]}\nnot-json\n",
        );
        let err = load_judgments(&p).unwrap_err();
        let msg = match err {
            Error::InvalidInput(m) => m,
            other => panic!("应为 InvalidInput，得到 {other:?}"),
        };
        assert!(msg.contains("第 2 行"), "msg = {msg}");
    }

    #[test]
    fn 非法grade报错() {
        let p = tmp_judgments(
            "badgrade",
            "{\"qid\":\"1\",\"query\":\"q\",\"type\":\"exact\",\"relevance\":[{\"source\":\"a\",\"grade\":4}]}",
        );
        let err = load_judgments(&p).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[test]
    fn 指向不存在source报错() {
        let p = tmp_judgments(
            "ghost",
            "{\"qid\":\"1\",\"query\":\"q\",\"type\":\"exact\",\"relevance\":[{\"source\":\"ghost\",\"grade\":1}]}",
        );
        let js = load_judgments(&p).unwrap();
        let analyzer = MixedAnalyzer::new();
        let chunker = Chunker::default();
        let mut index = Index::new();
        index
            .add(
                Document {
                    doc_id: 0,
                    source: "real".into(),
                    metadata: serde_json::json!({}),
                    content_hash: 0,
                },
                chunker.chunk(0, "文本"),
                &analyzer,
            )
            .unwrap();
        let err = validate_sources(&js, &index).unwrap_err();
        let msg = match err {
            Error::InvalidInput(m) => m,
            other => panic!("应为 InvalidInput，得到 {other:?}"),
        };
        assert!(msg.contains("ghost"), "msg = {msg}");
    }

    #[test]
    fn 空文件报错() {
        let p = tmp_judgments("empty", "");
        assert!(load_judgments(&p).is_err());
    }
}
