#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""eval_query_cover —— 「query ↔ 源段落」词面覆盖率诊断（V2 Step 9 / T7-16 的读数载体）。

# 为什么要有这个脚本

`eval-report.md` §8.16.8 与 `data/eval/t7-16/readings.tsv` 的 ③ 段给出了 6 个数，用来回答一个
**必答的问题**：

> T7-16 的 `bm25` 读数（0.9246）远高于主基线同图读数（0.6495）—— 是不是因为**生成 query 抄了源段落**？

（若抄写成立，BM25 的高分就是「白捡」的，全部读数不可用。）该诊断此前**只有数学定义、没有可执行
载体**（第 1 轮评审 **P3-1** 点名：`scripts/` 下 bigram / cover 相关文件零命中）⇒ 复现者得自行再实现
一遍「去空白 → 字符 2-gram → 逐正例源段取最大覆盖」。本脚本即该载体（与 PR #73 的 `P3-3` 同族：
**读数要能指到持久产物 / 可复跑脚本**）。

# 口径（与 `eval-report.md` §8.16.8 与 `v2-step9-design.md` §4.19.2 逐字一致）

    cover(q) = |2-gram(q) ∩ 2-gram(源段落)| / |2-gram(q)|

- 2-gram 之前先**去掉全部空白字符**（中文语料里空白位置不稳定，query 里也常带分隔空格）；
- 逐 query 取其**正例源段**（`relevance[].grade >= --min-grade`）的**最大值**；
- `qlen` = query 的**字符数**（原文长度，未去空白）；
- 两组用**同一个函数**计算（这是该诊断能作「对照」的前提）。

# 用法

    python3 scripts/eval_query_cover.py \\
        --set t7-16=data/eval/t7-16/agent-queries.jsonl \\
        --set main-baseline=data/t2-queries.jsonl

输出（TSV，与 `readings.tsv` 的 ③ 段**同形** ⇒ 可直接 diff）：

    set\tn\tcover_mean\tcover_median\tcover_ge_0.7\tcover_ge_0.9\tqlen_median
    t7-16\t80\t0.500048\t0.500000\t9\t0\t18
    main-baseline\t320\t0.518043\t0.500000\t76\t27\t12

`--self-test` 用**合成夹具**断言全部口径（含负例），零外部依赖、零网络。

# 边界（如实登记）

- 本脚本只算**词面覆盖**。它能**排除「抄写型泄漏」**，**不能排除**「生成分布与人类 query 分布不同」
  这一更一般的偏差 —— 别把「cover 不比人类高」读成「生成集无偏差」。
- 缺 `source` 的语料 / 引用了语料外 source 的 query / 无正例的 query ⇒ **报错或跳过并告警**，
  不静默吞（见 `--allow-...` 无此开关：这类问题应当修数据，而不是让诊断静默变小）。
