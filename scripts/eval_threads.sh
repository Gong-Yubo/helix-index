#!/usr/bin/env bash
#
# 并发检索吞吐（NFR-10 / V2 Step 6 · T7-17）一键复现实测
#
# 判据（**方案 A，评审 Q4 已拍板**，D-S6-08）：
#     --threads 4 的 QPS ≥ --threads 1 的 QPS × 2.5（近线性，允许 40% 折损）
# **前置条件**（先正确、后吞吐）：各档位的 hits（chunk_id + score）**逐位一致**
#     —— 不成立即真 bug，此时**任何吞吐数字都不予采信**（bench 自己会 Err）。
#
# 口径（设计 §4.6.2 / D-S6-09；评审对 D-S6-08 的补充 ②）：
#     · 预热丢弃首轮 + 顺序交错（1/2/4/8/1/2/4/8）—— 由 bench 的「两趟扫描、丢第一趟」实现
#     · 主指标 = **QPS（吞吐）**；辅指标 = 每线程延迟分位（发现「吞吐上去了但尾延迟炸了」）
#     · QPS 用**进程内**墙钟（spawn→join），不含进程启动与快照加载
#     · ⚠️ 并发下 per-query 延迟**含排队**，**不得与 NFR-02（单线程口径）横比**
#     · 报告必须标**运行范围**（核数 / 是否插电 / 是否在 CI）——脚本会自动采集并写入产物
#
# 用法：
#     ./scripts/eval_threads.sh                      # 复用/构建 12K 快照，档位 1,2,4,8
#     SNAPSHOT=/tmp/x.idx ./scripts/eval_threads.sh  # 复用一个已建好的快照（省去 ~4 分钟）
#     LEVELS=1,4 REPS=10 ./scripts/eval_threads.sh   # 缩减档（冒烟）
#
# 产物：${WORK}（默认 /tmp/helix-threads）/ 下 bench 的 --json + logs + result.json
#
# ⚠️ 分钟级、**不进 CI**（项目纪律：性能数字一律本地 release 跑，CI 共享 runner 不可引用）。
# ⚠️ 跑之前**别并行**跑 cargo / 其他守门 —— 会污染计时（perf-ab-calibration 既有教训）。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

SIZE="${SIZE:-12000}"
CORPUS="${CORPUS:-data/t2-corpus.jsonl}"
QUERIES="${QUERIES:-data/t2-queries.jsonl}"
LEVELS="${LEVELS:-1,2,4,8}"
MODES="${MODES:-bm25,vector,hybrid}"
REPS="${REPS:-20}"
K="${K:-10}"
BIN="${BIN:-target/release/helix}"
WORK="${WORK:-/tmp/helix-threads}"
SNAPSHOT="${SNAPSHOT:-}"

if [[ ! -x "${BIN}" ]]; then
    echo "错误：找不到可执行文件 ${BIN}，先跑 cargo build --release --workspace" >&2
    exit 2
fi
mkdir -p "${WORK}"

# ---------------------------------------------------------------- 运行范围
# 口径要求「报告必须标明运行范围」⇒ 自动采集，不靠人记
CORES="$( (sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo '?') )"
POWER="$( (pmset -g batt 2>/dev/null | grep -oE 'AC attached|Battery Power' | head -1 || true) )"
[[ -z "${POWER}" ]] && POWER="unknown"
IN_CI="${CI:-no}"

# ---------------------------------------------------------------- 快照
if [[ -z "${SNAPSHOT}" ]]; then
    SNAPSHOT="${WORK}/base.snapshot"
    if [[ ! -f "${SNAPSHOT}" ]]; then
        echo "==> 构建 ${SIZE} 篇快照（含向量；首次含模型加载，分钟级）"
        # 与 eval_perf.sh / eval_incremental.sh 同口径：--single-chunk ⇒ 1 篇 == 1 chunk
        "${BIN}" build --input "${CORPUS}" --output "${SNAPSHOT}" \
            --single-chunk --vectors > "${WORK}/build.log" 2>&1
        tail -3 "${WORK}/build.log" | sed 's/^/    /'
    else
        echo "==> 复用已有快照 ${SNAPSHOT}"
    fi
fi
if [[ ! -f "${SNAPSHOT}" ]]; then
    echo "错误：快照 ${SNAPSHOT} 不存在" >&2
    exit 2
fi

