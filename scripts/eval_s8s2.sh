#!/usr/bin/env bash
#
# V2 Step 8 · S8-08 / S8-T16 —— **spike S8-S2** 一键跑（决策门 = 设计 §4.11.3 的三条合取判据）
#
#   判据① **数值一致性**（`Q3′` 的前置）：同 query、单会话 vs 池化 ⇒ 向量**逐位相同**。
#          不满足 ⇒ **直接判「不投」**，后两条不必测。
#   判据② **吞吐增益**：`QPS(4)/QPS(1)` 的 **vector 路 ≥ 2.5×**（沿用 NFR-10 的既有判据）。
#   判据③ **峰值 RSS 增量**：**≤ 20%**（相对 `embed_sessions = 1` 臂；查询侧口径 = batch 1 × 短 query）。
#
# ⚠️ **三条全过才投**（合取，同 D-S6-01 的先例）。任一条不过就如实记「未达标」，
#    `NFR-10` ③ 与 `R43` **保持「打开」** —— 这是**完全可合并**的结论（Step 6 的 E2 是先例）。
#
# # 为什么逐档位**独立进程**
#
# 判据③要的是「查询侧峰值 RSS」。同一进程内跑多个档位会让**前一个档位的内存**留在进程里
# ⇒ 后一个档位的读数无意义（Step 6 已实测该形态：`1+2+4` 个 session 同驻约 22 GB，换页污染全部数字）。
# ⇒ 一个档位一个进程，RSS 由**外置** `/usr/bin/time -l`（macOS）或 `gtime -v`（Linux）采。
#
# # 为什么 search 臂要读**冻结图**
#
# `QPS(4)/QPS(1)` 只允许**一个**变量（并发度）。`hnsw_rs` 的拓扑用无种子 `OsRng`
# ⇒ 每臂自建索引等于每臂换一张图（`R51` / Step 7 的「图漂移」教训）⇒ 先把图落盘、
# 四臂 `--index` 读**同一份**内容（`S8-S1` 的 `A/B` 纪律同款）。
#
# # 用法
#
#   ./scripts/eval_s8s2.sh                    # 默认 QUERIES=300
#   QUERIES=600 ./scripts/eval_s8s2.sh        # 加大样本（注意：时间线性增长）
#   DOCS=4000 ./scripts/eval_s8s2.sh          # 冻结图规模
#
# # 产物
#
# - 全部落 **$RUN_DIR**（`mktemp -d`）：`logs/`（每档位的 stdout / stderr / time 输出）+ `index/`（冻结图）
#   + `readings.tsv`（解析后的读数）。**残留文件不会被当成新数据**。
# - 末尾打印**合取表**并给出「投 / 不投」的初判 —— ⚠️ 定稿口径是「达标则定稿、不达标则
#   收窄 + 保持打开」，由设计文档 §4.11.4 记录（本脚本只出读数与初判）。
#
# 依赖：bash / python3 / BSD `/usr/bin/time -l`（macOS）或 GNU `gtime -v`（Linux）。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

QUERIES="${QUERIES:-300}"
DOCS="${DOCS:-2000}"
SESSIONS_ARM="${SESSIONS_ARM:-4}"
THREADS_HI="${THREADS_HI:-4}"

RUN_DIR="$(mktemp -d)"
LOGS="$RUN_DIR/logs"
mkdir -p "$LOGS"
INDEX="$RUN_DIR/index/s8s2.idx"
mkdir -p "$(dirname "$INDEX")"

echo "=== spike S8-S2（S8-08 / S8-T16）：会话池的投 / 不投 ==="
echo "核数 = $( (sysctl -n hw.ncpu 2>/dev/null) || nproc 2>/dev/null || echo '?' )"
echo "档位：queries=$QUERIES docs=$DOCS sessions_arm=$SESSIONS_ARM threads={1,$THREADS_HI}"
echo "产物目录：$RUN_DIR"
echo

# ── 时间与 RSS 采集器（BSD / GNU 两套） ──
if /usr/bin/time -l true >/dev/null 2>&1; then
  TIME_KIND="bsd"
  time_wrap() { /usr/bin/time -l "$@"; }
