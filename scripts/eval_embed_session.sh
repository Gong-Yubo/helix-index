#!/usr/bin/env bash
#
# V2 Step 6 · T7-09 embed 并行 spike 一键跑（S6-01 / S6-02）
#
# # 为什么逐档位独立进程，而不是"一次进程内跑完所有档位"
#
# 设计 §4.2.5 原本要求"一次进程内跑完 E1/E2/E3"以消除跨进程漂移。**实现期实测推翻了这个前提**：
# 单 session 在 batch 64 × 长文本下的峰值 RSS 就达 **2~3.3 GB**（是激活张量，不是权重），
# 而同进程方案要同时驻留 `1 + 2 + 4 = 7` 个 session（本机 32 GB）⇒ 换页、所有档位被均匀拖慢。
# 见 `eval-report.md` §8.10 的「测量协议更正」。
#
# 现协议：**逐档位独立进程 + 按波交错**（A/B/C 各起一个进程算一波，跑 ROUNDS 波）。
#
# ⚠️ 预热口径（评审 F1 更正版 + 新增 I）：脚本级的 WARMUP 轮跑在**一次性进程**里，
#    只暖 OS 文件缓存；**每个被计时的波都是它自己那个进程的首次推理**（计时波一律 `--warmup 0`）。
#    这与设计 §4.2.1「每档位 1 轮完整语料、丢弃计时」的字面写法不同 —— 报告里已如实写明。
#    （`run_pass` 的秒表起在 `SessionPool::build` 之后，故模型加载与 ONNX 图优化本就不进计时。）
#
# # 用法
#
#   ./scripts/eval_embed_session.sh                                  # E1/E2
#   ./scripts/eval_embed_session.sh --coreml                         # 额外跑 coreml 变体（含 E3）
#   ./scripts/eval_embed_session.sh --coreml --coreml-static-shapes   # E3 的 EP 调优档
#   TEXTS=1000 ROUNDS=2 ./scripts/eval_embed_session.sh
#
# # 产物与失败语义（评审 F1 / 新增 J）
#
# - 全部产物落 **$RUN_DIR**（`mktemp -d`）⇒ 残留文件不会被当成新数据汇总；
# - 每次运行判**子进程退出码**，`time` 与子进程 stderr 全存 `$RUN_DIR/logs/`；
# - 载入 JSON 时**校验 `meta`**（texts / warmup / rounds / coreml_feature / coreml_static_shapes）
#   与本次参数一致、`configs[0].rounds_secs` 非空且长度等于 rounds；
# - 峰值 RSS 必须落在 `[RSS_MIN_GB, 0.9 × 物理内存]`，否则该波作废（不写进 TSV）；
# - 末尾输出**决策门合取表**（吞吐 ≥ +30% **且** 峰值 RSS 增量 ≤ +20% ⇒ 投 / 不投）。
#
# 依赖：bash / python3 / BSD `/usr/bin/time -l`（macOS）或 GNU `gtime -v`（Linux）。
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

TEXTS="${TEXTS:-4000}"
ROUNDS="${ROUNDS:-3}"
WARMUP="${WARMUP:-1}"
RSS_MIN_GB="${RSS_MIN_GB:-0.05}"
WITH_COREML=0
STATIC_SHAPES=0
for a in "$@"; do
    case "$a" in
        --coreml) WITH_COREML=1 ;;
        --coreml-static-shapes) STATIC_SHAPES=1 ;;
        -h | --help)
            sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "未知参数：${a}（可用 --coreml / --coreml-static-shapes）" >&2
            exit 2
            ;;
    esac
done

# 参数守卫（评审 K/L）：BSD/macOS 的 `seq 1 0` 输出 "1 0"（倒序）⇒ ROUNDS=0 会白跑两波
if [[ "$TEXTS" -lt 1 || "$ROUNDS" -lt 1 || "$WARMUP" -lt 0 ]]; then
    echo "参数越界：需 TEXTS ≥ 1、ROUNDS ≥ 1、WARMUP ≥ 0（实得 TEXTS=$TEXTS ROUNDS=$ROUNDS WARMUP=${WARMUP}）" >&2
    exit 2
fi

# 外置 time：**按能力选，不只按路径存在性**（评审 Q）—— Linux 上 /usr/bin/time 是 GNU time，不接受 `-l`
TIME_UNIT=""
if [[ "$(uname -s)" == "Darwin" && -x /usr/bin/time ]]; then
    TIME_BIN="/usr/bin/time"; TIME_FLAG="-l"; TIME_UNIT="bytes"
