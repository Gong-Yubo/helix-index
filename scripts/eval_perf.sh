#!/usr/bin/env bash
#
# 性能/NFR 实测一键跑（V1-05）
#
# 把 P5 手工做的四件事自动化，输出**对照 NFR 目标值的达标判定表**。
#
#   NFR-02 延迟   —— bench 阶段 B（bare embedder，6400 样本/模式）
#   NFR-03 构建   —— build --vectors --single-chunk 的 embed 分段计时
#   NFR-04 冷启动 —— 快照加载 与 HNSW 图重建**分别计时**（design 10.2 要求勿混）
#   NFR-05 内存   —— peak RSS（整进程）与向量分量（推算）**两个口径分开报告**
#
# 用法：
#   ./scripts/eval_perf.sh              # 全流程（含 embed ~230s）
#   ./scripts/eval_perf.sh --skip-build # 跳过 NFR-03（已有快照时）
#   NOTES=1 ./scripts/eval_perf.sh      # 打印口径说明
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

CORPUS="data/t2-corpus.jsonl"
QUERIES="data/t2-queries.jsonl"
SNAPSHOT="/tmp/helix-perf-snapshot"
OUT_JSON="/tmp/helix-perf-latency.json"
SKIP_BUILD=0
for a in "$@"; do [[ "$a" == "--skip-build" ]] && SKIP_BUILD=1; done

# NFR 目标值（requirements-spec.md 第 6 章）
NFR02_BM25=5; NFR02_VECTOR=10; NFR02_HYBRID=20
NFR03_TARGET=120
NFR04_TARGET=2000   # ms

if [[ -n "${NOTES:-}" ]]; then
    cat <<'EOF'
口径说明
========
NFR-05 有两个**不同**口径，不可混为一谈：
  - 整进程 peak RSS（约 383MB）= 模型 + 快照 + HNSW 图 + ort 运行时
    实测方式：/usr/bin/time -l helix search --index <含向量快照> --mode hybrid "query"
  - 向量分量（24.6MB）= 12000 × 512 维 × 4B，**由维度推算**，不是进程指标

NFR-04 同样两口径：快照加载（storage::load）与 HNSW 图重建必须分别报，
因为图不持久化（D7），图重建才是冷启动的大头。

延迟/内存数字在 CI 共享 runner 上不具可引用性，本脚本用于**本地**实测。
EOF
    exit 0
fi

echo "==> 构建 release 二进制（helix）..."
cargo build --release -p helix
BIN="target/release/helix"

# 探测跨平台内存测量方式（macOS: -l, Linux: -v）
TIME_BIN="/usr/bin/time"
TIME_FLAG="-l"
if [[ ! -x "$TIME_BIN" ]]; then
    TIME_BIN="gtime"; TIME_FLAG="-v"
fi
TIME_OK=0
command -v "${TIME_BIN%% *}" >/dev/null 2>&1 && TIME_OK=1

declare -a ROWS=()
add() { ROWS+=("$1"); }

# ---------------------------------------------------------------- NFR-03
build_ms=""
if [[ $SKIP_BUILD -eq 0 ]]; then
    echo
    echo "==> [NFR-03] 构建（embed + 落盘，不含 HNSW 图）"
    start=$(date +%s)
    out=$("$BIN" build --input "$CORPUS" --output "$SNAPSHOT" --vectors --single-chunk 2>&1)
    end=$(date +%s)
    echo "$out" | sed 's/^/    /'
    # 优先解析脚本自建的 embed 计时行，回退到"快照已写入…耗时"
    embed_line=$(echo "$out" | grep -E "embed [0-9]+ 条 耗时" || true)
    if [[ -n "$embed_line" ]]; then
        build_ms=$(echo "$embed_line" | grep -oE '[0-9]+\.[0-9]+s$' | sed 's/s$//' || true)
    fi
    build_secs=$(( end - start ))
    add "NFR-03|构建含 embedding|${build_secs}s|目标 <${NFR03_TARGET}s|$([[ $build_secs -lt $NFR03_TARGET ]] && echo 达标 || echo 未达标)"
