#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""静态检查：`docs/**/*.md` 与仓库根 `*.md` 里**代码之外**是否还留着长度为 1 的 ASCII `~`。

# 为什么需要这个门

GFM 把一对 ASCII `~` 当**删除线分隔符**（单波浪线即生效）⇒ 用 `~` 表区间时，
两处不相干的 `~` 会**自己配对**，把中间整段文字渲染成 `<del>`：

    源码：P0~P5 全部完成 … 下一阶段 P0~P6
    渲染：P0<del>P5 全部完成 … 下一阶段 P0</del>P6

`#79` 于 2026-09-28 记 **21 文件 / 177 处**；2026-10-30 实测 `main` 已经是
**23 文件 / 189 处** —— **一个月长了 12 处**，因为没有任何门在挡。

# 判据（为什么是「长度为 1 的 run」而不是「`<del>` 计数」）

`~` 的两种用法必须分开：

* **单个 `~`**（`P0~P5` / `R1 ~ R65`）——
  **会**与另一处单 `~` 配对，是误渲染的**唯一**来源 ⇒ **禁止**；
* **`~~…~~`**（`~~已作废~~` / `~~Q1~~ ✅ 已拍板`）——
  **刻意的**删除线，是**状态记号** ⇒ **允许**，且**不得**被「修」掉。

⇒ 判据 = **代码之外不存在长度为 1 的 `~` run**。这是充分条件（比「渲染后无多余 `<del>`」更强），
且**不需要 markdown 渲染器**就能在 CI 里判定（CI 里没有 `cmarkgfm`）。

⚠️ **不能**用「全仓 `<del>` = 0」当判据 —— PR #96 第 1 轮评审 **P2-1** 的实测教训：
那样会把 33 处**刻意**的 `~~` 一起清零，渲染从「划掉」退化成「正文多出全角波浪线」，
而「逆向文本比对」这类判据**看不见渲染语义**，于是双绿而实质退化。

# 边界（如实登记）

* **不判**「`~~` 是否配对完整」：孤立的 `~~` 不会渲染成删除线，属可读性问题、不是渲染事故；
* **不判**围栏代码块与行内代码内的 `~`：那里是字面量（`~/.cargo` 之类必须写进反引号）；
* ⚠️ **缩进式代码块（4 空格）与跨行行内代码跨度不在覆盖内**（第 2 轮评审 **P4-1** 指出）：
  两者都要块级 / 跨行上下文才能判准，**粗判的代价是「假绿」——比假红糟得多**（会让真事故漏网）。
  ⇒ 当前行为 = 按普通行扫描，因此这两种构造里的 `~` 会得到一条**假红**；真遇到就把它挪进
  **行内代码**或改**全角**即可（两条路都让门与渲染同时成立）。
  ⚠️ 我试过「跨行跨度状态机」，在 `docs/devel/v2-step8-design.md` 上**误吞 23 处真事故事件**
  （把 23 个单 `~` 当成代码内 ⇒ 门变瞎）⇒ **已回退**，只保留「引用块内的围栏」这一条（它不产生假绿）；
* 本门**只覆盖 `.md`**：`scripts/*.sh` 等由 `check_shell_expansion.py` 负责；
* 判据比「渲染无事故」**更严**（连 `R1 ~ R65` 这种带空格、实际不会配对的也禁）——
  这是刻意的：**文档里的 ASCII `~` 只允许出现在代码里**，表区间一律用全角 `～`。
  一致性比「逐处判断会不会配对」更便宜，也不会随上下文漂移。

用法::

    python3 scripts/check_doc_tilde.py --self-test   # 对照样本自证（含阴性对照）
    python3 scripts/check_doc_tilde.py [文件 …]      # 默认 docs/**/*.md + 仓库根 *.md