"""
from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path


class CoverError(Exception):
    """诊断链路的可读错误（与 `gen_t7_16.GenError` 同族：只报「哪里不对 + 怎么修」）。"""


# ---------------------------------------------------------------- 语料与 query 集

def load_corpus(path: Path) -> dict[str, str]:
    """`source -> text`。**与 `scripts/gen_t7_16.py::load_corpus` 同口径**（同样的字段名与报错）。"""
    docs: dict[str, str] = {}
    with open(path, encoding="utf-8") as fh:
        for lineno, line in enumerate(fh, 1):
            line = line.strip()
            if not line:
                continue
            try:
                d = json.loads(line)
            except json.JSONDecodeError as e:
                raise CoverError(f"语料第 {lineno} 行解析失败: {e}") from e
            if "source" not in d or "text" not in d:
                raise CoverError(f"语料第 {lineno} 行缺少 source/text")
            docs[str(d["source"])] = str(d["text"])
    if not docs:
        raise CoverError(f"语料为空: {path}")
    return docs


def load_queries(path: Path) -> list[dict]:
    """逐行读 query 资产（与 `data/t2-queries.jsonl` 同 schema）。"""
    rows: list[dict] = []
    with open(path, encoding="utf-8") as fh:
        for lineno, line in enumerate(fh, 1):
            line = line.strip()
            if not line:
                continue
            try:
                d = json.loads(line)
            except json.JSONDecodeError as e:
                raise CoverError(f"query 集第 {lineno} 行解析失败: {e}") from e
            if "query" not in d:
                raise CoverError(f"query 集第 {lineno} 行缺少 query")
            rel = d.get("relevance")
            if not isinstance(rel, list):
                raise CoverError(f"query 集第 {lineno} 行缺少 relevance 数组")
            rows.append(d)
    if not rows:
        raise CoverError(f"query 集为空: {path}")
    return rows


# ---------------------------------------------------------------- 度量（唯一实现）

def bigrams(text: str) -> set[str]:
    """**去全部空白**后的字符 2-gram 集合（`--min`/`max` 长度由调用方保证）。"""
    s = "".join(ch for ch in text if not ch.isspace())
    return {s[i:i + 2] for i in range(len(s) - 1)}


def cover(query: str, source_text: str) -> float:
    """`cover(q) = |2-gram(q) ∩ 2-gram(源段落)| / |2-gram(q)|`（q 不足 2 字符 ⇒ 0.0）。"""
    bq = bigrams(query)
    if not bq:
        return 0.0
    return len(bq & bigrams(source_text)) / len(bq)


def positives(row: dict, min_grade: int) -> list[str]:
    """该 query 的**正例** source 列表（`grade >= min_grade`）。

    ⚠️ 缺 `relevance` ⇒ 报错（与 `load_queries` 的校验同口径：**不静默当成「无正例」**，
    否则「数据缺字段」会被读成「这条 query 没有正确答案」）。
    """
    rel = row.get("relevance")
    if not isinstance(rel, list):
        raise CoverError("query 行缺少 relevance 数组")
    return [str(x["source"]) for x in rel if int(x.get("grade", 0)) >= min_grade]


def measure(rows: list[dict], docs: dict[str, str], min_grade: int) -> tuple[dict, list[str]]:
    """返回 `(统计量, 告警列表)`。**缺 source 引用 ⇒ 报错**（不静默缩小样本）。"""
    vals: list[float] = []
    lens: list[int] = []
    warn: list[str] = []
    missing: list[str] = []
    empty = 0
    for row in rows:
        srcs = positives(row, min_grade)
        if not srcs:
            empty += 1
            continue
        absent = [s for s in srcs if s not in docs]
        if absent:
            missing.extend(absent)
            continue
        vals.append(max(cover(row["query"], docs[s]) for s in srcs))
        lens.append(len(row["query"]))
    if missing:
        uniq = sorted(set(missing))
        raise CoverError(
            f"{len(missing)} 处 relevance 引用了语料外的 source（前几个：{uniq[:5]}）"
            f" ⇒ 语料与 query 集不配套；修数据，别让诊断静默变小"
        )
    if empty:
        warn.append(f"{empty} 条 query 无 `grade >= {min_grade}` 的正例 ⇒ 已跳过（不计入 n）")
    if not vals:
        raise CoverError("没有任何 query 可计入（全部无正例）")
    v = sorted(vals)
    stats = {
        "n": len(vals),
        "cover_mean": statistics.fmean(vals),
        "cover_median": statistics.median(vals),
        "cover_ge_0.7": sum(1 for x in vals if x >= 0.7),
        "cover_ge_0.9": sum(1 for x in vals if x >= 0.9),
        "qlen_median": statistics.median(lens),
    }
    return stats, warn


HEADER = ["set", "n", "cover_mean", "cover_median", "cover_ge_0.7", "cover_ge_0.9", "qlen_median"]


def format_row(name: str, s: dict) -> str:
    return "\t".join([
        name, str(s["n"]), f"{s['cover_mean']:.6f}", f"{s['cover_median']:.6f}",
        str(s["cover_ge_0.7"]), str(s["cover_ge_0.9"]), f"{s['qlen_median']:.0f}",
    ])


# ---------------------------------------------------------------- 离线自检（S9 家族口径）

def self_test() -> int:
    """**零外部依赖**：合成夹具 + 逐项断言（含负例）。CI 侧由 `eval-assets` job 跑。"""
    checks: list[tuple[str, bool]] = []

    def ck(name: str, ok: bool) -> None:
        checks.append((name, ok))
        print(f"  {'✅' if ok else '🔴'} {name}")

    def expect_err(name: str, fn) -> None:
        try:
            fn()
        except CoverError:
            ck(name, True)
        else:
            ck(name, False)

    # ① 度量本身
    ck("① 完全同文 ⇒ cover = 1.0", cover("检索算法", "检索算法") == 1.0)
    ck("① 完全无关 ⇒ cover = 0.0", cover("检索算法", "天气晴朗") == 0.0)
    ck("① 单字 query（无 2-gram）⇒ 0.0", cover("检", "检索算法") == 0.0)
    ck("① 去空白：'检索 算法' 与 '检索算法' 同结果",
       cover("检索 算法", "检索算法教程") == cover("检索算法", "检索算法教程"))
    ck("① 部分覆盖 = 交集/并集口径正确",
       abs(cover("检索算法", "检索向量") - 1 / 3) < 1e-12)  # {'检索','索算','算法'} ∩ {'检索'} = 1/3

    # ② 正例筛选
    row = {"query": "检索", "relevance": [{"source": "b", "grade": 0}, {"source": "a", "grade": 2}]}
    ck("② grade 阈值：只取 grade >= 1", positives(row, 1) == ["a"])
    ck("② min_grade=3 时 b/a 都不算", positives(row, 3) == [])

    # ③ 逐 query 取「正例最大值」而不是「首个正例」
    docs = {"a": "无关文本", "b": "检索算法"}
    s, _ = measure([{"query": "检索算法", "relevance": [
        {"source": "a", "grade": 1}, {"source": "b", "grade": 1}]}], docs, 1)
    ck("③ 取正例最大值（不是第一个）", abs(s["cover_mean"] - 1.0) < 1e-12)

    # ④ 统计量口径（均值 / 中位 / 计数 / qlen）
    rows = [
        {"query": "检索算法", "relevance": [{"source": "b", "grade": 1}]},   # 1.0
        {"query": "天气", "relevance": [{"source": "a", "grade": 1}]},       # 0.0（与「无关文本」无 2-gram 交集）
    ]
    s, _ = measure(rows, docs, 1)
    ck("④ n / 均值 / 中位", s["n"] == 2 and abs(s["cover_mean"] - 0.5) < 1e-12
       and abs(s["cover_median"] - 0.5) < 1e-12)
    ck("④ ≥0.7 与 ≥0.9 计数", s["cover_ge_0.7"] == 1 and s["cover_ge_0.9"] == 1)
    ck("④ qlen 取原文字符数（含空格）",
       measure([{"query": "检 索", "relevance": [{"source": "b", "grade": 1}]}], docs, 1)[0]["qlen_median"] == 3)

    # ⑤ 负例：不静默吞
    expect_err("⑤ 引用语料外 source ⇒ 报错",
               lambda: measure([{"query": "检索", "relevance": [{"source": "zzz", "grade": 3}]}], docs, 1))
    expect_err("⑤ 全部无正例 ⇒ 报错",
               lambda: measure([{"query": "检索", "relevance": [{"source": "a", "grade": 0}]}], docs, 1))
    expect_err("⑤ 缺 relevance ⇒ 报错", lambda: measure([{"query": "检索"}], docs, 1))

    # ⑥ 告警：无正例的 query 被跳过且**计入告警**
    s, warn = measure([
        {"query": "检索算法", "relevance": [{"source": "b", "grade": 1}]},
        {"query": "无正例", "relevance": [{"source": "a", "grade": 0}]},
    ], docs, 1)
    ck("⑥ 跳过无正例的 query 且 n 正确", s["n"] == 1)
    ck("⑥ 跳过时产出告警（不静默）", len(warn) == 1 and "已跳过" in warn[0])

    # ⑦ 输出格式与 readings.tsv ③ 段同形（7 列、tab 分隔、定长小数）
    line = format_row("t7-16", s)
    parts = line.split("\t")
    ck("⑦ 输出 7 列 tab 分隔", len(parts) == 7 and parts[0] == "t7-16")
    ck("⑦ 均值/中位 6 位小数、qlen 整数", len(parts[2].split(".")[1]) == 6 and parts[6].isdigit())
    ck("⑦ 表头与 readings.tsv 逐字一致", HEADER == "set\tn\tcover_mean\tcover_median\tcover_ge_0.7\tcover_ge_0.9\tqlen_median".split("\t"))

    # ⑧ 端到端：临时夹具跑一次 main 取数路径
    import tempfile
    with tempfile.TemporaryDirectory() as td:
        cp, qp = Path(td) / "corpus.jsonl", Path(td) / "queries.jsonl"
        cp.write_text("\n".join([
            json.dumps({"source": "a", "text": "无关文本"}, ensure_ascii=False),
            json.dumps({"source": "b", "text": "检索算法"}, ensure_ascii=False),
        ]) + "\n", encoding="utf-8")
        qp.write_text("\n".join([
            json.dumps({"qid": "q1", "query": "检索算法",
                        "relevance": [{"source": "b", "grade": 3}]}, ensure_ascii=False),
            json.dumps({"qid": "q2", "query": "天气",
                        "relevance": [{"source": "a", "grade": 0}]}, ensure_ascii=False),
        ]) + "\n", encoding="utf-8")
        op = Path(td) / "out.tsv"
        rc = main(["--corpus", str(cp), "--set", f"demo={qp}", "--out", str(op)])
        body = op.read_text(encoding="utf-8").splitlines() if op.exists() else []
        ck("⑧ 端到端 exit 0 且落盘 2 行（表头 + 1 组）", rc == 0 and len(body) == 2)
        ck("⑧ 端到端读数正确（1.0 / n=1）", len(body) == 2 and body[1].split("\t")[2] == "1.000000")

    bad = [n for n, ok in checks if not ok]
    print(f"\nS9 家族 · eval_query_cover 离线自检：{len(checks) - len(bad)}/{len(checks)} 通过"
          + (f"，失败：{bad}" if bad else ""))
    return 1 if bad else 0


# ---------------------------------------------------------------- CLI

def parse_args(argv=None):
    p = argparse.ArgumentParser(
        prog="eval_query_cover.py",
        description="query ↔ 源段落 词面覆盖率诊断（T7-16 泄漏对照；口径见 eval-report §8.16.8）",
    )
    p.add_argument("--corpus", type=Path, default=Path("data/t2-corpus.jsonl"))
    p.add_argument("--set", action="append", default=[], metavar="NAME=PATH",
                   help="可重复；两组用同一函数计算（对照的前提）")
    p.add_argument("--min-grade", type=int, default=1)
    p.add_argument("--out", type=Path, default=None, help="默认写 stdout")
    p.add_argument("--self-test", action="store_true", help="零网络自检（CI 用）")
    return p.parse_args(argv)


def main(argv=None) -> int:
    args = parse_args(argv)
    if args.self_test:
        print("=== eval_query_cover：离线自检（零网络） ===")
        return self_test()
    if not args.set:
        print("❌ 至少给一个 --set NAME=PATH", file=sys.stderr)
        return 2
    docs = load_corpus(args.corpus)
    lines = ["\t".join(HEADER)]
    for spec in args.set:
        if "=" not in spec:
            print(f"❌ --set 需形如 NAME=PATH，收到 {spec!r}", file=sys.stderr)
            return 2
        name, _, path = spec.partition("=")
        stats, warn = measure(load_queries(Path(path)), docs, args.min_grade)
        for w in warn:
            print(f"⚠️ [{name}] {w}", file=sys.stderr)
        print(f"[{name}] {path} ⇒ n={stats['n']} cover_mean={stats['cover_mean']:.6f}", file=sys.stderr)
        lines.append(format_row(name, stats))
    text = "\n".join(lines) + "\n"
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(text, encoding="utf-8")
        print(f"✅ 已写 {args.out}", file=sys.stderr)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
