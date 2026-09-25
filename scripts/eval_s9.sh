#!/usr/bin/env bash
#
# V2 Step 9 · S9-04 / S9-T11 —— **spike S9-S1** 一键跑（θ 灵敏度曲线 + 合取决策门 G1~G5）
#
# 判据与决策门全部来自 `docs/devel/v2-step9-design.md` §4.5（**已预注册**）：
#
#   G1  paraphrase 桶 `ΔMRR@10(1)`（自适应 − 固定权重）  **≥ +0.01**
#   G2  exact / natural / mixed 三桶**各自**的 ΔMRR        **≥ −0.0024**
#   G3  全量 320 条 ΔMRR                                   **≥ 0**
#   G4  G1∧G2∧G3 在 θ 的**一段区间**内同时成立（**≥ 3 个连续网格点** ⇒ 宽度 ≥ 0.10）
#   G5  结构条件：判据**只看名次**（MRR/NDCG 是 rank 的函数 ⇒ 对 `R46` 免疫）
#       + 精排**关** + **同一冻结图**（逐条自证，见下）
#
# ⚠️ **五门合取，缺一即判「不投」**（D-S9-08 / G4 是防过拟合的关键：**单点达标不算达标**）。
#    判「不投」是**完全可合并**的结论（Step 6 的 E1/E2/E3、Step 8 的 S8-S2 是先例）：
#    交付「工具 + 如实记录未达标」，`NFR-15` 保持「打开」并写明复审触发条件。
#
# # 协议（设计 §4.5，硬纪律）
#
#   ① 冻结图：所有臂 `--index <snap>`（**同一份**，指纹打印在 TSV 头）；`--runs 1`
#   ② `k = 10`；`candidate_k` 两侧相同（精排关 ⇒ `window = k` ⇒ `max(3k, 10) = 30`）
#   ③ baseline = `--rrf-weights 1.0,1.5`（= 今天的默认 = 「固定权重」臂）
#   ④ 逐 `(信号 × θ)` 跑自适应臂 ⇒ θ 灵敏度曲线（3 信号 × 21 点 = 63 臂）
#   ⑤ 配对：脚本按 `per_query` 的 `qid` **配对**，逐桶算 `ΔMRR@10(1)` + 4 桶 + 全局
#
# # 预注册的 θ 网格与「一个可解释的档」（脚本常量，判据可复核、不需人工解释）
#
#   θ ∈ {0.00, 0.05, …, 1.00}（21 点）；「档」= **≥ 3 个连续点**（宽度 ≥ 0.10）。
#   归一化：三个候选信号的值域都已归一到 `[0, 1]`（实现侧，见 `fusion/adaptive.rs`）。
#
# # G5 的三条自证（脚本内在执行，不是口头承诺）
#
#   ① 每个臂的 JSON `config.k == 10`、`config.rrf_k == 60`、`rerank.enabled == false`
#      ⇒ 「精排关 + k 固定」逐臂核过；
#   ② 所有臂读**同一份** `--index`（同一 `$INDEX` 变量 + 指纹写进 TSV 头）；
#   ③ 判据只消费 `per_query[].mrr`（名次函数）—— **不读 `score`**（`R46` 免疫）。
#
# # 内置自检（防「配对逻辑写错」这类静默失效）
#
#   `θ = 0.00` 臂 ⇒ 信号 `s < 0` **恒假** ⇒ 永不触发 ⇒ 与 baseline **逐桶 Δ == 0**。
#   脚本断言该零差（容差 `1e-12`）；不成立即说明配对/lane 对齐有问题，**直接退出**。
#   ⚠️ **每个信号各查一次**（第 1 轮评审正文 §三指出原稿只查第一个信号）。
#   它抓的是「配对逻辑错」——**抓不到**「某信号传参错」（那是 G5 的 `signal`/`θ` 断言，
#   见 `assert_g5`）；两者覆盖面**互补**，不要互相替代。
#
# # 用法
#
#   ./scripts/eval_s9.sh                                  # 默认冻结图 data/t2-frozen.snapshot
#   INDEX=data/other.snapshot ./scripts/eval_s9.sh        # 换冻结图（须自建）
#   SIGNALS="df" ./scripts/eval_s9.sh                     # 只跑一个信号（省时间）
#   OUT_DIR=data/eval/s9-2026-09-25 ./scripts/eval_s9.sh  # **把 readings.tsv 落一份持久产物**
#
# # 产物
#
#   落 `${RUN_DIR}`（缺省 `mktemp -d`，残留文件不会被当成新数据）：
#
#   json/anchors.json                  三路锚点（bm25 / vector / hybrid，S9-1b 的上界参照）
#   json/baseline.json                 固定权重臂
#   json/adaptive_<sig>_theta-<t>.json 63 个自适应臂
#   readings.tsv                       逐 (信号, θ, 桶) 的 ΔMRR + 判定（TSV 头含冻结图指纹）
#
#   ⚠️ **`readings.tsv` 是「读数要能指到产物」的唯一持久载体**（第 1 轮评审 **P3-3**：
#   原稿全靠 `mktemp -d`，临时目录一清，设计 §4.13.3 引用的「21 点全表」就**无处可指**）。
#   ⇒ 要长期留证就跑 `OUT_DIR=<dir>`（脚本把 TSV 复制到 `<dir>/readings.tsv`）；
#   本仓库已入库一份：`data/eval/s9-2026-09-25/readings.tsv`（见该目录 README）。
#
# 依赖：bash / python3 / 已构建的 `target/release/helix`（脚本会自动 `cargo build --release -p helix`）。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

