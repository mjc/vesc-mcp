# VESCM-178 integrated RAG performance report

Status: **quality and integrated evidence gates passed**

Retain quantized BGE and the registered-evidence ranking fix; do not promote INT8 or AMD GPU providers.

## Pinned identity

- Host: Tali (Ryzen 5 8600G)
- Corpus: `sha256:ee5aca5157a7cab911d8d70643cbb69de0421f7760582c88402e8d407f6e6b1e` (2854 documents / 13689 chunks)
- Model: `quantized BGE Small` at `ea104dacec62c0de699686887e3f920caeb4f3e3`
- Provider: CPUExecutionProvider; batch 8; 12 intra-op threads
- Runtime/toolchain: ONNX Runtime 1.26.0; rustc 1.96.0 (ac68faa20 2026-05-25); LLVM 22.1.2; `x86_64-unknown-linux-gnu`
- Release profile: LTO `true`; 1 codegen unit; no `target-cpu=native` override
- Sources: `bldc@c835e9f10989f217269efb4ec943dfea7d280dfd`, `vesc_tool@005a08a0189f6df83bb47fbe2f93a3320c15c11a`, `refloat@0ef6e99d8701886feeb7fe6c07cc4ec53fb3d97a`

## Lifecycle before/after

| Run | n | Build s median [range] | External s median [range] | Peak RSS B median [range] | Ingest s median [range] | Chunk s median [range] | Lexical s median [range] | Validate s median [range] |
|---|---|---|---|---|---|---|---|---|
| before | 3 | 22.733 [11.604–24.370] | 23.980 [12.240–25.510] | 3930877952 [3928489984–4021727232] | 0.224 [0.169–0.265] | 0.713 [0.543–1.067] | 3.587 [2.243–4.807] | 16.999 [8.165–17.833] |
| final | 3 | 6.477 [3.377–6.657] | 7.190 [3.810–7.530] | 1984876544 [1968308224–2023251968] | 0.228 [0.169–0.479] | 0.766 [0.520–0.913] | 4.269 [2.218–4.912] | 0.000 [0.000–0.001] |

## Full semantic builds

| Run | Build s | Provider s | Chunks/s | External s | Peak RSS KiB |
|---|---|---|---|---|---|
| 1 | 543.865 | 533.834 | 25.643 | 544.450 | 2453948 |
| 2 | 543.468 | 533.488 | 25.659 | 544.060 | 2469500 |
| 3 | 1153.759 | 1130.879 | 12.105 | 1155.190 | 2477960 |

Semantic build median [range]: build 543.865 [543.468–1153.759] s, provider 533.834 [533.488–1130.879] s, external 544.450 [544.060–1155.190] s, peak RSS 2469500 [2453948–2477960] KiB.

Run 3 phase isolation: inference 1130.879 s, vector finalization 0.096 s, encoding 1.034 s, writing 0.043 s.

## Quality

| Mode | R@5 | R@10 | MRR@10 | nDCG@10 | Exact ID top-1 |
|---|---|---|---|---|---|
| legacy | 0.3333 | 0.3333 | 0.3333 | 0.3333 | 1.0000 |
| lexical | 0.4417 | 0.4667 | 0.4624 | 0.4525 | 1.0000 |
| semantic | 0.8083 | 0.8333 | 0.7860 | 0.7560 | 0.8500 |
| hybrid | 0.7444 | 0.8417 | 0.5818 | 0.6302 | 1.0000 |
| hybrid (registered evidence fix) | 0.9083 | 0.9333 | 0.8861 | 0.8561 | 1.0000 |

Locked hybrid gates: R@5 ≥ 0.90, MRR@10 ≥ 0.80, nDCG@10 ≥ 0.80, exact-ID top-1 = 1.0. Result: **PASS**.

## Query latency

Cold initialization: 140512 µs. First query: 10908 µs. Warm embedding p50/p95: 2303/2970 µs (189 samples, 1 warmup, 3 repetitions across 63 queries and 13689 × 384-dimensional vectors).
Retained RSS delta: 8192 B (131002368 B before / 131010560 B after).

