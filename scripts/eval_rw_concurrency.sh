#!/usr/bin/env bash
#
# V2 Step 8 · S8-09 —— **读写并发标定**一键跑（`S8-T11` / `S8-T12` / `S8-T15`）
#
#   ① **S8-T11 合并期读延迟**（NFR-14 ②a·②b）：**②a 相对主判据** ——
#      「合并进行中」的 `took` P99 ÷ 「静止期」的 `took` P99 **≤ 1.2（拟）**；
#      **②b 绝对护栏** —— 合并期 P99 仍在 **NFR-02** 的该模式预算内。
#   ② **S8-T12 热路径四档**（量化架构 **R52**）：无 delta / 有 delta 无墓碑 /
#      有跨段墓碑 / 墓碑已 `merge_all()` 四档的 **BM25 lane** 耗时；
#      **第 4 档必须回到第 1 档量级**（本脚本取「≤ 1.2×」作可判定的读数口径）。
#   ③ **S8-T15 NFR-11 写延迟**：`commit()` 的**端到端 wall time** P50 / P99
#      （**真实** bge-small-zh-v1.5；拟值 `P50 ≤ 1.0s / P99 ≤ 2.0s`）。
#
# # 口径与边界（读数字之前先读这段）
#
# - 载体 = `crates/core/examples/eval_rw_concurrency.rs`（逐条读数带 `RWC ` 前缀）。
# - **`latency` / `hotpath` 用合成 embedder（LCG, dim=512）**：判据是**比值**，而查询侧编码是
#   两臂都要付的**恒定项** ⇒ 带上它会把比值**稀释**（偏松）。去掉它得到的是**更严**的判据，
#   且与 `NFR-10` ② 收窄后的适用范围（不含查询侧编码的读路径）口径一致。
#   ⚠️ 代价：本档**不**覆盖「真实模型下查询编码与合并的交互」（写侧由 `S8-T15` 覆盖）。
# - **A/B 纪律**：同一进程内 **A1（静止期，= B 的起始布局）→ B（合并期）→ A2（合并后）**，
#   全程共用**同一份内存图** ⇒ 「同一张冻结图」是字面成立的（跨进程做不到：`save` 前必须
#   `merge_all`（`D-S8-01`）⇒ 快照里没有 delta，重建的 delta 图带 `OsRng` 漂移）。
#   A1↔A2 的差给出**漂移带**；主判据用 A1（与 B 的起始布局逐位相同）。
# - **B 的样本 = 合并窗口内**（读端跑到写端置 `stop` 为止，再按序号切片）—— 若读端跑满固定条数，
#   窗口外的静止期样本会**稀释** B 的分位数 ⇒ 判据偏松、可能放过真实尖刺。脚本同时报 B2（全量）对照。
# - **②b 的 NFR-02 预算是「十万级」语料下标定的**，本档是 2000 篇 ⇒ **两者不是同一协议**，
#   ②b 只作**数量级护栏**（差一个量级才是信号），⚠️ **不得**据此声称 NFR-02 达标 / 不达标。
# - 逐档位**独立进程**（同 `eval_s8s2.sh` 的理由：同进程多档会互相污染读数）。
#
# # 用法
#
#   ./scripts/eval_rw_concurrency.sh                  # 默认 MAIN=2000 / DELTAS=8 / Q=300 / TOMB=20
#   MAIN=4000 DELTAS=16 ./scripts/eval_rw_concurrency.sh
#   SKIP_WRITE=1 ./scripts/eval_rw_concurrency.sh     # 跳过 S8-T15（需要真实模型）
#
# # 产物
#
# - 全部落 **${RUN_DIR}**（`mktemp -d`）：`logs/`（每档位 stdout）+ `readings.tsv`。
# - 末尾打印**合取表**与初判 —— ⚠️ 定稿口径（达标则去「拟」定稿 / 不达标则收窄 + 另立条目 +
#   保持打开 + 写明复审触发条件）由设计 §4.13.1 / §4.13.2 记录，本脚本只出读数与初判。
#
# 依赖：bash / python3 / 本地模型缓存（仅 `S8-T15`）。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

MAIN="${MAIN:-2000}"
DELTA="${DELTA:-200}"
DELTAS="${DELTAS:-8}"
Q="${Q:-300}"
TOMB="${TOMB:-20}"
BATCHES="${BATCHES:-40}"
BATCH_SIZE="${BATCH_SIZE:-64}"
SKIP_WRITE="${SKIP_WRITE:-0}"

