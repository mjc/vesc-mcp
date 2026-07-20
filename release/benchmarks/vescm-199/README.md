# VESCM-199 hardware placement profile

This profile keeps four decisions separate. Lookup stays on CPU on both the
Ryzen 5 8600G and Apple M1. Bulk embedding uses the RX 5700 XT only for the
exact measured gfx1010/Jina FP16/MIGraphX configuration. Planning stays in
deterministic Rust. Critique remains disabled.

| Stage | Ryzen 5 8600G | Apple M1 | RX 5700 XT 8 GB |
|---|---|---|---|
| lookup | lexical + graph CPU; Granite 97M INT8 CPU | lexical + graph CPU; Granite 97M INT8 CPU | not used |
| rerank | disabled; measured Ettin 32M INT8 CPU escalation | disabled; measured Ettin 32M INT8 CPU escalation | not used |
| ingestion | orchestration and CPU INT8 queries | CPU path only; no bulk claim | Jina v2 base code FP16, MIGraphX, 512 tokens, batch 8, length buckets, lossless mean pooling |
| planner | deterministic Rust | deterministic Rust | no default model; Nanbeige Q4 is measured only |
| critic | disabled | disabled | Bonsai 27B Q1 measured but rejected |

The Ryzen and M1 Granite reports use the same corpus digest, 128 chunks, 25
queries, batch 8, 512-token lossless input, two warmups, and the same pinned
model revision. Their query-embedding p50/p95 values are 3.40/7.64 ms and
3.01/3.39 ms respectively. The Ettin 32M runs use the same nine candidates,
512-token limit, batch 8, and quality gate: Ryzen is 41.08/61.29 ms and M1 is
49.18/56.25 ms per warm batch. Reranking stays disabled because it did not
improve path completeness.

Core ML is not labeled faster merely because its provider registered. The M1
workflow enables verbose ORT placement and rejects a result if any graph node
falls back to CPU. Until that proof passes, native ONNX CPU remains the M1
recommendation.

The RX ingestion result is scoped to the Ryzen 5 8600G + RX 5700 XT gfx1010
host, pinned ROCm/MIGraphX runtime and Jina FP16 artifact. The bounded 128-chunk
probe measured 17.45 chunks/s and 32.44 windows/s. The iGPU is not part of the
profile because it reset under the measured workload.

Run `python scripts/verify-hardware-placement.py`. Any runtime, model revision,
quantization, provider, GPU architecture, sequence length, batch size, pooling,
or fixture change requires a new bounded benchmark; this profile must never
silently follow an upgrade.
