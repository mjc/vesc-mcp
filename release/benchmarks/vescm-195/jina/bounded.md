# Embedding model bake-off

- Suite: vescm-165-v2-full-corpus
- Corpus digest: sha256:71342745041784d58f147b1cd99a7743377d3d9e0fa42ad4dab1f0aa280652ab
- Corpus: 2875 documents / 16586 chunks
- Evaluated chunks: 128

| Candidate | Quantization | Provider p50 (s) | Chunks/s | Fused R@5 | Fused R@10 | Fused MRR@10 | Semantic R@5 | Peak RSS (MiB) |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| lexical control | — | — | — | 0.0400 | 0.0600 | 0.0457 | — | — |
| jina-embeddings-v2-base-code-quantized | quantized | 45.127 | 2.84 | 0.5400 | 0.7200 | 0.2681 | 0.6200 | 3358.0 |
