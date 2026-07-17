# FastEmbed provider profiling

Status: initial Tali CPU/AMD-backend study, 2026-07-16.

The later Tali runs below supersede the earlier source-order batch numbers
where they use the corrected length-bucketed benchmark path.

The Rust indexing phases are not the current optimization target. The
measurements below use a deterministic 1,024-chunk sample from the 13,720-chunk
corpus, with C, Rust, Markdown, QML, short, and long chunks. Each RSS value is
the process peak reported by GNU `time -v`; it is not the retained RSS delta
reported by the benchmark.

## Test system

- Host: `mjc@tali`, Ryzen 5 8600G, 12 logical CPUs, Radeon RX 5700 XT.
- Shell: `nix develop` for CPU and `nix develop .#rocm` for AMD backends.
- Model revision: `ea104dacec62c0de699686887e3f920caeb4f3e3`.
- CPU model for the primary sweep: `Xenova/bge-small-en-v1.5` using the pinned
  `model_quantized.onnx` file.
- Each batch-size point ran in a fresh process. Multi-size in-process sweeps
  were discarded because ORT retained allocator state between provider
  instances and inflated RSS.

## Full immutable Git corpus profile

The full-corpus run used the pinned local checkouts at revisions
`c835e9f10989f217269efb4ec943dfea7d280dfd` (`bldc`),
`005a08a0189f6df83bb47fbe2f93a3320c15c11a` (`vesc_tool`), and
`0ef6e99d8701886feeb7fe6c07cc4ec53fb3d97a` (`refloat`). The CLI retains
`vesc` as the logical repository ID for the `bldc` checkout; it is not a
second firmware repository.

The baseline was the pre-`e32630c` lifecycle path, which reread and reopened a
freshly written lexical artifact during validation. The final path trusts the
writer's streaming digest and exact byte count, validates the manifest, and
checks fresh-file sizes; full checksum/decode validation remains for an
already-existing generation. Two independent final runs produced the same
corpus digest `sha256:ee5aca5157a7cab911d8d70643cbb69de0421f7760582c88402e8d407f6e6b1e`
and byte-identical artifacts.

| Measurement | Before | After |
|---|---:|---:|
| Documents / chunks | 2,854 / 13,689 | 2,854 / 13,689 |
| Visited / accepted / rejected files | 4,113 / 2,760 / 1,353 | 4,113 / 2,760 / 1,353 |
| Total build | 166.167 s | 60.160 s |
| Ingestion | 2.119 s | 2.202 s |
| Chunking | 8.912 s | 8.860 s |
| Corpus construction | 0.026 s | 0.023 s |
| Lexical index construction | 40.711 s | 39.083 s |
| Artifact encoding | 9.946 s | 9.840 s |
| Manifest serialization + writes | 0.002 s | 0.003 s |
| Fresh-generation validation | 104.308 s | 0.000835 s |
| External elapsed | 168.37 s | 63.74 s |
| External peak RSS | 2,170,830,848 B | 1,465,139,200 B |

The fresh-generation change reduced total build time by 63.8% and removed the
130 MiB lexical write-read-hash/reopen cycle. The serialized sizes reconcile as
follows: corpus `1,202,110` B, generation manifest `2,706,783` B, active
manifest `2,706,783` B, lexical artifact `135,840,360` B, and combined
generation-plus-active provenance `5,413,566` B (3.985% of the lexical
artifact). No inventory or diagnostic fields were removed.

### Instrumented Git-ingestion attribution

The full lexical build was repeated on Tali after adding aggregate Git-stage
observations. These are warm object-cache runs and are not a replacement for
the cold/uncached lifecycle comparison above. They isolate the actual Git
scans and object-processing stages without adding per-file timers or a worker
pool.

