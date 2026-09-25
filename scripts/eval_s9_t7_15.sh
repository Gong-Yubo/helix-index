#!/usr/bin/env bash
#
# V2 Step 9 · S9-06 / **T7-15** —— **假负例敏感度对照** 一键跑（设计 §4.7；**只作对照**）
#
# 问的问题：**随机负例段落**里可能藏着「其实相关但未标注」的文档。
#   把它整批换成「**仅 qrels 语料**」（= 只保留 320 条 query 的 qrels 段落，随机负例全剔），
#   读数会怎么变？⇒ 差值 = 随机负例带来的**假负例影响幅度**（`eval-report.md` §8.16 记录）。
#
# ⚠️ **解读边界（必须在报告里同写）**：这一换**同时**改变了两件事 ——
#   ① 假负例的多少；② **语料规模与统计量**（12000 → 5042 篇 ⇒ BM25 的 `idf`、HNSW 图都变）。
#   本脚本量出的是**两者的合成效应**，**不能**读成「纯假负例效应」。⇒ 结论不外推（`eval-report.md` §9）。
#
# # 口径（与主基线**逐项相同**，只换语料）
#
#   对照语料 = `data/t2-corpus.jsonl` 中 `source ∈ 全部 qrels 段落` 的行（**含 grade 0 已判定负例**
#   —— 与 `t2_prep.rs` 的「池内零假负例」同口径）；
#   建库口径 = `--vectors --single-chunk`（与主图同：12000 篇 ⇒ 12000 chunk）；
#   两臂 = 同一批 320 条 query × 同一 `-k/-runs/--rrf-*`，只有 `--index` 不同。
#
# # ⚠️ 为什么必须**重建索引**而不是在检索时过滤
#
#   过滤只挡「检索结果」，挡不住**统计量**（`idf` 按语料算、图按语料建）
#   ⇒ 那样量不出「语料里有没有那些段落」的影响。故本脚本会真建一份对照快照（约 95 s / 5042 篇）。
#
# # 用法
#
#   ./scripts/eval_s9_t7_15.sh                            # 缺对照资产则装配（约 1.5 min），然后跑两臂
#   OUT_DIR=data/eval/t7-15 ./scripts/eval_s9_t7_15.sh    # 读数落持久产物（与 eval_s9.sh 同口径）
#   REBUILD=1 ./scripts/eval_s9_t7_15.sh                  # 强制重装对照资产（改了口径必须重装）
#
# # 产物
#
#   ${RUN_DIR}/json/main.json     主基线臂（`data/t2-frozen.snapshot`）
#   ${RUN_DIR}/json/ctrl.json     对照臂（仅 qrels 语料）
#   ${RUN_DIR}/readings.tsv       逐 (桶, 臂) 的 MRR + 配对的 Δ（头含两个语料的指纹）
#
# 依赖：bash / python3 / 已构建的 `target/release/helix`（脚本会自动 `cargo build --release -p helix`）。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

MAIN_INDEX="${MAIN_INDEX:-data/t2-frozen.snapshot}"
QUERIES="${QUERIES:-data/t2-queries.jsonl}"
CORPUS="${CORPUS:-data/t2-corpus.jsonl}"
WORK="${WORK:-data/eval/t7-15}"
CTRL_INDEX="${CTRL_INDEX:-$WORK/qrels-only.snapshot}"

K_GRID="10"
RRF_K="60"
BASE_WEIGHTS="1.0,1.5"

RUN_DIR="$(mktemp -d)"
OUT_DIR="${OUT_DIR:-}"
JSON_DIR="$RUN_DIR/json"
mkdir -p "$JSON_DIR"
TSV="$RUN_DIR/readings.tsv"

BIN="target/release/helix"

echo "=== T7-15 假负例敏感度对照（只作对照；S9-06）==="
echo "主基线语料 = ${MAIN_INDEX}"
echo "对照语料   = ${CTRL_INDEX}（仅 qrels；随机负例全剔）"
echo "查询集     = ${QUERIES}（320 条）"
echo "共同参数   = --k ${K_GRID} --runs 1 --rrf-k ${RRF_K} --rrf-weights ${BASE_WEIGHTS}、精排关、--no-latency"
echo "产物目录   = ${RUN_DIR}"
if [ -n "$OUT_DIR" ]; then
  echo "持久产物   = ${OUT_DIR}/readings.tsv（跑完后复制）"
fi
echo

for f in "$MAIN_INDEX" "$QUERIES" "$CORPUS"; do
  if [ ! -f "$f" ]; then
    echo "❌ 缺输入：${f}" >&2
    exit 2
  fi
done

echo "── 构建（release；与 eval_s9.sh 同口径）──"
cargo build --release -p helix 2>&1 | tail -1

