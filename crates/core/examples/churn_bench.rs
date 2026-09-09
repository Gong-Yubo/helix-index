//! churn（零散写入）workload —— V2 Step 4 墓碑回收的验收基准（S4-09 / 验收 1）。
//!
//! 目的：在**可复现**的「零散写入」下测量「三体积不无界增长」（issue #21 主验收）。
//! 每轮随机删 `churn` 比例 + 追加等量新文档 → `save` → 记录；末轮后
//! `compact_and_save` → 再记录一次。输出 CSV + 判定行（验收 1 三条判据 + 验收 2）。
//!
//! # 为什么用合成 Embedder
//!
//! 体积/回收实验**不依赖语义相关性**（§4.8 / D-S4-06）：只需向量维度与条数正确、
//! 且能跨 `save`/`load` 复现（指纹含 embedder id + dim，见 `Embedder::id`）。
//! 内置 `SynthEmbedder`（LCG，dim=512，L2 归一化，id=`"synth-512"`）——
//! 确定性、无模型依赖、秒级，10 万级也几分钟内跑完。
//!
//! # 构造要点（务必理解）
//!
//! - **强制单 chunk**（`Chunker::new(200_000, 0)`）：每 doc == 1 chunk_id == 1 向量点，
//!   故本 workload 下「存活 chunk 数」==「有向量的存活数」⇒ 验收 3 退化为
//!   `nb_point == 存活 chunk 数`（对齐 §1.3 验收 3 的口径）。
//! - **全量向量**：所有 doc 都经 SynthEmbedder 入库 ⇒ 无「缺向量的存活 chunk」，
//!   简化判定（§4.4 两层比较在本 workload 不适用）。
//! - 追加文档的 `text` 带唯一后缀（churn 轮次 + 序号）⇒ `content_hash` 唯一，
//!   不被幂等 dedupe（FR-15）误吞。
//!
//! 运行（release，否则体积/耗时无意义；语料路径相对进程 cwd——repo root）：
//! ```
//! cargo run --release -p helix-core --example churn_bench -- \
//!     --corpus data/synth-10000-corpus.jsonl --size 10000 \
//!     --rounds 5 --churn 0.1 --out /tmp/churn.csv
//! ```
//! 输出 CSV（`round,snapshot_bytes,graph_bytes,data_bytes,raw_vectors,nb_point,
//! coldstart_ms,graph_status`）+ 末行判定 PASS/FAIL。
//!
//! ⚠️ 产物带 `SynthEmbedder` 指纹，**仅供本工具自测 / eval_churn.sh 消费**，
//! 不得当通用快照样例（真实装配 load 不出它——指纹不符，属预期）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use helix_core::chunk::Chunker;
use helix_core::embed::Embedder;
use helix_core::error::Result;
use helix_core::search::{GraphStatus, SearchIndexBuilder};

/// 确定性合成 Embedder（LCG + L2 归一化，dim=512）。
///
/// `id = "synth-512"` 固定 ⇒ 快照指纹含它；`eval_churn.sh`（churn_bench 自测闭环）
/// 用同一 SynthEmbedder 装配即可 load 出本工具产出的快照（配置指纹一致）。
///
/// ⚠️ 不要混淆 `helix compact` CLI：它走**默认装配**，对 `synth-512` 快照
/// （embedder_id 非空 ⇒ 严格校验）必然 `ConfigMismatch`、无法 load——CLI 只能
/// compact 用默认装配（bge）建的库（见 user-guide §1.5）。
struct SynthEmbedder {
    dim: usize,
}

impl Embedder for SynthEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| hash_unit_vec(t, self.dim)).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(hash_unit_vec(text, self.dim))
    }

    fn is_normalized(&self) -> bool {
        true // 输出已是 L2 单位向量
    }

    fn id(&self) -> &'static str {
        "synth-512"
    }
}

