#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""静态检查：`docs/**/*.md` 与仓库根 `*.md` 里 ```bash / ```sh 围栏块的形态。

# 为什么需要这个门

`scripts/check_shell_expansion.py` 只扫 `scripts/*.sh` 与仓库根 `*.sh`
⇒ **文档里的 bash 围栏块完全不被检查**。而文档里这类块的定位是「**可复制的真实命令**」，
粘贴即报错最伤人。

实测到的第一例（PR #70 第 3 轮评审 **P4-8**）：`docs/devel/v2-step9-design.md` 附录 B
曾写 `for θ in 0.00 … 1.00; do …`。`θ` 是多字节 UTF-8，bash 3.2 / 5.x 都**不接受**它做变量名：

    bash: `θ': not a valid identifier

而 `bash -n` **抓不到**它 —— 标识符非法是**执行期**错误，不是语法错误 ⇒ 只有**真执行**才暴露。

# 判据（三条）

1. **语法**：块的 `bash -n` 必须过。
2. **标识符合法**（主判据，**真执行**）：块内**声明**的 shell 标识符 ——
   `for` / `while read` 的循环变量、`read` 的目标、赋值目标（含
   `local` / `declare` / `export` / `readonly`）—— 逐个用
   `bash -c 'eval "$1=1"' _ <id>` **真跑一遍**：bash 对非法标识符直接报错 ⇒ 命中。
   ⚠️ 这条**不能**退化成「正则 + 字符集判断」：合法标识符的规则以 bash 为准
   （含 locale 相关行为），自己写一套必然要么漏、要么假报。
3. **`$VAR` 紧跟非 ASCII**（与 `check_shell_expansion.py` **同判据**，本门只是把范围扩到文档）：
   bash 3.2 在多字节 locale 下不把非 ASCII 字节当作变量名终止符 ⇒ `"$VAR（"` 会被当成
   一个变量名 ⇒ `set -u` 报 unbound variable。必须写 `${VAR}`。

# 边界：为什么**不做**「整块真执行」

块内命令的绝大多数依赖**构建产物 / 数据文件 / 占位符**（`./target/release/helix`、
`--index <snap>`、`data/t2-frozen.snapshot`、`make eval-*`）⇒ 整块执行必然大面积假失败，
一个会红的门就等于没有门。因此本门只对**可确定性判定**、且**跨机器一致**的部分做门
（语法 / 标识符 / 展开形态），并用 `--self-test` 的**阴性对照**自证判据有牙齿
（把 θ 那类写法喂进来**必须**报红）。

用法::

    python3 scripts/check_doc_shell.py --self-test   # 对照样本自证（好坏样本各判一次）
    python3 scripts/check_doc_shell.py [文件 …]      # 默认 docs/**/*.md + 仓库根 *.md

退出码：0 = 干净；1 = 有命中。
"""

from __future__ import annotations

import glob
import os
import re
import subprocess
import sys

LANGS = ("bash", "sh", "shell")
FENCE_OPEN = re.compile(r"^```(" + "|".join(LANGS) + r")\s*$")

# 与 check_shell_expansion.py 同判据：`$` + 标识符 + 紧跟一个非 ASCII 字节
VAR_NONASCII = re.compile(r"\$([A-Za-z_][A-Za-z0-9_]*)(?=[^\x00-\x7f])")

# 声明位置（只认**明显在声明**的形态，避免把普通文本误当标识符）
FOR_IN = re.compile(r"(?<![\w$])for\s+(\S+)\s+in\b")
FOR_DO = re.compile(r"(?<![\w$])for\s+(\S+)\s*;?\s*do\b")
READ = re.compile(r"(?<![\w$])read\b((?:\s+-[A-Za-z]+)*)\s+([^;|&\n]+)")
DECL = re.compile(r"(?<![\w$])(?:local|declare|export|readonly|typeset)\s+(?:-[A-Za-z]+\s+)*([^\W\d][\w]*)")
# 赋值目标：以「字母 / 下划线」开头才当成「想当标识符」（排除 `--grid=1`、`0.00=…`、`x+=…`）
ASSIGN = re.compile(
    r"(?<![\w$])([^\W\d][^\s=;|&()<>*?\"'`$!~{}\[\]#\\]*)=(?!=)"
)
# 形状闸（**不是**合法性判据）：合法与否一律交给 bash；这里只挡掉「显然不是标识符」的 token
ID_CANDIDATE = re.compile(r"^[^\W\d][\w]*$")