elif command -v gtime >/dev/null 2>&1; then
    TIME_BIN="gtime"; TIME_FLAG="-v"; TIME_UNIT="kb"
else
    echo "错误：找不到 BSD /usr/bin/time（Darwin）或 GNU time（gtime）—— 峰值 RSS 是决策门的一半，不能跳过" >&2
    exit 1
fi

TMP_ROOT="${TMPDIR:-/tmp}"
TMP_ROOT="${TMP_ROOT%/}"
RUN_DIR="$(mktemp -d "$TMP_ROOT/s6-embed-XXXXXX")"
mkdir -p "$RUN_DIR/logs"

phys_bytes() {
    if [[ "$(uname -s)" == "Darwin" ]]; then
        sysctl -n hw.memsize
    else
        awk '/MemTotal/{print $2*1024}' /proc/meminfo
    fi
}
PHYS_BYTES=$(phys_bytes)
PHYS_GB=$(python3 -c "print(f'{$PHYS_BYTES / 1024 / 1024 / 1024:.0f}')")
RSS_MAX_GB=$(python3 -c "print(f'{$PHYS_BYTES * 0.9 / 1024 / 1024 / 1024:.1f}')")

CFGS_CPU="e1,e2-2,e2-4"
CFGS_COREML="e1,e2-2,e2-4,e3-coreml"

VARIANT_COREML=0

# macOS `time -l` 的「maximum resident set size」是**字节**；GNU time 的是 **KB**
rss_to_gb() {
    if [[ "$TIME_UNIT" == "bytes" ]]; then
        python3 -c "print(f'{$1/1024/1024/1024:.2f}')"
    else
        python3 -c "print(f'{$1*1024/1024/1024:.2f}')"
    fi
}