INDEX="${INDEX:-data/t2-frozen.snapshot}"
QUERIES="${QUERIES:-data/t2-queries.jsonl}"
SIGNALS="${SIGNALS:-overlap df shape}"

# ── 预注册常量（设计 §4.5；**判据的严格度不许在出数后被事后解释**）──
K_GRID="10"
RRF_K="60"
BASE_WEIGHTS="1.0,1.5"
THETA_GRID="0.00 0.05 0.10 0.15 0.20 0.25 0.30 0.35 0.40 0.45 0.50 0.55 0.60 0.65 0.70 0.75 0.80 0.85 0.90 0.95 1.00"
G1_MIN="0.01"        # paraphrase ΔMRR ≥ +0.01
G2_MIN="-0.0024"     # 政策容差（交易余量）：exact/natural/mixed 各自 ΔMRR ≥ −0.0024
G3_MIN="0.0"         # 全量 ΔMRR ≥ 0
G4_MIN_POINTS="3"    # ≥ 3 连续网格点 ⇒ 宽度 ≥ 0.10

# 产物根：缺省 = 一次性临时目录。OUT_DIR 给定时**额外**把 readings.tsv 复制过去
# （第 1 轮评审 P3-3：读数要能指到**持久**产物 —— §4.13.3 引用的 TSV 曾无处可指）。
# 复制只在**全部臂跑完且判定成功后**，保证落盘的那份不是半成品。
RUN_DIR="$(mktemp -d)"
OUT_DIR="${OUT_DIR:-}"
JSON_DIR="$RUN_DIR/json"
mkdir -p "$JSON_DIR"
TSV="$RUN_DIR/readings.tsv"

BIN="target/release/helix"

echo "=== spike S9-S1（S9-04 / S9-05）：自适应融合的 投 / 不投 ==="
echo "冻结图 = $INDEX"
echo "查询集 = ${QUERIES}（协议：320 条、四桶各 80；**实际条数由脚本核实**）"
echo "信号 = ${SIGNALS}；θ 网格 = 21 点（0.00→1.00 步长 0.05）"
echo "基线 = --rrf-weights ${BASE_WEIGHTS}（P5 定稿 = 「固定权重」臂）"
echo "产物目录 = $RUN_DIR"
if [ -n "$OUT_DIR" ]; then
  echo "持久产物 = ${OUT_DIR}/readings.tsv（跑完后复制）"
fi
echo

