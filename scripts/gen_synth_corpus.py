#!/usr/bin/env python3
"""
生成**确定性**合成语料与查询（V2 Step 1 · S1-10 fixture）。

设计依据：`docs/devel/v2-step1-design.md` 未决问题 4 —— T12/T13/T14 需要的是
「选择度可控」，真实语料反而难保证，因此以确定性合成生成器为主。

产出（默认写到 `data/`）：
  synth-<N>-corpus.jsonl   每行 {"source","text","metadata"}
  synth-<N>-queries.jsonl  每行 {"qid","query","qtype","relevance":[{"grade","source"}]}
  synth-<N>-filters.json   过滤档位清单（含选择度与预期命中数），供 scripts/eval_filter.sh 驱动

# ⚠️ 用途边界（重要）

本 fixture **只用于性能与选择度测量**（延迟、返回条数、下推 vs post-filter 的
重合率）。它的相关性标注由构造规则直接生成（query 取自主题词表、相关文档 =
同主题文档），因此是**自我实现预言**——**不得用于任何相关性结论**
（不用于 MRR/NDCG 的横向比较，也不用于 Step 6 精排的效果验收）。
相关性字段存在的唯一理由，是让 `helix bench` 的效果阶段能跑通流程。

用法：
  python3 scripts/gen_synth_corpus.py                     # 10 万篇 + 200 query
  python3 scripts/gen_synth_corpus.py --n 10000           # 1 万篇（快，冒烟用）
  python3 scripts/gen_synth_corpus.py --n 100000 --queries 200 --out-dir data

同一 (--seed, --n, --queries) 必然逐字节复现。
"""

from __future__ import annotations

import argparse
import json
import random
import sys
from pathlib import Path

# ---------------------------------------------------------------------------
# 主题词表：20 个主题 × 8 个领域词。文本由「主题词 + 通用填充词」构成，
# 让向量路有真实的聚类结构（否则 10 万条随机向量的 HNSW 图退化，测不出东西）。
# ---------------------------------------------------------------------------
TOPICS: list[tuple[str, list[str]]] = [
    ("retrieval", ["倒排索引", "词项频率", "召回率", "分词器", "BM25", "查询扩展", "相关性排序", "检索延迟"]),
    ("vector", ["向量索引", "余弦相似度", "近邻搜索", "HNSW", "维度", "归一化", "近似检索", "嵌入模型"]),
    ("database", ["事务隔离", "行存", "写放大", "聚簇索引", "预写日志", "查询计划", "缓冲池", "MVCC"]),
    ("consensus", ["领导者选举", "日志复制", "法定人数", "一致性哈希", "任期", "脑裂", "状态机", "租约"]),
    ("compiler", ["语法树", "中间表示", "寄存器分配", "内联", "死代码消除", "类型推导", "代码生成", "窥孔优化"]),
    ("kernel", ["页表", "上下文切换", "中断", "伙伴分配器", "缺页异常", "调度器", "系统调用", "写时复制"]),
    ("network", ["拥塞控制", "滑动窗口", "三次握手", "TLS 握手", "路由表", "NAT 穿透", "重传", "队头阻塞"]),
    ("distributed", ["分片", "副本", "最终一致", "幂等", "限流", "熔断", "重试风暴", "灰度发布"]),
    ("security", ["密钥轮换", "签名验签", "零信任", "侧信道", "沙箱", "权限提升", "证书链", "审计日志"]),
    ("storage", ["列式存储", "压缩编码", "LSM 树", "布隆过滤器", "段合并", "快照", "写前日志", "冷热分层"]),
    ("mlops", ["特征工程", "模型漂移", "离线回溯", "批流一体", "超参搜索", "模型蒸馏", "量化", "评测集"]),
    ("nlp", ["注意力机制", "词嵌入", "序列标注", "语言模型", "提示词", "分词粒度", "语义匹配", "重排序"]),
    ("frontend", ["虚拟列表", "水合", "首屏渲染", "打包体积", "懒加载", "状态管理", "重排重绘", "服务端渲染"]),
    ("observability", ["链路追踪", "指标聚合", "日志采样", "基数爆炸", "告警收敛", "火焰图", "直方图", "根因定位"]),
    ("testing", ["回归测试", "变异测试", "覆盖率", "夹具", "幂等断言", "模糊测试", "黄金文件", "并发竞态"]),
    ("scheduler", ["时间轮", "优先级反转", "公平调度", "抢占", "协程", "批处理窗口", "延迟队列", "背压"]),
    ("streaming", ["水位线", "窗口聚合", "精确一次", "检查点", "乱序事件", "反压", "状态后端", "增量快照"]),
    ("api", ["版本兼容", "分页游标", "幂等键", "限流配额", "网关路由", "契约测试", "错误码", "批量接口"]),
    ("memory", ["对象池", "引用计数", "垃圾回收", "内存屏障", "缓存行", "伪共享", "逃逸分析", "内存泄漏"]),
    ("build", ["增量编译", "依赖图", "制品缓存", "交叉编译", "链接时优化", "产物指纹", "并行任务", "可复现构建"]),
]