# ── ① 装配对照资产（缺则建；REBUILD=1 强制）──
if [ ! -f "$CTRL_INDEX" ] || [ "${REBUILD:-0}" = "1" ]; then
  echo
  echo "── ① 装配「仅 qrels 语料」对照资产 ──"
  python3 - "$QUERIES" "$CORPUS" "$WORK" <<'PY'
import hashlib
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

queries_p, corpus_p, work_p = sys.argv[1], sys.argv[2], sys.argv[3]
queries, corpus, work = Path(queries_p), Path(corpus_p), Path(work_p)


def sha16(p):
    h = hashlib.sha256()
    with open(p, "rb") as fh:
        for blk in iter(lambda: fh.read(1 << 20), b""):
            h.update(blk)
    return h.hexdigest()[:16]


# qrels 段落集合（全部 relevance[].source，含 grade 0）
rel_sources, n_queries = set(), 0
with open(queries, encoding="utf-8") as fh:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        j = json.loads(line)
        n_queries += 1
        for r in j["relevance"]:
            rel_sources.add(str(r["source"]))

work.mkdir(parents=True, exist_ok=True)
out_path = work / "qrels-only-corpus.jsonl"
kept, seen, total = 0, set(), 0
with open(corpus, encoding="utf-8") as fin, open(out_path, "w", encoding="utf-8") as fout:
    for line in fin:
        if not line.strip():
            continue
        total += 1
        src = str(json.loads(line)["source"])
        if src in rel_sources:
            fout.write(line if line.endswith("\n") else line + "\n")
            kept += 1
            seen.add(src)

missing = sorted(rel_sources - seen)
if missing:
    print(f"❌ {len(missing)} 个 qrels source 不在语料里（前 5: {missing[:5]}）", file=sys.stderr)
    sys.exit(3)

