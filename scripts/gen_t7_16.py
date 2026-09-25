#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""gen_t7_16 —— T7-16「Agent query 评测集」生成器（**S9-06**，设计 §4.7 / D-S9-06 / D-S9-07）。

# 定位

**只作对照、不替换主基线**（`D-S9-06` / H7）；产物**冻结入库**（**R57**：资产为准，
不依赖「下次再跑就能得到同一份」）。`NFR-15` 的主判据**不建立在它上** —— 它的价值是
「外部效度增量」（人类 query 作为 Agent query 的代理这一已知偏差的反面样本）。

# 通道（`D-S9-07`，provider-agnostic）

| 环境变量 | 含义 |
| --- | --- |
| `HELIX_LLM_BASE_URL` | 服务根，例 `https://api.example.com/v1`（脚本会拼 `/chat/completions`） |
| `HELIX_LLM_API_KEY` | 密钥 —— **只走环境变量、绝不入库**（元信息里只记 base_url 的 **host**） |
| `HELIX_LLM_MODEL` | 模型名（**记入元信息** ⇒ R57 的可复现性凭据） |

# 🔴 R58（语料外发）：用户已拍板批准，但脚本仍要求**显式开关**

本脚本会把**语料段落正文**发给上述端点（这是 T7-16 的固有代价：不带语料就生成不出
「有答案的」query）。故：

    HELIX_LLM_ALLOW_EGRESS=1 python3 scripts/gen_t7_16.py --corpus … --n 80

未设该变量 ⇒ **拒绝启动**，并把「将要外发什么」（条数、端点 host、单篇字符数量级）先打印出来
让操作者过目。这样「外发」这件事在**每次**运行时都是**显式**的，不靠记忆。

# 生成协议（**Q9-4 定形**：多轮 + 自查，设计 §4.7）

- **轮 1（生成）**：给 K 篇候选段落 ⇒ 要求为**每一篇**写一条「用户会向检索系统提出的」中文 query
  （该段的正文就是这条 query 的答案）。
- **轮 2（自查 / 弱标注）**：回灌「轮 1 的 query + 该批**全部**候选段落」⇒ 要求逐段评
  `grade ∈ {0,1,2,3}`（**这就是弱标注**；不是把生成源段直接标成 3，而是**独立一轮**判定）。
- **一致性判定（自查的牙齿）**：轮 2 给**生成源段**的 grade **≥ 1** ⇒ 保留；否则**丢弃**该条
  ⇒ 通过率写进元信息（这是可报告的质量指标）。

# 产物（落 `<out>/`，默认 `data/eval/t7-16/`）

| 文件 | 内容 |
| --- | --- |
| `agent-queries.jsonl` | 与 `data/t2-queries.jsonl` **同 schema**：`{"qid","query","type","relevance":[{"source","grade"}]}` |
| `gen-meta.json` | 模型 / 端点 host / 温度 / 种子 / prompt 文本与其 sha256 / 通过率 / 计数（**R57**） |

⚠️ **`type` 固定为 `"agent"`，不冒充既有四桶**：`helix bench` 的分桶表是**固定四桶**
（`mixed/natural/exact/paraphrase`，见 `mode_to_json`），故本资产的**四桶行恒 `n=0`**；
对照读数取**全局行** + `per_query` 的逐 qid 配对（与 `scripts/eval_s9.sh` 同族口径）。

# 离线桩（**S9-T12**；CI **不发外部请求**）

    python3 scripts/gen_t7_16.py --self-test          # 零网络：内置桩跑全链并断言
    python3 scripts/gen_t7_16.py --stub <responses.json> …   # 自定义桩（同样零网络）

`--self-test` 断言的正是 S9-T12 要求的「**解析 / 落盘 / 配对**」三条 + 若干负例
（越界 grade、候选外文档、未开外发开关、桩响应带围栏与前后缀、落盘幂等、元信息不含密钥）。

# 为什么是脚本而不是 Rust 子命令

评测资产工具在本仓的载体是**脚本 / example**（`scripts/eval_s9.sh`、`examples/t2_prep.rs`），
且本 PR 的判据只涉及「文本进、JSON 出」—— 放进产品 crate 会**扩大产品面**（还要新增 HTTP 依赖
⇒ 动 `deny.toml` / MSRV / CI 依赖面），而与 PR9-2 已确立的「臂级离线分析属评测资产而非产品代码」
一致。**通道的实际执行**：`urllib`（标准库）⇒ 零新增依赖。
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

