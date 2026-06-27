CARGO ?= cargo

.DEFAULT_GOAL := check

.PHONY: check test fmt clippy doc clean

check: fmt clippy test doc

test:
	$(CARGO) nextest run --workspace --profile ci --features vesc-mcp-core/test-fixtures

fmt:
	$(CARGO) fmt --all --check

clippy:
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

doc:
	$(CARGO) doc --workspace --no-deps

clean:
	$(CARGO) clean