| K | Samples | Min µs | p50 µs | p95 µs | Max µs |
|---|---|---|---|---|---|
| 5 | 189 | 8148 | 8241 | 8373 | 8784 |
| 10 | 189 | 8348 | 8455 | 8798 | 10680 |
| 20 | 189 | 8547 | 8665 | 8913 | 11329 |
| 50 | 189 | 8343 | 8720 | 9064 | 9662 |

## Batch and thread sweeps

| Order | Batch | Seconds | Chunks/s | Padding % | Peak RSS GiB |
|---|---|---|---|---|---|
| source | 8 | 50.736 | 20.183 | 16.138 | 1.28 |
| source | 16 | 48.979 | 20.907 | 16.138 | 1.73 |
| source | 32 | 48.545 | 21.094 | 16.138 | 2.63 |
| source | 64 | 48.067 | 21.304 | 16.138 | 4.43 |
| source | 128 | 47.62 | 21.504 | 16.138 | 8.03 |
| source | 256 | 47.016 | 21.78 | 16.138 | 15.25 |
| length_bucketed | 8 | 32.984 | 31.046 | 0.364 | 1.29 |
| length_bucketed | 16 | 34.724 | 29.489 | 0.805 | 1.73 |
| length_bucketed | 32 | 39.863 | 25.688 | 1.604 | 2.6 |
| length_bucketed | 64 | 40.488 | 25.291 | 3.158 | 5.07 |

| Threads | Seconds | Chunks/s | Peak RSS GiB |
|---|---|---|---|
| 1 | 94.529 | 10.833 | 1.28 |
| 2 | 54.827 | 18.677 | 1.28 |
| 4 | 39.648 | 25.826 | 1.28 |
| 6 | 38.321 | 26.722 | 1.28 |
| 8 | 35.489 | 28.854 | 1.28 |
| 12 | 33.936 | 30.174 | 1.28 |

## Provider matrix

| Provider | Seconds | Chunks/s | Peak RSS KiB | Usable |
|---|---|---|---|---|
| CPU | 39.510 | 25.920 | 1338976 | True |
| ROCm RX | n/a | n/a | n/a | False |
| ROCm iGPU | n/a | n/a | n/a | False |
| MIGraphX RX | 71.012 | 14.420 | 2819364 | True |
| MIGraphX iGPU | 80.769 | 12.680 | 2805884 | True |

Git ingestion repetitions: 160.271 ms, 160.131 ms; full warm builds: [3.339, 3.347] s; byte-identical lexical artifact: True.

## Deterministic artifacts

| Artifact | Bytes | SHA-256 | Repeat identical |
|---|---|---|---|
| corpus | 1202110 | `e8f0337a616ea9fb7f22777f57c5db37efd1dcfe2c91236e03552078e593fef7` | True |
| lexical | 135840360 | `8a98d234a06de40ac7aa46aa5b0fcfad193ca8625f9ae3404d74bebd212a1459` | True |
| vectors | 22012069 | `08f02a0b0add25adbcb7e88b4f8be68d5a8f6a69d86d916864f7a9675ddcfb8f` | True |
| manifest | 2706852 | `e318935ee8f3cf2c26e00a82cede16344a43c4297805638badc8673a6e157e7e` | True |

## Retained and rejected attempts

| Attempt | Decision | Reason |
|---|---|---|
| lifecycle validation reuse | retained | In three directly comparable reruns, median build time fell from 22.733 s to 6.477 s, median validation from 16.999 s to 0.000153 s, and median peak RSS from 3.93 GB to 1.98 GB. |
| length bucketing, batch 8 | retained | Best controlled throughput with low padding and bounded memory. |
| INT8 BGE | rejected | 6.3% faster, but peak RSS increased 9.3%, semantic identifier top-1 fell to 0.75, and quality gates still failed. |
| AMD GPU providers | rejected | ROCm registration failed; MIGraphX was slower and used more memory than CPU. |

## Remaining top costs

- Semantic provider inference is about 98.2% of the full semantic build.
- Lexical construction remains the largest non-provider phase (4.269 s median in the comparable final reruns; 39.083 s in the original cold final run).
- Git ingestion is about 160 milliseconds of a 3.34-second warm build and is not the primary target.

## Acceptance gaps

None.

Evidence commits: `e32630c`, `4887951`, `332b418`, `5a84d0d`, `e292600`, `b9f9f43`
