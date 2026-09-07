#!/usr/bin/env bash
#
# download_t2ranking.sh —— 下载 T2Ranking 原始数据（P5 评测语料的上游）
#
# 用途：
#   data/t2-corpus.jsonl / data/t2-queries.jsonl 已随仓库分发，`helix bench` 开箱即用；
#   只有**需要重新装配子集**时（改抽样参数 / 上游数据集更新 / 校验可复现性）才跑本脚本。
#
# 用法：
#   ./data/download_t2ranking.sh                    # 下载全部三个文件（约 3.5GB）
#   ./data/download_t2ranking.sh queries.dev.tsv    # 只下载指定的文件
#   HF_ENDPOINT=https://hf-mirror.com ./data/download_t2ranking.sh   # 走镜像
#   SKIP_SHA=1 ./data/download_t2ranking.sh         # 跳过 sha256 校验（上游文件变更时）
#
# 产物：data/t2ranking/{collection.tsv,queries.dev.tsv,qrels.dev.tsv}
#   （该目录已在 .gitignore 中，原始数据不入库）
#
# 后续装配：
#   cargo run --release -p helix-core --example t2_prep -- \
#       --t2ranking data/t2ranking --out data
#
# 依据：docs/devel/eval-report.md §1（数据集与下载日期）、NOTICE §1（许可与引用义务）。
# 数据集许可 Apache-2.0，使用其转换产物须附来源与论文引用（见 NOTICE）。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${T2_DIR:-${SCRIPT_DIR}/t2ranking}"
ENDPOINT="${HF_ENDPOINT:-https://huggingface.co}"
BASE_URL="${ENDPOINT}/datasets/THUIR/T2Ranking/resolve/main/data"
SKIP_SHA="${SKIP_SHA:-0}"

# 清单：文件名 | 参考字节数（仅提示用，可能随上游重传变化） | sha256（2026-09-03 实测）
MANIFEST=(
  "queries.dev.tsv|939767|1df544dd04bf9b6d0de0dd77e0f3a84a0d74fc4bb9a1ff67b7306de8169135ba"
  "qrels.dev.tsv|6537957|a0356bd3c6d72c532ca17a4d88d7765554857f321346cf0f9cb4ad480738b25a"
  "collection.tsv|3659243528|07b84e543e9ba696124a727c00629d5bce586631648c436c98dd6e9b146da212"
)

# 选中的文件（命令行参数过滤，默认全选）
WANT=("$@")

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "错误：本机既无 sha256sum 也无 shasum，无法校验（可用 SKIP_SHA=1 跳过）" >&2
    return 1
  fi
}

human_size() {
  awk -v b="$1" 'BEGIN {
    split("B KB MB GB TB", u, " "); i = 1;
    while (b >= 1024 && i < 5) { b /= 1024; i++ }
    printf (i == 1) ? "%d %s" : "%.1f %s", b, u[i]
  }'
}

wanted() {
  [ ${#WANT[@]} -eq 0 ] && return 0
  local w
  for w in "${WANT[@]}"; do [[ "$w" == "$1" ]] && return 0; done
  return 1
}

mkdir -p "$OUT_DIR"
echo "==> 目标目录：$OUT_DIR"
echo "==> 数据源：$BASE_URL"
echo

for entry in "${MANIFEST[@]}"; do
  IFS='|' read -r name ref_bytes ref_sha <<<"$entry"
  wanted "$name" || continue

  dest="${OUT_DIR}/${name}"

  # 已存在且校验通过 → 跳过（支持断点续跑）
  if [[ -f "$dest" ]] && [[ "$SKIP_SHA" == "1" || "$(sha256_of "$dest")" == "$ref_sha" ]]; then
    printf '跳过 %-16s 已存在且校验通过（%s）\n' "$name" "$(human_size "$(wc -c <"$dest" | tr -d ' ')")"
    continue
  fi

  echo "下载 ${name}（约 $(human_size "$ref_bytes")）..."
  # -C - 续传；-f 服务端错误视为失败；--retry 抗抖动
  if command -v curl >/dev/null 2>&1; then
    curl -fL --retry 3 --retry-delay 2 --progress-bar -C - -o "${dest}.part" "${BASE_URL}/${name}"
  elif command -v wget >/dev/null 2>&1; then
    wget -c --show-progress -O "${dest}.part" "${BASE_URL}/${name}"
  else
    echo "错误：需要 curl 或 wget" >&2
    exit 1
  fi

  if [[ "$SKIP_SHA" == "1" ]]; then
    echo "     SKIP_SHA=1，跳过校验"
  else
    echo "     校验 sha256..."
    got="$(sha256_of "${dest}.part")"
    if [[ "$got" != "$ref_sha" ]]; then
      echo "错误：${name} 校验失败" >&2
      echo "  期望 $ref_sha" >&2
      echo "  实得 $got" >&2
      echo "  残留半成品：${dest}.part（可删除后重跑，或用 SKIP_SHA=1 接受上游变更并更新本脚本清单）" >&2
      exit 1
    fi
  fi

  mv "${dest}.part" "$dest"
  printf '完成 %-16s %s\n' "$name" "$(human_size "$(wc -c <"$dest" | tr -d ' ')")"
done

echo
echo "==> 全部就绪：$OUT_DIR"
echo "    下一步：cargo run --release -p helix-core --example t2_prep -- \\"
echo "              --t2ranking data/t2ranking --out data"
