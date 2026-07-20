# VESCM-200 fail-closed pipeline

The generated report executes the real bounded pipeline against the locked
six-facet, five-relationship historical native-loader case and its
missing-ChibiOS adversary. It compares deterministic hard rules, a strict fast
planner proposal, and planner plus critic proposal. Saved proposals are empty
monotonic extensions because VESCM-197 and VESCM-198 found no model that
improves the deterministic contract; the operating points still exercise and
account for their model-call boundaries.

All three operating points complete the gold path. None invokes the frontier
answerer for the missing-facet case, so `FrontierShortcutRate` is zero. The
report accounts separately for rounds, retained candidates, context bytes,
aggregate and maximum graph hops, and model calls. Run:

```sh
nix develop -c cargo run --release -p vesc-knowledge-index --bin pipeline_eval -- \
  tests/evaluation/v3/loader_path.json \
  release/benchmarks/vescm-200/report.json \
  release/benchmarks/vescm-200/report.md
python3 scripts/verify-bounded-pipeline.py
```
