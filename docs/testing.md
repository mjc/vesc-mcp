# Contributor testing

This guide is for people changing vesc-mcp source code. Users should install a
release archive instead; see [installation.md](installation.md).

The project follows red → green → refactor: add a failing test that describes
the behavior, make the smallest change that passes, then clean up without
changing behavior.

## Development requirements

- Rust 1.85 or newer
- `cargo-nextest`
- `make`
- optional `cargo-llvm-cov` for coverage

Clone the repository with its submodules before running catalog validation:

```bash
git clone --recurse-submodules <repository-url>
cd vesc-mcp
```

## Main checks

```bash
make check
cargo nextest run --workspace
cargo nextest run -p vesc-mcp-core -E 'test(fixtures_)'
cargo doc --workspace --no-deps
```

`make check` runs formatting, Clippy with warnings denied, the workspace test
suite, and documentation generation. The default fixture suite is offline and
does not require sibling source checkouts.

Nextest configuration lives in [`.config/nextest.toml`](../.config/nextest.toml).

## Test tiers

| Tier | Location | Purpose |
|------|----------|---------|
| Unit | `crates/*/src/**/*.rs` | Parsing and decision logic |
| Integration | `crates/*/tests/*.rs` | Crate boundaries and fixtures |
| MCP service | `crates/vesc-mcp-core/tests/*.rs` | Tool and resource routing |
| Transport | `crates/vesc-mcp-server/tests/*.rs` | stdio and Streamable HTTP behavior |

Synthetic packages live in [`tests/fixtures/`](../tests/fixtures/README.md).
Valid fixtures cover supported layouts; `broken-*` fixtures cover expected
error paths.

## TDD checklist

1. Add a failing test that names the behavior.
2. Run the narrowest relevant test and confirm the expected failure.
3. Implement the minimum change.
4. Run the narrow test, then `cargo nextest run --workspace`.
5. Refactor while tests remain green.
6. Use a `test(...)`, `feat(...)`, or `docs(...)` commit prefix and reference
   the relevant `VESCM-*` issue when one exists.

Integration tests should call tools through
`McpTestHarness::call_tool(name, json!({...}))` so they exercise the same
handlers as a live server.

## Focused checks

```bash
cargo nextest run -p vesc-mcp-core -E 'test(tool_)'
cargo nextest run -p vesc-domain -p vesc-mcp-core -E 'test(golden) | test(build_native_lib)'
cargo nextest run -p vesc-knowledge-index --features git-corpus -E 'binary(git_ingestion)'
cargo check -p vesc-knowledge-index
cargo check -p vesc-knowledge-index --features semantic-fastembed
```

The `git-corpus` tests create local repositories and require no network. The
semantic feature check compiles the local adapter but does not download a
model.

## Optional live-source tests

Ignored tests validate source attribution against local upstream checkouts.
Set only the roots you have, then run:

```bash
export VESC_REFLOAT_ROOT=/path/to/refloat
export VESC_ROOT=/path/to/vesc
export VESC_POC_ROOT=/path/to/vesc-rust-poc
cargo nextest run -p vesc-mcp-core --run-ignored all
```

Do not commit local paths or generated test output.

## Knowledge evaluation

Build and inspect a local artifact:

```bash
cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- build
cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- inspect
```

Run the locked evaluation gate:

```bash
cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- \
  evaluate --mode lexical --artifact target/knowledge-artifacts --gate --format json
```

The v1 thresholds are Recall@5 ≥ 0.90, MRR@10 ≥ 0.80, nDCG@10 ≥ 0.80, and
identifier top-1 = 1.0. A failed gate exits nonzero and reports affected query
IDs.

The in-process handler benchmark is:

```bash
cargo run --release -p vesc-mcp-server -- \
  --benchmark-search --artifact target/knowledge-artifacts \
  --warmup 3 --repetitions 10 --format json
```

This measures the search handler and JSON serialization, not MCP transport.
Record the operating system, architecture, corpus digest, warmup and
repetition counts, and memory measurement method with any performance claim.
Never record user names, hostnames, or personal paths.

## Tagged history ingestion

Build the version/change graph without running the embedding provider:

```bash
cargo run --release -p vesc-knowledge-index --features git-corpus \
  --bin gen-knowledge-index -- history-build --history-only \
  --repo /path/to/repository --repository refloat \
  --out target/tagged-history/refloat
```

Remove `--history-only` to build `vectors.json` through the persistent
content-addressed cache as well. Supply the normal `--semantic-*` model flags;
the Ryzen 5 8600G + RX 5700 XT hardware profile selects its measured FP16
MIGraphX ingestion configuration automatically. `--cache PATH` keeps the
cache outside the output directory when generations should share it.

`history.json` preserves tag aliases, release ancestry, version occurrences,
and added/modified/moved/removed evidence. `vectors.json` stores each unique
embedding input once and expands matching version occurrences only after
semantic scoring. Re-running the same embedding contract uses completed cache
batches, including after a truncated final cache write.

## Semantic evaluation

Semantic tests require a separately provisioned, pinned local model. The
normal server and builder never download one automatically.

```bash
cargo run --release -p vesc-knowledge-index \
  --features semantic-fastembed --bin gen-knowledge-index -- \
  evaluate --mode hybrid --artifact target/knowledge-artifacts-semantic \
  --semantic-model-dir /path/to/pinned-model \
  --semantic-model-id Xenova/bge-small-en-v1.5 \
  --semantic-model-revision <revision-from-manifest>
```

Keep run-specific results in CI artifacts or the task tracker rather than
committing workstation-specific reports. See
[provider-profiling.md](provider-profiling.md) for the current provider
recommendation.

### Semantic diagnostics

Before a costly semantic run, inspect the corpus's token lengths without
performing inference:

```bash
cargo run --release -p vesc-knowledge-index \
  --features semantic-fastembed --bin gen-knowledge-index -- \
  benchmark --mode semantic --artifact target/knowledge-artifacts-semantic \
  --semantic-model-dir /path/to/pinned-model \
  --semantic-model-id Xenova/bge-small-en-v1.5 \
  --semantic-model-revision <revision-from-manifest> \
  --semantic-token-statistics-only --format json
```

To measure the worst inputs, replace `--semantic-token-statistics-only` with:

```text
--semantic-longest-chunks 1 --semantic-batch-size 1 \
  --warmup 0 --repetitions 1
```

Increase the chunk count only after the single-input probe fits the available
memory.

The benchmark defaults to ONNX Runtime graph-optimization level 3. Use
`--semantic-graph-optimization-level 0`, `1`, `2`, or `3` only when comparing
runtime behavior. Use `--semantic-max-length` to benchmark a shorter input
length without changing the registered model profile; the override cannot
exceed the profile maximum. The build command accepts the same lower override;
configure `[knowledge.semantic] max_length` to match it at query time.

## Coverage

The line-coverage floor is 80% for library source in `vesc-domain`,
`vesc-knowledge-index`, `vesc-mcp-adapters`, and `vesc-mcp-core`. Policy and
exclusions live in [`.config/coverage.toml`](../.config/coverage.toml) and
[`.config/coverage-exclude.regex`](../.config/coverage-exclude.regex).

```bash
make coverage
make coverage-summary
make coverage-html
```

Coverage is report-only in CI. The server bootstrap, build scripts, binaries,
and vendored code are excluded from the per-crate floor.