CORPUS="${CORPUS:-data/t2-corpus.jsonl}"
QUERIES_FILE="${QUERIES_FILE:-data/t2-queries.jsonl}"

RUN_DIR="$(mktemp -d)"
LOGS="$RUN_DIR/logs"
mkdir -p "$LOGS"

echo "=== V2 Step 8 收尾标定（S8-T11 / S8-T12 / S8-T15）==="
echo "核数 = $( (sysctl -n hw.ncpu 2>/dev/null) || nproc 2>/dev/null || echo '?' )"
echo "档位：main=${MAIN} delta=${DELTA} deltas=${DELTAS} q=${Q} tombstones=${TOMB}"
echo "写延迟：batches=${BATCHES} batch_size=${BATCH_SIZE}（local-embed）"
echo "产物目录：${RUN_DIR}"
echo

echo "── 构建（release，读数只在 release 下有意义）──"
cargo build --release --quiet --example eval_rw_concurrency

EXE="target/release/examples/eval_rw_concurrency"

# ── 跑一档；$1 = 档位名，$2 = mode（位置参数），其余为透传参数（环境变量已在外面设）──
declare -a NAMES=()
run_arm() {
  local name="$1"; shift
  local out="$LOGS/${name}.out"
  echo "--- ${name} ---"
  if ! "$EXE" "$@" > "${out}" 2>&1; then
    echo "❌ 档位 ${name} 失败：" >&2
    tail -8 "${out}" >&2 || true
    exit 1
  fi
  grep -E "^RWC " "${out}" | sed 's/^/    /' || true
  NAMES+=("${name}")
}

# ⚠️ 参数一律走环境变量（`RWC_*`）：避免"参数写错跑成空进程"的形态（Step 7 踩过）。
export RWC_MAIN="${MAIN}" RWC_DELTA="${DELTA}" RWC_DELTAS="${DELTAS}" RWC_Q="${Q}" RWC_TOMB="${TOMB}"

echo
echo "── S8-T11：合并期读延迟（②a 相对 + ②b 绝对护栏）──"
run_arm lat-bm25 latency --search-mode bm25
run_arm lat-hybrid latency --search-mode hybrid

echo
echo "── S8-T12：热路径四档（BM25 lane）──"
run_arm hotpath hotpath

if [ "${SKIP_WRITE}" != "1" ]; then
  echo
  echo "── S8-T15：commit() 写延迟（真实本地 embedder）──"
  export RWC_BATCHES="${BATCHES}" RWC_BATCH_SIZE="${BATCH_SIZE}"
  run_arm write-latency write-latency
fi

# ── 读数落 TSV ──
TSV="${RUN_DIR}/readings.tsv"
: > "${TSV}"
printf 'arm\tkind\tp50_us\tp99_us\tn\textra\n' >> "${TSV}"
python3 - "${LOGS}" "${TSV}" "${NAMES[@]}" <<'PY'
import io, os, re, sys

logs, tsv = sys.argv[1], sys.argv[2]
names = sys.argv[3:]
rows = []
for name in names:
    txt = io.open(os.path.join(logs, name + ".out"), encoding="utf-8").read()
    for line in txt.splitlines():
        if not line.startswith("RWC arm="):
            continue
        m = re.match(r"RWC arm=(\S+) n=(\d+) mean_us=(\d+) p50_us=(\d+) p99_us=(\d+) max_us=(\d+)", line)
        if m:
            rows.append((name, m.group(1), m.group(4), m.group(5), m.group(2),
                         f"mean_us={m.group(3)} max_us={m.group(6)}"))
    for line in txt.splitlines():
        if line.startswith("RWC hot "):
            m = re.search(r"stage=(\d) desc=(\S+) p50_us=(\d+) p99_us=(\d+) tombstones=(\d+) segments=(\d+)", line)
            if m:
                rows.append((name, "stage" + m.group(1), m.group(3), m.group(4), "",
                             f"{m.group(2)} tombstones={m.group(5)} segments={m.group(6)}"))
with io.open(tsv, "a", encoding="utf-8") as f:
    for r in rows:
        f.write("\t".join(r) + "\n")
print(f"读数行数 = {len(rows)}")
PY

echo
echo "=== 读数（${TSV}）==="
cat "${TSV}"

# ── 合取表 ──
echo
python3 - "${LOGS}" "${TSV}" "${SKIP_WRITE}" <<'PY'
import io, os, re, sys

