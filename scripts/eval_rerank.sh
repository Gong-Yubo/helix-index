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
#   延迟    **固定子集**（`LAT_QUERIES`，默认 50 条）× `--reps/--warmup`
#           ⚠️ **必须用子集**：bench 的延迟阶段**没有**子集机制（对全量 judgments 遍历
#           warmup+reps 次），而 R=200 档每个 pass 会把 200 条交给精排。按实参复算：
#           320 queries × 23 passes × 200 docs ≈ 1.47M 次文档打分（fastembed 批 256 已计入）
#           ⇒ **全量跑一档是 2~25 小时**（依每文档有效耗时）。子集把它拉回可跑的量级。
#
# 用法：
#   ./scripts/eval_rerank.sh --index /tmp/t2-frozen.idx            # 全套（需 ≈2.19GB 模型）
#   ./scripts/eval_rerank.sh --build                               # 先建冻结图再跑
#   ./scripts/eval_rerank.sh --index X --freeze-only               # 只跑 S7-T11 前置自证
#   ./scripts/eval_rerank.sh --index X --dry-run                   # 只打印将执行的命令（不跑）
#   ./scripts/eval_rerank.sh --index X --cross-graph               # 另测跨图抖动（关精排 --runs 3）
#   ./scripts/eval_rerank.sh --index X --skip-freeze               # 跳过 S7-T11（已自证过时）
#   ./scripts/eval_rerank.sh --index X --lat-queries 30            # 延迟轴子集大小（默认 50；0 = 全量）
#   ./scripts/eval_rerank.sh --index X --query-sample 60           # **分层抽样**评测子集（确定性，见下）
#   ./scripts/eval_rerank.sh --index X --modes hybrid              # 只跑单路（省 2/3；单路对照用）
#   ./scripts/eval_rerank.sh --index X --skip-quality-axis         # 只跑延迟轴（分段续跑用）
#   ./scripts/eval_rerank.sh --index X --no-latency-axis           # 只跑质量轴（分段续跑用）
#
# ⚠️ **协议缩减（S7-04 实测后）**：设计 §4.6.1 的「全量 320 query × 5 档 × 2 轮 + 延迟轴
#   50 query×reps 20」在本机复算 ≈260 小时（实测每文档 ≈0.45s、与 R 近似线性）⇒ 不可行。
#   实际采用的缩减：`--query-sample 60`（**分层**：每个 type 均匀取，交错排列，
#   保证子集内的 type 分布与全量同构）+ `R ∈ {10,20,50}` + `--rounds 1` +
#   延迟轴 `--lat-queries 10`。
#   🔑 **缩减不破坏 Δ 的可比性**：控制组 A 与所有档位都用**同一子集**、同一 `--runs 1`
#   ⇒ 子集内的 Δ 仍是同口径；但**与 P5 的历史全量数字不可直接横比**（口径不同，见报告）。
#
# 产物（默认 /tmp/helix-rerank/）：
#   freeze-<i>.json     冻结自证的第 i 次
#   A-noop.json         控制组 A（精排关）
#   r<R>-round<t>.json  各档位（交错）
#   ml<M>.json          max_length 对照
#   lat-r<R>.json       延迟轴
#   summary.md          汇总表（R × 指标 × Δ vs A）
#   latency.md          延迟轴汇总（端到端 took 与 rerank_elapsed **分列**；NFR-12 判据）
#   queries-sample.jsonl 分层抽样的实得子集（`--query-sample N`；确定性 ⇒ 可复现）
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
# 延迟轴子集大小（**确定性**：取前 N 条 ⇒ 可复现）。0 = 全量（⚠️ 精排档可能小时级/档位）。
LAT_QUERIES="${LAT_QUERIES:-50}"
# **分层抽样**的评测子集大小（0 = 不抽样，用全量）。抽样是**确定性**的：
# 每个 `type` 均匀取 N/n_types 条，再按 type **交错排列** ⇒ ① 可复现；② `head -N`
# 取到的延迟轴子集也覆盖各 type（否则延迟轴会全落在同一个类型上）。
QUERY_SAMPLE="${QUERY_SAMPLE:-0}"
BUILD=0
DRY=0
SKIP_QUALITY=0
NO_LAT_AXIS=0
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
        --lat-queries) LAT_QUERIES="$2"; shift 2 ;;
        --modes) MODES="$2"; shift 2 ;;
        --query-sample) QUERY_SAMPLE="$2"; shift 2 ;;
        --skip-quality-axis) SKIP_QUALITY=1; shift ;;
        --no-latency-axis) NO_LAT_AXIS=1; shift ;;
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

# ⚠️ **就地校验** `FREEZE_N`：冻结自证要比较**至少两次**读数，而那个判据在 Python 里 ——
# 不在这里拦的话，`FREEZE_N=1` 会先跑完一次 bench（含模型加载）才失败，白等。（评审 P4-1）
if [[ $SKIP_FREEZE -eq 0 && $FREEZE_N -lt 2 ]]; then
    echo "错误：FREEZE_N=${FREEZE_N} 至少为 2（冻结自证要比较至少两份读数）" >&2
    exit 2