# ---- 预注册常量（改动 = 资产变更，须在元信息与报告里记录）----
DEFAULT_PER_BATCH = 4  # 每轮塞给模型的候选段落数
DEFAULT_SEED = 0x7431_2D31  # 「t7-16」的 ASCII 变体（与 t2_prep 的三个种子**刻意不同**）
DEFAULT_QID_PREFIX = "t7-16-"
DEFAULT_TEMPERATURE = 0.2
DEFAULT_TIMEOUT_S = 120
AGENT_TYPE = "agent"  # 自有标签：不冒充四桶（见模块文档）
GRADES = (0, 1, 2, 3)

SYS_ROUND1 = (
    "你是中文检索评测集的出题人。用户会给你若干「段落」，每段前面有 [序号] (source=编号)。\n"
    "请为**每一段**写**一条**中文检索 query，要求：\n"
    "1) 写成**用户会向检索系统提出的问题**（口语化、有真实信息需求），不要抄段落原文；\n"
    "2) 该段落（且只有该段落）应当能完整回答这条 query；\n"
    "3) 每条 query 长度 5~40 个字；\n"
    "4) **严格**只输出 JSON：{\"items\":[{\"doc\":\"<source 编号>\",\"query\":\"<query>\"}]}\n"
    "   不要输出任何解释、不要用 Markdown 代码围栏。"
)

SYS_ROUND2 = (
    "你是相关性标注员。用户会给你一条检索 query 和若干「候选段落」，"
    "每段前面有 [序号] (source=编号)。\n"
    "请对**每一段**判断它对这条 query 的相关性等级：\n"
    "  3 = 直接、完整地回答；2 = 高度相关但不完整；1 = 部分相关；0 = 不相关。\n"
    "**严格**只输出 JSON：{\"labels\":[{\"doc\":\"<source 编号>\",\"grade\":<0-3>}]}\n"
    "不要输出任何解释、不要用 Markdown 代码围栏。"
)


class GenError(RuntimeError):
    """生成链路上的可报告错误（**一律上抛，不静默降级** —— NFR-07）。"""


# ---------------------------------------------------------------- 解析 / 校验


def extract_json(text: str) -> dict:
    """从模型输出里取出**第一个** JSON 对象。

    🔑 模型常加 ```json 围栏或前后缀说明 ⇒ 这里先剥围栏，再按「首个 `{` 到末个 `}`」切片解析
    （**不**做「寻找合法 JSON 子串」式的猜测：解析不了就报错，宁可失败也不静默丢数据）。
    """
    s = (text or "").strip()
    if s.startswith("```"):
        s = s.split("\n", 1)[1] if "\n" in s else ""
        if s.rstrip().endswith("```"):
            s = s.rstrip()[:-3]
    lo, hi = s.find("{"), s.rfind("}")
    if lo < 0 or hi <= lo:
        raise GenError(f"模型输出里找不到 JSON 对象（前 120 字符: {s[:120]!r}）")
    try:
        obj = json.loads(s[lo : hi + 1])
    except json.JSONDecodeError as e:
        raise GenError(f"JSON 解析失败（{e}）；切片前 160 字符: {s[lo:lo + 160]!r}") from e
    if not isinstance(obj, dict):
        raise GenError(f"顶层不是对象而是 {type(obj).__name__}")
    return obj


def parse_items(raw: str, allowed_docs: set[str]) -> list[dict]:
    """轮 1 响应 ⇒ `[{"doc","query"}]`（**严格**：候选外文档 / 空 query 都报错）。"""
    obj = extract_json(raw)
    items = obj.get("items")
    if not isinstance(items, list) or not items:
        raise GenError(f"响应缺少非空 `items` 数组（键: {sorted(obj.keys())}）")
    out = []
    for i, it in enumerate(items):
        if not isinstance(it, dict):
            raise GenError(f"items[{i}] 不是对象")
        doc = str(it.get("doc", "")).strip()
        query = str(it.get("query", "")).strip()
        if doc not in allowed_docs:
            raise GenError(f"items[{i}] 的 doc={doc!r} 不在本批候选内（候选: {sorted(allowed_docs)}）")
        if not query:
            raise GenError(f"items[{i}] 的 query 为空")
        out.append({"doc": doc, "query": query})
    return out


def parse_labels(raw: str, allowed_docs: set[str]) -> list[dict]:
    """轮 2 响应 ⇒ `[{"doc","grade"}]`（**严格**：候选外文档 / grade 越界都报错）。"""
    obj = extract_json(raw)
    labels = obj.get("labels")
    if not isinstance(labels, list) or not labels:
        raise GenError(f"响应缺少非空 `labels` 数组（键: {sorted(obj.keys())}）")
    out = []
    for i, it in enumerate(labels):
        if not isinstance(it, dict):
            raise GenError(f"labels[{i}] 不是对象")
        doc = str(it.get("doc", "")).strip()
        if doc not in allowed_docs:
            raise GenError(f"labels[{i}] 的 doc={doc!r} 不在本批候选内")
        try:
            grade = int(it.get("grade"))
        except (TypeError, ValueError) as e:
            raise GenError(f"labels[{i}] 的 grade 不是整数: {it.get('grade')!r}") from e
        if grade not in GRADES:
            raise GenError(f"labels[{i}] 的 grade={grade} 越界（须 ∈ {GRADES}）")
        out.append({"doc": doc, "grade": grade})
    return out