meta = {
    "asset": "t7-15-qrels-only-corpus",
    "built_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "source_queries": queries_p,
    "source_queries_sha256_16": sha16(queries),
    "source_corpus": corpus_p,
    "source_corpus_sha256_16": sha16(corpus),
    "n_queries": n_queries,
    "n_qrels_sources": len(rel_sources),
    "n_rows_in": total,
    "n_rows_out": kept,
    "note": "qrels-only：随机负例段落全部剔除（grade 0 的已判定负例仍在）",
}
(work / "asset-meta.json").write_text(
    json.dumps(meta, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
)
print(
    f"  qrels 段落 {len(rel_sources)} 个 ⇒ 语料 {total} 行过滤为 {kept} 行"
    f"（自证缺失 = 0）；指纹 sha256 前 16 = {sha16(out_path)}"
)
PY
  "$BIN" build --input "$WORK/qrels-only-corpus.jsonl" \
    --output "$CTRL_INDEX" --vectors --single-chunk 2>&1 | tail -3
else
  echo
  echo "（对照索引已存在：${CTRL_INDEX}；REBUILD=1 可强制重装）"
fi

# ── ② 两臂 ──
run_arm() {
  local name="$1" index="$2"
  echo "--- ${name} ---"
  "$BIN" bench \
    --index "$index" \
    --queries "$QUERIES" \
    --modes bm25,vector,hybrid \
    --k "$K_GRID" \
    --runs 1 \
    --rrf-k "$RRF_K" \
    --no-latency \
    --json "$JSON_DIR/$name.json" \
    --rrf-weights "$BASE_WEIGHTS" > "$RUN_DIR/logs_$name.out" 2>&1 || {
      echo "❌ 臂 ${name} 失败：" >&2
      tail -8 "$RUN_DIR/logs_$name.out" >&2 || true
      exit 1
    }
  grep -E '^(hybrid|bm25|vector)' "$RUN_DIR/logs_$name.out" | sed 's/^/    /' || true
}

echo
echo "── ② 主基线臂（12000 篇：qrels + 7000 随机负例）──"
run_arm main "$MAIN_INDEX"
echo
echo "── ③ 对照臂（5042 篇：仅 qrels）──"
run_arm ctrl "$CTRL_INDEX"

# ── ③ 配对 + Δ ──
echo
echo "── ④ 按 qid 配对算 Δ（同一批 query，只有语料不同）──"
python3 - "$JSON_DIR" "$TSV" "$K_GRID" "$RRF_K" "$BASE_WEIGHTS" \
  "$MAIN_INDEX" "$CTRL_INDEX" "$QUERIES" <<'PY'
import io
import json
import sys

(json_dir, tsv_path, k_grid, rrf_k, base_weights,
 main_index, ctrl_index, queries_path) = sys.argv[1:]

BUCKETS = ["exact", "natural", "mixed", "paraphrase"]
MODES = ["bm25", "vector", "hybrid"]


def load(name):
    with io.open(f"{json_dir}/{name}.json", encoding="utf-8") as fh:
        return json.load(fh)


def per_query_mrr(doc, mode):
    return {p["qid"]: p["mrr"] for p in doc["modes"][mode]["per_query"]}


main, ctrl = load("main"), load("ctrl")
assert main["n_queries"] == ctrl["n_queries"], "两臂 query 数不一致"
assert main["n_corpus"] == 12000, f"主基线语料数 = {main['n_corpus']}（期望 12000）"
print(f"语料规模：主基线 {main['n_corpus']} 篇 → 对照 {ctrl['n_corpus']} 篇"
      f"（剔除随机负例 {main['n_corpus'] - ctrl['n_corpus']} 篇）")
print()
print("=== MRR@10(1)：两臂 × 三模式（同一批 query）===")
print(f"{'mode':8} {'主基线(12000)':>14} {'对照(5042)':>12} {'Δ（对照 − 主）':>16}")
rows = []
for mode in MODES:
    m_all = main["modes"][mode]["thr1"]["mrr"]
    c_all = ctrl["modes"][mode]["thr1"]["mrr"]
    print(f"{mode:8} {m_all:>14.4f} {c_all:>12.4f} {c_all - m_all:>+16.4f}")
    rows.append((mode, "global", m_all, c_all, c_all - m_all))
print()
print("=== hybrid 的逐桶对照 ===")
print(f"{'bucket':12} {'主基线':>10} {'对照':>10} {'Δ':>10}")
for b in BUCKETS:
    m_b = main["modes"]["hybrid"]["buckets"][b]["mrr"]
    c_b = ctrl["modes"]["hybrid"]["buckets"][b]["mrr"]
    n_b = main["modes"]["hybrid"]["buckets"][b]["n"]
    print(f"{b:12} {m_b:>10.4f} {c_b:>10.4f} {c_b - m_b:>+10.4f}   (n={n_b})")
    rows.append(("hybrid", b, m_b, c_b, c_b - m_b))

# 逐 query 配对（hybrid）：Δ 的分布形态（不只报均值）
m_pq, c_pq = per_query_mrr(main, "hybrid"), per_query_mrr(ctrl, "hybrid")
assert set(m_pq) == set(c_pq), "两臂 qid 集合不一致 ⇒ 配对无效"
deltas = sorted(c_pq[q] - m_pq[q] for q in m_pq)
n = len(deltas)
print()
print(f"=== hybrid 逐 query 配对（n={n}）===")
print(f"  均值 {sum(deltas) / n:+.4f} | 中位 {deltas[n // 2]:+.4f} | "
      f"min {deltas[0]:+.4f} | max {deltas[-1]:+.4f}")
print(f"  变差条数 = {sum(1 for d in deltas if d < 0)} / 变好 = {sum(1 for d in deltas if d > 0)} / 不变 = "
      f"{sum(1 for d in deltas if d == 0)}")

with io.open(tsv_path, "w", encoding="utf-8") as fh:
    fh.write(f"# main_index={main_index}\n")
    fh.write(f"# ctrl_index={ctrl_index}\n")
    fh.write(f"# queries={queries_path} n_queries={main['n_queries']}\n")
    fh.write(f"# n_corpus_main={main['n_corpus']} n_corpus_ctrl={ctrl['n_corpus']}\n")
    fh.write(f"# k={k_grid} rrf_k={rrf_k} baseline_weights={base_weights} rerank=off runs=1\n")
    fh.write(f"# paired_hybrid_mean_delta={sum(deltas) / n:+.10f} median={deltas[n // 2]:+.10f}\n")
    fh.write("mode\tbucket\tmrr_main\tmrr_ctrl\tdelta\n")
    for mode, bucket, m_v, c_v, d_v in rows:
        fh.write(f"{mode}\t{bucket}\t{m_v:.10f}\t{c_v:.10f}\t{d_v:+.10f}\n")
print()
print(f"读数已写入 {tsv_path}")
PY

echo
echo "产物目录：${RUN_DIR}"
if [ -n "$OUT_DIR" ]; then
  mkdir -p "$OUT_DIR"
  cp "$TSV" "$OUT_DIR/readings.tsv"
  echo "持久产物：${OUT_DIR}/readings.tsv"
fi
echo
echo "⚠️ 定稿口径（设计 §4.7）：本对照**只作对照、不替换主基线**；读数落 \`eval-report.md\` §8.16（PR9-4）。"
echo "   解读边界：Δ 是「假负例多少」与「语料规模/统计量」的**合成效应**，不能读成纯假负例效应。"