if [ ! -f "$INDEX" ]; then
  echo "❌ 冻结图不存在：${INDEX}（先跑 helix build --vectors ... --index ${INDEX}）" >&2
  exit 2
fi
if [ ! -f "$QUERIES" ]; then
  echo "❌ 查询集不存在：$QUERIES" >&2
  exit 2
fi

# 冻结图指纹（G5 自证之一：所有臂读同一份图，且该事实**写进产物**）
if command -v shasum >/dev/null 2>&1; then
  SNAP_SHA="$(shasum -a 256 "$INDEX" | awk '{print substr($1, 1, 16)}')"
else
  SNAP_SHA="$(sha256sum "$INDEX" | awk '{print substr($1, 1, 16)}')"
fi
SNAP_BYTES="$(wc -c < "$INDEX" | tr -d ' ')"
echo "冻结图指纹（sha256 前 16）= ${SNAP_SHA}；字节数 = ${SNAP_BYTES}"
echo

echo "── 构建（release；判据臂必须 release，否则读数量级不可比）──"
cargo build --release -p helix 2>&1 | tail -1

run_arm() {
  # $1 = 输出名；其余 = bench 参数
  local name="$1"; shift
  echo "--- $name ---"
  "$BIN" bench \
    --index "$INDEX" \
    --queries "$QUERIES" \
    --k "$K_GRID" \
    --runs 1 \
    --rrf-k "$RRF_K" \
    --no-latency \
    --json "$JSON_DIR/$name.json" \
    "$@" > "$RUN_DIR/logs_$name.out" 2>&1 || {
      echo "❌ 臂 $name 失败：" >&2
      tail -8 "$RUN_DIR/logs_$name.out" >&2 || true
      exit 1
    }
  grep -E '^(hybrid|自适应融合)' "$RUN_DIR/logs_$name.out" | sed 's/^/    /' || true
}

echo
echo "── ① 三路锚点（S9-1b 的上界参照：BM25 弱路 / Vector 强路 / 固定权重 hybrid）──"
run_arm anchors --modes bm25,vector,hybrid --rrf-weights "$BASE_WEIGHTS"

echo
echo "── ② 固定权重基线臂（= 「今天的行为」）──"
run_arm baseline --modes hybrid --rrf-weights "$BASE_WEIGHTS"

echo
echo "── ③ θ 灵敏度曲线（3 信号 × 21 点，全部同一冻结图）──"
for sig in $SIGNALS; do
  for t in $THETA_GRID; do
    run_arm "adaptive_${sig}_theta-${t}" \
      --modes hybrid --rrf-weights "$BASE_WEIGHTS" \
      --adaptive-fusion on --adaptive-fusion-signal "$sig" --adaptive-fusion-theta "$t"
  done
done

echo
echo "── ④ 配对与判定（脚本侧；python）──"
python3 - "$JSON_DIR" "$TSV" "$SIGNALS" "$THETA_GRID" \
  "$G1_MIN" "$G2_MIN" "$G3_MIN" "$G4_MIN_POINTS" \
  "$SNAP_SHA" "$SNAP_BYTES" "$K_GRID" "$RRF_K" "$BASE_WEIGHTS" "$INDEX" "$QUERIES" <<'PY'
import io
import json
import sys

(
    json_dir,
    tsv_path,
    signals_s,
    thetas_s,
    g1_min,
    g2_min,
    g3_min,
    g4_min_points,
    snap_sha,
    snap_bytes,
    k_grid,
    rrf_k,
    base_weights,
    index_path,
    queries_path,
) = sys.argv[1:]

SIGNALS = signals_s.split()
THETAS = thetas_s.split()
BUCKETS = ["exact", "natural", "mixed", "paraphrase"]
G1, G2, G3 = float(g1_min), float(g2_min), float(g3_min)
G4_MIN = int(g4_min_points)


def load(name):
    with io.open(f"{json_dir}/{name}.json", encoding="utf-8") as fh:
        return json.load(fh)


def per_query_mrr(doc):
    """qid → MRR@10(1)。**只消费名次函数**（不读 score ⇒ R46 免疫，G5③）。"""
    m = doc["modes"]["hybrid"]
    return {p["qid"]: p["mrr"] for p in m["per_query"]}


