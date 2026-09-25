# `data/eval/s9-2026-09-25/` —— spike S9-S1 的**持久读数产物**

本目录存放 V2 Step 9 · **PR9-2**（`scripts/eval_s9.sh`，S9-04 / S9-T11）在
**2026-09-25** 那一轮跑出的判定读数，供设计文档 `docs/devel/v2-step9-design.md`
**§4.13**（「判『不投』」的结论）与 `eval-report.md` §8.16（**PR9-4** 交付）**指认来源**。

## 为什么只有 TSV

第 1 轮评审 **P3-3** 指出：脚本原先把全部产物落 `mktemp -d`，临时目录一清，
§4.13.3 说的「21 点全表见脚本产物 `readings.tsv`」就**无处可指**（评审本机全盘搜过、零命中）。
⇒ 采纳该条：脚本加 `OUT_DIR=<dir>`（跑完并判定成功之后复制 TSV），**并把本轮 TSV 入库**。

臂级原始 JSON（65 份）**不入库**（体积大）；要它们就重跑脚本 —— 路径在脚本输出末尾打印。
⇒ 「读数可指到产物」这条对 **TSV** 成立，对**臂级 JSON** 不成立（设计 §4.13.7 已如实登记）。

## 文件

| 文件 | 内容 |
| --- | --- |
| `readings.tsv` | 315 行数据 = **3 信号 × 21 θ 点 × 5 口径**（exact / natural / mixed / paraphrase / global）+ 8 行头部元信息 |

`readings.tsv` 头部自带（⇒ 产物**自证**，不依赖本 README）：

```
# index=<冻结图路径>
# index_sha256_16=de7b3d1a8250a2d5 index_bytes=54844663
# queries=data/t2-queries.jsonl n_queries=320
# buckets=exact natural mixed paraphrase n_per_bucket=80
# k=10 rrf_k=60 baseline_weights=1.0,1.5 rerank=off runs=1
# gates: G1>=0.01 G2>=-0.0024 G3>=0.0 G4_min_consecutive=3
# qr1 paraphrase vector-minus-hybrid=+0.0592063492 n=80
# qr1 global     vector-minus-hybrid=-0.0112834821 n=320
```

⚠️ **两个 `qr1` 行不可互换**（第 1 轮评审 P3-1）：`paraphrase` 桶的 `+0.0592` 是 §4.13.2 声称的
「Q-R1 已复现」所指的那个量；`global` 的 `−0.0113` 是**同式的全量口径**——两者**符号相反**。

## 复现

```bash
OUT_DIR=data/eval/$(date +%F) ./scripts/eval_s9.sh
```

⚠️ **读数与机器无关（效果类），但与冻结图强相关**：脚本会把本轮冻结图的 sha256 前 16 写进 TSV 头；
换图之后读数值**不可与本目录的比**（跨口径数字不可相减，`eval-report.md` §9 的既有纪律）。

## 结论速览（细节见设计 §4.13）

- 三信号**皆判「不投」**（五门合取，全是 G4 这一关过不去）：`overlap` 最长连续达标 2 点（需 ≥3）；
  `df` 覆盖率在本语料上几乎恒 1.0（惰性）；`shape` paraphrase **+0.0592** 但 exact / natural 被「一刀降 0」砍掉。
- 确定性：**三轮全量复跑**，63 行曲线**逐位相同**。