# ---------------------------------------------------------------- 配对 / 组装


def pair(items: list[dict], labels: list[dict]) -> list[dict]:
    """轮 1 的 items 与轮 2 的 labels **按 doc 配对**，并施加自查判定。

    返回 `[{"doc","query","grade","self_check"}]`；`self_check` ∈ {`"pass"`,`"reject"`}：
    轮 2 给**生成源段**的 grade ≥ 1 ⇒ `pass`（否则该条无自洽标注，不能用）。
    """
    by_doc = {l["doc"]: l["grade"] for l in labels}
    missing = [it["doc"] for it in items if it["doc"] not in by_doc]
    if missing:
        raise GenError(f"轮 2 未对以下候选给出标注: {missing}（自查无法进行）")
    return [
        {
            "doc": it["doc"],
            "query": it["query"],
            "grade": by_doc[it["doc"]],
            "self_check": "pass" if by_doc[it["doc"]] >= 1 else "reject",
        }
        for it in items
    ]


def assemble(
    paired: list[dict],
    batch_labels: dict[str, list[dict]],
    *,
    qid_prefix: str,
    keep_docs: set[str],
) -> tuple[list[dict], dict]:
    """配对结果 ⇒ **与 `data/t2-queries.jsonl` 同 schema** 的 judgment 行 + 计数。

    - `qid` = `f"{qid_prefix}{序号:04d}"`（**按 doc 升序编号** ⇒ 与执行顺序无关、可复跑）
    - `relevance` = 该条所属批次的**全部**弱标注（含 grade 0 —— 与 t2 语料「已判定负例也进池」同口径），
      且**只含本批候选**（弱标注只对本批做过判断，不能凭空给别的段落 0 分）
    - 只保留 `self_check == "pass"` 且 `doc ∈ keep_docs` 的条目（`keep_docs` 用于去重：一个源段一条 query）
    """
    rows, dup_dropped, rejected = [], 0, 0
    seen_doc = set()
    for p in sorted(paired, key=lambda x: x["doc"]):
        if p["self_check"] != "pass":
            rejected += 1
            continue
        if p["doc"] in seen_doc or p["doc"] not in keep_docs:
            dup_dropped += 1
            continue
        seen_doc.add(p["doc"])
        rows.append(
            {
                "qid": f"{qid_prefix}{len(rows) + 1:04d}",
                "query": p["query"],
                "type": AGENT_TYPE,
                "relevance": [
                    {"source": l["doc"], "grade": l["grade"]}
                    for l in sorted(batch_labels[p["doc"]], key=lambda x: x["doc"])
                ],
            }
        )
    stats = {
        "paired": len(paired),
        "kept": len(rows),
        "rejected_self_check": rejected,
        "dropped_dup_or_filtered": dup_dropped,
    }
    return rows, stats


def validate_asset(rows: list[dict], corpus_sources: set[str]) -> None:
    """落盘前的**自证**（S9-T12 的「解析/落盘/配对」中的落盘面）。

    逐条断言：schema 键完全一致 / `qid` 唯一 / `type` 为 `AGENT_TYPE` / grade ∈ 0..3 /
    **每个 `source` 都在语料里**（等价于 `helix_core::bench::validate_sources` 的前置条件）。
    """
    if not rows:
        raise GenError("组装出的资产为空（全部条目都被自查/去重丢掉）")
    keys = {"qid", "query", "type", "relevance"}
    seen_qid = set()
    for i, r in enumerate(rows):
        if set(r.keys()) != keys:
            raise GenError(f"第 {i} 行 schema 键不符：{sorted(r.keys())} != {sorted(keys)}")
        if r["qid"] in seen_qid:
            raise GenError(f"qid 重复: {r['qid']}")
        seen_qid.add(r["qid"])
        if r["type"] != AGENT_TYPE:
            raise GenError(f"第 {i} 行 type={r['type']!r}（应为 {AGENT_TYPE!r}）")
        if not r["query"].strip():
            raise GenError(f"第 {i} 行 query 为空")
        if not r["relevance"]:
            raise GenError(f"第 {i} 行 relevance 为空")
        for e in r["relevance"]:
            if set(e.keys()) != {"source", "grade"}:
                raise GenError(f"第 {i} 行 relevance 条目键不符：{sorted(e.keys())}")
            if e["grade"] not in GRADES:
                raise GenError(f"第 {i} 行 grade={e['grade']} 越界")
            if e["source"] not in corpus_sources:
                raise GenError(f"第 {i} 行的 source={e['source']!r} 不在语料里（会被 bench 拒绝）")


