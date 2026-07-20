# Semantic search guidance

Semantic search is optional. The release-safe default is lexical search,
which is offline, deterministic, and strongest for exact firmware symbols.

## Current recommendation

- The Nix package uses `jinaai/jina-embeddings-v2-base-code` at revision
  `516f4baf13dec4ddddda8631e019b5737c8bc250`: FP16 for the measured RX 5700 XT
  ingestion path and INT8 on CPU for queries. The package pins queries to the
  validated 512-token contract.
- Use `auto` so an unavailable semantic runtime degrades to lexical search with
  a warning. Use explicit `hybrid` only when a capability error should stop the
  request, or `lexical` when semantic search is unwanted.
- Keep CPU as the default query execution provider. The RX 5700 XT exception
  below uses MIGraphX only for bulk ingestion.
- Keep `Xenova/bge-small-en-v1.5` at revision
  `ea104dacec62c0de699686887e3f920caeb4f3e3` only as a legacy fallback with a
  matching BGE vector artifact.

## Why lexical fallback remains required

Semantic and hybrid retrieval improve conceptual code queries, while lexical
retrieval remains strongest for exact firmware symbols and continues to work
without a model. The packaged `auto` mode retains both behaviors.

The server never downloads a model at startup. A semantic setup must provide
a local model directory, model ID, and exact revision matching the vector
artifact manifest.

Full-corpus semantic ingestion must not silently truncate input. It must use
the model's declared limit with lossless token-aware windowing or reject the
candidate. Pass `--semantic-lossless-windows` to `gen-knowledge-index build`
to preserve oversized documents by combining their window vectors. Diagnostic
probes may select fewer complete chunks instead.

The registered model profile sets the upper input length. The builder and
benchmark accept a lower `--semantic-max-length`; the query server must use the
same `[knowledge.semantic] max_length` value. The override cannot exceed the
registered maximum.

## RX 5700 XT Jina split preset

[`scripts/rx5700xt-jina-split.sh`](../scripts/rx5700xt-jina-split.sh) is the
reproducible exception for this exact combination:

- AMD Ryzen 5 8600G plus AMD Radeon RX 5700 XT (`gfx1010`), device 0
- `jinaai/jina-embeddings-v2-base-code` at revision
  `516f4baf13dec4ddddda8631e019b5737c8bc250`
- FP16 MIGraphX ingestion at 64 tokens, batch 64, lossless windows, and
  token-weighted mean pooling
- CPU INT8 queries at 512 tokens

When that CPU and PCI device `0x731f` are present, both pinned local ONNX files
match, and no semantic configuration was explicitly supplied, `build`
automatically selects the ingestion settings above. After the artifact exists,
the server automatically selects its CPU INT8 query model. The tracked
[`config.rx5700xt-jina-split.toml`](../config.rx5700xt-jina-split.toml) mirrors
the resolved query configuration for an explicit override.

The helper refuses a different CPU/GPU target or either unpinned ONNX file. Run
a small 16-chunk validation before committing to full ingestion:

```bash
scripts/rx5700xt-jina-split.sh verify
scripts/rx5700xt-jina-split.sh smoke
scripts/rx5700xt-jina-split.sh ingest
scripts/rx5700xt-jina-split.sh serve --http
```

This is scoped to the measured RX 5700 XT and pinned Nix ROCm/MIGraphX stack;
it is not a recommendation for RDNA1 generally. The Refloat all-tags cold run
processed 49 tags, 47 releases, 25,701 occurrences, and 3,506 unique embedding
inputs in 1 minute 50.48 seconds, with 3.42 GiB peak RSS. An immediate warm
repeat reused all 3,506 vectors without initializing ONNX and peaked at about
420 MiB RSS.

The Radeon 760M iGPU is not selected automatically. A July 19 retest of this
exact FP16 model reset the iGPU at both batch 8 and isolated batch 1 before a
timing report was produced. That conflicts with an earlier successful run and
needs a rebooted stability investigation before performance comparison.

## What to record when comparing providers

Performance results are meaningful only when they include the operating
system, CPU architecture, model ID and revision, corpus digest, batch size,
warmup and repetition counts, latency percentiles, and memory measurement
method. Do not publish user names, hostnames, personal paths, or unrelated
hardware inventory.

See [configuration.md](configuration.md#knowledge-search) to enable a local
model and [testing.md](testing.md) for contributor benchmarks.
