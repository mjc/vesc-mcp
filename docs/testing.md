# Testing and TDD Workflow

vesc-mcp follows **red → green → refactor** for every feature. Commit a failing test before implementation, make it pass with the smallest change, then refactor with tests still green.

## Quick commands

```bash
nix develop -c make check          # fmt + clippy + nextest + doc
nix develop -c cargo nextest run --workspace
nix develop -c cargo nextest run -p vesc-mcp-core -E 'test(fixtures_)'
nix develop -c cargo nextest run -p vesc-knowledge-index --features git-corpus -E 'binary(git_ingestion)'
nix build
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- build
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- inspect
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- evaluate --mode legacy --format text
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- evaluate --mode lexical --format json
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- evaluate --mode lexical --artifact target/knowledge-artifacts --gate --format json
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- evaluate --mode all --artifact target/knowledge-artifacts --format text
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- benchmark --artifact target/knowledge-artifacts --format json
nix develop -c cargo run -p vesc-mcp-server -- --benchmark-search --artifact target/knowledge-artifacts --warmup 3 --repetitions 10 --format json
```

`evaluate --gate` applies the locked v1 targets (Recall@5 >= 0.90, MRR@10 >=
0.80, nDCG@10 >= 0.80, and identifier top-1 = 1.0). A failed gate exits
nonzero and includes failed metrics plus the affected query IDs and returned
top-five IDs. `benchmark` records warmup/repetition counts, corpus/artifact
size, build/load/query/fusion percentiles, response-size percentiles, and
retained-RSS measurements. Evaluation reports also include duplicate rate
and diversity at five, plus deterministic category/source-family breakdowns
derived from the judged relevant IDs. Reports contain no timestamps or network
data.

`vesc-mcp-server -- --benchmark-search` measures the in-process search handler
plus JSON serialization across the locked suite. It deliberately does not
claim to measure stdio JSON-RPC transport; record that limitation with any
latency result. The benchmark uses the active artifact generation, so rebuilding
the corpus changes both the corpus digest and the cache key.

Configuration lives in [`.config/nextest.toml`](../.config/nextest.toml). The `ci` profile enables fail-fast and one retry.

The `git-corpus` fixture suite creates local bare repositories and reads exact
commit trees through `gix`; it requires no checkout or network. It covers the
reviewed VESC, VESC Tool, and Refloat source families plus deterministic
artifacts, provenance, resource bounds, and unsafe/non-text entry rejection.

## Test tiers

| Tier | Location | Examples |
|------|----------|----------|
| Unit | `crates/*/src/**/*.rs` (`#[cfg(test)]`) | `parse_pkgdesc_qml`, `decide_ping_echo` |
| Integration | `crates/*/tests/*.rs` | `fixtures_refloat_minimal_validates` |
| MCP service | `crates/vesc-mcp-core/tests/*.rs` | tool/resource routing through `McpTestHarness` |
| Transport smoke | `crates/vesc-mcp-server/tests/*.rs` | stdio and Streamable HTTP behavior |

## Fixtures

Synthetic workspaces live under [`tests/fixtures/`](../tests/fixtures/). See [`tests/fixtures/README.md`](../tests/fixtures/README.md) for the catalog of valid and broken layouts.

Use helpers from `vesc_mcp_core::test_support`:

```rust
use vesc_mcp_core::test_support::{TempWorkspace, fixture_path, read_fixture_file};

let root = fixture_path("refloat-minimal");
let pkgdesc = read_fixture_file("refloat-minimal", "pkgdesc.qml");
```

`TempWorkspace` creates an isolated temp directory (ported from vesc-rust-poc `test_support.rs`).

## TDD checklist for agents

1. **RED** — Add a failing test that names the behavior (e.g. `inspect_pkgdesc_returns_json_for_refloat_fixture`).
2. **GREEN** — Implement the minimum code to pass; run `cargo nextest run --workspace`.
3. **REFACTOR** — Extract shared logic into domain or `test_support`; keep tests green.
4. Commit with `test(...)` or `feat(...)` prefix and reference the Lific `VESCM-*` issue when applicable.

## Optional live-repo tests

Some catalog tests require sibling checkouts. Set env vars and run ignored tests explicitly:

```bash
export VESC_REFLOAT_ROOT=~/projects/refloat
export VESC_BLDC_ROOT=~/projects/bldc
export VESC_POC_ROOT=~/projects/vesc-rust-poc
nix develop -c cargo nextest run -p vesc-mcp-core --run-ignored all
```

