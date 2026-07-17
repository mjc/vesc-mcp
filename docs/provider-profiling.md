# FastEmbed provider profiling

Status: initial Tali CPU/AMD-backend study, 2026-07-16.

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

## Batch-size sweep

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

The full 13,689-vector semantic build was started with the pinned model and
production policy, then stopped after 485 s because the M1 host was on battery.
It produced no semantic artifact or quality result; resume it on AC power or on
Tali after the pinned source checkouts are available there.
