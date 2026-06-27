CARGO ?= cargo

.DEFAULT_GOAL := check

.PHONY: check test fmt clippy doc clean coverage coverage-html coverage-summary

COVERAGE_FLAGS = --workspace --profile ci --features vesc-mcp-core/test-fixtures
COVERAGE_IGNORE = $(shell tr -d '\n#' < .config/coverage-exclude.regex | head -1)

check: fmt clippy test doc

test:
	$(CARGO) nextest run $(COVERAGE_FLAGS)

coverage:
	$(CARGO) llvm-cov nextest run $(COVERAGE_FLAGS) \
		--ignore-filename-regex '$(COVERAGE_IGNORE)'

coverage-html:
	$(CARGO) llvm-cov nextest run $(COVERAGE_FLAGS) --html \
		--ignore-filename-regex '$(COVERAGE_IGNORE)'

coverage-summary:
	@bash scripts/coverage-summary.sh

fmt:
	$(CARGO) fmt --all --check

clippy:
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

doc:
	$(CARGO) doc --workspace --no-deps

clean:
	$(CARGO) clean
