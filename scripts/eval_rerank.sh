#!/usr/bin/env bash
#
# V2 Step 7 · S7-03 / S7-04：精排 A/B 标定一键跑（H3 默认 R + NFR-12 + R48）
#
# 把「一次一个档位」的手工 bench 变成一键扫描，并**先自证冻结图**再谈 A/B：
#
#   S7-T11  冻结自证：同一张图连续 N 次加载 ⇒ 所有 mode 的三项指标 Δ = 0
#           （不为 0 ⇒ 快照没被真正复用 ⇒ **后面所有数据作废**，脚本直接退出）
#   A/B     A = 精排关（NoOp，效果基线的唯一参照）/ B = R = k（隔离「只看 k 条」与「看更多」）
#   扫描    R 档位（默认 10/20/50/100/200），**顺序交错**多轮（跨时段漂移不偏向某一档）
#   R48     max_length 对照档（512 vs 1024）
#   延迟    固定子集 × --reps/--warmup（精排是百毫秒级 ⇒ 不能全量 × 20 reps）
#
# 用法：
#   ./scripts/eval_rerank.sh --index /tmp/t2-frozen.idx            # 全套（需 ≈2.19GB 模型）
#   ./scripts/eval_rerank.sh --build                               # 先建冻结图再跑
#   ./scripts/eval_rerank.sh --index X --freeze-only               # 只跑 S7-T11 前置自证
#   ./scripts/eval_rerank.sh --index X --dry-run                   # 只打印将执行的命令（不跑）
#   ./scripts/eval_rerank.sh --index X --cross-graph               # 另测跨图抖动（关精排 --runs 3）
#
# 产物（默认 /tmp/helix-rerank/）：
#   freeze-<i>.json     冻结自证的第 i 次
#   A-noop.json         控制组 A（精排关）
#   r<R>-round<t>.json  各档位（交错）
#   ml<M>.json          max_length 对照
#   lat-r<R>.json       延迟轴
#   summary.md          汇总表（R × 指标 × Δ vs A）
#
# ⚠️ **全部为本地 release 实测，不进 CI**：需要 2.19GB 模型 + T2Ranking 语料，
#    且延迟数字在共享 runner 上不具可引用性（本仓库既有纪律）。
# ⚠️ 本脚本**只产数据**；结论（默认 R 取多少 / NFR-12 定稿）由 S7-05 回填。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

BIN="${BIN:-target/release/helix}"
CORPUS="${CORPUS:-data/t2-corpus.jsonl}"
QUERIES="${QUERIES:-data/t2-queries.jsonl}"
MODES="${MODES:-bm25,vector,hybrid}"
INDEX=""
K="${K:-10}"
RS="${RS:-10 20 50 100 200}"
MAXLENS="${MAXLENS:-512 1024}"
ROUNDS="${ROUNDS:-2}"
OUT_DIR="${OUT_DIR:-/tmp/helix-rerank}"
FREEZE_N="${FREEZE_N:-3}"
REPS="${REPS:-20}"
WARMUP="${WARMUP:-3}"
# max_length 对照档固定的窗口（用默认拟值 20：R48 只问「截断是否吃掉了收益」，
# 与窗口大小是**两个**独立轴 ⇒ 固定一个再扫另一个）。
ML_WINDOW="${ML_WINDOW:-20}"
BUILD=0
DRY=0
SKIP_FREEZE=0
FREEZE_ONLY=0
CROSS_GRAPH=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --index) INDEX="$2"; shift 2 ;;
        --corpus) CORPUS="$2"; shift 2 ;;
        --queries) QUERIES="$2"; shift 2 ;;
        --k) K="$2"; shift 2 ;;
        --rs) RS="$2"; shift 2 ;;
        --maxlens) MAXLENS="$2"; shift 2 ;;
        --rounds) ROUNDS="$2"; shift 2 ;;
        --out) OUT_DIR="$2"; shift 2 ;;
        --reps) REPS="$2"; shift 2 ;;
        --build) BUILD=1; shift ;;
        --dry-run) DRY=1; shift ;;
        --skip-freeze) SKIP_FREEZE=1; shift ;;
        --freeze-only) FREEZE_ONLY=1; shift ;;
        --cross-graph) CROSS_GRAPH=1; shift ;;
        *) echo "未知参数: $1" >&2; exit 2 ;;
    esac
done

# `run`：dry-run 时只打印（`%q` 逐参数转义 ⇒ 复制的命令可直接粘贴执行）。
run() {
    if [[ $DRY -eq 1 ]]; then
        printf '  +'
        printf ' %q' "$@"
        printf '\n'
        return 0
    fi
    "$@"
}

