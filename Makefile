.PHONY: all fmt lint test deny shell doc tree clean eval-quality eval-perf report

all: fmt lint test deny shell

fmt:
	cargo fmt --all

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

# ADR-008 / NFR-09（依赖 License 白名单）+ 依赖安全公告（拟登记 R42）校验。
# 首次运行会自动安装 cargo-deny（约 2~5 分钟）。
#
# ⚠️ 2026-09-12：**把 advisories 也纳入这道门**。此前它被排除在外，而 main 上
# `cargo deny check advisories` 本来就是 FAILED（两条 `unmaintained`、均无升级路径）⇒
# 「排除」等于这道门不存在。现改为：在 `deny.toml` 对那两条**精确到 ID** 地 ignore
# （每条都写了「核实日期 + 可判定的解除条件」），并让本目标真的跑它 ⇒ **将来新增的公告仍会变红**。
#
# ⚠️ **本目标需要联网**：advisories 会拉 RustSec 数据库（缓存 `~/.cargo/advisory-db`）。
#    断网 / 防火墙拦 GitHub 时会在这一步失败，**且失败原因与代码无关**；
#    离线用 `cargo deny --offline check advisories`（走本地缓存）。
# 🔁 复核当前生效的 ignore 与理由：`cargo deny -L info check advisories`。
deny:
	@command -v cargo-deny >/dev/null 2>&1 || \
	  (echo "==> 首次安装 cargo-deny（约 2~5 分钟）..." && cargo install cargo-deny --locked --version 0.20.2)
	cargo deny check advisories licenses bans sources

# 静态检查：scripts/*.sh 里的 `$VAR` 紧跟非 ASCII 字符
# 原因：bash 3.2（macOS 自带）在多字节 locale 下不把非 ASCII 字节当作变量名终止符，
#       `"$VAR（"` 会被解析成一个变量名 ⇒ `set -u` 报 unbound variable 并中止；
#       `LC_ALL=C` 下不触发，故本地裸跑可能漏掉，须进守门
shell:
	python3 scripts/check_shell_expansion.py

doc:
	cargo doc --workspace --no-deps

tree:
	cargo tree --workspace

# ---- v1 收尾：评测脚本（详见 docs/devel/v1-finish-design.md）----

# 效果评测：三路对照（固化 P5 定稿参数，一键复现结论）
# 例：make eval-quality ARGS="--grid" / ARGS="--modes bm25 --analyzer charabia"
eval-quality:
	./scripts/eval_quality.sh $(ARGS)

# 性能/NFR 实测：NFR-02~05 达标判定表（本地跑，勿用 CI 共享 runner 的数字）
eval-perf:
	./scripts/eval_perf.sh $(ARGS)

# 由 bench --json 生成 markdown 表格，消除手工抄录
# 例：make report JSON=scripts/fixtures/p5-final-3way.json
#     make report JSON=... CHECK=--check    # 逐格对账模式
JSON ?= scripts/fixtures/p5-final-3way.json
report:
	python3 scripts/report.py $(JSON) $(CHECK)

clean:
	cargo clean
