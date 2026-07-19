# Embedding bake-off

- Suite: `vescm-165-v2-full-corpus`
- Corpus: `sha256:71342745041784d58f147b1cd99a7743377d3d9e0fa42ad4dab1f0aa280652ab`
- Documents / chunks: 2875 / 16586
- Evaluated chunks: 128

| Candidate | Provider (s) | Chunks/s | Peak RSS (bytes) | Semantic R@5 | Hybrid R@5 | Hybrid MRR@10 |
|---|---:|---:|---:|---:|---:|---:|
| snowflake-arctic-embed-xs-quantized-control | 6.990 | 18.313 | 3443609600 | 0.4400 | 0.4200 | 0.2280 |
| granite-embedding-97m-multilingual-r2-qint8-avx2 | 16.910 | 7.570 | 3456954368 | 0.5800 | 0.4400 | 0.2364 |
| granite-embedding-311m-multilingual-r2-qint8-avx2 | 48.150 | 2.658 | 3874549760 | 0.5200 | 0.4800 | 0.2423 |

Peak RSS is an externally measured process maximum; retained RSS deltas remain separate benchmark fields.
