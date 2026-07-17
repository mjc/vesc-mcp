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
- Keep CPU as the default execution provider. Tested accelerator paths did
  not improve this workload enough to justify their additional runtime and
  memory requirements.
- Keep the semantic build batch size at 8 by passing
  `--semantic-batch-size 8` to `gen-knowledge-index build`. This is a build-time
  command option, not a server configuration key. Larger batches provided
  small throughput gains at a steep memory cost.
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
candidate. Diagnostic probes may select fewer complete chunks instead.

## What to record when comparing providers

Performance results are meaningful only when they include the operating
system, CPU architecture, model ID and revision, corpus digest, batch size,
warmup and repetition counts, latency percentiles, and memory measurement
method. Do not publish user names, hostnames, personal paths, or unrelated
hardware inventory.

See [configuration.md](configuration.md#knowledge-search) to enable a local
model and [testing.md](testing.md) for contributor benchmarks.
