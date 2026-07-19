# VESCM-196 reranker decision

Status: complete on Ryzen 5 8600G and Apple M1.

## Decision

Keep **no reranker** as the default. The locked path suite is already PathComplete at the six-item budget, so none of the measured rerankers earns its additional latency. If reranking is explicitly enabled, retain independently inside repository × stage × exact-era facets; global reranking dropped mandatory evidence for every model.

| Candidate | Runtime | Init | Warm p50 | Warm p95 | pairs/s | Peak RSS | Global PathComplete | Per-facet PathComplete |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| no reranker | Rust control | — | — | — | — | — | 1 | 1 |
| Ettin 17M QINT8 AVX2 | ONNX Runtime CPU | 0.137 s | 17.63 ms | 18.49 ms | 509.5 | 125.0 MiB | 0 | 1 |
| Ettin 32M QINT8 AVX2 | ONNX Runtime CPU | 0.177 s | 41.08 ms | 61.29 ms | 206.5 | 145.8 MiB | 0 | 1 |
| Ettin 68M QINT8 AVX2 | ONNX Runtime CPU | 0.349 s | 115.60 ms | 142.48 ms | 75.6 | 218.6 MiB | 0 | 1 |
| Qwen3 Reranker 0.6B BF16 | PyTorch CPU ceiling | 16.396 s | 1.807 s | 1.807 s | 5.01 | 2.23 GiB | 0 | 1 |

Ryzen runs used the same nine locked identities, six-item global budget, per-facet quota 1, maximum length 512, and batch size 8. Ettin used 12 ONNX intra-op threads; the Qwen research ceiling used six PyTorch threads. The exact models, revisions, ONNX and modular-head hashes, provider library, runtime versions, scores, retention provenance, and raw metrics are in the adjacent JSON files.

The Ettin ONNX exports contain only `last_hidden_state`; the trained Sentence Transformers head is stored separately. The Rust adapter now applies CLS pooling, dense/GELU, layer normalization, and the final score layer. An independent NumPy/ONNX calculation reproduced the first batched Rust score (`7.260729` versus `7.260727`).

## Apple M1 lookup matrix

GitHub Actions run `29708595145` completed successfully on the official `macos-14` ARM64 M1 runner at commit `b25f4ba`. It used the same candidates, budget, model hashes, maximum length, batch size, and quota, with three ONNX intra-op threads.

| Candidate | Init | Warm p50 | Warm p95 | pairs/s | Peak RSS | Global PathComplete | Per-facet PathComplete |
|---|---:|---:|---:|---:|---:|---:|---:|
| Ettin 17M QINT8 | 0.319 s | 20.10 ms | 25.87 ms | 438.8 | 145.9 MiB | 0 | 1 |
| Ettin 32M QINT8 | 0.204 s | 49.18 ms | 56.25 ms | 181.4 | 149.4 MiB | 0 | 1 |
| Ettin 68M QINT8 | 0.370 s | 143.03 ms | 153.63 ms | 62.5 | 299.5 MiB | 0 | 1 |

The committed `m1/` directory contains the machine-readable reports, raw `/usr/bin/time -l` output, and workflow metadata. The null CPU field emitted by commit `b25f4ba` was normalized to the runner identity supplied by GitHub (`Apple M1`, `macos-14`, ARM64); later runs query `machdep.cpu.brand_string` directly.

## Scope limit

The locked VESCM-194 suite intentionally stores stable evidence metadata, not complete source passages. These results prove runtime, global-versus-facet retention, wrong-era rejection, and deterministic provenance. They do **not** establish standalone semantic reranker quality. Because PathComplete does not improve over the legal no-model mode, that limitation reinforces rather than weakens the no-reranker decision.
