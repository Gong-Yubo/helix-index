#!/usr/bin/env bash
#
# 效果评测一键跑（V1-04）
#
# 固化 P5 定稿参数，保证"一键复现 P5 结论"——避免评测数字锁在某个人的
# 终端历史里（T5-08 数据诚信的工程化保障）。
#
# 用法：
#   ./scripts/eval_quality.sh                                   # 三路对照（默认）
#   ./scripts/eval_quality.sh --grid                            # BM25 16 格网格搜索
#   ./scripts/eval_quality.sh --modes bm25 --analyzer charabia   # 分词对照
#   ./scripts/eval_quality.sh --runs 3 --json out.json           # 抖动披露
#
# 说明：
#   - 默认直接吃**已入库**的 data/t2-corpus.jsonl / data/t2-queries.jsonl，
#     无需 3.5GB 原始数据；重装配走 t2_prep.rs 是可选离线路径。
#   - 脚本内部走 **release** 二进制：debug 构建下 bench 会因 clap 短参
#     断言 panic（历史 bug，已修；但 debug 下延迟数字本就无效）。
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

CORPUS="${CORPUS:-data/t2-corpus.jsonl}"
QUERIES="${QUERIES:-data/t2-queries.jsonl}"

# P5 定稿参数（eval-report.md 第 5/6 节）
DEFAULT_MODES="bm25,vector,hybrid"
DEFAULT_WEIGHTS="1,1.5"   # RRF 权重：向量侧加权（等权会被弱路 BM25 稀释）
DEFAULT_RUNS="1"

# 未显式给出 --modes / --rrf-weights / --runs 时，注入定稿默认值
EXTRA_ARGS=()
has_arg() {
    local want="$1"; shift
    for a in "$@"; do [[ "$a" == "$want" || "$a" == "$want"=* ]] && return 0; done
    return 1
}
if ! has_arg "--modes" "$@";        then EXTRA_ARGS+=(--modes "$DEFAULT_MODES"); fi
if ! has_arg "--rrf-weights" "$@";  then EXTRA_ARGS+=(--rrf-weights "$DEFAULT_WEIGHTS"); fi
if ! has_arg "--runs" "$@";         then EXTRA_ARGS+=(--runs "$DEFAULT_RUNS"); fi
if ! has_arg "--input" "$@" && ! has_arg "--index" "$@"; then
    EXTRA_ARGS+=(--input "$CORPUS")
fi
if ! has_arg "--queries" "$@" && ! has_arg "-q" "$@"; then
    EXTRA_ARGS+=(--queries "$QUERIES")
fi

for f in "$CORPUS" "$QUERIES"; do
    [[ -f "$f" ]] || { echo "错误：缺少数据文件 $f（应已随仓库入库）" >&2; exit 2; }
done

echo "==> 构建 release 二进制（helix）..."
cargo build --release -p helix

BIN="target/release/helix"
echo "==> 运行效果评测"
echo "    helix bench ${EXTRA_ARGS[*]} $*"
echo
exec "$BIN" bench "${EXTRA_ARGS[@]}" "$@"
