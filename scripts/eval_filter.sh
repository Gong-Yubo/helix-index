#!/usr/bin/env bash
#
# V2 Step 1 · S1-10：过滤选择度扫描 + NFR-02 重测（T12 / T13 / T14）
# V2 Step 5 · S5-04：精确兜底阈值标定（S5-T10）——新增路径列与 --brute-fallback 旋钮
#
# 把「一次一个档位」的手工 bench 变成一键扫描，产出四元数据：
#
#   T12  选择度 × 延迟 × 召回（每个档位跑 bench，读 --json 汇总）
#   T13  过滤求值耗时对照（--filter-cost，旧全扫 vs 新位图谓词）
#   T14  NFR-02 重测：无过滤档位 P99 ≤ 基线 × 1.10，且 Top-10 重合率 ≥ 0.99
#   S5-04 标定：allowed × 路径（精确占比）× P99 × 召回 —— 填 D-S5-02 阈值与 NFR-13 预算
#
# 用法：
#   ./scripts/eval_filter.sh                       # 1 万级（约 2 分钟，本地冒烟）
#   ./scripts/eval_filter.sh --n 100000            # 10 万级（约 20~30 分钟）
#   ./scripts/eval_filter.sh --n 100000 --skip-build   # 已有快照时跳过构建
#   ./scripts/eval_filter.sh --levels none,sel-1%,sel-0.1%   # 只跑指定档位
#   ./scripts/eval_filter.sh --brute-fallback off  # A/B 对照：关闭兜底（回到 Step 5 之前）
#   ./scripts/eval_filter.sh --brute-fallback 256  # 覆盖阈值，扫不同分界
#
# 产物（/tmp）：
#   helix-filter-<n>-<档位>.json  每档位的 bench 明细
#   helix-filter-<n>.md           汇总 markdown 表
#
# ⚠️ 延迟数字在 CI 共享 runner 上不具可引用性，本脚本用于**本地**实测。
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

N=10000
QUERIES=200
REPS=20
MODES="bm25,vector,hybrid"
SKIP_BUILD=0
LEVELS=""
REGEN=0
BRUTE_FALLBACK=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --n) N="$2"; shift 2 ;;
        --queries) QUERIES="$2"; shift 2 ;;
        --reps) REPS="$2"; shift 2 ;;
        --modes) MODES="$2"; shift 2 ;;
        --levels) LEVELS="$2"; shift 2 ;;
        --brute-fallback) BRUTE_FALLBACK="$2"; shift 2 ;;
        --skip-build) SKIP_BUILD=1; shift ;;
        --regen) REGEN=1; shift ;;
        *) echo "未知参数: $1" >&2; exit 2 ;;
    esac
done

# 10 万级自动收敛规模：否则单档位就要 5 分钟以上，8 档位跑不完
if [[ $N -ge 50000 && $QUERIES -eq 200 ]]; then QUERIES=100; fi
if [[ $N -ge 50000 && $REPS -eq 20 ]]; then REPS=10; fi

CORPUS="data/synth-${N}-corpus.jsonl"
QUERY_FILE="data/synth-${N}-queries.jsonl"
LEVELS_JSON="data/synth-${N}-filters.json"
SNAPSHOT="/tmp/helix-filter-${N}.snapshot"

# 兜底开关标签：A/B 对照时产物分开落盘，避免互相覆盖（同一快照可复用）
# 默认阈值**从源码读**（S5-04 定稿后仍是同一处真源），避免脚本文案与常量漂移
DEFAULT_FB="$(sed -n 's/^pub const BRUTE_FALLBACK_MAX_ALLOWED: usize = \([0-9]*\);.*/\1/p' \
    crates/core/src/vector/hnsw_rs_index.rs | head -1)"
: "${DEFAULT_FB:?无法从 crates/core/src/vector/hnsw_rs_index.rs 读出 BRUTE_FALLBACK_MAX_ALLOWED}"
if [[ -n "$BRUTE_FALLBACK" ]]; then
    FB_TAG="-fb${BRUTE_FALLBACK}"
    FB_DESC="覆盖阈值 ${BRUTE_FALLBACK}"
    [[ "$BRUTE_FALLBACK" == "off" ]] && FB_DESC="关闭兜底（Step 5 之前的 ANN 行为）"
else
    FB_TAG=""
    FB_DESC="默认（${DEFAULT_FB}，S5-04 标定定稿）"
fi
OUT_MD="/tmp/helix-filter-${N}${FB_TAG}.md"
PY=${PYTHON:-python3}

# NFR-02 基线（P5 实测，eval-report.md 8.1）：无过滤档位 P99 不得超过其 1.10 倍
BASELINE_BM25=3.90
BASELINE_VECTOR=8.37
BASELINE_HYBRID=8.70

echo "==> 构建 release 二进制（helix）"
echo "    精确兜底：${FB_DESC}"
cargo build --release -p helix
BIN="target/release/helix"

