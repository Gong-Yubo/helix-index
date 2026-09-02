.PHONY: all fmt lint test deny doc tree clean

all: fmt lint test deny

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

doc:
	cargo doc --workspace --no-deps

tree:
	cargo tree --workspace

clean:
	cargo clean
