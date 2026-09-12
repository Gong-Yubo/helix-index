#!/usr/bin/env bash
#
# 增量构建（NFR-03 ②）一键复现实测（V2 Step 6 · S6-07 / T7-11 / FR-28）
#
# 目的：把「**增量追加** 10% 文档 < 首次全量 × 10% × 1.2」（D-J8 拆出的双口径之②）
# 在可复现的口径下实测出来，并把「增量省在哪」分解成 load / embed / save 三段。
#
# 口径（与 eval-report §8.2 的 NFR-03 ① 对齐，便于横比）：
#     helix build --single-chunk --vectors      ⇒ 1 篇 == 1 chunk
#
# 三个计时（**同一次运行内**取，避免跨时段数字不可相减）：
#     T_full : 全量建库 100% 篇（含 embed / 落盘）—— 判据的分母
#     T_base : 建 base 快照（(100-RATIO)% 篇）—— 追加的前置，**不计入判据**
#     T_inc  : 追加 RATIO% 篇（load 既有快照 → add → commit → save）—— 判据的分子
#     判据   : T_inc < T_full × RATIO% × 1.2
#
# 用法：
#     ./scripts/eval_incremental.sh                 # 默认 12,000 篇 / 追加 10%
#     SIZE=2000 ./scripts/eval_incremental.sh       # 缩减档（冒烟，约 1 分钟）
#     RATIO=20 ./scripts/eval_incremental.sh        # 换追加比例
#
# 产物：${WORK}（默认 /tmp/helix-incr）/ 下 base|full|incr 三套快照 + 各步日志
#       + result.json（机器可读）；判定表打到 stdout，数字人工贴进 eval-report。
#
# ⚠️ 分钟级（12K 档约 8 分钟）、**不进 CI**（项目纪律：性能数字一律本地 release 跑）。
# ⚠️ 跑之前**别并行**跑 cargo / 其他守门 —— 会污染计时（perf-ab-calibration 既有教训）。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

SIZE="${SIZE:-12000}"
RATIO="${RATIO:-10}"
CORPUS="${CORPUS:-data/t2-corpus.jsonl}"
BIN="${BIN:-target/release/helix}"
WORK="${WORK:-/tmp/helix-incr}"

if [[ ! -f "${CORPUS}" ]]; then
    echo "错误：找不到语料 ${CORPUS}（可用 CORPUS=<path> 覆盖）" >&2
    exit 2
fi
if [[ ! -x "${BIN}" ]]; then
    echo "错误：找不到可执行文件 ${BIN}，先跑 cargo build --release --workspace" >&2
    exit 2
fi

mkdir -p "${WORK}"

# ---------------------------------------------------------------- 语料切分
# base = 前 (100-RATIO)%，delta = 尾部 RATIO%。切分**确定性**（按行序，不用随机）。
python3 - "${CORPUS}" "${SIZE}" "${RATIO}" "${WORK}" <<'PY'
import pathlib
import sys

corpus, size, ratio, work = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), pathlib.Path(sys.argv[4])
lines = [l for l in pathlib.Path(corpus).read_text(encoding="utf-8").splitlines() if l.strip()]
total = min(size, len(lines))
lines = lines[:total]
n_delta = max(1, round(total * ratio / 100))
base, delta = lines[: total - n_delta], lines[total - n_delta:]

(work / "base.jsonl").write_text("\n".join(base) + "\n", encoding="utf-8")
(work / "delta.jsonl").write_text("\n".join(delta) + "\n", encoding="utf-8")
(work / "full.jsonl").write_text("\n".join(lines) + "\n", encoding="utf-8")

print(f"    语料切分：{corpus} 前 {total} 篇 = base {len(base)} + delta {len(delta)}")
PY

# ---------------------------------------------------------------- 计时工具
# 跑命令（输出进日志），把秒数打到 stdout。用 python 取时间戳是为了毫秒精度；
# 每次调用约 30ms 开销，相对秒级的被测对象可忽略。
run_timed() { # $1=step 名（决定日志名）; 其余为命令
    local step="$1"
    shift
    local t0 t1
    t0=$(python3 -c 'import time; print(time.time())')
    "$@" >"${WORK}/${step}.log" 2>&1
    t1=$(python3 -c 'import time; print(time.time())')
    python3 -c "print(f'{$t1 - $t0:.3f}')"
}

show_log_lines() { # 打印关键行（便于肉眼确认这一步真的做了事）
    grep -E "总耗时|embed |追加 |加载快照耗时|变化:" "${WORK}/$1.log" \
        | sed 's/^/      /' || true
}

COMMON=(--single-chunk --vectors)

echo
echo "==> 1/4 预热（首次会加载/下载模型；不计入任何计时）"
"${BIN}" build --input "${WORK}/base.jsonl" --output "${WORK}/warm.snapshot" \
    "${COMMON[@]}" >"${WORK}/warm.log" 2>&1
rm -f "${WORK}/warm.snapshot" "${WORK}/warm.snapshot.hnsw.graph" \
    "${WORK}/warm.snapshot.hnsw.data" "${WORK}/warm.snapshot.hnsw.manifest"
tail -2 "${WORK}/warm.log" | sed 's/^/    /'

