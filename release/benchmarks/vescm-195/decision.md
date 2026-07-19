# VESCM-195 bounded embedding decision

This is a release-mode screening run on the exact AMD Ryzen 5 8600G host. It
uses ONNX Runtime 1.26.0 CPUExecutionProvider, 12 intra-op threads, outer batch
8, 512-token lossless windows, all 25 v2 judged queries, every judged-relevant
chunk, and deterministic decoys for 128 evaluated chunks. The 16,586-chunk
corpus identity remains in the report; `evaluated_chunks` makes the benchmark
sample explicit. These limits are benchmark-only and do not limit production
ingestion.

Arctic XS and Granite 97M use ten generation samples. Granite 311M uses three
because it is only the family quality ceiling. All rows use two query warmups
and ten query-latency samples. External GNU `time` peak RSS is attached to the
machine-readable report.

## Decision

- Granite 97M improves bounded semantic R@5 from 0.44 to 0.58 and hybrid R@10
  from 0.64 to 0.68 versus Arctic XS. Its median query embedding is 3.4 ms
  versus 1.1 ms, and sampled ingestion is 7.57 versus 18.31 chunks/s.
- Granite 311M reaches hybrid R@10 0.70 and MRR@10 0.2423, but costs 8.9 ms
  median query embedding and 2.66 sampled chunks/s. That gain does not justify
  making it the normal lookup or ingestion model.
- Granite 97M is the selected Granite candidate for subsequent path-completeness
  and reranking work. Arctic XS remains the cheap common control until that
  end-to-end gate demonstrates a material path-completeness gain.

The Jina Code Embeddings 0.5B research control is CC-BY-NC-4.0 and is therefore
not eligible for the production default. PPLX contextual embeddings require a
separate ordered-document artifact because a chunk vector depends on sibling
context; they must not be mixed into the ordinary independent-chunk artifact
schema. The expensive PPLX and Qwen rows are deferred by the staged stop rule:
the first-stage Granite gain is modest after fusion and does not justify a full
Cartesian benchmark.

The GitHub-hosted `macos-14` arm64 M1 control measured Granite 97M at 3.01 ms
query p50 / 3.39 ms p95, 0.74 s cold initialization, 6.78 sampled chunks/s,
and 2.81 GiB peak RSS. ONNX Runtime selected CPUExecutionProvider with no
accelerator fallback. The raw report, timing output, and workflow identity are
stored under `m1/`.
