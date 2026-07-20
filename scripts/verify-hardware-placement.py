#!/usr/bin/env python3
"""Verify that the VESCM-199 profile is tied to its measured artifacts."""

import json
from pathlib import Path


ROOT = Path(__file__).parents[1]


def load(path: str):
    return json.loads((ROOT / path).read_text())


profile = load("release/benchmarks/vescm-199/profile.json")
ryzen = load("release/benchmarks/vescm-195/modern-bounded.json")
m1 = load("release/benchmarks/vescm-195/m1/granite-97m.json")
def granite(report):
    return next(
        candidate["benchmark"]
        for candidate in report["candidates"]
        if candidate["benchmark"]["model_id"].startswith("ibm-granite/granite-embedding-97m")
    )


ryzen_bench = granite(ryzen)
m1_bench = granite(m1)

for field in ("corpus_digest", "query_count", "corpus_chunks", "outer_batch_size",
              "effective_max_length", "warmup_iterations"):
    assert ryzen_bench[field] == m1_bench[field], field
assert ryzen_bench["model_id"] == m1_bench["model_id"]
assert ryzen_bench["model_revision"] == m1_bench["model_revision"]
assert ryzen_bench["query_count"] == 25 and ryzen_bench["corpus_chunks"] == 128
assert ryzen["machine"]["arch"] == "x86_64"
assert m1["machine"] == {"os": "macos", "arch": "aarch64", "rust_target": "aarch64-apple-darwin"}

ryzen_reranker = load("release/benchmarks/vescm-196/ettin-reranker-32m-v1-qint8-avx2.json")
m1_reranker = load("release/benchmarks/vescm-196/m1/ettin-reranker-32m-v1-qint8-avx2.json")
for field in ("suite_id", "case_id", "max_length", "batch_size", "facet_quota",
              "candidate_count", "candidate_set_sha256"):
    assert ryzen_reranker[field] == m1_reranker[field], field
assert ryzen_reranker["model"] == m1_reranker["model"]
assert ryzen_reranker["provider"] == m1_reranker["provider"] == "CPUExecutionProvider"

docs = (ROOT / "docs/provider-profiling.md").read_text()
for pin in ("AMD Ryzen 5 8600G", "AMD Radeon RX 5700 XT", "gfx1010",
            "jinaai/jina-embeddings-v2-base-code", "17.45 chunks/s", "32.44 windows/s"):
    assert pin in docs, pin

planner = load("release/benchmarks/vescm-197/decision.json")
critic = load("release/benchmarks/vescm-198/decision.json")
assert planner["default"] == "hard-coded-contract-only"
assert critic["default_planner"] == "hard-coded-contract-only"
assert critic["default_critic"] is None
assert profile["planner"]["default"].startswith("deterministic Rust")
assert profile["critic"]["default"].startswith("disabled")
assert "explicit bounded rebenchmark" in profile["pins"]["upgrade_policy"]

print("VESCM-199 hardware placement pins and comparable fixtures verified")