def qtypes(doc):
    return {p["qid"]: p["type"] for p in doc["modes"]["hybrid"]["per_query"]}


# ── G5 逐臂自证：k / rrf_k / rel_threshold / 精排关 / **本臂的信号与 θ**（任一不符即退出）──
def assert_g5(name, doc, expect_adaptive, sig=None, t=None):
    """`sig` / `t` 给定时，额外核 JSON 里的 `adaptive_fusion.signal` / `adaptive_fusion.theta`。

    🔑 为什么必须核这两个字段（第 1 轮评审 **P3-2**）：JSON 里记它们**正是为了**
    「21 个 θ 文件各自自证跑的哪一档」（设计 §4.10 ⑥）。若传参出错（例如 63 个臂
    全用了同一个信号或同一个 θ），门会**静默评估同一条曲线三遍**，而 TSV 的 signal
    列照样写三个名字 ⇒ 错标不可发现。

    ⚠️ θ 在 JSON 里是 **f32 舍入后的十进制**（实测 `0.55` 读回 `0.550000011920929`）
    ⇒ 容差必须 ≥ `1e-6`；用 `1e-9` 会把**每个**臂都误判成 G5 失败（评审实测）。
    """
    cfg = doc["config"]
    bad = []
    if int(cfg["k"]) != int(k_grid):
        bad.append(f"k={cfg['k']}")
    if abs(float(cfg["rrf_k"]) - float(rrf_k)) > 1e-9:
        bad.append(f"rrf_k={cfg['rrf_k']}")
    if doc.get("rerank", {}).get("enabled") is not False:
        bad.append(f"rerank={doc.get('rerank')}")
    if int(cfg.get("rel_threshold", 1)) != 1:
        bad.append(f"rel_threshold={cfg.get('rel_threshold')}")
    af = cfg.get("adaptive_fusion", {})
    if expect_adaptive:
        if not af.get("enabled"):
            bad.append("adaptive_fusion 未开启")
        if sig is not None and af.get("signal") != sig:
            bad.append(f"signal={af.get('signal')}（本臂应为 {sig}）")
        if t is not None:
            got = af.get("theta")
            if got is None or abs(float(got) - float(t)) > 1e-6:
                bad.append(f"theta={got}（本臂应为 {t}；f32 读数 ⇒ 容差 1e-6）")
    elif af.get("enabled"):
        bad.append("baseline 不得开自适应")
    if bad:
        print(f"❌ G5 自证失败（{name}）：{'; '.join(bad)}")
        sys.exit(3)


anchors = load("anchors")
baseline = load("baseline")
assert_g5("baseline", baseline, expect_adaptive=False)
assert_g5("anchors", anchors, expect_adaptive=False)

base_mrr = per_query_mrr(baseline)
qtype = qtypes(baseline)
n_q = len(base_mrr)

# ── 桶结构核查（第 1 轮评审 **P4-2**）：把「四桶等分、无空桶」从**打印**升级成**判据** ──
#   🔴 空桶的真危害：下方 `delta[b] = 0.0`（空桶没有 Δ）⇒ `0.0 >= G2` **恒真**
#      ⇒ 该桶被门**无条件放行**（本 320 条数据集不会发生，属潜伏陷阱）。
#   🔴 不等分的危害：桶均值与 §4.5 的「四桶等分」协议**不可比**（S9-1b 的阈值同理）。
bucket_n = {b: sum(1 for q in qtype if qtype[q] == b) for b in BUCKETS}
if sum(bucket_n.values()) != n_q:
    print(
        f"❌ 桶条数之和 {sum(bucket_n.values())} != query 总数 {n_q}"
        f"（桶条数 {bucket_n}）⇒ 有 query 的桶标签不在 {BUCKETS} 内"
    )
    sys.exit(6)