# 通用填充词：与主题无关，制造噪声，避免「主题 → 文本」一一映射而过于干净
FILLER = [
    "在生产环境中", "通常情况下", "需要注意", "从工程角度看", "经过实测", "与此同时",
    "一个常见的问题是", "在实践中", "由于历史原因", "理论上讲", "就近期的观察",
    "对于大多数场景", "在规模较小时", "当负载升高时", "按经验值", "综合考虑",
]

# ---------------------------------------------------------------------------
# 元数据字段设计
# ---------------------------------------------------------------------------
N_TENANTS = 10  # tenant：10 个值 → 10% 选择度
N_CATEGORIES = 50  # category：50 个值 → 2% 选择度
TIERS = [("free", 0.70), ("pro", 0.25), ("enterprise", 0.05)]
SCORE_MAX = 1000  # score：0..999，中基数，供 Range 过滤（**未降级**）
TS_BASE_MS = 1_700_000_000_000  # ts_ms 基准（2023-11 前后）
TS_SPAN_MS = 10_000_000  # ts_ms 取值区间跨度（≈2.8 小时）；10 万条随机落其中，
#   碰撞约 500 个 ⇒ 基数仍远超 1024，**必然降级**（高基数字段，评审 P2-2 主场景）
UUID_ALPHABET = "0123456789abcdef"


def build_meta(rng: random.Random, args: argparse.Namespace) -> dict:
    """按文档序号构造 metadata（所有取值由 rng 决定，rng 序列固定 ⇒ 确定性）。"""
    tenant = f"tenant-{rng.randrange(N_TENANTS):02d}"
    category = f"cat-{rng.randrange(N_CATEGORIES):03d}"
    tier = rng.choices([t for t, _ in TIERS], weights=[w for _, w in TIERS], k=1)[0]
    score = rng.randrange(SCORE_MAX)
    return {
        "tenant": tenant,
        "category": category,
        "tier": tier,
        "score": score,
        # 高基数字段：**必然**触发基数保护（默认 1024），用于验证降级回退路径。
        # ⚠️ 区间内**随机**而非递增：递增时间戳上的 Range 档位等价于"取前 X 篇"，
        # 与按 i%20 分配的主题强相关，会把过滤档位退化成内容档位。
        "ts_ms": TS_BASE_MS + rng.randrange(TS_SPAN_MS),
        "uuid": "".join(rng.choice(UUID_ALPHABET) for _ in range(16)),
        # 精确选择度档位：先全部置 miss，随后按序号确定性地翻成 hit
        "sel_001": "miss",
        "sel_01": "miss",
        "sel_10": "miss",
    }


def build_text(topic_words: list[str], rng: random.Random) -> str:
    # ⚠️ 主题词密度是 fixture 鉴别力的关键：BM25 走 OR 语义，若每篇只含 3~4 个
    # 主题词、query 又要命中 2~4 个，那么「allowed 集合内匹配 query 的文档」会
    # 少到 oracle 自己都凑不满 K（实测降到 ~2 条），重合率因此失去鉴别力。
    # 取 5~8 / 共 8 词，保证 OR 语义下候选覆盖 ≈ 全库，过滤后基线完整。
    picked = rng.sample(topic_words, rng.randint(5, 8))
    fillers = rng.sample(FILLER, rng.randint(2, 4))
    parts = []
    for w in picked:
        parts.append(f"{rng.choice(fillers)}{w}是关键环节。")
    # 让文本长度有差异（BM25 的 b 参数需要长度分布）
    if rng.random() < 0.3:
        parts.append("此外，" + "，".join(rng.sample(topic_words, 3)) + "也需要一并考虑。")
    return "".join(parts)