def doc_targets() -> list[str]:
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    targets = sorted(glob.glob(os.path.join(root, "docs", "**", "*.md"), recursive=True))
    targets += sorted(glob.glob(os.path.join(root, "*.md")))
    # 归档目录是历史留档（原文不改写）⇒ 与 .gitignore 口径一致地跳过
    return [p for p in targets if os.sep + "archive" + os.sep not in p]


def fenced_blocks(text: str) -> list[tuple[int, str, list[str]]]:
    """返回 [(围栏起始行号, 语言, 块内行)]。"""
    out, cur = [], None
    for lineno, line in enumerate(text.splitlines(), 1):
        if cur is None:
            m = FENCE_OPEN.match(line)
            if m:
                cur = (lineno, m.group(1), [])
            continue
        if line.strip() == "```":
            out.append(cur)
            cur = None
            continue
        cur[2].append(line)
    return out


def strip_comment(line: str) -> str:
    """极粗的去注释：只用于「找声明」，不追求 bash 级正确。

    行首 `#` / 空白后的 `#`（且不在引号内，简化处理）之后一律不参与声明抽取。
    """
    s = line.lstrip()
    if s.startswith("#"):
        return ""
    out, quote = [], None
    for i, ch in enumerate(line):
        if quote:
            out.append(ch)
            if ch == quote:
                quote = None
            continue
        if ch in "\"'":
            quote = ch
            out.append(ch)
            continue
        if ch == "#" and i > 0 and line[i - 1].isspace():
            break
        out.append(ch)
    return "".join(out)


def declared_identifiers(body: str) -> list[tuple[int, str]]:
    """返回 [(块内行号, 标识符)] —— 只收「明显在声明」的位置。"""
    found: list[tuple[int, str]] = []
    for i, raw in enumerate(body.splitlines(), 1):
        line = strip_comment(raw)
        if not line.strip():
            continue
        for rx in (FOR_IN, FOR_DO):
            m = rx.search(line)
            if m:
                found.append((i, m.group(1)))
        for m in READ.finditer(line):
            for word in m.group(2).split():
                if not word.startswith("-"):
                    found.append((i, word))
        for m in DECL.finditer(line):
            found.append((i, m.group(1)))
        for m in ASSIGN.finditer(line):
            tok = m.group(1)
            if ID_CANDIDATE.match(tok):
                found.append((i, tok))
    return found