echo "==> V2 Step 7 精排 A/B 标定"
echo "    二进制 = ${BIN}（⚠️ 必须是 --features local-rerank 构建的 release）"
echo "    K = ${K}，R 档位 = ${RS}，交错轮数 = ${ROUNDS}，max_length 档 = ${MAXLENS}"
echo "    产物目录 = ${OUT_DIR}"
echo

if [[ $BUILD -eq 1 ]]; then
    echo "==> 构建 release（含 local-rerank）..."
    run cargo build --release -p helix --features local-rerank
fi

if [[ -z "$INDEX" ]]; then
    if [[ $BUILD -eq 1 ]]; then
        # 冻结图：单 chunk 口径（与 P5 可比），图**落盘即冻结**（NFR-06 口径）。
        INDEX="${OUT_DIR}/t2-frozen.idx"
        echo "==> 建冻结图 ${INDEX}（只做一次；之后所有 A/B 都吃它的图 sidecar）"
        run mkdir -p "$OUT_DIR"
        run "$BIN" build --input "$CORPUS" --vectors --single-chunk --output "$INDEX"
    else
        echo "错误：必须给 --index <冻结图>（或用 --build 先建一张）" >&2
        echo "      精排 A/B 只在**同一张图**上有意义（设计 §3.1）。" >&2
        exit 2
    fi
fi

run mkdir -p "$OUT_DIR"

# ---------------------------------------------------------------------------
# 1. S7-T11 前置自证：冻结图连续 N 次加载必须逐位一致
# ---------------------------------------------------------------------------
# 不为 0 的唯一解释是「快照没被真正复用、每轮重建了图」——hnsw_rs 用 OS 熵
# （`--runs > 1` 的重建路径就靠这个制造差异）⇒ 那种情况下 A/B 的差值里混着
# 图漂移，**任何结论都不可引用**。所以它是**门**，不是诊断。
if [[ $SKIP_FREEZE -eq 0 ]]; then
    echo "==> [S7-T11] 冻结自证：同一张图连续 ${FREEZE_N} 次（--runs 1）"
    for i in $(seq 1 "$FREEZE_N"); do
        run "$BIN" bench --index "$INDEX" --queries "$QUERIES" --k "$K" --runs 1 \
            --modes "$MODES" --no-latency --json "${OUT_DIR}/freeze-${i}.json"
    done
    if [[ $DRY -eq 0 ]]; then
        python3 - "${OUT_DIR}"/freeze-*.json <<'PY'
import json
import pathlib
import sys

paths = sys.argv[1:]
if len(paths) < 2:
    sys.exit(f"✗ 冻结自证至少需要 2 份读数，实得 {len(paths)}")

docs = [json.loads(pathlib.Path(p).read_text(encoding="utf-8")) for p in paths]

# ⚠️ 先自证「真的跑到了」：空文件之间的比较同样「一致」（本项目反复踩过）
for p, d in zip(paths, docs):
    if not d.get("modes") or not d.get("n_queries"):
        sys.exit(f"✗ {p} 里没有 modes/n_queries ⇒ 没产出数据，本步「一致」无意义")

modes = sorted(docs[0]["modes"])
HDR = "\n⚠️ 冻结不成立 ⇒ 后面所有数据作废。请确认：① 快照带图 sidecar（不是每次重建）；\
② 用了 --runs 1；③ 没有并发跑别的 bench 抢图文件。\n"
bad = []
for mode in modes:
    ref = docs[0]["modes"][mode]["thr1"]
    for i, d in enumerate(docs[1:], 2):
        cur = d["modes"][mode]["thr1"]
        diff = {k: (ref[k], cur[k]) for k in ("recall", "mrr", "ndcg") if ref[k] != cur[k]}
        if diff:
            bad.append(f"  ✗ {mode}: 第 {i} 次与第 1 次不同 {diff}")
    # per-query 逐条比对（比宏平均更严：能抓到「个别 query 变了但均值抵消」）
    ref_pq = docs[0]["modes"][mode]["per_query"]
    for i, d in enumerate(docs[1:], 2):
        cur_pq = d["modes"][mode]["per_query"]
        if ref_pq != cur_pq:
            n = sum(1 for a, b in zip(ref_pq, cur_pq) if a != b)
            bad.append(f"  ✗ {mode}: 第 {i} 次有 {n} 条 query 明细不同")