echo
echo "==> 2/4 建 base 快照（追加的前置，不计入判据）"
T_BASE=$(run_timed base "${BIN}" build --input "${WORK}/base.jsonl" \
    --output "${WORK}/base.snapshot" "${COMMON[@]}")
echo "    T_base = ${T_BASE}s"
show_log_lines base

echo
echo "==> 3/4 全量建库（判据分母 T_full）"
T_FULL=$(run_timed full "${BIN}" build --input "${WORK}/full.jsonl" \
    --output "${WORK}/full.snapshot" "${COMMON[@]}")
echo "    T_full = ${T_FULL}s"
show_log_lines full

echo
echo "==> 4/4 追加（判据分子 T_inc）"
# 从 base 的**副本**追加，保留 base 供复查；sidecar 必须一起复制，
# 否则追加时会因缺图而降级重建 —— 那测的就不是「增量」了。
cp "${WORK}/base.snapshot" "${WORK}/incr.snapshot"
for suf in hnsw.graph hnsw.data hnsw.manifest; do
    if [[ -f "${WORK}/base.snapshot.${suf}" ]]; then
        cp "${WORK}/base.snapshot.${suf}" "${WORK}/incr.snapshot.${suf}"
    fi
done
T_INC=$(run_timed incr "${BIN}" build --index "${WORK}/incr.snapshot" \
    --input "${WORK}/delta.jsonl" "${COMMON[@]}")
echo "    T_inc = ${T_INC}s"
show_log_lines incr

# ---------------------------------------------------------------- 判定
python3 - "${WORK}" "${T_FULL}" "${T_BASE}" "${T_INC}" "${SIZE}" "${RATIO}" <<'PY'
import json
import pathlib
import re
import sys

work, t_full, t_base, t_inc, size, ratio = (
    pathlib.Path(sys.argv[1]),
    float(sys.argv[2]),
    float(sys.argv[3]),
    float(sys.argv[4]),
    int(sys.argv[5]),
    int(sys.argv[6]),
)

DUR_UNITS = (("ms", 1e-3), ("µs", 1e-6), ("ns", 1e-9), ("s", 1.0))


def parse_dur(text):
    """解析 Rust `Duration` 的 Debug 形式（"1.23s" / "817.1ms" / "12µs"）。"""
    for unit, mult in DUR_UNITS:
        if text.endswith(unit):
            return float(text[: -len(unit)]) * mult
    return float("nan")


def grab(log_name, pattern):
    p = work / log_name
    if not p.exists():
        return float("nan")
    m = re.search(pattern, p.read_text(encoding="utf-8"))
    return parse_dur(m.group(1)) if m else float("nan")


full = dict(
    embed=grab("full.log", r"embed \d+ 条 耗时 ([\d.]+(?:ms|µs|ns|s))"),
    save=grab("full.log", r"快照已写入 .*?耗时 ([\d.]+(?:ms|µs|ns|s))"),
)
incr = dict(
    load=grab("incr.log", r"加载快照耗时 ([\d.]+(?:ms|µs|ns|s))"),
    embed=grab("incr.log", r"embed \d+ 条 耗时 ([\d.]+(?:ms|µs|ns|s))"),
    save=grab("incr.log", r"快照已写入 .*?耗时 ([\d.]+(?:ms|µs|ns|s))"),
)

budget = t_full * ratio / 100 * 1.2
ok = t_inc < budget
verdict = "PASS" if ok else "FAIL"

print()
print("================ 增量构建实测（V2 Step 6 · S6-07 / NFR-03 ②） ================")
print(f"  语料档位     : {size} 篇（1 篇 == 1 chunk，--single-chunk）")
print(f"  追加比例     : {ratio}%")
print(f"  T_full       : {t_full:.3f}s   (embed {full['embed']:.1f}s / save {full['save']:.3f}s)")
print(f"  T_base       : {t_base:.3f}s   (仅前置，不计入判据)")
print(f"  T_inc        : {t_inc:.3f}s   (load {incr['load']:.3f}s / "
      f"embed {incr['embed']:.1f}s / save {incr['save']:.3f}s)")
print(f"  预算         : T_full × {ratio}% × 1.2 = {budget:.3f}s")
print(f"  判定         : T_inc {t_inc:.3f}s {'<' if ok else '>='} {budget:.3f}s  ⇒  {verdict}")
print("  口径         : 同一次运行内取数；T_inc 为端到端墙钟（含 load 既有索引 + 落盘）")
print("=============================================================================")

(work / "result.json").write_text(
    json.dumps(
        {
            "size": size,
            "ratio": ratio,
            "t_full_s": round(t_full, 3),
            "t_base_s": round(t_base, 3),
            "t_inc_s": round(t_inc, 3),
            "budget_s": round(budget, 3),
            "verdict": verdict,
            "full_breakdown": {k: round(v, 3) for k, v in full.items()},
            "incr_breakdown": {k: round(v, 3) for k, v in incr.items()},
        },
        indent=2,
        ensure_ascii=False,
    ),
    encoding="utf-8",
)
print(f"机器可读结果：{work / 'result.json'}")
sys.exit(0 if ok else 1)
PY
