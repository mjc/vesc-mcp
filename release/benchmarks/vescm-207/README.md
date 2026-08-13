# VESCM-207 CodeGraphRAG baseline

This directory locks the judged relationship corpus and the real-service
baseline used before graph ranking changes. The source corpus is
[`tests/evaluation/v4/queries.json`](../../../tests/evaluation/v4/queries.json)
and is copied here as `queries.json` for release-evidence consumers.

## Capture

Run twice against the authenticated local HTTP service, pinning the immutable
snapshot reported by the first run:

```sh
export VESC_MCP_HTTP_AUTH_TOKEN
nix develop -c python3 scripts/benchmark-vescm-207-baseline.py \
  --snapshot-id 1216231c4cc2f5df69ad9759ff3d3755fad25d6fe7fca69275ab793ef869834c \
  --output release/benchmarks/vescm-207/run-1.json
```

The checked-in captures use snapshot
`1216231c4cc2f5df69ad9759ff3d3755fad25d6fe7fca69275ab793ef869834c` and
corpus digest
`sha256:d4d385aab3f913b91552f38b5f0573112f38ebe35e78223d938fcd22ed3ca1ea`.
`run-1.json` and `run-2.json` have identical judged-output digests; the
comparison is summarized in `report.json`. Timing
and transport bytes are reported separately and are intentionally excluded
from that digest. Each capture runs every case through lexical, hybrid, and
auto mode; `auto` is the production fallback path.

## What the branch-added tests do

- `test_parse_sse_ignores_keepalive_and_malformed_events` protects parsing of
  streamable HTTP keep-alive frames and JSON-RPC events.
- `test_completeness_requires_reviewed_path_and_terms` prevents a hit with the
  right words but the wrong source path from counting as decisive evidence.
- `test_digest_excludes_transport_metrics` keeps the repeatability gate focused
  on ordered evidence identity rather than noisy latency or byte counts.
- `test_observed_trace_is_distinct_from_bounded_evaluation` keeps the six reads
  observed in the preserved historical session separate from the zero reads
  required when the first response contains all reviewed spans.

## Baseline result

The two captures contain 12 query cases × 3 retrieval modes (36 captures per
run) and 9 reviewed evidence targets. All 9 targets are present in each mode's default
`detail=full` first response, with zero same-file reads required by the
bounded-response evaluation. The preserved
historical trace records six manual reads before the completeness improvements.
The adversarial controls cover lexical-only sufficiency, a missing path, and
an unknown category. The captures contain 45 candidate rows, 85,206 bounded
context bytes, and 27 adjacency-expanded rows. Each result records repository,
immutable revision, source path, source span, chunk/document IDs, passage, and
follow-up resource identity when supplied by MCP. Server RSS is explicitly
reported as unavailable at the authenticated HTTP boundary; retained-memory
measurements remain the responsibility of the existing in-process benchmark.
The existing `hybrid_without_vector_artifact_recommends_lexical` Rust test
covers the missing-vector condition. A missing graph artifact cannot yet be
executed because VESCM-208 has not implemented graph artifacts; that gap is
separate and explicit in `report.json`.