if bad:
    print("\n".join(bad))
    sys.exit("✗ S7-T11 冻结自证**失败**" + HDR)

print(f"✓ 冻结自证通过：{len(docs)} 次读数 × {len(modes)} mode 全部逐位一致"
      f"（{docs[0]['n_queries']} queries，抖动带 = 0）")
PY
    fi
    echo
fi

if [[ $FREEZE_ONLY -eq 1 ]]; then
    echo "==> --freeze-only：到此为止"
    exit 0
fi

# ---------------------------------------------------------------------------
# 2. 控制组 A（精排关 = NoOp）：效果基线的唯一参照
# ---------------------------------------------------------------------------
echo "==> 控制组 A：精排关（NoOp）"
run "$BIN" bench --index "$INDEX" --queries "$QUERIES" --k "$K" --runs 1 \
    --modes "$MODES" --no-latency --json "${OUT_DIR}/A-noop.json"

# ---------------------------------------------------------------------------
# 3. 档位扫描（交错多轮）：R = k 是控制组 B
# ---------------------------------------------------------------------------
# ⚠️ R = k（默认档 10）**不是**多余的：它把「只看 k 条」与「看更多」分开 ——
# 没有它，「精排有效」与「窗口有效」归因不了（同 Step 6 E1/E2/E3 的动机）。
echo "==> 档位扫描（${ROUNDS} 轮交错）"
# 控制组 B = R = K 必须**在档位里**：它是「只看 k 条」的那一格。缺了它，
# 「精排有效」与「窗口有效」就归因不了（同 Step 6 E1/E2/E3 的动机，设计 §4.6）。
if ! echo " ${RS} " | grep -qE " ${K} "; then
    echo "    ⚠️ 警告：R 档位里没有 K=${K}（控制组 B 缺失）⇒ 无法把「只看 k 条」与" >&2
    echo "       「看更多」分开，Δ vs A 的归因会不完整。建议把 ${K} 加进 --rs。" >&2
fi
for t in $(seq 1 "$ROUNDS"); do
    for R in $RS; do
        run "$BIN" bench --index "$INDEX" --queries "$QUERIES" --k "$K" --runs 1 \
            --modes "$MODES" --no-latency --rerank-window "$R" \
            --json "${OUT_DIR}/r${R}-round${t}.json"
    done
done

# ---------------------------------------------------------------------------
# 4. max_length 对照档（R48：截断是否吃掉了长文档的收益）
# ---------------------------------------------------------------------------
echo "==> max_length 对照（固定窗口 R = ${ML_WINDOW}，逐档扫 max_length）"
for M in $MAXLENS; do
    run "$BIN" bench --index "$INDEX" --queries "$QUERIES" --k "$K" --runs 1 \
        --modes "$MODES" --no-latency --rerank-window "$ML_WINDOW" --rerank-max-length "$M" \
        --json "${OUT_DIR}/ml${M}.json"
done

# ---------------------------------------------------------------------------
# 5. 延迟轴（固定子集 × reps）
# ---------------------------------------------------------------------------
echo "==> 延迟轴（warmup ${WARMUP} + ${REPS} reps）"
run "$BIN" bench --index "$INDEX" --queries "$QUERIES" --k "$K" --runs 1 \
    --modes "$MODES" --json "${OUT_DIR}/lat-A-noop.json"
for R in $RS; do
    run "$BIN" bench --index "$INDEX" --queries "$QUERIES" --k "$K" --runs 1 \
        --modes "$MODES" --rerank-window "$R" \
        --reps "$REPS" --warmup "$WARMUP" --json "${OUT_DIR}/lat-r${R}.json"
done

# ---------------------------------------------------------------------------
# 6. 跨图抖动（可选）：**只能在关精排下测**（精排与 --runs > 1 互斥）
# ---------------------------------------------------------------------------
# 它是 A/B 的共同噪声源，也是「精排增量是否超过噪声」判据的输入之一。
if [[ $CROSS_GRAPH -eq 1 ]]; then
    echo "==> 跨图抖动（关精排，--runs 3；⚠️ 会重建图，与 A/B 用的冻结图无关）"
    run "$BIN" bench --index "$INDEX" --queries "$QUERIES" --k "$K" --runs 3 \
        --modes "$MODES" --no-latency --json "${OUT_DIR}/cross-graph.json"
fi

# ---------------------------------------------------------------------------
# 7. 汇总表
# ---------------------------------------------------------------------------
if [[ $DRY -eq 0 ]]; then
    echo "==> 汇总 ⇒ ${OUT_DIR}/summary.md"
    python3 - "$OUT_DIR" <<'PY'
