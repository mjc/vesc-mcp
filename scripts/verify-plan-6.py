#!/usr/bin/env python3
"""Requirement-by-requirement completion audit for VESCM-PLAN-6."""

import json
import subprocess
import sys
from pathlib import Path


root = Path(__file__).parents[1]


def load(path):
    return json.loads((root / path).read_text())


for verifier in (
    "verify-planner-bakeoff.py",
    "verify-bonsai-bakeoff.py",
    "verify-hardware-placement.py",
    "verify-codegraphrag-design.py",
    "verify-bounded-pipeline.py",
):
    subprocess.run([sys.executable, root / "scripts" / verifier], check=True)

decision = load("release/benchmarks/vescm-192/decision.json")
assert decision["plan"] == "VESCM-PLAN-6"
assert [stage["stage"] for stage in decision["stages"]] == [
    "guardrails", "retrieval", "reranking", "planning", "critique",
    "graph_and_history", "answer_gate",
]

suite = load("tests/evaluation/v3/loader_path.json")
assert suite["suite_id"] == "vescm-194-loader-path-v1"
case = suite["cases"][0]
assert len(case["judgments"]) == 6 and len(case["relationships"]) == 5
assert any(bundle["expected_missing_facets"] for bundle in case["adversarial_bundles"])

embedding = load("release/benchmarks/vescm-195/modern-bounded.json")
granite = next(
    candidate for candidate in embedding["candidates"]
    if candidate["benchmark"]["model_id"]
       == "ibm-granite/granite-embedding-97m-multilingual-r2"
)
assert granite["candidate"]["license"] == "Apache-2.0"
assert granite["candidate"]["production_eligible"] is True
assert granite["candidate"]["onnx_sha256"]

reranker = load("release/benchmarks/vescm-196/ettin-reranker-32m-v1-qint8-avx2.json")
assert reranker["model"]["license"] == "Apache-2.0"
assert reranker["candidate_set_sha256"] and reranker["model"]["onnx_sha256"]
assert "no reranker" in (root / "release/benchmarks/vescm-196/decision.md").read_text().lower()

planner = load("release/benchmarks/vescm-197/decision.json")
critic = load("release/benchmarks/vescm-198/decision.json")
assert planner["default"] == "hard-coded-contract-only"
assert planner["selection_rule_satisfied"] is False
assert critic["default_planner"] == "hard-coded-contract-only"
assert critic["default_critic"] is None and critic["bonsai_enabled"] is False
for path in (
    "release/benchmarks/vescm-197/granite-4.1-3b-q4_k_m.json",
    "release/benchmarks/vescm-197/nanbeige4.1-3b-q4_k_m.json",
    "release/benchmarks/vescm-197/qwen3.5-4b-q4_k_m.json",
    "release/benchmarks/vescm-198/bonsai-27b-q1_0.json",
    "release/benchmarks/vescm-198/ternary-bonsai-8b-q2_0-g128-fallback.json",
):
    assert load(path)["candidate"]["license"] == "Apache-2.0", path

profile = load("release/benchmarks/vescm-199/profile.json")
assert profile["lookup"]["ryzen_5_8600g"]["query_embedding"].endswith("CPUExecutionProvider")
assert profile["lookup"]["apple_m1"]["query_embedding"].endswith("CPUExecutionProvider")
assert "RX 5700 XT gfx1010" in profile["ingestion"]["ryzen_5_8600g_plus_rx_5700_xt"]["placement"]
assert "explicit bounded rebenchmark" in profile["pins"]["upgrade_policy"]

report = load("release/benchmarks/vescm-200/report.json")
assert all(row["complete_path_facets"] == 6 for row in report["operating_points"])
assert all(row["complete_path_relationships"] == 5 for row in report["operating_points"])
assert all(row["missing_facet_answered"] is False for row in report["operating_points"])
assert all(row["frontier_shortcut_rate"] == 0.0 for row in report["operating_points"])

recommended = decision["recommended_operating_points"]
assert recommended["ryzen_5_8600g_lookup"]["reranker"] is None
assert recommended["apple_m1_lookup"]["critic"] is None
assert recommended["rx_5700_xt_assisted"]["critic"] is None
assert decision["release_gates"]["frontier_shortcut_rate"] == 0.0

print("VESCM-PLAN-6 completion audit verified")
