#!/usr/bin/env bash
#
# V2 Step 10 · PR10-1 · S10-01 / S10-T19
# T7-27「prefilter / 排序键索引 投或不投」的 spike **读数载体**
#
# 复用既有 fixture（`data/synth-<n>-filters.json` 的 `none` 与 `ts-range-degraded` 两档）
# 与既有 bench，**双规模**（1 万 / 10 万）跑，产出：
#
#   /tmp/helix-s10-<n>-<tag>.json    每档的 bench 明细
#   /tmp/helix-s10-readings.tsv      本次运行的读数（判据的直接来源）
#
# 判据（D-S10-06 换后；设计 §4.7）：
#   主  `Metrics.filter_eval` 的**规模增长率**（10 万 ÷ 1 万；解耦 ⇔ **亚线性**）
#   主  降级档 `filter_eval` 占端到端 `took`（P50）的比例
#   辅  **降级可观测面**（`Metrics.filter_degraded`；本 PR 新增，D-S10-07）
#   ❌  `vector_shortfall` **不用于本判定**（它对字段降级结构性盲，见设计 §2.4 / R60）
#
# 决策门 G1 ~ G4 = **合取**（全部满足才判「投」）；阈值在出数前**预注册**（见
# `data/eval/t7-27/README.md`），本脚本只**报数**、**不**在这里改阈值。
#
# 用法：
#   ./scripts/eval_step10_filter.sh                # 双规模（10 万级 embed 约 6~10 分钟）
#   ./scripts/eval_step10_filter.sh --skip-build   # 复用已有快照（快照在 /tmp）
#   ./scripts/eval_step10_filter.sh --n 10000      # 只跑一档规模
#   ./scripts/eval_step10_filter.sh --reps 5       # 指定重复数（默认 5）
#
# ⚠️ 延迟类读数在 CI 共享 runner 上**不具可引用性** ⇒ 本脚本是**本地实测**载体、**不进 CI**
#    （同 `eval_filter.sh` / `eval_s9.sh` 的处置）。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

REPS=5
SKIP_BUILD=0
ONLY_N=""
DEGRADED_LEVEL="ts-range-degraded"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --n) ONLY_N="$2"; shift 2 ;;
        --reps) REPS="$2"; shift 2 ;;
        --skip-build) SKIP_BUILD=1; shift ;;
        *) echo "未知参数: $1" >&2; exit 2 ;;
    esac
done

PY=${PYTHON:-python3}
BIN="target/release/helix"
TSV="/tmp/helix-s10-readings.tsv"

if [[ ! -x "$BIN" ]]; then
    echo "==> 构建 release 二进制（helix）"
    cargo build --release -p helix
fi

# 双规模；`--n` 指定时只跑那一档
if [[ -n "$ONLY_N" ]]; then
    NS=("$ONLY_N")
else
    NS=(10000 100000)
fi

mkdir -p data/eval/t7-27

# 从 fixture 读「降级档」的 spec（**不硬编码**：档位定义的真源是 filters.json）
degraded_spec() {
    "$PY" -c "
import json, sys
lv = json.load(open('data/synth-${1}-filters.json'))['levels']
hit = [x for x in lv if x['name'] == '${DEGRADED_LEVEL}']
if not hit:
    sys.exit('fixture 里没有档位 ${DEGRADED_LEVEL}')
print(hit[0]['spec'])
"
}

: > "${TSV}.tmp"
printf 'n\ttag\tmode\tmean_filter_eval_us\tshortfall_kernel\texact_ratio\tdegraded_ratio\tp50_ms\tp99_ms\tmean_hits\n' >> "${TSV}.tmp"

# 增量合并：**只丢掉本次要重跑的规模**，其它规模的历史读数保留
# （否则 `--n 10000` 单跑一次会把上一次的 10 万行抹掉）
if [[ -f "$TSV" ]]; then
    export KEEP_N
    KEEP_N="$(IFS=,; echo "${NS[*]}")"
    "$PY" - "$TSV" "${TSV}.tmp" <<'PYEOF'
import csv, os, sys
src, dst = sys.argv[1], sys.argv[2]
drop = set(os.environ["KEEP_N"].split(","))
with open(src, encoding="utf-8") as f, open(dst, "a", encoding="utf-8") as g:
    for r in csv.DictReader(f, delimiter="\t"):
        if r["n"] not in drop:
            g.write("\t".join(r[k] for k in
                              ("n", "tag", "mode", "mean_filter_eval_us", "shortfall_kernel",
                               "exact_ratio", "degraded_ratio", "p50_ms", "p99_ms",
                               "mean_hits")) + "\n")
PYEOF
fi
mv "${TSV}.tmp" "$TSV"