# ---------------------------------------------------------------- fixture
if [[ $REGEN -eq 1 || ! -f "$CORPUS" || ! -f "$QUERY_FILE" || ! -f "$LEVELS_JSON" ]]; then
    echo
    echo "==> 生成合成 fixture（${N} 篇 × ${QUERIES} query，确定性）"
    "$PY" scripts/gen_synth_corpus.py --n "$N" --queries "$QUERIES"
else
    echo
    echo "==> 复用已有 fixture：${CORPUS}（加 --regen 强制重生成）"
fi

# ---------------------------------------------------------------- 快照
if [[ $SKIP_BUILD -eq 0 ]]; then
    echo
    echo "==> 构建快照（含向量；10 万级 embed 约 5~10 分钟）"
    "$BIN" build --input "$CORPUS" --output "$SNAPSHOT" --vectors --single-chunk
else
    [[ -f "$SNAPSHOT" ]] || { echo "错误：--skip-build 但快照不存在 $SNAPSHOT" >&2; exit 2; }
    echo "==> 复用已有快照：$SNAPSHOT"
fi

# ---------------------------------------------------------------- 档位扫描
echo
echo "==> 扫描过滤档位（modes=${MODES}，reps=${REPS}）"

# 从 filters.json 读档位；--levels 传了就只跑指定档位（逗号分隔的 name）
# ⚠️ 分隔符用 `|` 不能用 tab：tab 属 IFS 空白字符，空字段（无过滤档位的 spec
# 为空）会被 read 压缩掉，导致 name/spec/sel 三列错位
read_levels() {
    "$PY" -c "
import json
want = [s for s in '${LEVELS}'.split(',') if s]
lv = json.load(open('${LEVELS_JSON}'))['levels']
for x in lv:
    if not want or x['name'] in want:
        print(x['name'] + '|' + x['spec'] + '|' + str(x['selectivity']))
"
}

declare -a ROWS=()
while IFS='|' read -r name spec sel; do
    out_json="/tmp/helix-filter-${N}-${name}${FB_TAG}.json"
    echo
    echo "---- 档位 ${name}（spec=${spec:-<无过滤>}，声明选择度 ${sel}，兜底 ${FB_DESC}）----"
    args=(--index "$SNAPSHOT" --queries "$QUERY_FILE" --modes "$MODES"
          --reps "$REPS" --json "$out_json")
    if [[ -n "$spec" ]]; then
        args+=(--filter "$spec" --filter-cost)
    fi
    if [[ -n "$BRUTE_FALLBACK" ]]; then
        args+=(--brute-fallback "$BRUTE_FALLBACK")
    fi
    "$BIN" bench "${args[@]}" 2>&1 | sed 's/^/    /'
    ROWS+=("${name}|${sel}|${out_json}")
done < <(read_levels)

# ---------------------------------------------------------------- 汇总
echo
echo "==> 汇总三元数据"
# 用环境变量把档位清单传给下面的 python（heredoc 加引号 ⇒ shell 不展开，
# 否则 python 代码里的 ${...} 会被当成 shell 变量）
export ROWS_JOINED="$(printf '%s\n' "${ROWS[@]}")"
"$PY" - "${OUT_MD}" "${N}" "${BASELINE_BM25}" "${BASELINE_VECTOR}" "${BASELINE_HYBRID}" "${FB_DESC}" <<'PYEOF'
import json, os, sys

out_md, n = sys.argv[1], sys.argv[2]
baseline = {"bm25": float(sys.argv[3]), "vector": float(sys.argv[4]), "hybrid": float(sys.argv[5])}
fb_desc = sys.argv[6]
rows = [l.split("|") for l in os.environ["ROWS_JOINED"].splitlines() if l.strip()]

lines = []


def emit(s=""):
    print(s)
    lines.append(s)


def exact_cell(mode, L):
    """bm25 模式不走向量路，'精确占比' 在该列没有意义 ⇒ 打 '-' 而不是 0%"""
    if mode == "bm25":
        return "-"
    return f"{L.get('vector_route_exact_ratio', 0) * 100:>5.0f}%"


emit()
emit("=" * 114)
emit(f"V2 Step 5 · S5-04 精确兜底标定 + S1-10 选择度扫描（{n} 篇合成语料，release）")
emit(f"精确兜底：{fb_desc}")
emit("=" * 114)
emit(f"{'档位':<18} {'选择度':>9} {'mode':<7} {'P50(ms)':>9} {'P99(ms)':>9} {'条数':>6} "
     f"{'缺口':>6} {'内核缺口':>9} {'精确':>6} {'重合率':>8} {'判定':>10}")
emit("-" * 114)

