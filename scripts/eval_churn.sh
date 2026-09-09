#!/usr/bin/env bash
#
# churn（零散写入）workload 实测（V2 Step 4 · S4-09）
#
# 目的：把「三体积不无界增长」（issue #21 主验收）在可复现的零散写入 workload 下
# 实测并出判定表。调用 `churn_bench` example（内置确定性 SynthEmbedder，dim=512）：
#
#   - 每轮随机删 churn×N + 追加等量新 doc → save → 记录（快照/graph/data 字节、
#     raw_vectors 条数、图点数、GraphStatus）
#   - 末轮 compact_and_save → 再记录一次
#   - 输出 CSV + 判定行（J1 图回收 / J2 不累积 / J3 reload Loaded → VERDICT）
#
# 用法：
#   ./scripts/eval_churn.sh            # 默认：10K × churn {0.1,0.3} × 5 轮
#   SIZE=100000 ./scripts/eval_churn.sh  # 扩展档（合成 embedder，分钟级）
#   ROUNDS=3 CHURN=0.1 ./scripts/eval_churn.sh  # 单档覆盖
#
# 产物：各档 CSV 到 /tmp/helix-churn-*.csv；markdown 判定表打到 stdout，
#       实测数字由人工/脚本贴进 docs/devel/eval-report.md §8.8。
#
# ⚠️ 本 benchmark 只量「体积/回收」，不量相关性（合成向量无语义），故数字
#    只在体积回收上有意义（与设计 §4.8 口径一致）。跑 release 才有意义。
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

SIZE="${SIZE:-10000}"
ROUNDS="${ROUNDS:-5}"
CHURN="${CHURN:-}"
CORPUS="data/synth-10000-corpus.jsonl"
if [[ "$SIZE" -gt 10000 ]]; then
    CORPUS="data/synth-100000-corpus.jsonl"
fi

# ⚠️ 合成语料不入库（git 只跟踪 .gitkeep，见 `git ls-files data/`）。fresh clone 上
# 缺失时**自动生成**：`gen_synth_corpus.py` 确定性（同 seed/n 逐字节复现，S1-10），
# 结果与曾贴进 eval-report §8.8 的数字同源。生成 100K 档默认也产出 queries/filters
#（本脚本只用 corpus），确定性强、成本可控，故不额外只生成 corpus 的开关。
# 注：churn_bench 只取语料**前 SIZE 行**，故语料文件恒按档位生成 synth-10000/100000，
#   与 override 的 SIZE（如冒烟 400）无关。
if [[ ! -f "$CORPUS" ]]; then
    echo "==> 语料缺失 ${CORPUS}，自动调用确定性生成器（S1-10）..."
    GEN_N=10000; [[ "$SIZE" -gt 10000 ]] && GEN_N=100000
    if ! python3 scripts/gen_synth_corpus.py --n "$GEN_N" >/dev/null; then
        echo "错误：语料生成失败 ${CORPUS}（需 python3）" >&2
        exit 2
    fi
    [[ -f "$CORPUS" ]] || { echo "错误：生成后仍缺 ${CORPUS}" >&2; exit 2; }
fi

echo "==> 构建 release（churn_bench + helix）..."
cargo build --release -p helix-core --example churn_bench
EXAMPLE="target/release/examples/churn_bench"

# 判定表
declare -a ROWS=()
add() { ROWS+=("$1"); }

run_one() { # $1=size $2=churn $3=rounds $4=tag
    local size="$1" churn="$2" rounds="$3" tag="$4"
    local csv="/tmp/helix-churn-${tag}.csv"
    echo
    echo "==> churn_bench size=$size churn=${churn} rounds=${rounds} (tag=${tag})"
    out=$("$EXAMPLE" --corpus "$CORPUS" --size "$size" --rounds "$rounds" \
          --churn "$churn" --out "$csv" 2>&1)
    echo "$out" | sed 's/^/    /'
    # 末轮 compact 行：compact,snap,grap,data,raw,nb,coldstart,status
    compact_row=$(grep '^compact,' "$csv" 2>/dev/null || true)
    local c_snap c_g c_d c_nb c_stat
    c_snap=$(echo "$compact_row" | cut -d, -f2)
    c_g=$(echo   "$compact_row" | cut -d, -f3)
    c_d=$(echo   "$compact_row" | cut -d, -f4)
    c_nb=$(echo  "$compact_row" | cut -d, -f6)
    c_stat=$(echo "$compact_row" | cut -d, -f8)
    # 判定汇总行（churn_bench 的 J1/J2/J3 + VERDICT）
    verdict=$(echo "$out" | grep -E '^VERDICT' | awk '{print $2}')
    j1=$(echo "$out" | grep -oE 'J1_gc_reclaims_graph=(true|false)' | cut -d= -f2)
    j2=$(echo "$out" | grep -oE 'J2_no_growth=(true|false)' | cut -d= -f2)
    j3=$(echo "$out" | grep -oE 'J3_reload_loaded=(true|false)' | cut -d= -f2)
    # 汇总表只放 4 列（tab 分隔，避免 `|` 混进列内容再被当分隔符）：
    #   档位 | compact 后体积 | 判定 | VERDICT
    add "$(printf '%s\t%s\t%s\t%s' \
        "$tag" \
        "snap=$(hum $c_snap) graph=$(hum $c_g) data=$(hum $c_d) nb=$c_nb $c_stat" \
        "J1回收=$(tf $j1) J2不累积=$(tf $j2) J3Loaded=$(tf $j3)" \
        "$verdict")"
}

hum() { # 字节 → 人类可读（MB/KB/B）
    local b="${1:-0}"
    if (( b >= 1048576 )); then awk -v v="$b" 'BEGIN{printf "%.1fMB", v/1048576}'
    elif (( b >= 1024 )); then awk -v v="$b" 'BEGIN{printf "%.1fKB", v/1024}'
    else printf '%dB' "$b"; fi
}

tf() { # true→✓ false→✗
    [[ "${1:-}" == "true" ]] && printf '✓' || printf '✗'
}

if [[ -n "$CHURN" ]]; then
    run_one "$SIZE" "$CHURN" "$ROUNDS" "s${SIZE}-c${CHURN}"
else
    run_one "$SIZE" "0.1" "$ROUNDS" "s${SIZE}-c01"
    run_one "$SIZE" "0.3" "$ROUNDS" "s${SIZE}-c03"
fi

echo
echo "==================== churn 实测汇总（V2 Step 4 · §8.8） ===================="
printf '%-16s %-44s %-26s %s\n' "档位" "compact 后体积" "判定" "VERDICT"
echo "--------------------------------------------------------------------------------------"
for row in "${ROWS[@]}"; do
    IFS=$'\t' read -r a b c d <<<"$row"
    printf '%-16s %-44s %-26s %s\n' "$a" "$b" "$c" "$d"
done
echo "--------------------------------------------------------------------------------------"
echo
echo "各档 CSV 在 /tmp/helix-churn-*.csv（逐轮明细），把上面判定 + 明细整理进 docs/devel/eval-report.md §8.8。"

