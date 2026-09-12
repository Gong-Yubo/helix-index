#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""静态检查：shell 脚本里 `$VAR` 紧跟非 ASCII 字符 —— bash 3.2 下会解析错。

# 为什么需要这个门

macOS 自带 `/bin/bash` 是 **3.2.57**（GPLv2 之后未再更新）。在**多字节 locale** 下，
bash 3.2 **不把非 ASCII 字节当作变量名的终止符**：`"$label（"` 里的「（」是 U+FF08
（3 字节），整个「`label` + `（` 的字节」被当成一个变量名 ⇒ 开了 `set -u` 就直接

    line 187: label?: unbound variable

并**中止脚本**。加花括号写成 `"${label}（"` 即可。

# 为什么必须是静态检查（不能靠跑一次脚本）

1. **locale 依赖**：`LC_ALL=C` / `POSIX` 下**不触发**（本机 Agent shell 就是 C locale，
   所以本地裸跑看不出来），`*.UTF-8` 下必触发 —— 而 UTF-8 才是人用终端时的常态。
   ⇒ 同一个脚本在不同 locale 下「有时能跑」，靠跑一次证明不了什么。
2. **`bash -n` 抓不到**：语法完全合法，是**运行期**才炸。
3. **CI 覆盖不到**：`scripts/eval_*.sh` 需要下载 ONNX 模型 / 跑分钟级基准，不进 CI
   （见 `docs/devel/v2-step6-design.md` 的 F7 裁定）⇒ 脚本改动原本**没有任何自动检查**。

本门与 locale、OS、bash 版本都无关（纯文本扫描），因此可以直接放进 CI。

用法：
    python3 scripts/check_shell_expansion.py [目录...]      # 默认 scripts/ 与仓库根
退出码：0 = 干净；1 = 有命中。
"""

from __future__ import annotations

import glob
import os
import re
import sys

# `$` + 标识符 + 紧跟一个非 ASCII 字节（≥ 0x80 即多字节序列的首字节）
PATTERN = re.compile(r"\$([A-Za-z_][A-Za-z0-9_]*)(?=[^\x00-\x7f])")


def default_targets() -> list[str]:
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    targets = sorted(glob.glob(os.path.join(root, "scripts", "*.sh")))
    targets += sorted(glob.glob(os.path.join(root, "*.sh")))
    return targets


def scan(path: str) -> list[tuple[int, int, str, str]]:
    """返回 (行号, 列号, 变量名, 行内容) 列表。"""
    hits = []
    with open(path, encoding="utf-8", errors="replace") as fh:
        for lineno, line in enumerate(fh, 1):
            for m in PATTERN.finditer(line):
                hits.append((lineno, m.start() + 1, m.group(1), line.rstrip("\n")))
    return hits


def main(argv: list[str]) -> int:
    targets = argv[1:] or default_targets()
    if not targets:
        print("check_shell_expansion: 没找到待检查的 .sh 文件", file=sys.stderr)
        return 1

    bad = 0
    for path in targets:
        hits = scan(path)
        if not hits:
            continue
        bad += len(hits)
        rel = os.path.relpath(path)
        print(f"✗ {rel}: {len(hits)} 处 `$VAR` 紧跟非 ASCII", file=sys.stderr)
        for lineno, col, name, line in hits:
            print(f"    {rel}:{lineno}:{col}  ${name}", file=sys.stderr)
            print(f"        {line.strip()[:110]}", file=sys.stderr)

    if bad:
        print(
            "\n修法：把 `$VAR` 写成 `${VAR}`（只加一对方括号）。\n"
            "原因：bash 3.2（macOS 自带）在多字节 locale 下不把非 ASCII 字节当作变量名\n"
            "      终止符，`\"$VAR（\"` 会被当成一个变量名 ⇒ `set -u` 报 unbound variable\n"
            "      并中止脚本；`LC_ALL=C` 下不触发，故本地裸跑可能看不出来。",
            file=sys.stderr,
        )
        return 1

    print(f"check_shell_expansion: OK（{len(targets)} 个脚本，0 处命中）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