| Measurement | First run | Repeat |
|---|---:|---:|
| Total build | 3.339 s | 3.347 s |
| Git ingestion | 160.271 ms | 160.131 ms |
| Git tree walk | 4.831 ms | 4.398 ms |
| Candidate ordering/accounting | 12 µs | 12 µs |
| Blob object load | 27.766 ms | 27.774 ms |
| Binary scan | 247 µs | 253 µs |
| UTF-8 normalization | 10.352 ms | 10.390 ms |
| Document metadata/identifier work | 111.317 ms | 111.546 ms |
| External peak RSS | 1,903,472 kB | 1,900,236 kB |

Both runs examined 4,113 tree entries, retained 2,760 candidates, loaded
14,646,390 blob bytes, and produced 1,353 bounded diagnostics. The lexical
artifact was byte-identical in both runs with SHA-256
`8a98d234a06de40ac7aa46aa5b0fcfad193ca8625f9ae3404d74bebd212a1459`.

Valgrind Massif on a separate full build recorded a 1.476 GiB peak useful heap.
The optimized binary's symbols are insufficient for reliable per-call
allocation attribution, so this is corpus-wide allocator evidence; the
stage counters above are the authoritative Git attribution. Git ingestion is
about 4.8% of this warm build and is no longer a material target. No scan
rewrite, allocation rewrite, or Rayon/background pool is justified.

## Full semantic artifact and quality result

After the battery-limited Mac run, the same pinned corpus was built on Tali.
This is the meaningful x86 CPU baseline: explicit `CPUExecutionProvider`,
quantized BGE Small, batch 8, stable length bucketing, and 12 intra-op
threads. The model revision was
`ea104dacec62c0de699686887e3f920caeb4f3e3`; the source revisions are the ones
listed above. Two fresh release builds were byte-identical.

| Measurement | First run | Repeat |
|---|---:|---:|
| Documents / chunks | 2,854 / 13,689 | 2,854 / 13,689 |
| Total build | 543.865 s | 543.468 s |
| Provider inference | 533.834 s | 533.488 s |
| Provider throughput | 25.643 chunks/s | 25.659 chunks/s |
| Embedding input | 0.192 s | 0.190 s |
| Vector finalization | 0.043 s | 0.032 s |
| External elapsed | 9:04.45 | 9:04.06 |
| External peak RSS | 2,453,948 kB | 2,469,500 kB |

The first run used 1,146% process-tree CPU, consistent with approximately
11 of Tali's 12 logical CPUs. The provider accounted for 98.2% of the measured
build time, confirming that further Rust-side indexing work is not justified
without new profiling.

The output sizes and checksums were stable across both runs:

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| `corpus.json` | 1,202,110 | `e8f0337a616ea9fb7f22777f57c5db37efd1dcfe2c91236e03552078e593fef7` |
| `lexical.json` | 135,840,360 | `8a98d234a06de40ac7aa46aa5b0fcfad193ca8625f9ae3404d74bebd212a1459` |
| `vectors.bin` | 22,012,069 | `08f02a0b0add25adbcb7e88b4f8be68d5a8f6a69d86d916864f7a9675ddcfb8f` |
| `manifest.json` | 2,706,852 | `e318935ee8f3cf2c26e00a82cede16344a43c4297805638badc8673a6e157e7e` |

The full-corpus quality evaluation is informative but does not pass the
locked retrieval gates (`recall@5 >= 0.90`, `MRR >= 0.80`, `nDCG >= 0.80`,
identifier top-1 = 1.0):

| Mode | Recall@5 | Recall@10 | MRR | nDCG | Identifier top-1 |
|---|---:|---:|---:|---:|---:|
| Legacy | 0.3333 | 0.3333 | 0.3333 | 0.3333 | 1.0000 |
| Lexical | 0.4417 | 0.4667 | 0.4624 | 0.4525 | 1.0000 |
| Semantic | 0.8083 | 0.8333 | 0.7860 | 0.7560 | 0.8500 |
| Hybrid | 0.7444 | 0.8417 | 0.5818 | 0.6302 | 1.0000 |

Consequently this artifact is retained as a reproducible benchmark result,
not promoted into the Nix release payload. The semantic run is faster and
deterministic, but the full-corpus quality gate still requires investigation.

## Batch-size sweep

### Corrected inference-order sweep