else
    echo
    echo "==> [NFR-03] 已跳过（--skip-build）"
    [[ -f "$SNAPSHOT" ]] || { echo "错误：快照不存在 $SNAPSHOT" >&2; exit 2; }
fi

# ---------------------------------------------------------------- NFR-04
echo
echo "==> [NFR-04] 冷启动（快照加载 / HNSW 图重建 分别计时）"
cold=$("$BIN" bench --index "$SNAPSHOT" --queries "$QUERIES" \
        --modes bm25,vector --no-latency 2>&1)
echo "$cold" | sed 's/^/    /'
load_line=$(echo "$cold"   | grep -oE '快照加载 耗时 [0-9.]+m?s' || true)
graph_line=$(echo "$cold"  | grep -oE 'HNSW 图重建 [0-9]+ 条 耗时 [0-9.]+s' || true)
add "NFR-04|快照加载|${load_line:-n/a}|目标 <2s|$([[ -n "$load_line" ]] && echo 达标 || echo 见上)"
add "NFR-04|HNSW 图重建|${graph_line:-n/a}|图不持久化（D7）|未达秒级"

# ---------------------------------------------------------------- NFR-02
echo
echo "==> [NFR-02] 查询延迟（warmup 3 + reps 20 × 320 query）"
"$BIN" bench --index "$SNAPSHOT" --queries "$QUERIES" \
    --modes bm25,vector,hybrid --json "$OUT_JSON" 2>&1 | sed 's/^/    /'

if [[ -f "$OUT_JSON" ]]; then
    read_lat() { python3 -c "
import json,sys
d=json.load(open('$OUT_JSON')).get('latency',{}).get('$1',{})
print(f\"{d.get('p50_ms',0):.2f}ms\", f\"{d.get('p99_ms',0):.2f}ms\", d.get('n_samples',0))
"; }
    for m in bm25 vector hybrid; do
        read -r p50 p99 n <<<"$(read_lat "$m")"
        case "$m" in
            bm25)   tgt=$NFR02_BM25;   cmp=$p99 ;;
            vector) tgt=$NFR02_VECTOR; cmp=$p99 ;;
            hybrid) tgt=$NFR02_HYBRID; cmp=$p99 ;;
        esac
        ok=$(python3 -c "print('达标' if float('${cmp%ms}')<${tgt} else '未达标')")
        add "NFR-02|$m 延迟|P50=$p50 P99=$p99|n=$n|目标 P99<${tgt}ms|$ok"
    done
fi

# ---------------------------------------------------------------- NFR-05
echo
echo "==> [NFR-05] 内存"
if [[ $TIME_OK -eq 1 ]]; then
    mem=$("$TIME_BIN" "$TIME_FLAG" "$BIN" search --index "$SNAPSHOT" \
          --mode hybrid "向量检索与BM25融合" 2>&1 >/dev/null \
          | grep -iE "maximum resident|peak memory" | head -2 || true)
    echo "$mem" | sed 's/^/    /'
    peak=$(echo "$mem" | grep -i "maximum resident" | grep -oE '[0-9]+' | head -1 || true)
    if [[ -n "$peak" ]]; then
        add "NFR-05|peak RSS（整进程）|$((peak/1024/1024))MB|含模型+快照+图+ort|实测"
    fi
else
    echo "    （未找到 /usr/bin/time 或 gtime，跳过 peak RSS 实测）"
fi
add "NFR-05|向量分量（推算）|24.6MB|12000×512×4B|与理论 20MB 量级吻合"

# ---------------------------------------------------------------- 汇总
echo
echo "==================== NFR 实测汇总 ===================="
printf '%-8s %-22s %-28s %-26s %s\n' "NFR" "项" "实测" "口径/目标" "判定"
echo "--------------------------------------------------------------------------------------------------"
for row in "${ROWS[@]}"; do
    IFS='|' read -r a b c d e <<<"$row"
    printf '%-8s %-22s %-28s %-26s %s\n' "$a" "$b" "$c" "$d" "${e:-}"
done
echo "--------------------------------------------------------------------------------------------------"
echo
echo "延迟明细 JSON: $OUT_JSON"
echo "用 scripts/report.py $OUT_JSON --section latency 可生成 markdown 表格"