if any(bucket_n[b] == 0 for b in BUCKETS):
    print(
        f"❌ 空桶（桶条数 {bucket_n}）⇒ 该桶 Δ 恒 0.0、G2 对它**恒真**（门被放行）"
        "⇒ 读数不可信"
    )
    sys.exit(7)
if len(set(bucket_n.values())) != 1:
    print(
        f"❌ 四桶条数不等（{bucket_n}）⇒ 与 §4.5 的四桶等分协议不可比，桶均值口径不成立"
    )
    sys.exit(8)
print(
    f"桶结构核查：四桶各 {bucket_n[BUCKETS[0]]} 条（等分 ✓，合计 {n_q} 条）；"
    "桶均值与 S9-1b 一律按**实际桶大小**计算"
)
print()

# ── 三路锚点（S9-1b 上界参照 + Q-R1 复现）──
print("=== 三路锚点（同一冻结图；MRR@10(1)）===")
print(f"{'mode':8} {'全局':>8} " + " ".join(f"{b:>10}" for b in BUCKETS))
anc_mrr = {}
for mode in ["bm25", "vector", "hybrid"]:
    m = anchors["modes"][mode]
    anc_mrr[mode] = {p["qid"]: p["mrr"] for p in m["per_query"]}
    row = " ".join(f"{m['buckets'][b]['mrr']:>10.4f}" for b in BUCKETS)
    print(f"{mode:8} {m['thr1']['mrr']:>8.4f} {row}")
print()
# 🔑 Q-R1 复核（第 1 轮评审 **P3-1**）：**两个口径都打印**，且标签与公式必须同一口径。
#   原稿用「全量」公式却标「paraphrase 桶」—— 实测两值**符号相反**（桶 +0.0592 vs
#   全局 −0.0113）⇒ 复跑者会在桶的标签下读到负数，以为「Q-R1 没复现」。
para_qs = [q for q in base_mrr if qtype[q] == "paraphrase"]
gap_bucket = sum(
    anc_mrr["vector"][q] - anc_mrr["hybrid"][q] for q in para_qs
) / len(para_qs)
gap_global = sum(anc_mrr["vector"][q] - anc_mrr["hybrid"][q] for q in base_mrr) / n_q
print(
    f"Q-R1 复核（paraphrase 桶、逐 query 配对均值）：vector − hybrid = "
    f"{gap_bucket:+.4f}（n={len(para_qs)}）"
)
print(f"  同式全量口径（**参考值，不可与本行互换**）：{gap_global:+.4f}（n={n_q}）")
print(
    "  期望方向 = **桶值为正**（>0 ⇒ hybrid 在 paraphrase 桶被弱路拖到 vector 之下）"
    "⇒ 设计 §1.1 的问题陈述在本图复现"
)
print()