# 跑一次「单档位、单轮」的独立进程（所有计时与预热调用都是单轮）。
# stdout：`<耗时s> <吞吐条/s> <峰值RSS GB>`；`record=1` 时把 RSS 追加进 TSV。
# 任一步校验失败 ⇒ 非零退出（调用方立即中止，不产出半真数据）。
run_one() {
    local label="$1" bin="$2" cfg="$3" warmup="$4" json="$5" record="$6"
    local tag="${label}-${cfg}-w${warmup}-${RANDOM}"
    local log="$RUN_DIR/logs/${tag}.log"
    local raw rc rss gb out

    local static_arg=()
    [[ "$STATIC_SHAPES" == "1" ]] && static_arg=(--coreml-static-shapes)

    set +e
    # `ORT_LOG` 是尽力而为：该版本若不识别 ort 日志环境变量则无副作用；stderr 一定落 log（评审 P）
    raw=$(ORT_LOG=warning "$TIME_BIN" "$TIME_FLAG" "$bin" \
        --texts "$TEXTS" --rounds 1 --warmup "$warmup" --configs "$cfg" \
        "${static_arg[@]+"${static_arg[@]}"}" --json "$json" 2>&1 >/dev/null)
    rc=$?
    set -e
    printf '%s\n' "$raw" >"$log"

    if [[ $rc -ne 0 ]]; then
        echo "作废：$cfg 退出码 ${rc}（详见 ${log}）" >&2
        return 1
    fi

    # 退出码 0 也要核 meta 与样本量 —— `|| true` 时代这两条都看不见（评审 F1 / 新增 J）
    if ! python3 - "$json" "$TEXTS" "$warmup" "$VARIANT_COREML" "$STATIC_SHAPES" <<'PY'
import json, sys
path, texts, warmup = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
coreml, static = sys.argv[4] == "1", sys.argv[5] == "1"
d = json.load(open(path))
m = d["meta"]
checks = [
    (m["texts"] == texts, f"meta.texts={m['texts']} != {texts}（残留文件？）"),
    (m["warmup"] == warmup, f"meta.warmup={m['warmup']} != {warmup}"),
    (m["rounds"] == 1, f"meta.rounds={m['rounds']} != 1"),
    (bool(m["coreml_feature"]) == coreml, "meta.coreml_feature 与本次变体不符"),
    (bool(m.get("coreml_static_shapes", False)) == static, "meta.coreml_static_shapes 不符"),
]
for ok, msg in checks:
    if not ok:
        raise SystemExit(f"meta 校验失败：{msg}")
cs = d["configs"]
if len(cs) != 1:
    raise SystemExit(f"期望单档位，实得 {len(cs)} 个")
secs = cs[0]["rounds_secs"]
if len(secs) != 1 or secs[0] <= 0:
    raise SystemExit(f"rounds_secs 异常：{secs}")
PY
    then
        echo "作废：$cfg 的 JSON 未通过校验（详见 ${log}）" >&2
        return 1
    fi

    rss=$(echo "$raw" | grep -iE "maximum resident" | grep -oE '[0-9]+' | head -1 || true)
    if [[ -z "$rss" ]]; then
        echo "作废：$cfg 未能解析峰值 RSS（详见 ${log}）" >&2
        return 1
    fi
    gb=$(rss_to_gb "$rss")
    # RSS 量级区间：太低 ⇒ 解析错；太高 ⇒ 多半是 OOM 前的异常态（评审 F1）
    if ! python3 -c "import sys; sys.exit(0 if $RSS_MIN_GB <= $gb <= $RSS_MAX_GB else 1)"; then
        echo "作废：$cfg 峰值 RSS ${gb}GB 落在合理区间 [${RSS_MIN_GB}, ${RSS_MAX_GB}] 之外（详见 ${log}）" >&2
        return 1
    fi
    [[ "$record" == "1" ]] && printf '%s\t%s\n' "$cfg" "$gb" >>"$RUN_DIR/${label}-rss.tsv"

    out=$(python3 -c "
import json
secs = json.load(open('$json'))['configs'][0]['rounds_secs'][0]
print(f'{secs:.2f} {$TEXTS/secs:.1f} $gb')
")
    echo "$out"
}

run_variant() {
    local label="$1" feature="$2" cfgs="$3"
    echo
    echo "=============================================================="
    echo "== 变体：${label}（--features ${feature:-<none>}；coreml_static_shapes=${STATIC_SHAPES}）"
    echo "=============================================================="

    local feat_arg=()
    [[ -n "$feature" ]] && feat_arg=(--features "$feature")  # 空数组在 bash 3.2 + set -u 下用 + 惯用法展开

    local bin="$RUN_DIR/bin-${label}"
    cargo build -q -p helix-core --release "${feat_arg[@]+"${feat_arg[@]}"}" --example bench_embed_session
    cp -f target/release/examples/bench_embed_session "$bin"

    VARIANT_COREML=0
    [[ "$label" == "coreml" ]] && VARIANT_COREML=1

    local IFS=','; read -r -a arr <<<"$cfgs"; unset IFS
    local w c r out secs thr gb

    if [[ "$WARMUP" -gt 0 ]]; then
        echo
        echo "---- 预热：$WARMUP 轮 × 逐档位独立进程（丢弃；⚠️ 只暖 OS 缓存，暖不到被计时的进程）----"
        for w in $(seq 1 "$WARMUP"); do
            for c in "${arr[@]}"; do
                out=$(run_one "$label" "$bin" "$c" 0 "$RUN_DIR/warmup-${label}-${c}-${w}.json" 0)
                IFS=' ' read -r secs _thr _gb <<<"$out"
                echo "  [warmup $w] $(printf '%-12s' "$c") ${secs}s（丢弃）"
            done
        done
    fi

    echo
    echo "---- 计时：$ROUNDS 波 × 逐档位独立进程（交错 A/B/…；每波 --warmup 0）----"
    for r in $(seq 1 "$ROUNDS"); do
        for c in "${arr[@]}"; do
            out=$(run_one "$label" "$bin" "$c" 0 "$RUN_DIR/${label}-${c}-r${r}.json" 1)
            IFS=' ' read -r secs thr gb <<<"$out"
            echo "  [wave $r] $(printf '%-12s' "$c") ${secs}s  ${thr} 条/s  peak RSS ${gb} GB"
        done
    done

    echo
    echo "---- 汇总（${label}）｜语料 ${TEXTS} 段 ｜ 波数 ${ROUNDS} ｜ 每档位样本量 = ${ROUNDS} ----"
    python3 - "$RUN_DIR" "$label" "$ROUNDS" "$cfgs" <<'PY'
import json, os, statistics, sys
run_dir, label, rounds, cfgs = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4].split(",")

data = {}
for c in cfgs:
    secs, thr, meta = [], [], None
    for r in range(1, rounds + 1):
        cfg = json.load(open(f"{run_dir}/{label}-{c}-r{r}.json"))["configs"][0]
        meta = cfg
        secs.append(cfg["rounds_secs"][0])
        thr.append(cfg["throughput_per_sec"])
    data[c] = {"secs": secs, "median": statistics.median(secs),
               "median_thr": statistics.median(thr), "meta": meta}

rss = {}
p = f"{run_dir}/{label}-rss.tsv"
if os.path.exists(p):
    for line in open(p).read().strip().split("\n"):
        if line:
            k, v = line.split("\t")
            rss.setdefault(k, []).append(float(v))

base = data["e1"]["median"] if "e1" in data else None
print(f"{'档位':<12}{'sess':>5}{'intra':>6}{'ep':>7}{'每波(s)':>22}{'中位':>8}{'最小':>8}{'最大':>8}{'吞吐':>8}{'相对E1':>8}{'峰值RSS':>9}")
band = []
for c in cfgs:
    d = data[c]
    m = d["meta"]
    sp = (max(d["secs"]) - min(d["secs"])) / min(d["secs"]) * 100
    band.append((c, sp))
    waves = "/".join(f"{x:.1f}" for x in d["secs"])
    rel = f"{base / d['median']:.2f}x" if base else "—"
    peak = f"{max(rss[c]):.2f}GB" if c in rss else "—"
    print(f"{c:<12}{m['sessions']:>5}{str(m['intra_threads']):>6}{m['ep']:>7}"
          f"{waves:>22}{d['median']:>8.2f}{min(d['secs']):>8.2f}"
          f"{max(d['secs']):>8.2f}{d['median_thr']:>8.1f}{rel:>8}{peak:>9}")

if "e1" in data:
    e1 = data["e1"]["secs"]
    print(f"\n控制组（e1）三点估计：min {min(e1):.2f} / max {max(e1):.2f} ⇒ "
          f"(max−min)/min = {(max(e1) - min(e1)) / min(e1) * 100:.1f}%")

worst = max(band, key=lambda t: t[1])
print(f"全体档位最差带宽：{worst[0]} {worst[1]:.1f}%"
      + ("（≤10% ⇒ 跨档位可直接比较）" if worst[1] <= 10 else "（>10% ⇒ 该档位的相对值不宜当精确点估计）"))

print("\n决策门（D-S6-01：吞吐 ≥ +30% 且 峰值 RSS 增量 ≤ +20%，**取合取**）")
if "e1" not in data:
    print("  ⚠️ 档位表缺 e1 ⇒ 无法归一，不判定")
else:
    e1_max = max(rss["e1"]) if "e1" in rss else None
    print(f"  {'候选':<12}{'吞吐增益':>10}{'≥+30%':>8}{'RSS增量':>10}{'≤+20%':>8}{'合取':>8}")
    for c in cfgs:
        if c == "e1":
            continue
        gain = (base / data[c]["median"] - 1) * 100
        g_ok = gain >= 30
        if e1_max and c in rss:
            inc = (max(rss[c]) / e1_max - 1) * 100
            r_txt, r_ok = f"{inc:+.1f}%", inc <= 20
        else:
            r_txt, r_ok = "未测量", False
        print(f"  {c:<12}{gain:>+9.1f}%{'过' if g_ok else '不过':>8}{r_txt:>10}"
              f"{'过' if r_ok else '不过':>8}{('投' if (g_ok and r_ok) else '不投'):>8}")
    print("  ⚠️ RSS 增量以 **e1 为基线**：NFR-05 的 372MB 是 search 路径口径，对 build 路径是空条件")
PY
}

echo "=============================================================="
echo "== V2 Step 6 · T7-09 spike（S6-01/S6-02）"
echo "== 语料 ${TEXTS} 段 / 波数 ${ROUNDS} / 预热 ${WARMUP} 轮（仅暖 OS 缓存）"
echo "== 运行范围：$(uname -srm)；核数 $(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo '?')；内存 ${PHYS_GB} GB；开始 $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
echo "=============================================================="

run_variant "cpu" "" "$CFGS_CPU"
if [[ $WITH_COREML -eq 1 ]]; then
    run_variant "coreml" "coreml" "$CFGS_COREML"
fi

echo
echo "产物目录：$RUN_DIR"
echo "落表：写入 docs/devel/eval-report.md §8.10，并引用上面的合取判定表。"