# ---------------------------------------------------------------- 语料 / 抽样


def load_corpus(path: Path) -> list[tuple[str, str]]:
    docs: list[tuple[str, str]] = []
    with open(path, encoding="utf-8") as fh:
        for lineno, line in enumerate(fh, 1):
            line = line.strip()
            if not line:
                continue
            try:
                d = json.loads(line)
            except json.JSONDecodeError as e:
                raise GenError(f"语料第 {lineno} 行解析失败: {e}") from e
            if "source" not in d or "text" not in d:
                raise GenError(f"语料第 {lineno} 行缺少 source/text")
            docs.append((str(d["source"]), str(d["text"])))
    if not docs:
        raise GenError(f"语料为空: {path}")
    return docs


def sample_docs(
    docs: list[tuple[str, str]], n: int, seed: int, *, exclude: set[str] | None = None
) -> list[tuple[str, str]]:
    """**确定性**抽样：按 `sha256(f"{seed}:{source}")` 的哈希秩取最小 `n` 个。

    与 `t2_prep` 的 xxh64 哈希秩**同思路**（不做 Bernoulli 计数，保「同种子重跑同结果」），
    但**刻意换了一把哈希** —— T7-16 是独立对照集，不该与主基线共享抽样无关性假设。
    """
    exclude = exclude or set()
    pool = [d for d in docs if d[0] not in exclude]
    if len(pool) < n:
        raise GenError(f"候选池不足：可用 {len(pool)} < 请求 {n}")
    ranked = sorted(pool, key=lambda t: hashlib.sha256(f"{seed}:{t[0]}".encode()).digest())
    return ranked[:n]


# ---------------------------------------------------------------- 传输（唯一网络缝）


def chat_once(
    base_url: str, api_key: str, model: str, messages: list[dict], *, temperature: float,
    seed: int, timeout_s: int,
) -> str:
    """一次 `POST {base}/chat/completions`（OpenAI 兼容）。**唯一**发网络请求的地方。"""
    url = base_url.rstrip("/") + "/chat/completions"
    body = json.dumps(
        {
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "seed": seed,
            "stream": False,
        },
        ensure_ascii=False,
    ).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {api_key}"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout_s) as resp:
            payload = json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        snippet = e.read()[:300].decode("utf-8", "replace")
        raise GenError(f"HTTP {e.code} from {url}: {snippet}") from e
    except urllib.error.URLError as e:
        raise GenError(f"连不上 {url}: {e}") from e
    try:
        return payload["choices"][0]["message"]["content"]
    except (KeyError, IndexError, TypeError) as e:
        raise GenError(f"响应形状不符（缺 choices[0].message.content）: {str(payload)[:200]}") from e


class StubTransport:
    """离线桩（**S9-T12**）：按序返回固定响应，**不发任何网络请求**。"""

    def __init__(self, responses: list[str]):
        if not responses:
            raise GenError("桩响应为空")
        self.responses = list(responses)
        self.calls: list[list[dict]] = []

    def __call__(self, messages: list[dict], **_) -> str:
        self.calls.append(messages)
        idx = len(self.calls) - 1
        if idx >= len(self.responses):
            raise GenError(f"桩响应用尽（第 {idx + 1} 次调用）")
        return self.responses[idx]


# ---------------------------------------------------------------- 编排