fi

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
# 0. 分层抽样（可选）：把评测集缩到**本机跑得完**的规模，同时保持 type 分布同构
# ---------------------------------------------------------------------------
# ⚠️ 抽样必须**确定性**（否则报告的数字不可复现，T5-08 数据诚信）：每个 `type` 均匀取
# `N/n_types` 条（按原顺序等步长），再按 type **交错排列** ⇒ ① 同一命令必得同一子集；
# ② `head -N` 取到的延迟轴子集也覆盖各 type。
if [[ "$QUERY_SAMPLE" != "0" ]]; then
    QS="${OUT_DIR}/queries-sample.jsonl"
    echo "==> 分层抽样 ${QUERY_SAMPLE} 条 ⇒ ${QS}"
    if [[ $DRY -eq 1 ]]; then
        printf '  + [分层抽样 %s 条 ⇒ %q]\n' "$QUERY_SAMPLE" "$QS"
    else
        python3 - "$QUERIES" "$QS" "$QUERY_SAMPLE" <<'PYEOF'
import collections
import json
import pathlib
import sys

src_p, out_p, n_total = sys.argv[1], sys.argv[2], int(sys.argv[3])
qs_all = [
    json.loads(l)
    for l in pathlib.Path(src_p).read_text(encoding="utf-8").splitlines()
    if l.strip()
]
by_type = collections.OrderedDict()
for q in qs_all:
    by_type.setdefault(q.get("type", "?"), []).append(q)