echo
echo "运行范围：核数=${CORES} ｜ 供电=${POWER} ｜ CI=${IN_CI}"
echo "档位=${LEVELS} 模式=${MODES} reps=${REPS} K=${K}"
echo "快照=${SNAPSHOT} 查询=${QUERIES}"

# ---------------------------------------------------------------- 测量
echo
echo "==> 跑 bench（阶段 B2 并发吞吐）"
if ! "${BIN}" bench --index "${SNAPSHOT}" --queries "${QUERIES}" \
        --modes "${MODES}" --threads "${LEVELS}" --reps "${REPS}" --k "${K}" \
        --json "${WORK}/threads.json" > "${WORK}/bench.log" 2>&1; then
    # ⚠️ 不要让它被 `set -e` 静默带走：失败必须把日志尾部打出来
    echo "    ❌ bench 失败（exit≠0），日志尾部：" >&2
    tail -20 "${WORK}/bench.log" | sed 's/^/      /' >&2
    exit 1
fi
echo "    ✅ bench exit=0"
# 只打印阶段 B2 那一段（其余阶段已在 eval_perf.sh 里有专门口径）
sed -n '/== 并发吞吐/,$p' "${WORK}/bench.log" | sed 's/^/    /'

# ---------------------------------------------------------------- 判定
python3 - "${WORK}" "${LEVELS}" "${CORES}" "${POWER}" "${IN_CI}" "${REPS}" <<'PY'
import json
import pathlib
import sys

work = pathlib.Path(sys.argv[1])
levels = [int(x) for x in sys.argv[2].split(",")]
cores, power, in_ci, reps = sys.argv[3], sys.argv[4], sys.argv[5], int(sys.argv[6])

d = json.loads((work / "threads.json").read_text(encoding="utf-8")).get("threads", {})
if not d:
    print("\n❌ threads.json 里没有 threads 段 —— bench 没跑到阶段 B2（--threads 只给了单档 1？）")
    sys.exit(1)

print()
print("=============== 并发检索吞吐实测（V2 Step 6 · T7-17 / NFR-10 方案 A）===============")
print(f"  运行范围 : 核数 {cores} ｜ 供电 {power} ｜ CI={in_ci} ｜ reps={reps} ｜ 档位 {levels}")
print(f"  判据     : QPS(4) ≥ QPS(1) × 2.5（D-S6-08 方案 A）")
print("  前置条件 : 各档 hits（chunk_id + score）逐位一致（不成立时 bench 已直接 Err）")
print()
print(f"  {'mode':<8} {'threads':>8} {'QPS':>10} {'加速比':>8} {'全局P50(ms)':>12} {'全局P99(ms)':>12}")
print("  " + "-" * 62)

verdicts = {}
for mode, lv in d.items():
    for t in sorted((int(k) for k in lv.keys() if k.isdigit())):
        r = lv[str(t)]
        print(
            f"  {mode:<8} {t:>8} {r['qps']:>10.1f} "
            f"{r['speedup_vs_first_level']:>7.2f}× {r['global_p50_ms']:>12.3f} {r['global_p99_ms']:>12.3f}"
        )
    n = lv.get("nfr10")
    if n:
        ok = n["verdict"] == "pass"
        verdicts[mode] = n
        print(
            f"  {'':<8} {'→ NFR-10':>8} QPS(4)/QPS(1) = {n['ratio']:.2f} "
            f"≥ {n['threshold']:.1f} ? {'✅ PASS' if ok else '❌ FAIL'}"
        )
    print()

all_pass = bool(verdicts) and all(v["verdict"] == "pass" for v in verdicts.values())
print("  " + "=" * 62)
print(f"  总判定: {'✅ PASS（全部模式达标）' if all_pass else '❌ 未全部达标'}")
if len(levels) > 1:
    print("  ⚠️ 加速比是**相对首档**（本脚本首档为 %d）——不是跨机可引用指标（D-S6-08 反对方案 B）。" % levels[0])
print("  ⚠️ 并发下 per-query 延迟含排队，**不得**与 NFR-02（单线程口径）横比。")
print("===============================================================================")

(work / "result.json").write_text(
    json.dumps(
        {
            "run_scope": {"cores": cores, "power": power, "in_ci": in_ci, "reps": reps, "levels": levels},
            "modes": d,
            "all_pass": all_pass,
        },
        indent=2,
        ensure_ascii=False,
    ),
    encoding="utf-8",
)
print(f"机器可读结果：{work / 'result.json'}")
sys.exit(0 if all_pass else 1)
PY