/// 文本 → 512 维 L2 单位向量（LCG，跨进程确定性可复现）。
/// 与 `tests/common` 的 `TestEmbedder` 同思路，但维度固定 512、id 固定 `synth-512`。
fn hash_unit_vec(text: &str, dim: usize) -> Vec<f32> {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (dim as u64).wrapping_mul(0x9e37_79b9);
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    let mut next = || {
        h ^= h << 13;
        h ^= h >> 7;
        h ^= h << 17;
        ((h >> 11) as u32 as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    let mut v: Vec<f32> = (0..dim).map(|_| next()).collect();
    // L2 归一化（数值上可能极小，防除零）
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// xorshift 确定性命中选择器（删/追的随机性可复现）。
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

struct Args {
    corpus: PathBuf,
    size: usize,
    rounds: usize,
    churn: f64, // 0~1
    out: PathBuf,
    dim: usize,
}

fn main() -> anyhow::Result<()> {
    let a = parse_args()?;
    println!(
        "churn_bench: size={} rounds={} churn={:.0}% dim={}",
        a.size,
        a.rounds,
        a.churn * 100.0,
        a.dim
    );

    // 读语料正文（只取 text；向量由 SynthEmbedder 决定，不需要真实 embed）
    let all_texts = read_texts(&a.corpus)?;
    if all_texts.is_empty() {
        anyhow::bail!("语料为空: {}", a.corpus.display());
    }

    // ---- 确定性初始语料池：取前 size 个的 text（追加时复用 + 后缀保唯一）----
    let base_texts: Vec<String> = all_texts.iter().take(a.size).cloned().collect();

    // ---- 门面层装配（SynthEmbedder + HNSW + 强制单 chunk）----
    // （主建库与 reload 各自调 `synth_builder` 拿新装配；build/load 都消费装配）
    let dir = tempfile::tempdir()?;
    let snap = dir.path().join("churn.idx");

    // ---- 第 0 轮：建 size 个 → save 基线 ----
    let mut idx = synth_builder(a.dim).build();
    let mut alive: std::collections::HashSet<u32> =
        std::collections::HashSet::with_capacity(a.size); // 存活 doc_id
    for t in &base_texts {
        let out = idx.add(make_doc(t, None))?;
        alive.insert(out.doc_id);
    }
    idx.save(&snap)?;
    let base_snapshot_bytes = on_disk(&snap).0;

    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);

    // CSV 输出缓冲
    let mut rows: Vec<String> = Vec::new();
    rows.push("round,snapshot_bytes,graph_bytes,data_bytes,raw_vectors,nb_point,coldstart_ms,graph_status".to_string());

    // 删/追加：随机挑 churn×alive 个删（从存活集剔除），再追加等量
    let mut next_new = a.size; // 追加文档全局序号（text 唯一，防 FR-15 dedupe）

    for r in 0..a.rounds {
        let del_n = ((alive.len() as f64) * a.churn).floor() as usize;
        let del_n = del_n.clamp(1, alive.len());
        // 随机抽 del_n 个存活 doc_id 删（Fisher-Yates）
        let mut pool: Vec<u32> = alive.iter().copied().collect();
        for i in 0..del_n {
            let j = rng.below(pool.len() - i) + i;
            pool.swap(i, j);
        }
        for &d in &pool[..del_n] {
            idx.remove(d)?;
            alive.remove(&d);
        }
        // 追加等量新文档
        for _ in 0..del_n {
            let t = unique_text(&base_texts, &mut next_new, r);
            let out = idx.add(make_doc(&t, Some(r)))?;
            alive.insert(out.doc_id);
        }
        idx.save(&snap)?;
        rows.push(record_row(r, &snap, &idx));
    }

    // ---- 末轮：compact_and_save → 落盘压实 + 重发 manifest ----
    let rep = idx.compact_and_save(&snap)?;
    let (c_s, c_g, c_d) = on_disk(&snap);
    let c_stats = idx.tombstone_stats();
    let c_nb = graph_nb(&snap);

    // ---- 验收 2：重新 load（同 SynthEmbedder 装配）→ 冷启动 + Loaded ----
    let lstart = Instant::now();
    let reloaded = synth_builder(a.dim).load(&snap)?;
    let coldstart_ms = lstart.elapsed().as_millis();
    let loaded_ok = matches!(reloaded.graph_status(), GraphStatus::Loaded);

    // compact 行（CSV）：`coldstart_ms` 填 reload 实测值、`graph_status` 填 reload 后
    // 的最终状态（Loaded = 快路径）；不再像旧版对普通行那样占位填 0（评审建议 3）。
    rows.push(format!(
        "compact,{c_s},{c_g},{c_d},{},{c_nb},{coldstart_ms},{}",
        c_stats.raw_vectors,
        gs_label(reloaded.graph_status())
    ));

    // ---- 判定行（验收 1 三条判据 + 验收 2；eval_churn.sh 据此 PASS/FAIL）----
    // J1（回收发生）：末轮 compact 后图 sidecar 回落（< compact 前那一轮）——本行即
    //    compact 行，故与「compact 前的末个普通轮」图字节对比，见 rows 倒数第 2 行。
    let last_round_graph = graph_bytes_of_last_round(&rows);
    let j1 = c_g < last_round_graph; // 图字节回落 = 墓碑点被回收
                                     // J2（不累积）：末轮 compact 后快照正文字节 相对 首轮全活基线 增量 <15%
                                     //    （Design：单次波动 ±10~15%；都约 size 个存活，只差文本后缀与死亡槽）
    let j2 = (c_s as f64) < (base_snapshot_bytes as f64) * 1.15;
    // J3（验收 2）：reload 走快路径 Loaded（非 Rebuilt）
    let j3 = loaded_ok;

    // ---- 写 CSV + 判定 ----
    let csv = rows.join("\n");
    if a.out.as_os_str() == "-" {
        println!("{csv}");
    } else {
        std::fs::write(&a.out, csv)?;
        println!("CSV 已写: {}", a.out.display());
    }
    println!(
        "compact_judgment: snapshot_before_last={} -> compact_snapshot={} | last_round_graph={} -> compact_graph={} | reload_coldstart_ms={}",
        base_snapshot_bytes, c_s, last_round_graph, c_g, coldstart_ms
    );
    println!(
        "J1_gc_reclaims_graph={} J2_no_growth={} J3_reload_loaded={}  (reclaimed_chunks={}, remapped={})",
        j1, j2, j3, rep.reclaimed_chunks, rep.remapped
    );
    let all_ok = j1 && j2 && j3;
    println!("VERDICT: {}", if all_ok { "PASS" } else { "FAIL" });
    Ok(())
}

/// 取 CSV 数据行里「compact 前的最后一个普通轮」的 graph_bytes。
/// 数据行形如 `r0,...` / `r1,...`；compact 行以 `compact` 开头。
fn graph_bytes_of_last_round(rows: &[String]) -> u64 {
    for line in rows.iter().rev() {
        if let Some(stripped) = line.strip_prefix('r') {
            // `rN,snap,graph,data,...` → 取第 3 列（index 2）
            if let Some(third) = stripped.split(',').nth(2) {
                if let Ok(v) = third.parse() {
                    return v;
                }
            }
        }
    }
    0
}

/// SynthEmbedder + HNSW + 强制单 chunk 的门面装配。
/// 建库与 reload 都用它，保证快照指纹（embedder id + dim）一致可复现。
fn synth_builder(dim: usize) -> SearchIndexBuilder {
    SearchIndexBuilder::default()
        .embedder(Some(Arc::new(SynthEmbedder { dim }) as Arc<dyn Embedder>))
        .vector_backend(helix_core::search::VectorBackend::Hnsw)
        .chunker(Chunker::new(200_000, 0)) // 单 chunk：doc == chunk == 1 向量点
        .batch_size(4096) // 大缓冲：万级批 embed
}

/// 追加文本：从 corpus 取 base + 轮次/序号后缀 → content_hash 唯一（防 FR-15 dedupe）。
fn unique_text(base: &[String], next: &mut usize, round: usize) -> String {
    let b = &base[*next % base.len()];
    *next += 1;
    format!("{b} churn-r{round}-{n}", n = *next)
}

fn make_doc(text: &str, round: Option<usize>) -> helix_core::document::Document {
    let source = match round {
        Some(r) => format!("churn-add-r{r}"),
        None => "churn-base".to_string(),
    };
    helix_core::document::Document::new(text).with_source(source)
}

fn record_row(r: usize, snap: &std::path::Path, idx: &helix_core::search::SearchIndex) -> String {
    let (s, g, d) = on_disk(snap);
    let stats = idx.tombstone_stats();
    let nb = graph_nb(snap);
    let gs = idx.graph_status();
    format!(
        "r{r},{s},{g},{d},{},{nb},0,{}",
        stats.raw_vectors,
        gs_label(gs)
    )
}

fn graph_nb(path: &std::path::Path) -> u64 {
    let paths = helix_core::storage::graph_paths(path);
    match helix_core::storage::read_manifest(&paths.manifest) {
        Ok(Some(m)) => m.nb_point,
        _ => 0,
    }
}

fn on_disk(path: &std::path::Path) -> (u64, u64, u64) {
    let paths = helix_core::storage::graph_paths(path);
    let s = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let g = std::fs::metadata(&paths.graph)
        .map(|m| m.len())
        .unwrap_or(0);
    let d = std::fs::metadata(&paths.data).map(|m| m.len()).unwrap_or(0);
    (s, g, d)
}

fn gs_label(s: &GraphStatus) -> &'static str {
    match s {
        GraphStatus::Loaded => "Loaded",
        GraphStatus::Rebuilt(_) => "Rebuilt",
        GraphStatus::NotApplicable => "NotApplicable",
        GraphStatus::PersistFailed(_) => "PersistFailed",
    }
}

fn read_texts(path: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let content = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (lineno, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| anyhow::anyhow!("第 {} 行非法 JSON: {e}", lineno + 1))?;
        let t = v
            .get("text")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow::anyhow!("第 {} 行缺 text", lineno + 1))?;
        out.push(t.to_string());
    }
    Ok(out)
}

fn parse_args() -> anyhow::Result<Args> {
    let mut corpus = PathBuf::from("data/synth-10000-corpus.jsonl");
    let mut size = 10000usize;
    let mut rounds = 5usize;
    let mut churn = 0.1f64;
    let mut out = PathBuf::from("-");
    let dim = 512usize;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        // 拆成 (name, Option<inline_value>)：--name=value → 内联值；--name → None
        let (name, inline) = match arg.strip_prefix("--") {
            Some(rest) => match rest.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (rest.to_string(), None),
            },
            None => anyhow::bail!("未知参数: {arg}（支持 --corpus --size --rounds --churn --out）"),
        };
        let value = match inline {
            Some(v) => v,
            None => args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--{name} 缺少值"))?,
        };
        match name.as_str() {
            "corpus" => corpus = PathBuf::from(value),
            "size" => size = value.parse()?,
            "rounds" => rounds = value.parse()?,
            "churn" => churn = value.parse()?,
            "out" => out = PathBuf::from(value),
            other => anyhow::bail!("未知参数: --{other}"),
        }
    }
    Ok(Args {
        corpus,
        size,
        rounds,
        churn,
        out,
        dim,
    })
}