n_types = len(by_type)
per = max(1, n_total // n_types)
lanes = []
for _t, qs in by_type.items():
    step = len(qs) / per
    lanes.append([qs[int(i * step)] for i in range(per)])
# 交错：第 i 轮取各 type 的第 i 条 ⇒ head 子集覆盖各 type
out = [lanes[i % n_types][i // n_types] for i in range(per * n_types)]
pathlib.Path(out_p).write_text(
    "\n".join(json.dumps(q, ensure_ascii=False) for q in out) + "\n", encoding="utf-8"
)
print(
    f"✓ 分层抽样：{len(qs_all)} → {len(out)} 条"
    f"（{n_types} 个 type 各 {per} 条，交错排列）"
)
if len(out) != n_total:
    print(f"  ⚠️ 请求 {n_total} 条、实得 {len(out)} 条（{n_total} 不能被 {n_types} 整除）")
PYEOF
    fi
    QUERIES="$QS"
fi

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
if [[ $SKIP_QUALITY -eq 1 ]]; then
    echo "==> --skip-quality-axis：跳过控制组 A / 档位扫描 / max_length 对照"
fi

if [[ $SKIP_QUALITY -eq 0 ]]; then
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

fi  # ← SKIP_QUALITY（质量轴段）

# ---------------------------------------------------------------------------
# 5. 延迟轴（**固定子集** × reps）
# ---------------------------------------------------------------------------
# ⚠️ 这里**必须**落地子集：见文件头的复算（全量 × R=200 是小时级/档位）。
# 子集取 `$QUERIES` 的**前 N 行**（确定性 ⇒ 可复现）；⚠️ 前 N 条的类型分布可能有偏，
# 但延迟轴只看耗时分布的量级、不做类型细分 ⇒ 可接受。
# ⚠️ **控制组 A 与各精排档必须用同一子集、同一 reps/warmup** —— 否则「精排 vs NoOp」的
# Δ 是静默错口径（NFR-07 要防的「看错一行、结论作废」的近亲，而延迟 Δ 正是 NFR-12 的输入）。
LAT_Q="$QUERIES"
if [[ "$LAT_QUERIES" == "0" ]]; then
    echo "    ⚠️ LAT_QUERIES=0 ⇒ 延迟轴跑**全量** queries；精排档可能是小时级/档位（见文件头复算）" >&2
else
    LAT_Q="${OUT_DIR}/lat-queries.jsonl"
    if [[ $DRY -eq 1 ]]; then
        printf '  + head -n %q %q > %q\n' "$LAT_QUERIES" "$QUERIES" "$LAT_Q"
    else
        head -n "$LAT_QUERIES" "$QUERIES" > "$LAT_Q"
    fi
fi
if [[ $NO_LAT_AXIS -eq 1 ]]; then
    echo "==> --no-latency-axis：跳过延迟轴（质量轴与它可分段续跑）"
fi

if [[ $NO_LAT_AXIS -eq 0 ]]; then
echo "==> 延迟轴（子集 = ${LAT_QUERIES} 条 / 0 表示全量；warmup ${WARMUP} + ${REPS} reps）"
run "$BIN" bench --index "$INDEX" --queries "$LAT_Q" --k "$K" --runs 1 \
    --modes "$MODES" --reps "$REPS" --warmup "$WARMUP" --json "${OUT_DIR}/lat-A-noop.json"
for R in $RS; do
    run "$BIN" bench --index "$INDEX" --queries "$LAT_Q" --k "$K" --runs 1 \
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

fi  # ← NO_LAT_AXIS（延迟轴段）

# ---------------------------------------------------------------------------
# 7. 汇总表（质量轴；⚠️ 只在跑了质量轴时输出 —— 否则会因缺 A-noop.json 而中止）
# ---------------------------------------------------------------------------
if [[ $SKIP_QUALITY -eq 0 && $DRY -eq 0 ]]; then
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
    # 「轮间极差」的前提是**各轮真的跑了同一档**：若 OUT_DIR 里残留上一轮的产物、文件串档，
    # 两个**不同档位**之间的差会被当成「抖动」，直接污染 §4.6.4 的「Δ 首次 < 抖动带」判据。
    # 这里用 bench 的 `rerank` 块（本 PR 在 bench 侧加的自证出口，id 含窗口与 max_length）
    # 做**串档检测** —— 比逐位比对读数便宜，且抓的是另一种错。
    sigs = {json.dumps(d.get("rerank", {}), sort_keys=True) for d in docs}
    if not docs[0].get("rerank", {}).get("enabled") or len(sigs) != 1:
        print(
            f"    ⚠️ R={r}: 各轮 rerank 块不一致或未开启 ⇒ 疑串档/未生效：{sorted(sigs)}",
            file=sys.stderr,
        )
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

# ---------------------------------------------------------------------------
# 8. 延迟轴汇总（NFR-12 的判据 —— **必须与质量轴分开看**）
# ---------------------------------------------------------------------------
# ⚠️ 与第 7 段分成两个产物是刻意的：两者可以**分段续跑**（`--skip-quality-axis` /
# `--no-latency-axis`），且「效果提升」与「延迟代价」是两个独立结论（设计 §4.6.3）。
if [[ $NO_LAT_AXIS -eq 0 && $DRY -eq 0 ]]; then
    echo "==> 延迟轴汇总 ⇒ ${OUT_DIR}/latency.md"
    python3 - "$OUT_DIR" <<'PYEOF'
import json
import pathlib
import sys

out = pathlib.Path(sys.argv[1])
docs = []
p0 = out / "lat-A-noop.json"
if p0.exists():
    docs.append(("NoOp（关）", json.loads(p0.read_text(encoding="utf-8"))))
# 文件名 `lat-r<R>.json`；按 R 数值排序（不是字典序，否则 r100 < r20）
for f in sorted(
    out.glob("lat-r*.json"), key=lambda x: int(x.stem[5:]) if x.stem[5:].isdigit() else 0
):
    docs.append((f"R={f.stem[5:]}", json.loads(f.read_text(encoding="utf-8"))))
if not docs:
    sys.exit("✗ 没有任何 lat-*.json ⇒ 延迟轴没跑成")


def fmt(x, nd=1):
    return "-" if x is None else f"{x:.{nd}f}"


lines = [
    "# V2 Step 7 延迟轴（NFR-12 的原始读数；结论由 S7-05 回填）",
    "",
    "⚠️ **分列是刻意的**（设计 §4.6.3）：端到端 `took` 含召回 / 融合 / 回捞 / 组装，",
    "**只有 `rerank_elapsed` 才是 NFR-12 的口径** —— 只看前者无法把精排成本与外层成本分开。",
    "",
    "| 档位 | mode | 端到端 P50 | 端到端 P99 | 精排 P50 | 精排 P99 | 精排样本 n | 平均交接条数 |",
    "| --- | --- | --- | --- | --- | --- | --- | --- |",
]
for label, d in docs:
    for mode in sorted(d.get("latency", {})):
        v = d["latency"][mode]
        lines.append(
            "| "
            + " | ".join(
                [
                    label,
                    mode,
                    fmt(v.get("p50_ms")),
                    fmt(v.get("p99_ms")),
                    fmt(v.get("rerank_p50_ms")),
                    fmt(v.get("rerank_p99_ms")),
                    str(v.get("rerank_n", 0)),
                    fmt(v.get("mean_rerank_window")),
                ]
            )
            + " |"
        )
lines += [
    "",
    "⚠️ **`精排样本 n` 是 `rerank_elapsed` 分位数的分母**（早退 / 未开精排的响应不计入）",
    "⇒ 它与 `n_samples` 不等是**正常**的；为 0 说明该档位根本没跑精排（如控制组 A）。",
    "⚠️ **样本 < 100 时 P99 只是装饰**（perf-ab-calibration 规则 3）⇒ 提升 `--reps` 才有判别力。",
    "⚠️ **跨档位不可横比 `score`**（R47），但**延迟可以横比**（同一台机、同一张冻结图）。",
]
(out / "latency.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
print(f"✓ 已写 {out / 'latency.md'}（{len(docs)} 个档位）")
PYEOF
fi

echo
echo "==> 完成。产物在 ${OUT_DIR}（summary.md / latency.md 为汇总）"
echo "    ⚠️ 结论（默认 R / NFR-12）由 S7-05 回填进 docs/devel/eval-report.md §8.14"
