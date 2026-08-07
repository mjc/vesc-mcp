# VESCM-258 rewrite reconciliation benchmark

Run from the repository root:

```console
CARGO_BUILD_JOBS=1 nix develop --option builders '' -c \
  cargo bench -p vesc-knowledge-index --bench corpus_pipeline -- \
  '*persisted*removed_tips'
```

The fixture builds one immutable complete-history predecessor with 128 retained
source files and 64 divergent tips, then compares removing those tips by
reconciling the persisted Tantivy artifact with building the same final corpus
cold. Fixture construction is outside the measured region.

## 2026-08-07 Tali result

| Metric | Reconcile | Cold |
|---|---:|---:|
| Callgrind instructions | 48,630,463 | 45,323,033 |
| Estimated cycles | 64,841,401 | 59,622,745 |
| DHAT total bytes | 6,774,355 | 6,545,585 |
| DHAT allocation blocks | 11,158 | 9,811 |
| DHAT bytes at peak | 0 | 5,127,456 |

The small fixture exposes the fixed cost of cloning and deleting from an
existing index: reconciliation uses about 9% more estimated cycles and 3.5%
more total allocated bytes than rebuilding this tiny final corpus. It does not
retain a corpus-sized live heap at DHAT's peak, while the cold writer retains
5.13 MB. The service path also preserves unchanged semantic rows; the
cold-equivalence tests verify that only changed embedding inputs reach the
provider.

The benchmark is a regression harness, not a claim that tiny rewrites are
always faster. Its hard requirements are bounded live memory, no retained-body
materialization, exact deletion, and immutable predecessor reuse. Real managed
histories benefit from avoiding retained lexical reconstruction and embedding
inference as their retained corpus grows.