for n in "${NS[@]}"; do
    CORPUS="data/synth-${n}-corpus.jsonl"
    QUERIES="data/synth-${n}-queries.jsonl"
    SNAPSHOT="/tmp/helix-s10-${n}.snapshot"

    if [[ ! -f "$CORPUS" || ! -f "$QUERIES" ]]; then
        echo "错误：缺 fixture ${CORPUS} / ${QUERIES}" >&2
        exit 2
    fi

    if [[ $SKIP_BUILD -eq 0 ]]; then
        echo
        echo "==> 构建快照（含向量，${n} 篇；10 万级 embed 约 6~10 分钟）"
        "$BIN" build --input "$CORPUS" --output "$SNAPSHOT" --vectors --single-chunk
    else
        [[ -f "$SNAPSHOT" ]] || { echo "错误：--skip-build 但快照不存在 ${SNAPSHOT}" >&2; exit 2; }
        echo
        echo "==> 复用已有快照：${SNAPSHOT}"
    fi

    SPEC="$(degraded_spec "$n")"

    # ---- 档 1：无过滤（基线；不传 --filter，也不传 --filter-cost）----
    echo
    echo "---- ${n} 档位 none（无过滤）----"
    "$BIN" bench --index "$SNAPSHOT" --queries "$QUERIES" \
        --modes bm25,vector,hybrid --reps "$REPS" \
        --json "/tmp/helix-s10-${n}-none.json" 2>&1 | sed 's/^/    /'

    # ---- 档 2：降级字段 Range（第一用例）----
    echo
    echo "---- ${n} 档位 ${DEGRADED_LEVEL}（spec=${SPEC}）----"
    "$BIN" bench --index "$SNAPSHOT" --queries "$QUERIES" \
        --modes bm25,vector,hybrid --reps "$REPS" \
        --filter "$SPEC" --filter-cost \
        --json "/tmp/helix-s10-${n}-degraded.json" 2>&1 | sed 's/^/    /'

    "$PY" - "$n" "$TSV" <<'PYEOF'
import json, sys
n, tsv = sys.argv[1], sys.argv[2]
rows = []
for tag in ("none", "degraded"):
    d = json.load(open(f"/tmp/helix-s10-{n}-{tag}.json"))
    L = d["latency"]["hybrid"]
    rows.append((n, tag, "hybrid",
                 f"{L['mean_filter_eval_us']:.2f}",
                 f"{L['vector_shortfall_kernel']:.2f}",
                 f"{L['vector_route_exact_ratio']:.3f}",
                 f"{L['filter_degraded_ratio']:.3f}",
                 f"{L['p50_ms']:.4f}",
                 f"{L['p99_ms']:.4f}",
                 f"{L['mean_hits']:.2f}"))
with open(tsv, "a", encoding="utf-8") as f:
    for r in rows:
        f.write("\t".join(str(x) for x in r) + "\n")
print(f"    已写入 {len(rows)} 行 -> {tsv}")
PYEOF
done

echo
echo "==> 读数（本次运行）："
column -t -s $'\t' "$TSV" 2>/dev/null || cat "$TSV"
echo
echo "==> 判据与决策门 G1 ~ G4 的对照（口径见 data/eval/t7-27/README.md）"
# shellcheck disable=SC2016  # python 里的 $ 不需要 shell 展开（单引号 heredoc）
"$PY" - <<'PYEOF'
import csv, os

rows = {}
with open("/tmp/helix-s10-readings.tsv", encoding="utf-8") as f:
    for r in csv.DictReader(f, delimiter="\t"):
        rows[(r["n"], r["tag"])] = r


def get(n, tag, k):
    return float(rows[(str(n), tag)][k])


have = lambda n: (str(n), "degraded") in rows and (str(n), "none") in rows

if have(10000) and have(100000):
    g1 = get(10000, "degraded", "mean_filter_eval_us") >= 1000.0
    g1b = get(100000, "degraded", "mean_filter_eval_us") >= 5000.0
    ratio = get(100000, "degraded", "mean_filter_eval_us") / get(
        10000, "degraded", "mean_filter_eval_us"
    )
    # 🔑 G2 的**前提**：降级档必须「全部走全扫」——否则 filter_eval 里混着
    #    「索引路径」与「全扫路径」两种成本，跨规模比值不可解释（D-S10-07 的用途）
    pre = {n: get(n, "degraded", "degraded_ratio") for n in (10000, 100000)}
    pre_ok = all(v == 1.0 for v in pre.values())
    print(f"  前提（G2 可解释性）：降级档 degraded_ratio = "
          f"1 万 {pre[10000]:.3f} / 10 万 {pre[100000]:.3f}（须 = 1.0 {'✅' if pre_ok else '❌'}）")
    g2 = pre_ok and ratio >= 3.0
    # 占端到端 P50 的比例（两档各算一次）
    frac = {
        n: get(n, "degraded", "mean_filter_eval_us") / 1000.0
        / get(n, "degraded", "p50_ms")
        for n in (10000, 100000)
    }
    g3 = all(v >= 0.10 for v in frac.values())
    g4r = {
        n: get(n, "degraded", "mean_filter_eval_us")
        / get(n, "none", "mean_filter_eval_us")
        for n in (10000, 100000)
    }
    g4 = all(v >= 100.0 for v in g4r.values())
    print(f"  G1 降级档 filter_eval 非零：1 万 {get(10000,'degraded','mean_filter_eval_us'):.2f} µs "
          f"(>=1000 {'✅' if g1 else '❌'})；10 万 {get(100000,'degraded','mean_filter_eval_us'):.2f} µs "
          f"(>=5000 {'✅' if g1b else '❌'})")
    print(f"  G2 跨规模增长率：{ratio:.2f}× (>=3× {'✅' if g2 else '❌'})")
    print(f"  G3 占端到端 P50：1 万 {frac[10000]*100:.1f}%；10 万 {frac[100000]*100:.1f}% "
          f"(>=10% {'✅' if g3 else '❌'})")
    print(f"  G4 降级/无过滤：1 万 {g4r[10000]:.1f}×；10 万 {g4r[100000]:.1f}× (>=100× {'✅' if g4 else '❌'})")
    allk = g1 and g1b and g2 and g3 and g4
    print(f"  => 合取（全部门满足才判「投」）：{'✅ 投' if allk else '❌ 不投'}")
else:
    print("  （只跑了一档规模 ⇒ 跨规模门 G2 无法判定；请两档都跑）")
PYEOF