def gen_corpus(args: argparse.Namespace) -> tuple[list[dict], list[int]]:
    """返回 (docs, 每篇文档的 topic 下标)。"""
    rng = random.Random(args.seed)
    n = args.n
    docs: list[dict] = []
    topic_of: list[int] = []

    for i in range(n):
        ti = i % len(TOPICS)
        topic_name, topic_words = TOPICS[ti]
        meta = build_meta(rng, args)
        meta["topic"] = topic_name
        docs.append(
            {
                "source": f"synth-{i:07d}",
                "text": build_text(topic_words, rng),
                "metadata": meta,
            }
        )
        topic_of.append(ti)

    # ---- 选择度档位：随机不重复抽样（rng 状态固定 ⇒ 仍是确定性的）----
    #
    # ⚠️⚠️ 这里**绝不能**用等距抽样（如 `i * 100`）：主题是按 `i % 20` 分配的，
    # 步长 100 与 20 不互质 ⇒ allowed 文档会**全部落在同一个主题**上，过滤档位
    # 与内容完全相关。首版就是这样，实测后果：只有 1/20 的 query 能返回 10 条，
    # 其余 19/20 返回 0 条，平均 1.83/10 —— 指标被数据缺陷而非实现问题主导。
    #
    # `ts_ms` 同理：递增时间戳上的 Range 档位等于"取前 X 篇"，同样是前缀相关。
    # 所以时间戳改为**区间内随机**，Range 档位才能均匀覆盖各主题。
    hit_001 = set(rng.sample(range(n), max(1, n // 1000)))  # 0.1%
    hit_01 = set(rng.sample(range(n), max(1, n // 100)))  # 1%
    hit_10 = set(rng.sample(range(n), max(1, n // 10)))  # 10%
    for i, d in enumerate(docs):
        d["metadata"]["sel_001"] = "hit" if i in hit_001 else "miss"
        d["metadata"]["sel_01"] = "hit" if i in hit_01 else "miss"
        d["metadata"]["sel_10"] = "hit" if i in hit_10 else "miss"

    return docs, topic_of


def gen_queries(docs: list[dict], topic_of: list[int], args: argparse.Namespace) -> list[dict]:
    """为每个主题抽取若干 query，相关文档 = 同主题文档（见文件头的用途边界警告）。"""
    rng = random.Random(args.seed + 1)
    n_topics = len(TOPICS)
    by_topic: dict[int, list[int]] = {t: [] for t in range(n_topics)}
    for i, t in enumerate(topic_of):
        by_topic[t].append(i)

    queries: list[dict] = []
    per_topic = max(1, args.queries // n_topics)
    for ti in range(n_topics):
        if len(queries) >= args.queries:
            break
        topic_name, topic_words = TOPICS[ti]
        pool = by_topic[ti]
        for k in range(per_topic):
            if len(queries) >= args.queries:
                break
            # 2~3 个关键词（配合 build_text 的 5~8 词密度，保证过滤后基线完整）
            kws = rng.sample(topic_words, rng.randint(2, 3))
            query_text = " ".join(kws)
            qid = f"q{ti:02d}-{k:02d}"
            # 相关文档：同主题文档里，包含 query 全部关键词的优先（grade 3/2），
            # 其余同主题文档 grade 1。最多标注 --max-rel 条以控制文件体积。
            hits = [i for i in pool if all(w in docs[i]["text"] for w in kws)]
            rest = [i for i in pool if i not in set(hits)]
            rel: list[dict] = []
            for rank, i in enumerate(hits[: args.max_rel]):
                rel.append({"grade": 3 if rank < 3 else 2, "source": docs[i]["source"]})
            for i in rest[: max(0, args.max_rel - len(rel))]:
                rel.append({"grade": 1, "source": docs[i]["source"]})
            queries.append(
                {
                    "qid": qid,
                    "query": query_text,
                    # 注意字段名是 `type`（core 的 Judgment 用 #[serde(rename = "type")]），
                    # 不是 qtype——写错会 "missing field `type`"
                    "type": topic_name,
                    "relevance": rel,
                }
            )
    return queries


def gen_filters(n: int) -> dict:
    """过滤档位清单：`scripts/eval_filter.sh` 按此驱动选择度扫描。"""
    return {
        "corpus_size": n,
        "levels": [
            {"name": "none", "spec": "", "selectivity": 1.0, "note": "无过滤（热路径基线，T14①）"},
            {
                "name": "sel-0.1%",
                "spec": "sel_001=hit",
                "selectivity": 0.001,
                "note": "极低选择度：R13 整图遍历最坏档，预期 vector 返回不足 k",
            },
            {
                "name": "sel-1%",
                "spec": "sel_01=hit",
                "selectivity": 0.01,
                "note": "低选择度：allowed(1e3) < ef(256) ⇒ 仍整图遍历",
            },
            {
                "name": "sel-10%",
                "spec": "sel_10=hit",
                "selectivity": 0.1,
                "note": "中选择度：allowed(1e4) > ef ⇒ 剪枝生效，fast 路径",
            },
            {
                "name": "tenant-10%",
                "spec": "tenant=tenant-03",
                "selectivity": 0.1,
                "note": "自然分布字段（对照 sel-10% 的人为均匀分布）",
            },
            {
                "name": "tier-5%",
                "spec": "tier=enterprise",
                "selectivity": 0.05,
                "note": "非均匀自然分布（权重 0.05）",
            },
            {
                "name": "score-range",
                "spec": "score>=0,score<100",
                "selectivity": 0.1,
                "note": "Range 过滤（**未降级**字段）：下推有效路径",
            },
            {
                "name": "ts-range-degraded",
                # 区间前 1/1000 ⇒ 精确 0.1%；ts_ms 在区间内随机 ⇒ 覆盖全部主题
                "spec": f"ts_ms>={TS_BASE_MS},ts_ms<{TS_BASE_MS + TS_SPAN_MS // 1000}",
                "selectivity": 0.001,
                "note": "**降级字段 Range**（评审 P2-2 第一用例）：Q-I1 对它整体失效，"
                "预期与全扫等价；该档位用于量化这个代价而非验证优化",
            },
        ],
    }


def main() -> int:
    ap = argparse.ArgumentParser(description="生成确定性合成语料（S1-10 fixture）")
    ap.add_argument("--n", type=int, default=100_000, help="文档数（默认 100000）")
    ap.add_argument("--queries", type=int, default=200, help="query 数（默认 200）")
    ap.add_argument("--seed", type=int, default=20260905, help="随机种子（默认 20260905）")
    ap.add_argument("--max-rel", type=int, default=30, help="每 query 最多标注条数（默认 30）")
    ap.add_argument("--out-dir", type=Path, default=Path("data"), help="输出目录（默认 data/）")
    args = ap.parse_args()

    if args.n < 1 or args.queries < 1:
        print("错误：--n / --queries 必须为正", file=sys.stderr)
        return 2

    args.out_dir.mkdir(parents=True, exist_ok=True)
    tag = f"synth-{args.n}"
    corpus_path = args.out_dir / f"{tag}-corpus.jsonl"
    queries_path = args.out_dir / f"{tag}-queries.jsonl"
    filters_path = args.out_dir / f"{tag}-filters.json"

    print(f"生成语料：{args.n} 篇（seed={args.seed}）...")
    docs, topic_of = gen_corpus(args)
    print(f"生成查询：{args.queries} 条...")
    queries = gen_queries(docs, topic_of, args)

    with corpus_path.open("w", encoding="utf-8") as f:
        for d in docs:
            f.write(json.dumps(d, ensure_ascii=False) + "\n")
    with queries_path.open("w", encoding="utf-8") as f:
        for q in queries:
            f.write(json.dumps(q, ensure_ascii=False) + "\n")
    filters_path.write_text(
        json.dumps(gen_filters(args.n), ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )

    # 自检：档位命中数是否与声明的选择度吻合（避免档位漂移却无人察觉）
    print("\n档位自检（实际命中 / 声明选择度）：")
    for lv in gen_filters(args.n)["levels"]:
        if not lv["spec"]:
            print(f"  {lv['name']:<20} {args.n:>8}  (无过滤)")
            continue
        conds = [c for c in lv["spec"].split(",")]
        cnt = 0
        for d in docs:
            ok = True
            for c in conds:
                if ">=" in c:
                    f_, v = c.split(">=")
                    ok &= float(d["metadata"][f_]) >= float(v)
                elif "<" in c:
                    f_, v = c.split("<")
                    ok &= float(d["metadata"][f_]) < float(v)
                elif "=" in c:
                    f_, v = c.split("=")
                    ok &= str(d["metadata"][f_]) == v
            cnt += ok
        flag = "" if abs(cnt / args.n - lv["selectivity"]) < 0.02 else "  ⚠️ 与声明不符"
        print(f"  {lv['name']:<20} {cnt:>8}  ({cnt / args.n:.4%} / 声明 {lv['selectivity']:.4%}){flag}")

    print(f"\n已写出：\n  {corpus_path}\n  {queries_path}\n  {filters_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