logs, tsv, skip_write = sys.argv[1], sys.argv[2], sys.argv[3]
txt = io.open(tsv, encoding="utf-8").read()

def grab(arm, kind):
    for line in txt.splitlines():
        f = line.split("\t")
        if f[0] == arm and f[1] == kind:
            return f
    return None

def num(s, default=None):
    try:
        return float(s)
    except Exception:
        return default

def mark(ok):
    return "✅ PASS" if ok else "❌ FAIL"

print("=== 合取表（设计 §4.13.1 / §4.13.2）===")
ratios = {}
for arm, budget_ms in (("lat-bm25", 5.0), ("lat-hybrid", 20.0)):
    log = io.open(os.path.join(logs, arm + ".out"), encoding="utf-8").read().replace("\n", " ")
    m = re.search(r"p99_b1_over_a1=([0-9.]+) p99_b2_over_a1=([0-9.]+) p99_b1_over_a2=([0-9.]+) "
                  r"a1_p99_us=(\d+) a2_p99_us=(\d+) b1_p99_us=(\d+)", log)
    if not m:
        print(f"②a {arm}: ⚠️ 读数缺失")
        continue
    r_a1, r_b2, r_a2, a1, a2, b1 = (float(m.group(1)), float(m.group(2)), float(m.group(3)),
                                    int(m.group(4)), int(m.group(5)), int(m.group(6)))
    ratios[arm] = r_a1
    # 主判据用 A1 基线（= B 的**起始布局**，逐位相同）；另两个是稳健性对照。
    print(f"②a {arm:10s} 合并期/静止期 P99 = {r_a1:.4f}（**A1 基线**，主判据）  {mark(r_a1 <= 1.2)}"
          f"   [A1={a1}us A2={a2}us B1={b1}us]")
    print(f"    ⓘ 稳健性对照：B2（全量样本）/A1 = {r_b2:.4f}；B1/A2（合并后基线）= {r_a2:.4f}"
          f"  ⇒ 三者取最保守的 {max(r_a1, r_b2, r_a2):.4f}")
    print(f"②b {arm:10s} 合并期 P99 = {b1/1000:.2f} ms vs NFR-02 该模式预算 {budget_ms} ms"
          f"  {mark(b1/1000 <= budget_ms)}   ⚠️ 数量级护栏（语料规模与 NFR-02 的原协议不同）")

hot = {}
for line in txt.splitlines():
    f = line.split("\t")
    if f[0] == "hotpath" and f[1].startswith("stage"):
        hot[f[1]] = (num(f[2]), num(f[3]))
if "stage1" in hot and "stage4" in hot:
    r = hot["stage4"][0] / max(hot["stage1"][0], 1)
    print(f"① S8-T12 档4/档1 的 BM25 lane P50 比 = {r:.4f}  {mark(r <= 1.2)}"
          f"   [档1={hot['stage1'][0]:.0f}us 档2={hot.get('stage2',(0,0))[0]:.0f}us"
          f" 档3={hot.get('stage3',(0,0))[0]:.0f}us 档4={hot['stage4'][0]:.0f}us]")
    print(f"   （档3 − 档1 = {hot.get('stage3',(0,0))[0] - hot['stage1'][0]:+.0f}us = R52 的量化读数）")

if skip_write != "1":
    log = io.open(os.path.join(logs, "write-latency.out"), encoding="utf-8").read()
    m = re.search(r"p50_s=([0-9.]+) p99_s=([0-9.]+) n=(\d+)", log)
    if m:
        p50, p99, n = float(m.group(1)), float(m.group(2)), int(m.group(3))
        print(f"② NFR-11 写延迟 P50 = {p50:.3f}s {mark(p50 <= 1.0)} / P99 = {p99:.3f}s "
              f"{mark(p99 <= 2.0)}  (n={n})")
    else:
        print("② NFR-11 写延迟: ⚠️ 读数缺失")
else:
    print("② NFR-11 写延迟：SKIP_WRITE=1 ⇒ 本轮未测")
PY

echo
echo "⚠️ 定稿口径 = 「达标则去『拟』定稿；不达标则收窄 + 另立条目 + 保持打开 + 写明复审触发条件」，"
echo "   由设计 §4.13.1 / §4.13.2 与 eval-report §8.15 记录；本脚本只出读数与初判。"
echo "产物目录：${RUN_DIR}"
