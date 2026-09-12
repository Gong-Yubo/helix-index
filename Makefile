.PHONY: all fmt lint test deny shell doc tree clean eval-quality eval-perf report

all: fmt lint test deny shell

fmt:
	cargo fmt --all

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

# ADR-008 / NFR-09：依赖 License 白名单校验。首次运行会自动安装 cargo-deny（约 2~5 分钟）
deny:
	@command -v cargo-deny >/dev/null 2>&1 || \
	  (echo "==> 首次安装 cargo-deny（约 2~5 分钟）..." && cargo install cargo-deny --locked)
	cargo deny check licenses bans sources

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
