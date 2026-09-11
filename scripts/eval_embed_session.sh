#!/usr/bin/env bash
#
# V2 Step 6 · T7-09 embed 并行 spike 一键跑（S6-01 / S6-02）
#
# # 为什么逐档位独立进程，而不是"一次进程内跑完所有档位"
#
# 设计 §4.2.5 原本要求"一次进程内跑完 E1/E2/E3"以消除跨进程漂移。**实现期实测推翻了这个前提**：
# 单 session 在 batch 64 × 长文本下的峰值 RSS 就达 **~2 GB**（是激活张量，不是权重），
# 而同进程方案要同时驻留 `1 + 2 + 4 = 7` 个 session（本机 32 GB）⇒ 换页、所有档位被均匀拖慢，
# 且 `e2-4` 单档位实测就 13 GB。见 `eval-report.md` §8.10 的「测量协议更正」。
#
# 现协议（**按波交错**，跨进程但仍在时间上交错）：
#   1) 逐档位先跑 WARMUP 轮预热（丢弃）——每个进程自带模型加载，不计入任何计时；
#   2) 然后 ROUNDS 波，每波按 A/B/C 顺序**各起一个独立进程**跑 1 轮
#      ⇒ 与"同进程 A/B/A/B"的漂移控制等价（交错在"波"这一层），但内存相互隔离；
#   3) 每次运行都用外置 `/usr/bin/time -l` 包住 ⇒ 顺带得到**可归因的分档位峰值 RSS**
#      —— 这正是同进程方案给不出、且必须用 `unsafe` 才能补上的那件事。
#
# 用法：
#   ./scripts/eval_embed_session.sh                 # E1/E2
#   ./scripts/eval_embed_session.sh --coreml        # 额外用 --features coreml 重测（含 E3）
#   TEXTS=1000 ROUNDS=2 ./scripts/eval_embed_session.sh
#
# 依赖：bash / python3（数值汇总）/ `/usr/bin/time -l`（macOS 自带；Linux 需 GNU time 的 `gtime -v`）。
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

TEXTS="${TEXTS:-4000}"
ROUNDS="${ROUNDS:-3}"
WARMUP="${WARMUP:-1}"
WITH_COREML=0
for a in "$@"; do [[ "$a" == "--coreml" ]] && WITH_COREML=1; done

TIME_BIN="/usr/bin/time"
TIME_FLAG="-l"
if [[ ! -x "$TIME_BIN" ]]; then
    TIME_BIN="gtime"; TIME_FLAG="-v"
fi
TIME_OK=0
command -v "${TIME_BIN%% *}" >/dev/null 2>&1 && TIME_OK=1
if [[ $TIME_OK -eq 0 ]]; then
    echo "错误：找不到 /usr/bin/time 或 gtime —— 峰值 RSS 无法采集，而它是 D-S6-01 决策门的一半" >&2
    exit 1
fi

CFGS_CPU="e1,e2-2,e2-4"
CFGS_COREML="e1,e2-2,e2-4,e3-coreml"

# macOS `time -l` 的「maximum resident set size」单位是**字节**；GNU time 的是 **KB**
rss_to_gb() {
    if [[ "$TIME_FLAG" == "-l" ]]; then
        python3 -c "print(f'{$1/1024/1024/1024:.2f}')"
    else
        python3 -c "print(f'{$1*1024/1024/1024:.2f}')"
    fi
}

