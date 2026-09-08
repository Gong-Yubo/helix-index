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
[[ -f "$CORPUS" ]] || { echo "错误：语料缺失 $CORPUS" >&2; exit 2; }

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
    # 提取判定行与关键量
    jline=$(echo "$out" | grep -E '^J1_|^VERDICT' | tr '\n' ' ')
    compact_row=$(grep '^compact,' "$csv" 2>/dev/null || true)
    # compact_row 列：compact,snap,grap,data,raw,nb,coldstart,status
    local c_snap c_g c_raw c_nb c_stat
    c_snap=$(echo "$compact_row" | cut -d, -f2)
    c_g=$(echo   "$compact_row" | cut -d, -f3)
    c_raw=$(echo "$compact_row" | cut -d, -f5)
    c_nb=$(echo  "$compact_row" | cut -d, -f6)
    c_stat=$(echo "$compact_row" | cut -d, -f8)
    verdict=$(echo "$out" | grep -E '^VERDICT' | awk '{print $2}')
    add "${tag}|snapshot=${c_snap}B|graph=${c_g}B|raw=${c_raw}|nb=${c_nb}|status=${c_stat}|${jline}|${verdict}"
}

if [[ -n "$CHURN" ]]; then
    run_one "$SIZE" "$CHURN" "$ROUNDS" "s${SIZE}-c${CHURN}"
else
    run_one "$SIZE" "0.1" "$ROUNDS" "s${SIZE}-c01"
    run_one "$SIZE" "0.3" "$ROUNDS" "s${SIZE}-c03"
fi

echo
echo "==================== churn 实测汇总（V2 Step 4 · §8.8） ===================="
printf '%-20s %-28s %-26s %s\n' "档位" "compact 后体积" "判定" "VERDICT"
echo "--------------------------------------------------------------------------------------"
for row in "${ROWS[@]}"; do
    IFS='|' read -r a b c d <<<"$row"
    printf '%-20s %-28s %-26s %s\n' "$a" "$b" "$d" "$c"
done
echo "--------------------------------------------------------------------------------------"
echo
echo "把上面的判定表（含 churn_bench 输出的 VERDICT 行）整理进 docs/devel/eval-report.md §8.8。"
