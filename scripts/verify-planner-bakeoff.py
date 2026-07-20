#!/usr/bin/env python3
"""Verify the committed VESCM-197 planner reports and default decision."""

import importlib.util
import json
import pathlib
import re
import sys


ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.dont_write_bytecode = True
REPORTS = ROOT / "release/benchmarks/vescm-197"


def read(path: pathlib.Path) -> dict:
    with path.open(encoding="utf-8") as source:
        return json.load(source)


def load_harness():
    spec = importlib.util.spec_from_file_location(
        "planner_benchmark", ROOT / "scripts/benchmark-planners.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main() -> None:
    harness = load_harness()
    models = read(ROOT / "tests/benchmark/planner-models.json")
    loader_case = read(ROOT / "tests/evaluation/v3/loader_path.json")["cases"][0]
    cases = read(ROOT / "tests/evaluation/v3/planner_cases.json")
    by_id = {case["id"]: case for case in cases["cases"]}

    baseline = read(REPORTS / "hard-coded-contract-only.json")
    assert baseline["summary"] == {
        "schema_valid_rate": 1,
        "query_facets_exact_rate": 1,
        "query_quality_rate": 1,
        "false_requirement_output_rate": 0,
        "answer_leak_rate": 0,
        "repeatability_rate": 1,
    }

    summaries = {"hard-coded-contract-only": baseline["summary"]}
    for model in models["candidates"]:
        name = model["name"]
        report = read(REPORTS / f"{name}.json")
        assert report["candidate"] == model
        assert report["suite_id"] == cases["suite_id"]
        assert report["runtime"]["llama_cpp_commit"] == models["llama_cpp_commit"]
        assert "RX 5700 XT 50th Anniversary" in report["runtime"]["device_inventory"]
        assert len(report["cases"]) == len(by_id)
        for row in report["cases"]:
            case = by_id[row["case_id"]]
            bundle = next(
                bundle
                for bundle in loader_case["adversarial_bundles"]
                if bundle["id"] == case["bundle_id"]
            )
            expected_prompt = harness.prompt(
                loader_case, bundle, case["expected_query_facets"]
            )
            assert row["prompt_sha256"] == harness.hashlib.sha256(
                expected_prompt.encode()
            ).hexdigest()
            assert len(row["repeats"]) == 2
            assert row["repeatable"] == (
                len({repeat["content"] for repeat in row["repeats"]}) == 1
            )
            for repeat in row["repeats"]:
                expected_score = harness.score(
                    repeat["content"],
                    case["expected_query_facets"],
                    case["keywords"],
                    loader_case["question"],
                )
                assert repeat["score"] == expected_score
        recalculated = harness.summarize(report["cases"])
        for key, value in recalculated.items():
            assert report["summary"][key] == value
        assert report["summary"]["cold_wall_p50_seconds"] > 0
        assert report["summary"]["warm_wall_p50_seconds"] > 0

        log = (REPORTS / f"{name}.server.log").read_text(encoding="utf-8")
        assert "using device Vulkan0 (AMD Radeon RX 5700 XT 50th Anniversary" in log
        offload = re.search(r"offloaded (\d+)/(\d+) layers to GPU", log)
        assert offload and offload.group(1) == offload.group(2)
        summaries[name] = report["summary"]

    assert summaries["granite-4.1-3b-q4_k_m"]["query_quality_rate"] < 1
    assert summaries["nanbeige4.1-3b-q4_k_m"]["schema_valid_rate"] == 0
    assert summaries["qwen3.5-4b-q4_k_m"]["query_quality_rate"] == 1
    assert summaries["qwen3.5-4b-q4_k_m"]["warm_wall_p50_seconds"] > 0

    decision = read(REPORTS / "decision.json")
    assert decision["default"] == "hard-coded-contract-only"
    assert decision["optional_model"] is None
    assert decision["selection_rule_satisfied"] is False
    print("VESCM-197 planner reports verified; deterministic Rust remains default")


if __name__ == "__main__":
    main()