# ── 逐臂 Δ + 门判定 ──
rows = []  # (signal, theta, bucket, delta, n)
results = {}  # signal -> list of (theta, ok, per-bucket deltas)
for sig in SIGNALS:
    results[sig] = []
    for t in THETAS:
        doc = load(f"adaptive_{sig}_theta-{t}")
        assert_g5(
            f"adaptive_{sig}_theta-{t}", doc, expect_adaptive=True, sig=sig, t=t
        )
        arm = per_query_mrr(doc)
        if set(arm) != set(base_mrr):
            print(f"❌ qid 集合不一致（{sig} θ={t}）⇒ 配对无效")
            sys.exit(4)
        d = {q: arm[q] - base_mrr[q] for q in arm}
        d_by_bucket = {b: [d[q] for q in d if qtype[q] == b] for b in BUCKETS}
        delta = {b: (sum(v) / len(v) if v else 0.0) for b, v in d_by_bucket.items()}
        delta["global"] = sum(d.values()) / len(d)

        # 🔑 内置自检（协议 §「内置自检」）：θ = 0.00 ⇒ `s < 0` 恒假 ⇒ 永不触发
        # ⇒ 必须与 baseline **逐 query 零差**。不成立说明配对 / lane 对齐有 bug。
        # ⚠️ **每个信号各查一次**（第 1 轮评审正文 §三：原稿用全局 `zero_seen` 标志
        #     ⇒ 只有第一个信号被自检 —— 而「某信号传参错」恰是 P3-2 那一类，靠本自检查不到；
        #     现在三个信号的 θ=0.00 臂都查，代价可忽略）。
        if float(t) == 0.0:
            worst = max(abs(v) for v in d.values())
            if worst > 1e-12:
                print(
                    f"❌ 内置自检失败（{sig} θ=0.00 与 baseline 不逐位一致，"
                    f"max|Δ| = {worst:.3e}）"
                )
                print("   ⇒ 配对逻辑或 lane 对齐有问题，读数不可信")
                sys.exit(5)
            print(f"✅ 内置自检通过（{sig} θ=0.00 臂与 baseline 逐 query 零差）")

        # G1 / G2 / G3
        c1 = delta["paraphrase"] >= G1
        c2 = all(delta[b] >= G2 for b in ["exact", "natural", "mixed"])
        c3 = delta["global"] >= G3
        ok = c1 and c2 and c3

        # S9-1b（**§1.3 的辅助判据，不是 §4.5 的门**）：paraphrase 桶追平 vector 单路
        #   ⇒ 阈值 = vector 该桶 MRR − 0.0024（同一个政策容差；见 §1.3 的 P4-5 口径统一）
        # ⚠️ 分母用**实际桶大小**（评审 P4-2）：原稿硬编码 `/ 80` ⇒ 换 `QUERIES=` 后
        #    两个均值会**静默错 80/n 倍**（S9-1b 不进门，但会照常打印 ✅/❌）。
        vec_para = sum(anc_mrr["vector"][q] for q in para_qs) / len(para_qs)
        arm_para = sum(arm[q] for q in para_qs) / len(para_qs)
        s9_1b = arm_para >= vec_para - 0.0024

        results[sig].append((t, ok, delta, s9_1b, arm_para, vec_para))
        for b in BUCKETS + ["global"]:
            n = len(d_by_bucket[b]) if b in d_by_bucket else len(d)
            rows.append((sig, t, b, delta[b], n))
print()

# ── 打印曲线 ──
def mark(b):
    return "✅" if b else "❌"


for sig in SIGNALS:
    print(f"=== θ 灵敏度曲线：信号 = {sig} ===")
    print(
        f"{'θ':>5} {'paraphrase':>11} {'exact':>9} {'natural':>9} {'mixed':>9} "
        f"{'全局':>9}  G1 G2 G3  S9-1b  判定"
    )
    printed_header = False
    for t, ok, delta, s9_1b, arm_para, vec_para in results[sig]:
        c1 = delta["paraphrase"] >= G1
        c2 = all(delta[b] >= G2 for b in ["exact", "natural", "mixed"])
        c3 = delta["global"] >= G3
        if not printed_header:
            # S9-1b 的参照（同一冻结图的 vector 单路读数）打印一次，便于复算
            print(f"      （S9-1b 参照：vector paraphrase = {vec_para:.4f}"
                  f" ⇒ 阈值 {vec_para - 0.0024:.4f}）")
            printed_header = True
        print(
            f"{t:>5} {delta['paraphrase']:>+11.4f} {delta['exact']:>+9.4f} "
            f"{delta['natural']:>+9.4f} {delta['mixed']:>+9.4f} {delta['global']:>+9.4f}  "
            f" {mark(c1)}  {mark(c2)}  {mark(c3)}   {mark(s9_1b)}    {'✅达标' if ok else '—'}"
        )
    print()