退出码：0 = 干净；1 = 有命中。
"""

from __future__ import annotations

import glob
import re
import subprocess
import sys

# 引用块前缀（可嵌套：`> > `）；剥掉它再判围栏 —— 否则 `> ``` ` 这类块会被当成正文
QUOTE = re.compile(r"^[ \t]*(?:>[ \t]*)+")
FENCE = re.compile(r"^(?P<indent>[ \t]*)(?P<marker>`{3,}|~{3,})(?P<info>.*)$")
LINK_DST = re.compile(r"\]\([^)]*\)")
LINK_REF = re.compile(r"^\s*\[[^\]]+\]:\s*\S+")
# 行内代码跨度：**只认行内配对**。跨行跨度不认（见 docstring 边界段：认错会假绿）
CODE_SPAN = re.compile(r"(`{1,3})(?!`).*?(?<!`)\1(?!`)")
RUN = re.compile(r"~+")


def doc_targets() -> list[str]:
    out = subprocess.run(["git", "ls-files", "*.md"], capture_output=True, text=True, check=True).stdout
    return [f for f in out.split("\n") if f]


def mask(pattern: re.Pattern[str], line: str) -> str:
    """等长掩码：命中的片段替换成等长空格（保持列号对齐）。"""
    return pattern.sub(lambda m: " " * len(m.group(0)), line)


def _quote_depth(line: str) -> int:
    """引用块的嵌套层数（`> > x` ⇒ 2）。"""
    d = 0
    i = 0
    n = len(line)
    while i < n:
        while i < n and line[i] in " \t":
            i += 1
        if i < n and line[i] == ">":
            d += 1
            i += 1
            continue
        break
    return d


def violations(text: str) -> list[tuple[int, int, str]]:
    """返回 (行号, 列号, 上下文)；行号/列号从 1 起。"""
    out: list[tuple[int, int, str]] = []
    fence: tuple[str, int, int] | None = None  # (marker char, marker len, quote depth)
    for lineno, line in enumerate(text.split("\n"), 1):
        depth = _quote_depth(line)
        body = QUOTE.sub("", line)
        m = FENCE.match(body)

        if fence is not None:
            # 引用块结束（层数变浅）⇒ 该围栏随之结束，当前行按普通行继续判
            if depth < fence[2]:
                fence = None
            elif m and m.group("marker")[0] == fence[0] and len(m.group("marker")) >= fence[1] \
                    and m.group("info").strip() == "":
                fence = None
                continue
            else:
                continue

        if m:
            fence = (m.group("marker")[0], len(m.group("marker")), depth)
            continue

        masked = mask(LINK_REF, line)
        masked = mask(LINK_DST, masked)
        masked = mask(CODE_SPAN, masked)
        for run in RUN.finditer(masked):
            if len(run.group(0)) == 1:
                i = run.start()
                out.append((lineno, i + 1, line[max(0, i - 24) : i + 25]))
    return out


def scan(path: str) -> list[str]:
    with open(path, encoding="utf-8") as f:
        text = f.read()
    return [
        f"{path}:{lineno}:{col}: 代码之外的单个 `~`（会与别处的 `~` 配成删除线）—— {ctx!r}"
        for lineno, col, ctx in violations(text)
    ]


def selftest() -> int:
    """阳性 + 阴性对照：判据必须**同时**能报红与保持安静。"""
    cases = [
        # (说明, 文本, 期望命中数)
        ("阴性对照：紧贴区间（真事故形态）", "P0~P5 全部完成", 1),
        ("阴性对照：两处单 ~ 会配对", "P0~P5 … P0~P6", 2),
        ("阳性对照：全角 ～", "P0～P5 全部完成", 0),
        ("阳性对照：行内代码里的 ~", "`P0~P5` 与 `~/.cargo`", 0),
        ("阳性对照：刻意 `~~删除线~~`", "~~已作废~~ 之后重写", 0),
        ("阴性对照：带空格的 `R1 ~ R65`（1 个 `~`，照样禁，见判据）", "R1 ~ R65", 1),
        ("阳性对照：围栏块内的 ~", "```text\nP0~P5\n```", 0),
        ("阳性对照：**引用块内**的围栏（第 2 轮评审 P4-1 已修）", "> ```text\n> a~b\n> ```\n", 0),
        ("阳性对照：嵌套引用块内的围栏", "> > ```text\n> > x~y\n> > ```\n", 0),
        ("已知边界（**假红**，见 docstring）：跨行行内代码跨度", "`a~b\nc` 之后\n", 1),
        ("阳性对照：链接目标里的 ~", "[x](./a~b.md)", 0),
        ("阴性对照：混在刻意 `~~` 里的一处单 ~", "~~a~~ 与 `x` 和 b~c", 1),
    ]
    ok = True
    for name, text, want in cases:
        got = len(violations(text))
        mark = "OK  " if got == want else "FAIL"
        if got != want:
            ok = False
        print(f"{mark} {name}: 命中 {got}（期望 {want}）")
    print("self-test:", "PASS" if ok else "FAIL")
    return 0 if ok else 1


def main(argv: list[str]) -> int:
    if argv and argv[0] == "--self-test":
        return selftest()
    targets = argv if argv else doc_targets()
    hits: list[str] = []
    for t in targets:
        hits.extend(scan(t))
    for h in hits:
        print(h)
    print(f"检查 {len(targets)} 个 md：命中 {len(hits)} 处")
    return 1 if hits else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