verdicts = []
for name, sel, path in rows:
    try:
        d = json.load(open(path))
    except Exception as e:
        emit(f"{name:<18} {sel:>9}  <读取失败: {e}>")
        continue
    lat = d.get("latency", {})
    first = True
    for mode in ["bm25", "vector", "hybrid"]:
        if mode not in lat:
            continue
        L = lat[mode]
        p50, p99 = L.get("p50_ms", 0), L.get("p99_ms", 0)
        hits, short = L.get("mean_hits", 0), L.get("mean_shortfall", 0)
        short_k = L.get("vector_shortfall_kernel", 0)
        ratio = L.get("vector_route_exact_ratio", 0)
        # 重合率来自 modes.<mode>.filter_quality（无过滤档位没有该字段）
        fq = d.get("modes", {}).get(mode, {}).get("filter_quality")
        if fq:
            ov = f"{fq.get('recall_vs_oracle', 0):.4f}"
            if fq.get("oracle_mean_hits", 0) < 0.9 * 10:
                ov += "*"  # 基线<K，鉴别力有限
        else:
            ov = "-"
        note = ""
        if name == "none":
            tgt = baseline[mode] * 1.10
            ok = "达标" if p99 <= tgt else "超基线×1.10"
            note = f"基线 {baseline[mode]:.2f}→限 {tgt:.2f}"
            verdicts.append((name, mode, p99, tgt, ok))
        else:
            # Step 5：判据从「缺口」升级为「路径 + 缺口」——
            # 精确路径下缺口通常为 0，但**不是恒等式**（allowed 来自 Index、扫描枚举的是
            # 图中的点）：只看缺口会把「没兜底」与「兜底了」判成一样；反过来
            # 「精确但缺口>0」也不是矛盾，而是「图未覆盖全部 allowed」的诊断信号。
            if ratio > 0.5:
                ok = "精确兜底" if short_k < 0.5 else "精确但缺口>0"
            else:
                ok = "" if short_k < 0.5 else "缺口>0"
        emit(f"{name if first else '':<18} {sel if first else '':>9} {mode:<7} "
             f"{p50:>9.2f} {p99:>9.2f} {hits:>6.2f} {short:>6.2f} {short_k:>9.2f} "
             f"{exact_cell(mode, L):>6} {ov:>8} {ok:>10}" + (f"  {note}" if note else ""))
        first = False
emit("-" * 114)
emit("「缺口」= 用户视角 min(K, allowed) − 返回条数；「内核缺口」= metrics.vector_shortfall（融合前候选池）。")
emit("「精确」= metrics.vector_route==Exact 的响应占比；>50% 表示该档位确实走了精确兜底。")
emit("⚠️ 精确路径下「内核缺口」**通常**为 0，但不是恒等式：allowed 来自 Index、扫描枚举的是图中的点，")
emit("   图滞后于索引时它仍 > 0(那时它是「图未覆盖全部 allowed」的诊断信号)。两种读数都要连看「精确」。")
emit("重合率后带 * 表示 oracle 基线自身 < K，此时只能证明「下推没比 post-filter 更差」。")

emit()
emit("== 路径与耗时分解（内核 metrics 均值，D-S5-06）==")
emit(f"{'档位':<18} {'mode':<7} {'精确':>6} {'bm25(ms)':>9} {'vector(ms)':>11} "
     f"{'过滤求值(ms)':>12} {'n_metrics':>10}")
emit("-" * 114)
for name, _sel, path in rows:
    try:
        d = json.load(open(path))
    except Exception:
        continue
    first = True
    for mode in ["bm25", "vector", "hybrid"]:
        L = d.get("latency", {}).get(mode)
        if not L:
            continue
        emit(f"{name if first else '':<18} {mode:<7} {exact_cell(mode, L):>6} "
             f"{L.get('mean_bm25_ms', 0):>9.3f} {L.get('mean_vector_ms', 0):>11.3f} "
             f"{L.get('mean_filter_eval_us', 0) / 1000.0:>12.3f} {L.get('n_metrics', 0):>10}")
        first = False
emit("-" * 114)
emit("「过滤求值」独立于召回路径（D-S5-09）：降级字段档位上它自己就可能 ~8ms。兜底做完仍超")
emit("NFR-13 预算时，靠它区分「兜底没生效」与「过滤求值本身贵」——后者不归本 Step 管。")

emit()
emit("== NFR-02 重测（T14①：无过滤档位 P99 ≤ 基线 × 1.10）==")
for name, mode, p99, tgt, ok in verdicts:
    mark = "✅" if ok == "达标" else "❌"
    emit(f"  {mark} {mode:<7} P99={p99:6.2f}ms  限 {tgt:6.2f}ms  ({ok})")

with open(out_md, "w", encoding="utf-8") as f:
    f.write(f"# V2 Step 5 · S5-04 精确兜底标定（{n} 篇合成语料，release）\n\n```\n")
    f.write("\n".join(lines))
    f.write("\n```\n")
emit(f"\n汇总已写出：{out_md}")
PYEOF

echo
echo "每档位明细 JSON: /tmp/helix-filter-${N}-<档位>${FB_TAG}.json"