# ── G4：最长连续达标区间 ──
print("=== 合取决策门（设计 §4.5；五门缺一即「不投」）===")
verdicts = {}
for sig in SIGNALS:
    flags = [ok for (_, ok, _, _, _, _) in results[sig]]
    best_len, best_start, cur_len, cur_start = 0, 0, 0, 0
    for i, f in enumerate(flags):
        if f:
            if cur_len == 0:
                cur_start = i
            cur_len += 1
            if cur_len > best_len:
                best_len, best_start = cur_len, cur_start
        else:
            cur_len = 0
    g4 = best_len >= G4_MIN
    any_ok = any(flags)
    verdicts[sig] = (g4, best_len, best_start, any_ok, sum(flags))
    if g4:
        lo = THETAS[best_start]
        hi = THETAS[best_start + best_len - 1]
        span = f"θ ∈ [{lo}, {hi}]（{best_len} 连续点，宽度 ≈ {float(hi) - float(lo):.2f}）"
    elif best_len >= 2:
        lo = THETAS[best_start]
        hi = THETAS[best_start + best_len - 1]
        width = float(hi) - float(lo)
        span = f"最长连续 {best_len} 点（θ ∈ [{lo}, {hi}]，宽度 ≈ {width:.2f}）< {G4_MIN} ⇒ G4 不成立"
    elif any_ok:
        span = f"仅 {sum(flags)} 个**互不相邻**的点达标 ⇒ G4 不成立"
    else:
        span = "无任何点达标"
    print(f"[{sig:7}] G4 最长连续达标区间: {span}")
    print(f"           判定 = {'✅ 投' if g4 else '❌ 不投'}")
print()

print("G5（结构条件）自证：")
print(f"  · k = {k_grid}、rrf_k = {rrf_k}、rel_threshold = 1、rerank.enabled = false —— **逐臂核过**")
print(f"  · 同一冻结图：{index_path}（sha256 前 16 = {snap_sha}，{snap_bytes} 字节）—— 所有臂共用")
print(f"  · 判据只消费 per_query[].mrr（名次函数）⇒ 对 R46（score 语义随开关变）免疫")
print(f"  · 查询集：{queries_path}（{n_q} 条，与四桶 n=80 自洽）")
print()

# ── 写 TSV（头含冻结图指纹 ⇒ 产物自证）──
with io.open(tsv_path, "w", encoding="utf-8") as fh:
    fh.write(f"# index={index_path}\n")
    fh.write(f"# index_sha256_16={snap_sha} index_bytes={snap_bytes}\n")
    fh.write(f"# queries={queries_path} n_queries={n_q}\n")
    fh.write(f"# buckets={' '.join(BUCKETS)} n_per_bucket={bucket_n[BUCKETS[0]]}\n")
    fh.write(f"# k={k_grid} rrf_k={rrf_k} baseline_weights={base_weights} rerank=off runs=1\n")
    fh.write(f"# gates: G1>={G1} G2>={G2} G3>={G3} G4_min_consecutive={G4_MIN}\n")
    # Q-R1 的两个口径都进产物（评审 P3-1：单一「桶」标签覆盖不了全量公式）
    fh.write(f"# qr1 paraphrase vector-minus-hybrid={gap_bucket:+.10f} n={len(para_qs)}\n")
    fh.write(f"# qr1 global     vector-minus-hybrid={gap_global:+.10f} n={n_q}\n")
    fh.write("signal\ttheta\tbucket\tdelta_mrr\tn\n")
    for sig, t, b, dv, n in rows:
        fh.write(f"{sig}\t{t}\t{b}\t{dv:.10f}\t{n}\n")

print(f"读数已写入 {tsv_path}")
print()
print("⚠️ 定稿口径（D-S9-10 / §4.5）：**判「不投」是合法结论** —— 交付「工具 + 如实记录未达标」，")
print("   `NFR-15` 保持「打开」并写明复审触发条件（照 S8-09 对 NFR-14 的三条并记）。")
print("   本脚本只出读数与判定；定稿落 `eval-report.md` §8.16 与四处定义面。")
PY

echo
echo "产物目录：$RUN_DIR"
if [ -n "$OUT_DIR" ]; then
  mkdir -p "$OUT_DIR"
  cp "$TSV" "$OUT_DIR/readings.tsv"
  echo "持久产物：${OUT_DIR}/readings.tsv（$(wc -l < "$OUT_DIR/readings.tsv" | tr -d ' ') 行）"
  echo "  ⚠️ 臂级 JSON 仍在临时目录（体积大、不入库）；持久留证以本 TSV + 冻结图指纹为准"
fi