def valid_bash_identifier(ident: str) -> bool:
    """**真执行**：交给 bash 判定标识符是否合法（不用自己写字符集规则）。"""
    try:
        proc = subprocess.run(
            ["bash", "-c", 'eval "$1=1"', "_", ident],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return True  # 环境跑不了 bash 时不误报
    return proc.returncode == 0


def syntax_ok(body: str) -> bool:
    try:
        proc = subprocess.run(
            ["bash", "-n", "-s"], input="\n".join(body).encode(),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10,
        )
    except (OSError, subprocess.SubprocessError):
        return True
    return proc.returncode == 0


def scan_block(lineno: int, body: list[str]) -> list[str]:
    """返回该块的命中描述列表（空 = 干净）。"""
    hits = []
    if not syntax_ok(body):
        hits.append(f"L{lineno}: 块内 bash 语法不过（`bash -n`）")
    seen = set()
    for i, ident in declared_identifiers("\n".join(body)):
        if ident in seen:
            continue
        seen.add(ident)
        if not valid_bash_identifier(ident):
            hits.append(f"L{lineno + i}: 非法标识符 `{ident}`（bash 拒绝：not a valid identifier）")
    for i, raw in enumerate(body, 1):
        for m in VAR_NONASCII.finditer(raw):
            hits.append(f"L{lineno + i}: `${m.group(1)}` 紧跟非 ASCII ⇒ 须写成 `${{{m.group(1)}}}`")
    return hits


def scan(path: str) -> list[str]:
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
    hits = []
    for lineno, _lang, body in fenced_blocks(text):
        hits += scan_block(lineno, body)
    return hits


# ─────────────────────────── 对照样本 ───────────────────────────
# 判据必须能被**阴性对照**证伪：把 θ 那类写法（bash -n 放行、执行期才炸）喂进来必须报红。
CASES = [
    ("好样本(普通命令序列)", 0, "```bash\ncargo run -p helix -- build --input data/corpus.jsonl\n"
                          "helix search --index /tmp/demo.snapshot --mode bm25 \"如何加快\"\n```\n"),
    ("好样本(合法循环变量 + 花括号展开)", 0, "```bash\nfor i in 1 2 3; do echo \"${i}（次）\"; done\n"
                                      "while read -r line; do echo \"$line\"; done < f.txt\n```\n"),
    ("坏样本(for θ —— P4-8 形态，bash -n 放行、执行期炸)", 1,
     "```bash\nfor θ in 0.00 0.25 0.50; do echo \"$θ\"; done\n```\n"),
    ("坏样本(`$VAR` 紧跟非 ASCII)", 1, "```bash\nlabel=abc\necho \"$label（组）\"\n```\n"),
    ("坏样本(块内 bash 语法不过)", 1, "```bash\nif true; then echo x\n```\n"),
    ("好样本(非 bash 围栏必须跳过)", 0, "```text\nfor θ in 0.00; do :; done\n```\n"),
]


def selftest() -> int:
    ok = True
    for name, want, text in CASES:
        hits = []
        for lineno, _lang, body in fenced_blocks(text):
            hits += scan_block(lineno, body)
        bad = bool(hits) != bool(want)
        ok = ok and not bad
        print(f"{'ERR' if bad else 'OK '} {name}: 命中={len(hits)}（期望 {'有' if want else '无'}）")
        for h in hits:
            print(f"      · {h}")
    print("self-test:", "PASS" if ok else "FAIL")
    return 0 if ok else 1


def main(argv: list[str]) -> int:
    args = argv[1:]  # argv[0] 是脚本名（与 check_shell_expansion.py 同约定）
    if args and args[0] == "--self-test":
        return selftest()
    targets = args or doc_targets()
    if not targets:
        print("check_doc_shell: 没找到待检查的 .md", file=sys.stderr)
        return 1

    bad = 0
    blocks = 0
    for path in targets:
        rel = os.path.relpath(path)
        with open(path, encoding="utf-8") as fh:
            text = fh.read()
        blocks += len(fenced_blocks(text))
        hits = scan(path)
        if not hits:
            continue
        bad += len(hits)
        print(f"✗ {rel}: {len(hits)} 处", file=sys.stderr)
        for h in hits:
            print(f"    {rel}: {h}", file=sys.stderr)

    if bad:
        print(
            "\n修法：\n"
            "  · 非法标识符 ⇒ 换 ASCII 名（`for θ in …` → `for theta in …`），"
            "或把 θ 放进数组/参数里（`for theta in …; do … --adaptive-fusion-theta \"$theta\"`）；\n"
            "  · `$VAR` 紧跟非 ASCII ⇒ 写成 `${VAR}`；\n"
            "  · 语法 ⇒ 按 `bash -n` 报的位置补完。\n"
            "原因：文档里的 bash 块定位是「可复制的真实命令」⇒ 必须真能粘贴执行；"
            "而标识符非法是**执行期**错误，`bash -n` 抓不到（详见本脚本 docstring）。",
            file=sys.stderr,
        )
        return 1

    print(f"check_doc_shell: OK（{len(targets)} 个 .md、{blocks} 个 bash/sh 围栏块，0 处命中）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