# 跑一次「单档位、单轮」的独立进程。stdout 打印 `<耗时s> <吞吐> <峰值RSS GB>`。
# `record=1` 时把 RSS 追加进 TSV（供汇总段复查）——**预热轮必须传 0**，
# 否则预热进程的 RSS 会混进"各波峰值"表里（它是合法测量，但不属于任何一波）。
run_one() {
    local label="$1" bin="$2" cfg="$3" warmup="$4" json="$5" record="$6"
    local raw rss gb
    raw=$("$TIME_BIN" "$TIME_FLAG" "$bin" \
        --texts "$TEXTS" --rounds 1 --warmup "$warmup" --configs "$cfg" --json "$json" 2>&1 >/dev/null || true)
    rss=$(echo "$raw" | grep -iE "maximum resident" | grep -oE '[0-9]+' | head -1 || true)
    if [[ -z "$rss" ]]; then
        echo "错误：未能解析峰值 RSS（${TIME_BIN} ${TIME_FLAG} 输出异常）：$(echo "$raw" | head -2)" >&2
        exit 1
    fi
    gb=$(rss_to_gb "$rss")
    [[ "$record" == "1" ]] && printf '%s\t%s\n' "$cfg" "$gb" >>"/tmp/s6-embed-${label}-rss.tsv"
    python3 -c "
import json
c = json.load(open('$json'))['configs'][0]
print(f\"{c['rounds_secs'][0]:.2f} {c['throughput_per_sec']:.1f} $gb\")
"
}

run_variant() {
    local label="$1" feature="$2" cfgs="$3"
    echo
    echo "=================================================================="
    echo "== 变体：$label（--features ${feature:-<none>}）"
    echo "=================================================================="

    local feat_arg=()
    [[ -n "$feature" ]] && feat_arg=(--features "$feature")  # 空数组在 bash 3.2 + set -u 下必须用 + 惯用法展开

    # 独立构建 + 固化二进制副本（coreml 变体会覆盖 target 下的同名产物）
    local bin="/tmp/s6-bench-embed-${label}"
    cargo build -q -p helix-core --release "${feat_arg[@]+"${feat_arg[@]}"}" --example bench_embed_session
    cp -f target/release/examples/bench_embed_session "$bin"

    local IFS=','; read -r -a arr <<<"$cfgs"; unset IFS
    rm -f "/tmp/s6-embed-${label}-rss.tsv"

    # ---- 1) 预热（丢弃、不计时）----
    echo
    echo "---- 预热：$WARMUP 轮 × 逐档位独立进程（丢弃）----"
    local w c
    # ⚠️ 必须显式守卫：BSD/macOS 的 `seq 1 0` 会输出 "1 0"（倒序），会多跑一轮预热
    if [[ "$WARMUP" -gt 0 ]]; then
        for w in $(seq 1 "$WARMUP"); do
            for c in "${arr[@]}"; do
                IFS=' ' read -r secs _thr _gb \
                    <<<"$(run_one "$label" "$bin" "$c" 0 "/tmp/s6-warmup-${label}-${c}-${w}.json" 0)"
                echo "  [warmup $w] $(printf '%-12s' "$c") ${secs}s（丢弃）"
            done
        done
    fi

    # ---- 2) 计时：ROUNDS 波，每波 A/B/C 各一个独立进程 ----
    echo
    echo "---- 计时：$ROUNDS 波 × 逐档位独立进程（交错 A/B/…）----"
    local r
    for r in $(seq 1 "$ROUNDS"); do
        for c in "${arr[@]}"; do
            IFS=' ' read -r secs thr gb \
                <<<"$(run_one "$label" "$bin" "$c" 0 "/tmp/s6-embed-${label}-${c}-r${r}.json" 1)"
            echo "  [wave $r] $(printf '%-12s' "$c") ${secs}s  ${thr} 条/s  peak RSS ${gb} GB"
        done
    done

    # ---- 3) 汇总表 + 决策门 ----
    echo
    echo "---- 汇总（$label）----"
    python3 - "$label" "$ROUNDS" "${arr[@]}" <<'PY'
import json, statistics, sys
label, rounds = sys.argv[1], int(sys.argv[2])
cfgs = sys.argv[3:]

data = {}
for c in cfgs:
    secs, thr = [], []
    meta = None
    for r in range(1, rounds + 1):
        cfg = json.load(open(f"/tmp/s6-embed-{label}-{c}-r{r}.json"))["configs"][0]
        meta = cfg
        secs.append(cfg["rounds_secs"][0])
        thr.append(cfg["throughput_per_sec"])
    data[c] = {"secs": secs, "median_secs": statistics.median(secs),
               "median_thr": statistics.median(thr), "meta": meta}

base = data["e1"]["median_secs"]
print(f"{'档位':<12}{'sess':>5}{'intra':>7}{'ep':>7}{'每波耗时(s)':>26}{'中位数(s)':>11}{'吞吐(条/s)':>11}{'相对E1':>9}")
for c in cfgs:
    d = data[c]
    m = d["meta"]
    print(f"{c:<12}{m['sessions']:>5}{str(m['intra_threads']):>7}{m['ep']:>7}"
          f"{'/'.join(f'{s:.1f}' for s in d['secs']):>26}{d['median_secs']:>11.2f}"
          f"{d['median_thr']:>11.1f}{base / d['median_secs']:>8.2f}x")

e1 = data["e1"]["secs"]
drift = (e1[-1] / e1[0] - 1) * 100
print(f"\n控制组（e1）波间漂移：{drift:+.1f}%（min {min(e1):.2f}s / max {max(e1):.2f}s）")
if abs(drift) > 10:
    print("⚠️ 漂移 > 10% ⇒ 跨档位比较请先按控制组归一（perf-ab-calibration 纪律）")

print("\n决策门（D-S6-01：吞吐增益 ≥ +30% 且 峰值 RSS 增量 ≤ +20%，取合取）：")
for c in cfgs:
    if c == "e1":
        continue
    gain = (base / data[c]["median_secs"] - 1) * 100
    print(f"  {c:<12} 吞吐增益 {gain:+6.1f}%  ⇒ {'过' if gain >= 30 else '不过'}（RSS 增量见下段）")

json.dump({c: {"secs": d["secs"], "median_secs": d["median_secs"],
               "median_throughput": d["median_thr"], "meta": d["meta"]}
           for c, d in data.items()},
          open(f"/tmp/s6-embed-{label}-summary.json", "w"), ensure_ascii=False, indent=2)
PY

    # ---- 4) 峰值 RSS（取各波最大值 = 最保守口径）----
    echo
    echo "---- 峰值 RSS（逐档位独立进程；各波最大值）----"
    python3 - "$label" <<'PY'
import sys
label = sys.argv[1]
rows = [l.split("\t") for l in open(f"/tmp/s6-embed-{label}-rss.tsv").read().strip().split("\n") if l]
by = {}
for cfg, gb in rows:
    by.setdefault(cfg, []).append(float(gb))
base = None
print(f"{'档位':<12}{'各波峰值RSS(GB)':>26}{'最大':>8}{'相对E1增量':>12}")
for cfg, v in by.items():
    mx = max(v)
    inc = "" if cfg == "e1" else f"{(mx / base - 1) * 100:+.1f}%"
    print(f"{cfg:<12}{'/'.join(f'{x:.2f}' for x in v):>26}{mx:>8.2f}{inc:>12}")
    if cfg == "e1":
        base = mx
print("\n⚠️ RSS 增量只在**同一变体内**比较；跨变体（cpu vs coreml）EP 不同，不可比。")
print("⚠️ 本口径是 **build 路径**（batch 64 × 长文本）的峰值，与 NFR-05 的 372MB（search 路径 / batch 1）**不可比**。")
PY
}

echo "== V2 Step 6 · T7-09 spike（S6-01/S6-02）：语料 ${TEXTS} 段；计时 ${ROUNDS} 波（预热 ${WARMUP} 轮）=="
echo "运行范围：$(uname -srm)；核数 $(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo '?')；开始 $(date -u '+%Y-%m-%dT%H:%M:%SZ')"

run_variant "cpu" "" "$CFGS_CPU"
if [[ $WITH_COREML -eq 1 ]]; then
    run_variant "coreml" "coreml" "$CFGS_COREML"
fi

echo
echo "产物：/tmp/s6-embed-<label>-<cfg>-r<N>.json（逐轮）+ /tmp/s6-embed-<label>-summary.json（汇总）"
echo "落表：写进 docs/devel/eval-report.md §8.10，并按 D-S6-01 给出「投 / 不投」结论。"