elif command -v gtime >/dev/null 2>&1 && gtime -v true >/dev/null 2>&1; then
  TIME_KIND="gnu"
  time_wrap() { gtime -v "$@"; }
else
  echo "❌ 需要 BSD /usr/bin/time -l 或 GNU gtime -v 来采峰值 RSS" >&2
  exit 2
fi
echo "RSS 采集器：$TIME_KIND"

# 从 time 输出里取峰值 RSS（**统一换算成 KiB**）。
#
# ⚠️ **单位陷阱（实测踩到）**：BSD `/usr/bin/time -l` 的 `maximum resident set size` 是**字节**，
# GNU `gtime -v` 的 `Maximum resident set size (kbytes)` 才是 KiB。直接当同一个量用会让
# 「增量 ≤ 20%」这类**比值**判据照样成立、但**报出来的绝对值差 1024 倍** ⇒ 报告不可读。
# ⇒ 这里显式换算，并在 TSV 头写明口径。
peak_rss_kib() {
  local f="$1"
  if [ "$TIME_KIND" = "bsd" ]; then
    awk '/maximum resident set size/{printf "%d\n", $1 / 1024; exit}' "$f"
  else
    awk '/Maximum resident set size \(kbytes\)/{print $NF; exit}' "$f"
  fi
}

# ── 跑一个档位；$1=名字 $2..=参数 ──
declare -a NAMES=()
declare -a QPS=()
declare -a RSS=()
run_arm() {
  local name="$1"; shift
  local out="$LOGS/$name.out" err="$LOGS/$name.time"
  echo "--- $name ---"
  time_wrap cargo run --release --quiet --example spike_s8s2 -- "$@" \
      > "$out" 2> "$err" || {
        echo "❌ 档位 $name 失败：" >&2
        tail -5 "$out" >&2 || true
        tail -5 "$err" >&2 || true
        exit 1
      }
  local qps rss
  qps="$(grep -oE 'qps=[0-9.]+' "$out" | head -1 | cut -d= -f2 || true)"
  rss="$(peak_rss_kib "$err" || true)"
  echo "    $(grep -E '^SPIKE_S8S2 ' "$out" | head -1)"
  echo "    peak RSS = ${rss:-?} KiB"
  NAMES+=("$name"); QPS+=("${qps:-}"); RSS+=("${rss:-}")
}

# ── 判据①：数值一致性（不测时间） ──
echo
run_arm consistency consistency

# ── 冻结图（A/B 纪律） ──
echo
run_arm prepare prepare "$DOCS" "$INDEX"

# ── 判据③ 的 RSS 臂（查询侧口径：batch 1 × 短 query，**不含建库**） ──
echo
echo "── RSS / encode 臂（batch 1 × 短 query）──"
run_arm "query-s1-t1" query 1 1 "$QUERIES"
run_arm "query-s1-t$THREADS_HI" query 1 "$THREADS_HI" "$QUERIES"
run_arm "query-s$SESSIONS_ARM-t1" query "$SESSIONS_ARM" 1 "$QUERIES"
run_arm "query-s$SESSIONS_ARM-t$THREADS_HI" query "$SESSIONS_ARM" "$THREADS_HI" "$QUERIES"

# ── 判据② 的 vector 路臂（同一张冻结图） ──
echo
echo "── vector 路 QPS 臂（冻结图 = 共享变量已钉住）──"
run_arm "search-s1-t1" search 1 1 "$QUERIES" "$INDEX"
run_arm "search-s1-t$THREADS_HI" search 1 "$THREADS_HI" "$QUERIES" "$INDEX"
run_arm "search-s$SESSIONS_ARM-t1" search "$SESSIONS_ARM" 1 "$QUERIES" "$INDEX"
run_arm "search-s$SESSIONS_ARM-t$THREADS_HI" search "$SESSIONS_ARM" "$THREADS_HI" "$QUERIES" "$INDEX"

# ── 读数落 TSV + 合取表 ──
TSV="$RUN_DIR/readings.tsv"
: > "$TSV"
printf 'arm\tqps\trss_kib\n' >> "$TSV"
for i in "${!NAMES[@]}"; do
  printf '%s\t%s\t%s\n' "${NAMES[$i]}" "${QPS[$i]}" "${RSS[$i]}" >> "$TSV"
done

