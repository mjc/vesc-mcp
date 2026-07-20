#!/usr/bin/env python3
"""Verify the generated VESCM-200 end-to-end report."""

import json
from pathlib import Path


root = Path(__file__).parents[1]
report = json.loads((root / "release/benchmarks/vescm-200/report.json").read_text())
assert report["suite_id"] == "vescm-194-loader-path-v1"
assert len(report["operating_points"]) == 3
assert {row["operating_point"] for row in report["operating_points"]} == {
    "hard_rules_only", "fast_planner", "planner_and_critic"
}
for row in report["operating_points"]:
    assert row["complete_path_answered"] is True
    assert row["complete_path_facets"] == 6
    assert row["complete_path_relationships"] == 5
    assert row["missing_facet_answered"] is False
    assert row["missing_facets"]
    assert row["frontier_shortcut_rate"] == 0.0
    assert row["complete_usage"]["rounds"] <= 4
    assert row["complete_usage"]["candidates"] <= 128
    assert row["complete_usage"]["context_bytes"] <= 65536
    assert row["complete_usage"]["graph_hops"] == 5
    assert row["complete_usage"]["max_graph_hops"] <= 4
    assert row["complete_usage"]["model_calls"] <= 4

print("VESCM-200 bounded fail-closed pipeline report verified")
