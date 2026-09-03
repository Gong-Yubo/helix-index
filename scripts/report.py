#!/usr/bin/env python3
"""由 `helix bench --json` 的输出生成 markdown 表格，供 eval-report.md 使用。

存在理由
--------
P5 的 eval-report.md 表格是**手工从终端抄录**的，逐桶 p 值更是手工计算。
手工抄录无法审计——本脚本让报告数字**机械可复现**，是 T5-08 数据诚信的工程化保障。

逐桶符号检验的重算契约（重要）
------------------------------
`helix bench --json` 的 `modes.*.buckets` **只有 n/recall/mrr/ndcg，不含 p 值**
（见 crates/cli/src/bench.rs 的 mode_to_json）；逐桶 p 值只能由 per-query NDCG 重算。

重算必须严格对齐 `crates/core/src/bench/metrics.rs::sign_test` 的约定：
  - 二项精确检验（非正态近似）
  - **单侧**：P(X >= positive)
  - **零值（并列）排除在 n 之外**
只有同时满足这三点，输出才能与 eval-report.md 现有表格逐格一致。

用法
----
    python3 scripts/report.py scripts/fixtures/p5-final-3way.json
    python3 scripts/report.py <json> --section all       # 默认
    python3 scripts/report.py <json> --section summary   # 只出总表
    python3 scripts/report.py <json> --check             # 对账模式（见下）

对账模式（--check）
------------------
与 eval-report.md 现有表格逐格比对，用于验证"手工抄录无错"。
注意：这只验证**抄录**正确，**不验证跑分可复现**——HNSW 图每次构建不同，
vector/hybrid 有 run-to-run 抖动（NDCG 极差 ~0.0024，见 eval-report 3.4）。
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

# eval-report.md 3.3 的逐桶数值，作为对账基准（手工抄录于 P5 结束时）
CHECK_BASELINE = {
    "buckets": {
        "exact": {"bm25": 0.5744, "vector": 0.5968, "hybrid": 0.6148},
        "natural": {"bm25": 0.4824, "vector": 0.5393, "hybrid": 0.5395},
        "mixed": {"bm25": 0.5089, "vector": 0.5341, "hybrid": 0.5392},
        "paraphrase": {"bm25": 0.2306, "vector": 0.4294, "hybrid": 0.3799},
    },
    "bucket_sign": {
        # (hybrid>bm25 的 +/0/-, p) 与 (hybrid>vector 的 +/0/-, p)
        "exact": ((49, 3, 28, 0.0110), (45, 4, 31, 0.0677)),
        "natural": ((54, 0, 26, 0.0012), (40, 7, 33, 0.2414)),
        "mixed": ((48, 1, 31, 0.0356), (39, 6, 35, 0.3638)),
        "paraphrase": ((58, 11, 11, 3.78e-09), (25, 11, 44, 1.0000)),
    },
}

MODES = ["bm25", "vector", "hybrid"]
BUCKETS = ["exact", "natural", "mixed", "paraphrase"]


# --------------------------------------------------------------------------
# 符号检验：严格对齐 metrics.rs::sign_test
# --------------------------------------------------------------------------

def log2_choose(n: int, k: int) -> float:
    """log2(C(n,k))，用 lgamma 避免大数溢出。"""
    return (
        math.lgamma(n + 1) - math.lgamma(k + 1) - math.lgamma(n - k + 1)
    ) / math.log(2.0)


def sign_test(positive: int, n: int) -> float:
    """二项精确单侧检验 P(X >= positive)，p=0.5。

    对应 Rust `bench::sign_test`：
      - 若 positive <= n/2 直接返回 1.0（不可能显著）
      - 否则累加尾部 sum_{k=positive}^{n} C(n,k) / 2^n，用 log-sum-exp 防溢出
    """
    if n == 0:
        return 1.0
    if positive <= n / 2:
        return 1.0
    logs = [log2_choose(n, k) for k in range(positive, n + 1)]
    m = max(logs)
    log2_p = m + math.log2(sum(2.0 ** (x - m) for x in logs)) - n
    return 2.0 ** log2_p


def compare(a_ndcg: dict[str, float], b_ndcg: dict[str, float], qids: list[str]):
    """配对比较 a vs b，返回 (positive, zero, negative, p)。"""
    pos = zero = neg = 0
    for q in qids:
        x, y = a_ndcg[q], b_ndcg[q]
        if x > y + 1e-12:
            pos += 1
        elif x < y - 1e-12:
            neg += 1
        else:
            zero += 1
    return pos, zero, neg, sign_test(pos, pos + neg)


def fmt_p(p: float) -> str:
    if p >= 1e-4:
        return f"{p:.4f}"
    return f"{p:.2e}"


# --------------------------------------------------------------------------
# 数据加载
# --------------------------------------------------------------------------

class BenchResult:
    def __init__(self, data: dict):
        self.data = data
        self.modes = data.get("modes", {})
        # per-query NDCG：mode -> qid -> ndcg / type
        self.ndcg: dict[str, dict[str, float]] = {}
        self.qtype: dict[str, str] = {}
        for mode, md in self.modes.items():
            per = md.get("per_query", [])
            self.ndcg[mode] = {p["qid"]: p["ndcg"] for p in per}
            for p in per:
                self.qtype.setdefault(p["qid"], p.get("type", "?"))
        self.qids = sorted(self.qtype)

    def agg_thr1(self, mode: str) -> dict:
        return self.modes.get(mode, {}).get("thr1", {})

    def agg_thr2(self, mode: str) -> dict:
        return self.modes.get(mode, {}).get("thr2", {})

    def bucket_ndcg(self, mode: str, qtype: str) -> float:
        qs = [q for q in self.qids if self.qtype[q] == qtype]
        if not qs:
            return float("nan")
        return sum(self.ndcg.get(mode, {}).get(q, 0.0) for q in qs) / len(qs)


# --------------------------------------------------------------------------
# 各表格生成
# --------------------------------------------------------------------------

def section_summary(r: BenchResult) -> list[str]:
    lines = [
        "### 三路对照总表（320 query，K=10，宏平均）",
        "",
        "| mode | Recall@10(1) | Recall@10(2) | MRR@10(1) | MRR@10(2) | NDCG@10 |",
        "| --- | --- | --- | --- | --- | --- |",
    ]
    for m in MODES:
        a1, a2 = r.agg_thr1(m), r.agg_thr2(m)
        lines.append(
            f"| {m} | {a1.get('recall', 0):.4f} | {a2.get('recall', 0):.4f} "
            f"| {a1.get('mrr', 0):.4f} | {a2.get('mrr', 0):.4f} | {a1.get('ndcg', 0):.4f} |"
        )
    return lines


def section_significance(r: BenchResult) -> list[str]:
    lines = [
        "### 符号检验（配对 NDCG@10，二项精确单侧）",
        "",
        "| 对比 | + / 0 / − | p 值 |",
        "| --- | --- | --- |",
    ]
    for other in ["bm25", "vector"]:
        if other not in r.ndcg:
            continue
        pos, zero, neg, p = compare(r.ndcg["hybrid"], r.ndcg[other], r.qids)
        lines.append(f"| hybrid vs {other} | {pos} / {zero} / {neg} | {fmt_p(p)} |")
    return lines


def section_buckets(r: BenchResult) -> list[str]:
    lines = [
        "### 分桶（NDCG@10）与逐桶符号检验",
        "",
        "| bucket | n | bm25 | vector | hybrid | hybrid>bm25 (p) | hybrid>vector (p) |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]
    for b in BUCKETS:
        qs = [q for q in r.qids if r.qtype[q] == b]
        if not qs:
            continue
        cells = []
        for other in ["bm25", "vector"]:
            pos, zero, neg, p = compare(r.ndcg["hybrid"], r.ndcg[other], qs)
            cells.append(f"{pos}/{zero}/{neg} ({fmt_p(p)})")
        lines.append(
            f"| {b} | {len(qs)} "
            f"| {r.bucket_ndcg('bm25', b):.4f} | {r.bucket_ndcg('vector', b):.4f} "
            f"| {r.bucket_ndcg('hybrid', b):.4f} | {cells[0]} | {cells[1]} |"
        )
    return lines


def section_runs(r: BenchResult) -> list[str]:
    runs = r.data.get("runs")
    if not runs:
        return []
    lines = [
        "### run-to-run 抖动（`--runs`）",
        "",
        "| mode | NDCG 极差 |",
        "| --- | --- |",
    ]
    for m in MODES:
        vals = [run.get("ndcg", {}).get(m) for run in runs]
        vals = [v for v in vals if v is not None]
        if not vals:
            continue
        lines.append(f"| {m} | {max(vals) - min(vals):.4f} |")
    return lines


def section_grid(r: BenchResult) -> list[str]:
    grid = r.data.get("grid")
    if not grid:
        return []
    lines = ["### BM25 网格搜索（NDCG@10）", "", "| k1 \\ b |"]
    bs = sorted({g["b"] for g in grid})
    k1s = sorted({g["k1"] for g in grid})
    lines[-1] = "| k1 \\ b | " + " | ".join(f"{b}" for b in bs) + " |"
    lines.append("| --- | " + " | ".join("---" for _ in bs) + " |")
    table = {(g["k1"], g["b"]): g["ndcg"] for g in grid}
    for k1 in k1s:
        row = [f"{k1}"]
        for b in bs:
            v = table.get((k1, b))
            row.append(f"{v:.4f}" if v is not None else "—")
        lines.append("| " + " | ".join(row) + " |")
    return lines


def section_latency(r: BenchResult) -> list[str]:
    lat = r.data.get("latency")
    if not lat:
        return []
    lines = [
        "### 延迟（release，warmup + reps × queries）",
        "",
        "| mode | P50(ms) | P99(ms) | 样本数 |",
        "| --- | --- | --- | --- |",
    ]
    for m in MODES:
        d = lat.get(m)
        if not d:
            continue
        lines.append(
            f"| {m} | {d.get('p50_ms', 0):.2f} | {d.get('p99_ms', 0):.2f} "
            f"| {d.get('n_samples', 0)} |"
        )
    return lines


# --------------------------------------------------------------------------
# 对账模式
# --------------------------------------------------------------------------

def run_check(r: BenchResult) -> int:
    """与 eval-report.md 现有数值逐格比对。返回不匹配项数。"""
    bad = 0
    print("== 对账模式：脚本输出 vs eval-report.md 现有数值 ==\n")

    print("-- 分桶 NDCG@10 --")
    for b in BUCKETS:
        base = CHECK_BASELINE["buckets"].get(b, {})
        for m in MODES:
            got = r.bucket_ndcg(m, b)
            want = base.get(m)
            if want is None:
                continue
            ok = abs(got - want) < 5e-4
            bad += 0 if ok else 1
            print(
                f"  {'✅' if ok else '❌'} {b:<11} {m:<8} 脚本={got:.4f} 报告={want:.4f}"
            )

    print("\n-- 逐桶符号检验 --")
    for b in BUCKETS:
        base = CHECK_BASELINE["bucket_sign"].get(b)
        if not base:
            continue
        qs = [q for q in r.qids if r.qtype[q] == b]
        for other, want in zip(["bm25", "vector"], base):
            pos, zero, neg, p = compare(r.ndcg["hybrid"], r.ndcg[other], qs)
            w_pos, w_zero, w_neg, w_p = want
            ok_counts = (pos, zero, neg) == (w_pos, w_zero, w_neg)
            # p 值用相对容差比对（极小数如 3.78e-09 用数量级比较）
            ok_p = abs(p - w_p) <= max(abs(w_p) * 0.02, 1e-4)
            ok = ok_counts and ok_p
            bad += 0 if ok else 1
            print(
                f"  {'✅' if ok else '❌'} {b:<11} vs {other:<7} "
                f"脚本={pos}/{zero}/{neg} p={fmt_p(p)}  "
                f"报告={w_pos}/{w_zero}/{w_neg} p={fmt_p(w_p)}"
            )

    print(f"\n{'✅ 全部逐格一致' if bad == 0 else f'❌ {bad} 项不一致'}")
    print("（注：本对账只验证「手工抄录无错」，不验证跑分可复现——")
    print("  HNSW 图每次构建不同，vector/hybrid 有 run-to-run 抖动。）")
    return bad


# --------------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("json", help="helix bench --json 的输出文件")
    ap.add_argument("--section", default="all",
                    choices=["all", "summary", "significance", "buckets",
                             "runs", "grid", "latency"])
    ap.add_argument("--check", action="store_true",
                    help="对账模式：与 eval-report.md 现有数值逐格比对")
    args = ap.parse_args()

    path = Path(args.json)
    if not path.exists():
        print(f"错误：文件不存在 {path}", file=sys.stderr)
        return 2
    r = BenchResult(json.loads(path.read_text()))

    if args.check:
        return 1 if run_check(r) else 0

    sections = {
        "summary": section_summary,
        "significance": section_significance,
        "buckets": section_buckets,
        "runs": section_runs,
        "grid": section_grid,
        "latency": section_latency,
    }
    names = list(sections) if args.section == "all" else [args.section]
    for i, name in enumerate(names):
        lines = sections[name](r)
        if not lines:
            continue
        if i:
            print()
        print("\n".join(lines))
    return 0


if __name__ == "__main__":
    sys.exit(main())