The bake-off accepted `--semantic-length-bucketed true`, but its benchmark
path did not pass the provider's length-bucketed inference order into vector
construction. That made ORT see a long sequence of changing dynamic shapes and
caused the isolated BGE batch-8 full run to grow to 28,780,316 kB RSS before it
was killed. The lifecycle path already applied the order; the benchmark path
now does too, with a regression test covering the call and stable artifact
ordering.

The fixed full BGE run used the clean, frame-pointer-enabled release binary on
Tali with batch 8, 12 intra-op threads, CPU execution, and length bucketing:

| Measurement | Fixed full run |
|---|---:|
| Documents / chunks | 2,869 / 13,720 |
| Provider inference | 526.498 s |
| Provider throughput | 26.06 chunks/s |
| Total build | 526.737 s |
| External elapsed | 9:19.33 |
| External peak RSS | 3,441,648 kB (3.28 GiB) |
| Retained RSS delta | 65,536 B |
| Vector artifact | 22,061,917 B |

The benchmark's retained RSS delta is not peak RSS; the latter remains an
external `time -v` measurement. A symbolized heaptrack run on a 1,024-chunk
sample stayed bounded and attributed allocations to tokenizer work, FastEmbed
token-statistics cloning, and ORT session execution. It did not reproduce a
large Rust-side retained allocation.

Source-order batching, one fresh process per point, 1,024 chunks, default ORT
threading, one measured repetition:

| Batch | Provider time (s) | Chunks/s | Padding | Peak RSS (GiB) |
|---:|---:|---:|---:|---:|
| 8 | 50.736 | 20.183 | 16.138% | 1.28 |
| 16 | 48.979 | 20.907 | 16.138% | 1.73 |
| 32 | 48.545 | 21.094 | 16.138% | 2.63 |
| 64 | 48.067 | 21.304 | 16.138% | 4.43 |
| 128 | 47.620 | 21.504 | 16.138% | 8.03 |
| 256 | 47.016 | 21.780 | 16.138% | 15.25 |

Batch 256 buys only 7.9% over batch 8 while consuming 15.25 GiB. It is not a
production choice.

The corrected length-bucketed batch sweep on the same host and sample was
different: batch 1 took 25.522 s (40.12 chunks/s) and 0.84 GiB peak RSS; batch
8 took 33.686 s (30.40 chunks/s) and 1.28 GiB; batch 16 took 34.946 s (29.30
chunks/s) and 1.71 GiB; and batch 32 took 35.699 s (28.68 chunks/s) and 2.60
GiB. Batch 64 did not complete under this sample's dynamic-shape working set.
These results make batch 1 the safe Jina starting point, but a full-corpus
production change still requires a full BGE quality/determinism run.

## Model memory probes

The pinned Arctic XS quantized model completed 1,024 chunks at batch 1 in
13.985 s of provider time with 0.82 GiB peak RSS. Arctic S completed the same
sample in 25.609 s with 0.85 GiB peak RSS. The pinned Jina code quantized model
completed a 64-chunk batch-1 probe in 40.732 s with 1.83 GiB peak RSS; its
token profile reached 1,767 tokens without truncation. A 1,024-chunk Jina probe
was stopped before completion because its 8,192-token profile produced an
unboundedly expensive working set on this CPU. It is not a valid throughput
result and should not be compared with the BGE/Arctic sample results.

## Padding and length bucketing

The sample contains 439,680 real tokens. Source-order batch 8 pads to 524,288
tokens. Stable token-length bucketing keeps the final artifact order unchanged
while changing only inference order.

| Mode | Batch | Repetitions | Provider p50 (s) | Chunks/s | Real / padded tokens | Padding | Peak RSS (GiB) |
|---|---:|---:|---:|---:|---:|---:|---:|
| Source order | 8 | 3 | 42.300 | 24.208 | 439,680 / 524,288 | 16.138% | 1.29 |
| Length bucketed | 8 | 3 | 32.984 | 31.046 | 439,680 / 441,288 | 0.364% | 1.29 |
| Length bucketed | 16 | 3 | 34.724 | 29.489 | 439,680 / 443,248 | 0.805% | 1.73 |
| Length bucketed | 32 | 1 | 39.863 | 25.688 | 439,680 / 446,848 | 1.604% | 2.60 |
| Length bucketed | 64 | 1 | 40.488 | 25.291 | 439,680 / 454,016 | 3.158% | 5.07 |