The clean-start retrieval check is intentionally offline:

```bash
nix develop -c cargo check -p vesc-knowledge-index
nix develop -c cargo check -p vesc-knowledge-index --features semantic-fastembed
```

The first command proves the default path does not compile the optional
embedding adapter; the second checks the runnable ONNX-backed adapter and does
not provision or download a model. Provisioning is a separate, explicit
`semantic-fastembed-online` command. A real hybrid evaluation is:

```bash
nix develop -c cargo run -p vesc-knowledge-index --features semantic-fastembed --bin gen-knowledge-index -- \
  evaluate --mode hybrid --artifact target/knowledge-artifacts-semantic \
  --semantic-model-dir target/models/bge-small-en-v1.5 \
  --semantic-model-id Xenova/bge-small-en-v1.5 \
  --semantic-model-revision <revision-from-manifest>
```

The hybrid path uses a shallow lexical floor: RRF records overlapping semantic
evidence, semantic-only chunks can fill gaps, and top lexical evidence cannot
be displaced by an uncalibrated model. Keep run-specific metrics in Lific or CI
artifacts rather than this guide. The default server path remains
lexical/offline; hybrid is available only when a pinned local semantic
capability is explicitly configured.

## Semantic benchmark reports

Semantic measurements keep model inference separate from exact vector search and
record the model ID/revision, corpus digest, build identity, batch size, vector
artifact size, warmup/repetition counts, cold initialization and first-query-
after-build timings, timing percentiles, and retained-RSS measurements. The
RSS fields are `rss_before_queries_bytes`, `rss_after_queries_bytes`, and
`rss_retained_delta_bytes`; they are not peak RSS. Use `--semantic-batch-sizes
4,8,16,32,64` to emit one JSON/Markdown batch-sweep report while reusing the
same initialized model.
Use a pinned local model and select JSON for machine-readable storage or
Markdown for review:

```bash
nix develop -c cargo run -p vesc-knowledge-index --release \
  --features semantic-fastembed --bin gen-knowledge-index -- \
  benchmark --mode semantic \
  --artifact target/knowledge-artifacts-semantic-v2 \
  --semantic-model-dir target/models/bge-small-en-v1.5 \
  --semantic-model-id Xenova/bge-small-en-v1.5 \
  --semantic-model-revision <revision-from-manifest> \
  --semantic-batch-size 8 --limits 5,10,20,50 \
  --warmup 3 --repetitions 10 --format json
```

Run the semantic command through `nix develop`; the dev shell supplies the
matching ONNX Runtime shared library. If the command is launched outside the
Nix shell, the adapter fails fast with an actionable runtime error rather than
allowing the loader to stall.

Run the command in release mode for production numbers. Capture peak RSS
separately with the host tool (`/usr/bin/time -lp` on macOS,
`/usr/bin/time -v` on Linux), `getrusage`, or a sampling harness; do not put
that external peak result in the retained-RSS comparison fields or table. The
benchmark does not claim to measure MCP transport overhead.
The vector artifact format is versioned; rebuild artifacts generated before
the current dense-cosine-v2 format before benchmarking them.

## Negative fixtures

Broken fixtures under `tests/fixtures/broken-*` drive validation tests. A test asserting missing assets or bad wire bytes should **pass** when the fixture is broken; the tool under test should return errors when pointed at those paths.

## CI

GitHub Actions runs `nix develop -c make check`, which invokes `cargo nextest run --workspace`. No external repos are required for the default fixture suite.

A separate **coverage** job (report-only) runs `cargo llvm-cov` and uploads `lcov.info` as an artifact.

## Coverage

Per-crate **line coverage floor: 80%** for `vesc-domain`, `vesc-knowledge-index`, `vesc-mcp-adapters`, and `vesc-mcp-core`. Policy and excludes are in [`.config/coverage.toml`](../.config/coverage.toml).

Excluded from reports: `vendor/` and std. See [`.config/coverage-exclude.regex`](../.config/coverage-exclude.regex).

```bash
nix develop -c make coverage          # workspace run
nix develop -c make coverage-summary  # per-crate lib src % vs 80% floor
nix develop -c make coverage-html     # HTML report (same exclusions)
```

After `make coverage`, open the HTML report or inspect a single crate:

```bash
cargo llvm-cov report -p vesc-mcp-core --summary-only
```

Excluded from the floor: `vesc-mcp-server` bootstrap, `build.rs`, `src/bin/*`, and `vendor/`.