def run_pipeline(
    docs: list[tuple[str, str]],
    *,
    n: int,
    per_batch: int,
    seed: int,
    qid_prefix: str,
    transport,
    meta_extra: dict,
) -> tuple[list[dict], dict]:
    """抽样 → 逐批（轮 1 + 轮 2）→ 配对 → 组装。`transport(messages, ...) -> str`。

    抽样的 **候选池 = 全部语料**；每批 `per_batch` 篇（同一批的段落互相充当**候选负例**
    ⇒ 轮 2 才能给出 grade 0 —— 这正是弱标注的来源）。
    """
    picked = sample_docs(docs, n, seed)
    corpus_sources = {d[0] for d in docs}
    all_text = dict(docs)
    rows: list[dict] = []
    paired_all: list[dict] = []
    batch_labels: dict[str, list[dict]] = {}
    keep_docs = {d[0] for d in picked}

    for start in range(0, len(picked), per_batch):
        batch = picked[start : start + per_batch]
        docs_block = "\n\n".join(
            f"[{i}] (source={src})\n{all_text[src][:1200]}" for i, (src, _) in enumerate(batch)
        )
        allowed = {src for src, _ in batch}

        sys1, sys2 = SYS_ROUND1, SYS_ROUND2
        raw1 = transport(
            [{"role": "system", "content": sys1}, {"role": "user", "content": docs_block}]
        )
        items = parse_items(raw1, allowed)

        # 轮 2 逐条自查（每条 query 单独一次调用：候选 = 本批全部段落）
        for it in items:
            q_block = f"query：{it['query']}\n\n候选段落：\n{docs_block}"
            raw2 = transport(
                [{"role": "system", "content": sys2}, {"role": "user", "content": q_block}]
            )
            labels = parse_labels(raw2, allowed)
            batch_labels[it["doc"]] = labels
            paired_all.extend(pair([it], labels))

    rows, stats = assemble(
        paired_all, batch_labels, qid_prefix=qid_prefix, keep_docs=keep_docs
    )
    validate_asset(rows, corpus_sources)

    stats["sampled_docs"] = len(picked)
    stats["pass_rate"] = (
        round(stats["kept"] / stats["paired"], 4) if stats["paired"] else 0.0
    )
    meta = dict(meta_extra)
    meta["counts"] = stats
    meta["self_check"] = {
        "rule": "轮 2 给生成源段的 grade ≥ 1 才保留（否则丢弃）",
        "pass_rate": stats["pass_rate"],
    }
    return rows, meta


# ---------------------------------------------------------------- 落盘