The controlled source/bucket comparison uses the same batch, 12 intra-op
threads, warmup, and three repetitions. Bucketing improves provider p50 by
22.0% and reduces padding by 97.7%.

Token statistics for the bucketed sample: minimum 42, median 512, p95 512,
maximum 512; 723/1,024 chunks (70.6%) exceed the model limit and are
truncated. Total untruncated tokens are 1,408,954. Bucketing does not change
the truncation policy.

## ORT intra-op thread sweep

Length-bucketed batch 8, 1,024 chunks, 12-thread host, one warmed measured
repetition per fresh process:

| Intra-op threads | Provider time (s) | Chunks/s | Peak RSS (GiB) |
|---:|---:|---:|---:|
| 1 | 94.529 | 10.833 | 1.28 |
| 2 | 54.827 | 18.677 | 1.28 |
| 4 | 39.648 | 25.826 | 1.28 |
| 6 | 38.321 | 26.722 | 1.28 |
| 8 | 35.489 | 28.854 | 1.28 |
| 12 | 33.936 | 30.174 | 1.28 |

A direct `/proc/<pid>/stat` sample of the recommended 12-thread run averaged
11.06 CPU cores, or 92.2% of the 12-core allowance. `scxtop` showed the
expected quiet initialization, one-core tokenizer/model startup, and then
steady all-core ORT execution; CPU frequency held approximately 4.97–5.00
GHz. GNU `time` process-tree percentages were anomalous at the highest thread
setting and are intentionally not used as the CPU-utilization result.

FastEmbed 5.17.3 configures graph optimization level 3 and disables memory
pattern optimization. Its public initialization options expose intra-op
threads but not inter-op threads or CPU arena controls, so those settings were
not changed through unsupported environment overrides.

## Model-file smoke comparison

Same Tali configuration, length-bucketed batch 8, 12 intra-op threads, one
repetition. These are throughput/RSS results only; retrieval-quality parity
still requires building matching vector artifacts for each candidate.

| ONNX file | Provider time (s) | Peak RSS (GiB) | Output dimension | Artifact size |
|---|---:|---:|---:|---:|
| `model_quantized.onnx` | 33.936 | 1.28 | 384 | ~1.65 MiB |
| `model_int8.onnx` | 32.117 | 1.45 | 384 | ~1.65 MiB |
| `model_uint8.onnx` | 39.437 | 1.53 | 384 | ~1.65 MiB |
| `model_fp16.onnx` | 65.551 | 1.53 | 384 | ~1.65 MiB |

The INT8 result is promising but is not a production recommendation until a
matching artifact and retrieval-quality comparison are complete.

### Full-corpus INT8 quality comparison

The full pinned corpus was then built with `model_int8.onnx` on Tali under the
same explicit CPU, batch 8, length-bucketed, 12-thread configuration. This is
the first model-file candidate with both full-corpus throughput and quality
evidence.

| Measurement | Quantized baseline | INT8 |
|---|---:|---:|
| Provider inference | 533.488 s | 499.959 s |
| Provider throughput | 25.659 chunks/s | 27.380 chunks/s |
| External elapsed | 9:04.06 | 8:30.58 |
| Peak RSS | 2,469,500 kB | 2,700,224 kB |
| Vector artifact | 22,012,069 B | 22,012,069 B |
| Vector SHA-256 | `08f02a0b…5ddcfb8f` | `b7d7714e…63051bb5` |

INT8 is 6.3% faster but retains 9.3% more peak RSS. Corpus and lexical
artifacts remained identical. Full quality remained below the locked gates:

| Mode | Recall@5 | Recall@10 | MRR | nDCG | Identifier top-1 |
|---|---:|---:|---:|---:|---:|
| Semantic | 0.8083 | 0.8500 | 0.7933 | 0.7546 | 0.7500 |
| Hybrid | 0.7528 | 0.8583 | 0.5950 | 0.6448 | 1.0000 |

The INT8 model is therefore rejected as the production replacement: its
throughput gain is modest, its memory cost is higher, and identifier quality
is worse than the quantized baseline. The complete reports remain benchmark
evidence; no release artifact was changed.

## AMD provider result

The first `nix develop .#rocm` shell used nixpkgs ONNX Runtime 1.26 with
`rocmSupport = true`. That output contained MIGraphX but not the ROCm EP:
there was no `libonnxruntime_providers_rocm.so`, diagnostics reported
`ROCMExecutionProvider=false`, and explicit ROCm registration failed before
device selection. Device 0 (RX 5700 XT) and device 1 (Radeon 760M iGPU) both
failed this way. The failure is fatal by design; neither run was allowed to
silently fall back to CPU.

The flake uses nixpkgs' supported AMD configuration and exposes the actual
MIGraphX backend separately. The resulting ORT 1.26 output reports
`MIGraphXExecutionProvider=true`; the shell also provides a writable
`ORT_MIGRAPHX_MODEL_CACHE_PATH` for compiled graphs. The fixed matrix below
uses the same 1,024-chunk sample, batch 8, 12 intra-op threads, and length
bucketing. Provider time excludes model initialization; external elapsed time
and peak RSS come from GNU `time -v`.

| Runtime / device | Selected provider | Provider p50 | Provider chunks/s | External elapsed | Peak RSS |
|---|---|---:|---:|---:|---:|
| CPU / 8600G | `CPUExecutionProvider` | 39.510 s | 25.92 | 42.46 s | 1,338,976 kB |
| ROCm / RX 5700 XT `gfx1010` | registration failed | — | — | 3.38 s | 837,552 kB |
| ROCm / Radeon 760M `gfx1103` | registration failed | — | — | smoke failed | — |
| MIGraphX / RX 5700 XT `gfx1010` | `MIGraphXExecutionProvider`, device 0 | 71.012 s | 14.42 | 152.82 s | 2,819,364 kB |
| MIGraphX / Radeon 760M `gfx1103` | `MIGraphXExecutionProvider`, device 1 | 80.769 s | 12.68 | 163.80 s | 2,805,884 kB |

The sysfs counters confirmed device routing during the MIGraphX runs: device 0
made `card1` (`1002:731f`, RX 5700 XT) busy, while device 1 made `card0`
(`1002:15bf`, Radeon 760M) busy. Both AMD backends are materially slower than
the 8600G CPU baseline for this quantized BGE workload, and both retain roughly
2.7 GiB peak RSS. Keep CPU as the production default. Treat ROCm as an
explicit diagnostic failure until a ROCm-enabled ORT <= 7.0-compatible build
is intentionally supplied; do not call the MIGraphX measurements ROCm
measurements.

## Current recommendation

The production build now defaults to
`--semantic-length-bucketed true --semantic-batch-size 8`. It uses eight
intra-op threads on Apple Silicon M1 and the process CPU allowance elsewhere
(twelve on Tali under its current CPU allowance); explicit flags still override
these defaults. The build path performs bounded tokenizer passes to choose
inference order and restores stable chunk-ID order in the artifact. Keep the
CPU quantized model as the baseline until INT8 quality is measured. Do not
select batch sizes above 32 without a machine-specific memory budget.

Remaining validation:

1. Build matching quantized and INT8 sample/full artifacts and compare the
   existing retrieval evaluation suite.
2. Verify the same stable vector ordering and exact top-K IDs for source-order
   and bucketed builds.
3. Run one integrated before/after build with the new opt-in policy and report
   external peak RSS before making it the default.

The earlier 13,689-vector semantic build on the M1 was stopped after 485 s
because that host was on battery. The completed Tali run above supersedes that
attempt for throughput profiling; the M1 remains unbenchmarked for this full
corpus until it is on AC power.
