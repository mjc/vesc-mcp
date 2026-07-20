#!/usr/bin/env python3
"""Verify VESCM-198 reports, GPU evidence, and disabled-default decision."""

import importlib.util
import json
import pathlib
import re
import sys


sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parent.parent
REPORTS = ROOT / "release/benchmarks/vescm-198"


def read(path: pathlib.Path) -> dict:
    with path.open(encoding="utf-8") as source:
        return json.load(source)


def load(name: str, path: pathlib.Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def verify_gpu_log(name: str, layers: str, model_mib: str, peak_mib: str) -> None:
    log = (REPORTS / f"{name}.server.log").read_text(encoding="utf-8")
    assert "using device Vulkan0 (AMD Radeon RX 5700 XT 50th Anniversary" in log
    assert f"offloaded {layers} layers to GPU" in log
    assert f"Vulkan0 model buffer size =  {model_mib} MiB" in log
    assert re.search(rf"RX 5700 XT.*\(\s*{peak_mib} =", log)


def main() -> None:
    planner = load("planner_benchmark", ROOT / "scripts/benchmark-planners.py")
    critic = load("critic_benchmark", ROOT / "scripts/benchmark-bonsai-critic.py")
    manifest = read(ROOT / "tests/benchmark/bonsai-models.json")
    candidates = {candidate["name"]: candidate for candidate in manifest["candidates"]}
    loader_case = read(ROOT / "tests/evaluation/v3/loader_path.json")["cases"][0]
    cases = read(ROOT / "tests/evaluation/v3/planner_cases.json")
    by_id = {case["id"]: case for case in cases["cases"]}
    bundles = {
        bundle["id"]: bundle for bundle in loader_case["adversarial_bundles"]
    }

    planner_names = (
        "bonsai-27b-q1_0",
        "ternary-bonsai-8b-q2_0-g128-fallback",
    )
    reports = {}
    for name in planner_names:
        report = read(REPORTS / f"{name}.json")
        assert report["candidate"] == candidates[name]
        assert report["runtime"]["llama_cpp_commit"] == manifest["runtime"]["commit"]
        for row in report["cases"]:
            case = by_id[row["case_id"]]
            expected_prompt = planner.prompt(
                loader_case, bundles[case["bundle_id"]], case["expected_query_facets"]
            )
            assert row["prompt_sha256"] == planner.hashlib.sha256(
                expected_prompt.encode()
            ).hexdigest()
            assert len(row["repeats"]) == 2
            for repeat in row["repeats"]:
                assert repeat["score"] == planner.score(
                    repeat["content"],
                    case["expected_query_facets"],
                    case["keywords"],
                    loader_case["question"],
                )
        reports[name] = report

    verify_gpu_log("bonsai-27b-q1_0", "65/65", "3446.26", "4250")
    verify_gpu_log(
        "ternary-bonsai-8b-q2_0-g128-fallback", "37/37", "1918.05", "3166"
    )
    assert reports["bonsai-27b-q1_0"]["summary"]["schema_valid_rate"] == 0
    fallback = reports["ternary-bonsai-8b-q2_0-g128-fallback"]["summary"]
    assert fallback["schema_valid_rate"] == 1 / 3
    assert fallback["answer_leak_rate"] == 1 / 3

    critic_report = read(REPORTS / "nanbeige-plus-bonsai-27b-critic.json")
    assert critic_report["candidate"] == candidates["bonsai-27b-q1_0"]
    for row in critic_report["cases"]:
        case = by_id[row["case_id"]]
        expected_prompt = critic.prompt(
            planner,
            loader_case,
            bundles[case["bundle_id"]],
            case,
            row["source_planner_content"],
        )
        assert row["prompt_sha256"] == planner.hashlib.sha256(
            expected_prompt.encode()
        ).hexdigest()
        assert len(row["repeats"]) == 2
        for repeat in row["repeats"]:
            assert repeat["score"] == critic.score(
                planner, repeat["content"], case, loader_case["question"]
            )
    assert critic_report["summary"]["schema_valid_rate"] == 0
    assert critic_report["summary"]["query_quality_rate"] == 0
    verify_gpu_log(
        "nanbeige-plus-bonsai-27b-critic", "65/65", "3446.26", "4250"
    )

    loads = read(REPORTS / "load-results.json")
    mainline_log = (REPORTS / loads["mainline_g64"]["log"]).read_text()
    prism_log = (REPORTS / loads["prism_g64"]["log"]).read_text()
    assert loads["mainline_g64"]["exit_code"] == 1
    assert "invalid ggml type 42" in mainline_log
    assert loads["prism_g64"]["exit_code"] == 1
    assert "offset 174722688, expected 165015872" in prism_log

    baseline = read(REPORTS / "hard-coded-contract-only.json")
    assert baseline["summary"]["query_quality_rate"] == 1
    decision = read(REPORTS / "decision.json")
    assert decision["default_planner"] == "hard-coded-contract-only"
    assert decision["default_critic"] is None
    assert decision["bonsai_enabled"] is False
    assert decision["escalation_rule"]["observed_model_call_rate"] == 0
    print("VESCM-198 reports verified; Bonsai remains disabled")


if __name__ == "__main__":
    main()