def write_assets(out_dir: Path, rows: list[dict], meta: dict) -> tuple[bytes, bytes]:
    """写 `agent-queries.jsonl` + `gen-meta.json`，返回两者的 bytes（离线桩自检要核幂等）。"""
    out_dir.mkdir(parents=True, exist_ok=True)
    jsonl = "".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows).encode("utf-8")
    meta_b = (json.dumps(meta, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
    (out_dir / "agent-queries.jsonl").write_bytes(jsonl)
    (out_dir / "gen-meta.json").write_bytes(meta_b)
    return jsonl, meta_b


# ---------------------------------------------------------------- 外发守卫（R58）


def egress_guard(docs: list[tuple[str, str]], n: int, base_url: str, *, env: dict) -> None:
    """**R58 的显式开关**：未设 `HELIX_LLM_ALLOW_EGRESS=1` ⇒ 拒绝启动并打印将外发什么。"""
    if env.get("HELIX_LLM_ALLOW_EGRESS") == "1":
        return
    host = re.sub(r"^https?://", "", base_url).split("/")[0]
    chars = sum(len(t) for _, t in docs[:n]) if n <= len(docs) else sum(len(t) for _, t in docs)
    raise GenError(
        "拒绝启动：语料外发未显式开启（R58）。\n"
        f"  将外发：约 {n} 篇语料段落（合计约 {chars} 字符）→ 端点 host = {host}\n"
        "  该外发已由用户 2026-09-26 拍板批准（R58），但**每次运行都要显式确认**：\n"
        "  ⇒ HELIX_LLM_ALLOW_EGRESS=1 python3 scripts/gen_t7_16.py …\n"
        "  （只想离线验证链路请用 --self-test / --stub，两者都不发网络请求）"
    )


# ---------------------------------------------------------------- S9-T12 离线桩自检


def _stub_r1(docs: list[tuple[str, str]]) -> str:
    return json.dumps(
        {
            "items": [
                {"doc": src, "query": f"关于第{i + 1}篇的提问：{t[:8]}？"}
                for i, (src, t) in enumerate(docs)
            ]
        },
        ensure_ascii=False,
    )


def self_test() -> int:
    """**S9-T12**：零网络的离线桩，断言 解析 / 落盘 / 配对 三条 + 8 个负例。"""
    checks: list[tuple[str, bool]] = []

    def ck(name: str, ok: bool) -> None:
        checks.append((name, ok))
        print(f"  {'✅' if ok else '🔴'} {name}")

    fake_docs = [("d1", "第一段正文：如何申请公积金贷款"), ("d2", "第二段正文：公积金提取条件"), ("d3", "第三段正文：社保转移流程")]
    allowed = {s for s, _ in fake_docs}
    seed, prefix = 7, "t7-16-"

    def expect_err(name: str, fn) -> None:
        try:
            fn()
        except GenError:
            ck(name, True)
        else:
            ck(f"{name}（应报错却通过了）", False)

    # ① 正常桩：解析 + 配对 + 组装 + 落盘
    stub = StubTransport([
        _stub_r1(fake_docs),                                     # 轮 1
        json.dumps({"labels": [{"doc": "d1", "grade": 3}, {"doc": "d2", "grade": 0}, {"doc": "d3", "grade": 1}]}),
        json.dumps({"labels": [{"doc": "d1", "grade": 0}, {"doc": "d2", "grade": 2}, {"doc": "d3", "grade": 0}]}),
        json.dumps({"labels": [{"doc": "d1", "grade": 1}, {"doc": "d2", "grade": 0}, {"doc": "d3", "grade": 3}]}),
    ])
    rows, meta = run_pipeline(
        fake_docs, n=3, per_batch=3, seed=seed, qid_prefix=prefix, transport=stub,
        meta_extra={"llm": {"model": "stub-1", "temperature": 0.2, "seed": seed}},
    )
    ck("① 正常桩：产出 3 条自查通过", len(rows) == 3 and meta["counts"]["pass_rate"] == 1.0)
    ck("① schema 与 t2-queries 同构（qid/query/type/relevance）",
       all(set(r) == {"qid", "query", "type", "relevance"} for r in rows))
    ck("① type 固定 agent（不冒充四桶）", all(r["type"] == "agent" for r in rows))
    ck("① 每条 relevance 覆盖本批全部候选（含 grade 0）",
       all(len(r["relevance"]) == 3 for r in rows))
    ck("① qid 唯一且带前缀", len({r["qid"] for r in rows}) == 3 and all(r["qid"].startswith(prefix) for r in rows))

    # ② 落盘幂等（同桩跑两次逐字节相同）
    import tempfile

    with tempfile.TemporaryDirectory() as td:
        jsonl_a, meta_a = write_assets(Path(td) / "a", rows, meta)
        jsonl_b, meta_b = write_assets(Path(td) / "b", rows, meta)
        ck("② 落盘幂等（同输入逐字节相同）", jsonl_a == jsonl_b and meta_a == meta_b)
        ck("② 落盘行数 = 组装条数", jsonl_a.decode("utf-8").count("\n") == len(rows))

    # ③ 元信息：可复现性凭据齐备且**不含密钥**
    meta["llm"]["api_key_sha256_16"] = "NOT_RECORDED"
    blob = json.dumps(meta, ensure_ascii=False)
    ck("③ 元信息含 model/温度/种子（R57）",
       all(k in meta["llm"] for k in ("model", "temperature", "seed")))
    ck("③ 元信息**不含**密钥字面量", "sk-" not in blob and "api_key\"" not in blob)

    # ④ 解析容错：围栏 + 前后缀说明
    ck("④ 容忍 ```json 围栏与前后缀说明",
       extract_json("说明文字\n```json\n{\"items\":[{\"doc\":\"d1\",\"query\":\"q\"}]}\n```\n以上。")["items"][0]["doc"] == "d1")

    # ⑤ 负例：候选外文档 / 空 query / grade 越界 / 缺数组 / 非对象
    expect_err("⑤ 轮 1 候选外文档 ⇒ 报错", lambda: parse_items('{"items":[{"doc":"X","query":"q"}]}', allowed))
    expect_err("⑤ 轮 1 空 query ⇒ 报错", lambda: parse_items('{"items":[{"doc":"d1","query":"  "}]}', allowed))
    expect_err("⑤ 轮 2 grade=4 ⇒ 报错", lambda: parse_labels('{"labels":[{"doc":"d1","grade":4}]}', allowed))
    expect_err("⑤ 轮 2 grade 非整数 ⇒ 报错", lambda: parse_labels('{"labels":[{"doc":"d1","grade":"高"}]}', allowed))
    expect_err("⑤ 缺 items 数组 ⇒ 报错", lambda: parse_items('{"foo":1}', allowed))
    expect_err("⑤ 输出无 JSON ⇒ 报错", lambda: parse_items("抱歉我不能完成", allowed))
    expect_err("⑤ 轮 2 漏标某候选 ⇒ 配对报错",
               lambda: pair([{"doc": "d1", "query": "q"}], [{"doc": "d2", "grade": 1}]))

    # ⑥ 自查的牙齿：源段 grade 0 ⇒ 该条被丢（且计数如实）
    rejected_rows, rejected_meta = run_pipeline(
        fake_docs, n=3, per_batch=3, seed=seed, qid_prefix=prefix,
        transport=StubTransport([
            _stub_r1(fake_docs),
            json.dumps({"labels": [{"doc": "d1", "grade": 0}, {"doc": "d2", "grade": 0}, {"doc": "d3", "grade": 0}]}),
            json.dumps({"labels": [{"doc": "d1", "grade": 0}, {"doc": "d2", "grade": 1}, {"doc": "d3", "grade": 0}]}),
            json.dumps({"labels": [{"doc": "d1", "grade": 1}, {"doc": "d2", "grade": 0}, {"doc": "d3", "grade": 0}]}),
        ]),
        meta_extra={"llm": {"model": "stub-1"}},
    )
    ck("⑥ 自查拒绝生效（2/3 被丢，只留 d2）",
       [r["qid"] for r in rejected_rows] == [f"{prefix}0001"]
       and rejected_meta["counts"]["rejected_self_check"] == 2
       and rejected_rows[0]["relevance"] == [{"source": "d1", "grade": 0}, {"source": "d2", "grade": 1}, {"source": "d3", "grade": 0}])

    # ⑦ 外发守卫（R58）：未显式开启 ⇒ 拒绝；开启 ⇒ 放行
    expect_err("⑦ 未设 HELIX_LLM_ALLOW_EGRESS ⇒ 拒绝外发",
               lambda: egress_guard(fake_docs, 3, "https://api.example.com/v1", env={}))
    try:
        egress_guard(fake_docs, 3, "https://api.example.com/v1", env={"HELIX_LLM_ALLOW_EGRESS": "1"})
        ck("⑦ 设 HELIX_LLM_ALLOW_EGRESS=1 ⇒ 放行", True)
    except GenError:
        ck("⑦ 设 HELIX_LLM_ALLOW_EGRESS=1 ⇒ 放行", False)

    # ⑧ 抽样性质（**不在此处复刻哈希公式** —— 否则锁的是用例自己抄的那份实现）
    pool = [(f"d{i:03d}", "t") for i in range(200)]
    s5 = [s for s, _ in sample_docs(pool, 5, 42)]
    s3 = [s for s, _ in sample_docs(pool, 3, 42)]
    ck("⑧ 抽样确定性（同种子同序）", s5 == [s for s, _ in sample_docs(pool, 5, 42)])
    ck("⑧ 秩最小性（top-3 必是 top-5 的前缀）", s3 == s5[:3])
    ck("⑧ 无重复 / 是语料子集", len(s5) == len(set(s5)) == 5 and set(s5) <= {s for s, _ in pool})
    ck("⑧ 不是「取前 n 个」（与输入序不同）", s5 != [s for s, _ in pool[:5]])
    ck(
        "⑧ 种子确实影响结果（20 个种子里 ≥2 种排序）",
        len({tuple(s for s, _ in sample_docs(pool, 5, sd)) for sd in range(20)}) >= 2,
    )
    ck(
        "⑧ exclude 生效（排除项不再出现）",
        not (set(s for s, _ in sample_docs(pool, 5, 42, exclude=set(s5))) & set(s5)),
    )
    expect_err("⑧ 候选池不足 ⇒ 报错", lambda: sample_docs(pool, 999, 42))
    expect_err("⑧ 空资产 ⇒ 组装后校验报错",
               lambda: validate_asset([], {"d1"}))

    n_ok = sum(1 for _, ok in checks if ok)
    print(f"\nS9-T12 离线桩自检：{n_ok}/{len(checks)} 通过")
    return 0 if n_ok == len(checks) else 1


# ---------------------------------------------------------------- main


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="gen_t7_16",
        description="T7-16 Agent query 评测集生成器（S9-06；外部 LLM API + 多轮自查 + 冻结入库）",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "环境变量：HELIX_LLM_BASE_URL / HELIX_LLM_API_KEY / HELIX_LLM_MODEL；"
            "外发需额外 HELIX_LLM_ALLOW_EGRESS=1（R58）。\n"
            "离线验证链路：--self-test（内置桩）/ --stub <file>（自定义桩），两者都不发网络请求。"
        ),
    )
    p.add_argument("--corpus", type=Path, default=Path("data/t2-corpus.jsonl"),
                   help="语料 JSONL（每行 {\"source\",\"text\"}；默认 data/t2-corpus.jsonl）")
    p.add_argument("--n", type=int, default=80, help="生成多少条 Agent query（默认 80）")
    p.add_argument("--per-batch", type=int, default=DEFAULT_PER_BATCH,
                   help=f"每批塞给模型的候选段落数（默认 {DEFAULT_PER_BATCH}；同批互为负例）")
    p.add_argument("--out", type=Path, default=Path("data/eval/t7-16"),
                   help="产物目录（默认 data/eval/t7-16）")
    p.add_argument("--seed", type=lambda s: int(s, 0), default=DEFAULT_SEED,
                   help=f"抽样/采样种子（默认 {DEFAULT_SEED}）")
    p.add_argument("--temperature", type=float, default=DEFAULT_TEMPERATURE,
                   help=f"采样温度（默认 {DEFAULT_TEMPERATURE}）")
    p.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT_S,
                   help=f"单次请求超时秒（默认 {DEFAULT_TIMEOUT_S}）")
    p.add_argument("--qid-prefix", default=DEFAULT_QID_PREFIX, help="qid 前缀")
    p.add_argument("--stub", type=Path, default=None,
                   help="离线桩：按序返回文件里 JSON 数组中的字符串（**不发网络**）")
    p.add_argument("--self-test", action="store_true",
                   help="跑 S9-T12 离线桩自检并退出（**零网络**）")
    return p.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.self_test:
        print("=== S9-T12：T7-16 生成器的离线桩自检（不发网络请求）===")
        return self_test()

    base_url = os.environ.get("HELIX_LLM_BASE_URL", "")
    api_key = os.environ.get("HELIX_LLM_API_KEY", "")
    model = os.environ.get("HELIX_LLM_MODEL", "")
    if not (base_url and model):
        print("❌ 缺少 HELIX_LLM_BASE_URL / HELIX_LLM_MODEL（D-S9-07 的通道三环境变量）", file=sys.stderr)
        return 2

    docs = load_corpus(args.corpus)
    corpus_sha = hashlib.sha256(args.corpus.read_bytes()).hexdigest()[:16]

    if args.stub is None:
        egress_guard(docs, args.n, base_url, env=dict(os.environ))
        if not api_key:
            print("❌ 真实生成还需 HELIX_LLM_API_KEY（密钥不入库）", file=sys.stderr)
            return 2
        host = re.sub(r"^https?://", "", base_url).split("/")[0]
        print(f"=== T7-16 生成（**外发已开启**）：{args.n} 条 ⇒ {host} / {model} ===")
        transport = lambda msgs, **kw: chat_once(  # noqa: E731
            base_url, api_key, model, msgs,
            temperature=args.temperature, seed=args.seed, timeout_s=args.timeout,
        )
        egress_note = {"approved_by_user": "2026-09-26（R58）", "endpoint_host": host,
                       "n_docs_to_send": args.n}
    else:
        stub_responses = json.loads(args.stub.read_text(encoding="utf-8"))
        if not isinstance(stub_responses, list):
            print("❌ --stub 文件须是 JSON 字符串数组", file=sys.stderr)
            return 2
        print(f"=== T7-16 生成（**离线桩**，{len(stub_responses)} 条响应，不发网络）===")
        transport = StubTransport([str(x) for x in stub_responses])
        egress_note = {"approved_by_user": "n/a（离线桩，未外发）", "endpoint_host": "<stub>",
                       "n_docs_to_send": 0}

    rows, meta = run_pipeline(
        docs, n=args.n, per_batch=args.per_batch, seed=args.seed, qid_prefix=args.qid_prefix,
        transport=transport,
        meta_extra={
            "asset": "t7-16-agent-queries",
            "built_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "egress": egress_note,
            "llm": {"model": model or "<stub>", "temperature": args.temperature,
                    "seed": args.seed, "timeout_s": args.timeout,
                    "base_url_host": egress_note["endpoint_host"]},
            "prompts": {
                "round1_text": SYS_ROUND1,
                "round1_sha256_16": hashlib.sha256(SYS_ROUND1.encode()).hexdigest()[:16],
                "round2_text": SYS_ROUND2,
                "round2_sha256_16": hashlib.sha256(SYS_ROUND2.encode()).hexdigest()[:16],
            },
            "corpus": {"path": str(args.corpus), "sha256_16": corpus_sha, "n_rows": len(docs)},
            "schema": "与 data/t2-queries.jsonl 同构；type 固定 \"agent\"（不冒充四桶）",
        },
    )
    jsonl, _ = write_assets(args.out, rows, meta)
    print(f"✅ 产出 {len(rows)} 条（自查通过率 {meta['counts']['pass_rate']}）→ {args.out}/agent-queries.jsonl")
    print(f"   元信息（R57：模型/prompt/种子）→ {args.out}/gen-meta.json")
    print(f"   对照跑法：./target/release/helix bench --index data/t2-frozen.snapshot "
          f"--queries {args.out}/agent-queries.jsonl --modes bm25,vector,hybrid --k 10 --runs 1 "
          f"--rrf-k 60 --rrf-weights 1.0,1.5 --no-latency --json <out>.json")
    print(f"   （⚠️ 四桶行恒 n=0 —— 本资产的 type 是 \"agent\"，对照取**全局行** + per_query 配对）")
    assert jsonl  # 保证产物非空（validate_asset 已断言 rows 非空）
    return 0


if __name__ == "__main__":
    sys.exit(main())