import json
import pathlib
import sys

out = pathlib.Path(sys.argv[1])


def load(name):
    p = out / name
    if not p.exists():
        return None
    return json.loads(p.read_text(encoding="utf-8"))


def trio(doc, mode):
    t = doc["modes"][mode]["thr1"]
    return t["recall"], t["mrr"], t["ndcg"]


base = load("A-noop.json")
if base is None:
    sys.exit("✗ 缺 A-noop.json（控制组 A 没跑成）")

rows = []
noop = {m: trio(base, m) for m in base["modes"]}
rows.append(("NoOp（关）", "—", noop))

# 同档位多轮 ⇒ 取「轮间极差」，它是本档位的**抖动读数**（应远小于档位间差值）
rounds = {}
for p in sorted(out.glob("r*-round*.json")):
    r = int(p.name.split("-")[0][1:])
    rounds.setdefault(r, []).append(json.loads(p.read_text(encoding="utf-8")))

for r in sorted(rounds):
    docs = rounds[r]
    acc = {m: trio(docs[0], m) for m in docs[0]["modes"]}
    spread = 0.0
    for m in docs[0]["modes"]:
        for i, t in enumerate(("recall", "mrr", "ndcg")):
            vals = [trio(d, m)[i] for d in docs]
            spread = max(spread, max(vals) - min(vals))
    # 关精排的读数随 R 应该**逐位不变** ⇒ 这才是「窗口没生效」的判据之一
    q = docs[0].get("rerank", {})
    rows.append((f"R={r}", f"轮间极差 {spread:.4f}", acc))

lines = ["# V2 Step 7 精排标定（原始读数；结论由 S7-05 回填）", ""]
lines.append("口径：同一张冻结图 + `--runs 1`；只比**序数指标**（Recall/MRR/NDCG）。")
lines.append("⚠️ **不同 R 的 `score` 不可逐位横比**（R47：`PaddingStrategy::BatchLongest`）。")
lines.append("")
modes = sorted(base["modes"])
hdr = ["档位", "轮间极差"] + [f"{m}·{k}" for m in modes for k in ("recall", "mrr", "ndcg")]
lines.append("| " + " | ".join(hdr) + " |")
lines.append("| " + " | ".join(["---"] * len(hdr)) + " |")
for label, spread, acc in rows:
    cells = [label, spread]
    for m in modes:
        t = acc.get(m)
        cells += ["-", "-", "-"] if t is None else [f"{t[0]:.4f}", f"{t[1]:.4f}", f"{t[2]:.4f}"]
    lines.append("| " + " | ".join(cells) + " |")

lines += ["", "## Δ vs 控制组 A（精排关）", ""]
lines.append("| 档位 | " + " | ".join(f"{m}·MRR Δ" for m in modes) + " |")
lines.append("| " + " | ".join(["---"] * (len(modes) + 1)) + " |")
for label, _spread, acc in rows[1:]:
    cells = [label]
    for m in modes:
        t = acc.get(m)
        cells.append("-" if t is None else f"{t[1] - noop[m][1]:+.4f}")
    lines.append("| " + " | ".join(cells) + " |")

lines += ["", "## max_length 对照（R48）", ""]
for p in sorted(out.glob("ml*.json")):
    d = json.loads(p.read_text(encoding="utf-8"))
    r = d.get("rerank", {})
    cells = [f"{p.stem}（id={r.get('id', '-')}）"]
    for m in modes:
        t = trio(d, m)
        cells.append(f"{m}: MRR {t[1]:.4f} / NDCG {t[2]:.4f}")
    lines.append("- " + " | ".join(cells))

lines += [
    "",
    "## 决策门（设计 §4.6.4，三种结论都要能出口）",
    "",
    "1. **无提升** ⇒ 如实记录 + 分析 R48 的截断影响（**不得粉饰**）；",
    "2. **有提升且拐点可识别**（Δ 首次 < 抖动带）⇒ 默认 `R = R*`；",
    "3. **有提升但拐点不可识别** ⇒ 默认 `R = 50` + 记明局限。",
]
(out / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
print(f"✓ 已写 {out / 'summary.md'}（{len(rows) - 1} 个档位）")
PY
fi

echo
echo "==> 完成。产物在 ${OUT_DIR}（summary.md 为汇总）"
echo "    ⚠️ 结论（默认 R / NFR-12）由 S7-05 回填进 docs/devel/eval-report.md §8.14"
