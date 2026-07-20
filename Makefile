CARGO ?= cargo

.DEFAULT_GOAL := check

.PHONY: check test fmt clippy feature-boundaries doc clean coverage coverage-html coverage-summary

COVERAGE_FLAGS = --workspace --profile ci --features vesc-mcp-core/test-fixtures
COVERAGE_IGNORE = $(shell awk 'substr($$0, 1, 1) != sprintf("%c", 35) { print; exit }' .config/coverage-exclude.regex)
LCOV_INFO ?= lcov.info

check: fmt clippy feature-boundaries test doc

test:
	$(CARGO) nextest run $(COVERAGE_FLAGS)

coverage:
	$(CARGO) llvm-cov nextest $(COVERAGE_FLAGS) \
		--ignore-filename-regex '$(COVERAGE_IGNORE)' \
		--lcov --output-path '$(LCOV_INFO)'
	@bash scripts/coverage-summary.sh '$(LCOV_INFO)'

coverage-html:
	$(CARGO) llvm-cov nextest $(COVERAGE_FLAGS) --html \
		--ignore-filename-regex '$(COVERAGE_IGNORE)'

coverage-summary:
	@bash scripts/coverage-summary.sh '$(LCOV_INFO)'

fmt:
	$(CARGO) fmt --all --check

clippy:
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

feature-boundaries:
	$(CARGO) check -p vesc-mcp-core --no-default-features

doc:
	RUSTDOCFLAGS='-D warnings' $(CARGO) doc --workspace --no-deps

clean:
	$(CARGO) clean
