# Semantic search guidance

Semantic search is optional. The release-safe default is lexical search,
which is offline, deterministic, and strongest for exact firmware symbols.

## Current recommendation

- Use `lexical` unless conceptual queries materially benefit from a local
  semantic model.
- Use `Xenova/bge-small-en-v1.5` at revision
  `ea104dacec62c0de699686887e3f920caeb4f3e3`, with
  `onnx/model_quantized.onnx` provisioned locally as `model.onnx`. This pinned
  bakeoff identity is recorded in
  [`tests/benchmark/bakeoff-models.json`](../tests/benchmark/bakeoff-models.json).
- Keep CPU as the default query execution provider. The RX 5700 XT exception
  below uses MIGraphX only for bulk ingestion.
- Keep the semantic build batch size at 8 and enable stable length bucketing by
  passing `--semantic-batch-size 8 --semantic-length-bucketed true` to
  `gen-knowledge-index build`. These are build-time command options, not server
  configuration keys. Larger batches provided small throughput gains at a
  steep memory cost.
- Use `auto` for graceful fallback to lexical results. Use explicit `hybrid`
  only when a capability error should stop the request.

## Why semantic search is not the default

The evaluated semantic and hybrid artifacts improved some conceptual queries
but did not pass all locked retrieval-quality thresholds. Exact identifier
quality remains especially important for firmware and ABI work. Semantic
artifacts are therefore opt-in rather than release defaults.

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
- FP16 MIGraphX ingestion at 512 tokens, batch 8, lossless windows, and stable
  length bucketing
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
it is not a recommendation for RDNA1 generally. The 128-chunk ingestion probe
measured 17.45 chunks/s (32.44 windows/s), projecting roughly 42--50 minutes
for the full corpus. The projection is not a completed full ingestion result.

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