echo
echo "=== 读数（${TSV}）==="
column -t -s $'\t' "$TSV" 2>/dev/null || cat "$TSV"

python3 - "$TSV" "$THREADS_HI" "$SESSIONS_ARM" "$LOGS" <<'PY'
import io, re, sys

tsv, hi, sm, logs = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
rows = {}
for line in io.open(tsv, encoding="utf-8").read().splitlines()[1:]:
    if not line.strip():
        continue
    a, q, r = (line.split("\t") + ["", ""])[:3]
    try:
        rows[a] = (float(q) if q else None, int(r) if r else None)
    except ValueError:
        rows[a] = (None, None)

def qps(arm):
    v = rows.get(arm, (None, None))[0]
    return v

def rss(arm):
    v = rows.get(arm, (None, None))[1]
    return v

# ── 判据① ──
cons = io.open(f"{logs}/consistency.out", encoding="utf-8").read()
c1 = "pass=true" in cons
intra = "intra_threads_same=true" in cons
slot = "slot_same=true" in cons
sess_q = "sessions_same_query=true" in cons
sess_d = "sessions_same_docs=true" in cons

# ── 判据②：pooled 臂的 QPS(4)/QPS(1) ──
pool_1, pool_hi = qps(f"search-s{sm}-t1"), qps(f"search-s{sm}-t{hi}")
base_1, base_hi = qps(f"search-s1-t1"), qps(f"search-s1-t{hi}")
r_pool = (pool_hi / pool_1) if (pool_1 and pool_hi) else None
r_base = (base_hi / base_1) if (base_1 and base_hi) else None
c2 = (r_pool is not None) and (r_pool >= 2.5)

# ── 判据③：RSS 增量（查询侧口径） ──
r_1, r_sm = rss("query-s1-t1"), rss(f"query-s{sm}-t1")
inc = ((r_sm - r_1) / r_1 * 100.0) if (r_1 and r_sm) else None
c3 = (inc is not None) and (inc <= 20.0)

def mark(b):
    return "✅ PASS" if b else "❌ FAIL"

print()
print("=== 合取表（设计 §4.11.3）===")
print(f"判据① 数值一致性（生产路径：同池轮转 4 槽 / sessions 1vs{sm}）  {mark(c1)}"
      f"   [1a 槽同构={slot} 1b query侧={sess_q} 入库侧={sess_d}]")
print(f"    ⓘ ①c（**非生产路径**对照）intra_threads 是否改变数值：{intra}"
      f"（{'不变 ⇒ 池内仍可按需分化' if intra else '变 ⇒ 槽必须同构（本实现已如此）'}）")
if r_pool is None:
    print(f"判据② QPS({hi})/QPS(1) vector 路 ≥ 2.5×                ⚠️ 读数缺失")
else:
    print(f"判据② QPS({hi})/QPS(1) vector 路 ≥ 2.5×                {mark(c2)}"
          f"   实测 pooled(sessions={sm}) = {r_pool:.2f}×"
          f"；对照 未池化(sessions=1) = {r_base:.2f}×" if r_base else "")
if inc is None:
    print("判据③ 峰值 RSS 增量 ≤ 20%（查询侧口径）           ⚠️ 读数缺失")
else:
    print(f"判据③ 峰值 RSS 增量 ≤ 20%（查询侧口径）           {mark(c3)}"
          f"   实测 sessions=1: {r_1} KiB → sessions={sm}: {r_sm} KiB ⇒ {inc:+.1f}%")

overall = c1 and c2 and c3
print()
print(f"⇒ **三条全过（合取）**：{'✅ 可判「投」' if overall else '❌ 判「不投」'}")
if not c1:
    print("   ⚠️ 判据①不过 ⇒ 按设计 §4.11.2 **直接判「不投」**，后两条不必测（池化会破坏 NFR-10 ① 的前置）")
print()
print("⚠️ 定稿口径（2026-09-21 用户拍板）：**达标则去「拟」定稿；不达标则判据适用范围收窄 +")
print("   另立条目如实记未达标 + 整体保持「打开」+ 写明复审触发条件**（三条同时写）。")
print("   本脚本只出读数与初判，定稿落设计 §4.11.4 与四处定义面。")
PY

echo
echo "产物目录：$RUN_DIR"
