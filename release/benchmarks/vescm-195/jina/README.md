# Jina v2 base-code bounded comparison

The pinned Jina v2 base-code INT8 CPU model was run on the Ryzen 5 8600G
through the same VESCM-195 bounded protocol as Granite 97M: 25 judged queries,
128 judged/decoy chunks, 512-token lossless windows, batch 8, 12 intra-op
threads, two warmups, and ten release-mode repetitions.

| Metric | Jina v2 INT8 | Granite 97M INT8 |
| --- | ---: | ---: |
| Semantic R@5 | 0.62 | 0.58 |
| Semantic R@10 | 0.72 | 0.66 |
| Hybrid R@5 | 0.54 | 0.44 |
| Hybrid R@10 | 0.72 | 0.68 |
| Hybrid MRR@10 | 0.2681 | 0.2364 |
| Query embedding p50 | 5.135 ms | 3.401 ms |
| Sampled ingestion | 2.84 chunks/s | 7.57 chunks/s |
| Vector bytes, 128 chunks | 402,600 | 206,006 |
| External peak RSS | 3.28 GiB | 3.22 GiB |
| Retained query RSS delta | 256 KiB | -60 KiB |

External peak RSS covers the complete benchmark process, including the loaded
corpus and benchmark harness, rather than model weights alone. Jina's ONNX file
is 154.4 MiB and Granite's is 93.7 MiB. Jina's 768-dimensional vectors require
about twice the storage of Granite's 384-dimensional vectors.

Jina is the measured quality winner and already has the matched FP16 MIGraphX
ingestion / INT8 CPU-query path. Granite remains the faster, smaller backup.
