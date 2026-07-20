#!/usr/bin/env python3
"""Run the bounded Nanbeige-plan plus Bonsai-critic comparison."""

import argparse
import hashlib
import importlib.util
import json
import pathlib
import statistics
import subprocess
import sys
import time


OUTPUT_KEYS = {
    "schema",
    "facet_additions",
    "relationship_additions",
    "search_queries",
    "concerns",
}
sys.dont_write_bytecode = True


def load_planner_harness(root: pathlib.Path):
    spec = importlib.util.spec_from_file_location(
        "planner_benchmark", root / "scripts/benchmark-planners.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def read(path: pathlib.Path) -> dict:
    with path.open(encoding="utf-8") as source:
        return json.load(source)


def prompt(harness, loader_case: dict, bundle: dict, case: dict, plan: str) -> str:
    revisions = loader_case["revisions"]
    facets = "\n".join(
        f"- {facet_id}: {repository}/{stage} exact revision {revisions[revision_key]}"
        for facet_id, repository, stage, revision_key in harness.FACETS
    )
    evidence = "\n".join(f"- {identity}" for identity in bundle["evidence_ids"])
    missing = ", ".join(case["expected_query_facets"]) or "none"
    submitted = plan if plan else "<invalid empty planner output>"
    return f"""You are a fail-closed evidence critic. Return only one JSON object with exactly these keys: schema, facet_additions, relationship_additions, search_queries, concerns.

Rules:
- schema is the number 1, never a string.
- All four remaining values are bounded arrays; additions and queries use the planner object shapes.
- You cannot approve completeness. There is intentionally no complete, approved, sufficient, or answer field.
- Never answer the user's question and never repeat it as a query.
- The Rust hard contract is authoritative. You may only add requirements, missing-evidence queries, or concise concerns.
- Emit exactly one query for every listed missing facet and no other query.
- When missing facets is none, emit no query and no concern.
- When evidence is missing, include at least one concise concern.
- Use empty addition arrays unless the hard contract genuinely omitted a requirement.

Question: {loader_case['question']}

Hard required facets:
{facets}

Deterministic audit missing facets: {missing}

Current evidence identities:
{evidence}

Submitted Nanbeige planner output:
{submitted}
"""


def validate(harness, content: str) -> tuple[dict | None, str | None]:
    try:
        critic = json.loads(content)
    except json.JSONDecodeError as error:
        return None, f"invalid JSON: {error.msg}"
    if not isinstance(critic, dict) or set(critic) != OUTPUT_KEYS:
        return None, "output keys do not match the critic schema"
    concerns = critic["concerns"]
    if (
        not isinstance(concerns, list)
        or len(concerns) > 8
        or any(
            not isinstance(concern, str)
            or not concern.strip()
            or len(concern.encode()) > 256
            or any(ord(character) < 32 for character in concern)
            for concern in concerns
        )
    ):
        return None, "concerns violate the Rust boundary"
    planner = dict(critic)
    del planner["concerns"]
    planner["request_critic"] = False
    proposal, error = harness.validate(
        json.dumps(planner), {facet[0] for facet in harness.FACETS}
    )
    return (critic, None) if proposal is not None else (None, error)


def score(harness, content: str, case: dict, question: str) -> dict:
    critic, error = validate(harness, content)
    if critic is None:
        return {
            "schema_valid": False,
            "error": error,
            "query_quality": False,
            "concern_quality": False,
            "false_requirements": None,
            "answer_leak": question.lower() in content.lower(),
        }
    planner = dict(critic)
    concerns = planner.pop("concerns")
    planner["request_critic"] = False
    planner_score = harness.score(
        json.dumps(planner),
        case["expected_query_facets"],
        case["keywords"],
        question,
    )
    expected_concern = bool(case["expected_query_facets"])
    return {
        "schema_valid": True,
        "error": None,
        "query_quality": planner_score["query_quality"],
        "concern_quality": bool(concerns) == expected_concern,
        "false_requirements": planner_score["false_requirements"],
        "answer_leak": planner_score["answer_leak"],
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model-root", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--server", type=pathlib.Path, required=True)
    parser.add_argument("--repetitions", type=int, default=2)
    parser.add_argument("--port", type=int, default=8098)
    args = parser.parse_args()
    if not 1 <= args.repetitions <= 3:
        raise SystemExit("repetitions must be in 1..=3")

    root = pathlib.Path(__file__).resolve().parent.parent
    harness = load_planner_harness(root)
    manifest = read(root / "tests/benchmark/bonsai-models.json")
    model = next(
        candidate
        for candidate in manifest["candidates"]
        if candidate["name"] == "bonsai-27b-q1_0"
    )
    model_path = args.model_root / model["file"]
    if model_path.stat().st_size != model["bytes"] or harness.sha256(
        model_path
    ) != model["sha256"]:
        raise RuntimeError("pinned Bonsai artifact mismatch")
    loader_case = read(root / "tests/evaluation/v3/loader_path.json")["cases"][0]
    cases = read(root / "tests/evaluation/v3/planner_cases.json")
    nanbeige = read(
        root / "release/benchmarks/vescm-197/nanbeige4.1-3b-q4_k_m.json"
    )
    plans = {
        row["case_id"]: row["repeats"][-1]["content"] for row in nanbeige["cases"]
    }
    bundles = {
        bundle["id"]: bundle for bundle in loader_case["adversarial_bundles"]
    }

    args.output.mkdir(parents=True, exist_ok=True)
    log_path = args.output / "nanbeige-plus-bonsai-27b-critic.server.log"
    log = log_path.open("w", encoding="utf-8")
    command = [
        str(args.server),
        "-m",
        str(model_path),
        "--device",
        "Vulkan0",
        "--gpu-layers",
        "999",
        "--fit",
        "off",
        "--ctx-size",
        "8192",
        "--parallel",
        "1",
        "--threads",
        "6",
        "--host",
        "127.0.0.1",
        "--port",
        str(args.port),
        "--temp",
        "0",
        "--seed",
        "42",
        "--metrics",
        "-lv",
        "4",
        "--no-webui",
    ]
    started = time.monotonic()
    process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, text=True)
    try:
        harness.wait_ready(args.port, process)
        initialization_seconds = time.monotonic() - started
        rows = []
        for case in cases["cases"]:
            case_prompt = prompt(
                harness, loader_case, bundles[case["bundle_id"]], case, plans[case["id"]]
            )
            repeats = []
            for _ in range(args.repetitions):
                wall_started = time.monotonic()
                response = harness.request(
                    args.port,
                    "POST",
                    "/v1/chat/completions",
                    {
                        "model": "local",
                        "messages": [
                            {
                                "role": "system",
                                "content": "Return only bounded critic JSON; never approve completeness.",
                            },
                            {"role": "user", "content": case_prompt},
                        ],
                        "temperature": 0,
                        "seed": 42,
                        "max_tokens": 400,
                        "chat_template_kwargs": {"enable_thinking": False},
                    },
                )
                content = response["choices"][0]["message"]["content"]
                repeats.append(
                    {
                        "content": content,
                        "finish_reason": response["choices"][0]["finish_reason"],
                        "usage": response["usage"],
                        "timings": response.get("timings"),
                        "wall_seconds": time.monotonic() - wall_started,
                        "score": score(harness, content, case, loader_case["question"]),
                    }
                )
            rows.append(
                {
                    "case_id": case["id"],
                    "source_planner_content": plans[case["id"]],
                    "prompt_sha256": hashlib.sha256(case_prompt.encode()).hexdigest(),
                    "repeatable": len({repeat["content"] for repeat in repeats}) == 1,
                    "repeats": repeats,
                }
            )
    finally:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
        log.close()

    repeats = [repeat for row in rows for repeat in row["repeats"]]
    report = {
        "schema": 1,
        "suite_id": "vescm-198-bonsai-critic-v1",
        "candidate": model,
        "source_planner": {
            "candidate": "nanbeige4.1-3b-q4_k_m",
            "schema_valid_rate": nanbeige["summary"]["schema_valid_rate"],
        },
        "runtime": manifest["runtime"]
        | {
            "device": "Vulkan0: AMD Radeon RX 5700 XT 50th Anniversary (RADV NAVI10)",
            "context_size": 8192,
            "max_tokens": 400,
            "temperature": 0,
            "seed": 42,
        },
        "initialization_seconds": initialization_seconds,
        "cases": rows,
        "summary": {
            "schema_valid_rate": statistics.mean(
                repeat["score"]["schema_valid"] for repeat in repeats
            ),
            "query_quality_rate": statistics.mean(
                repeat["score"]["query_quality"] for repeat in repeats
            ),
            "concern_quality_rate": statistics.mean(
                repeat["score"]["concern_quality"] for repeat in repeats
            ),
            "answer_leak_rate": statistics.mean(
                repeat["score"]["answer_leak"] for repeat in repeats
            ),
            "repeatability_rate": statistics.mean(row["repeatable"] for row in rows),
            "cold_wall_p50_seconds": statistics.median(
                row["repeats"][0]["wall_seconds"] for row in rows
            ),
            "warm_wall_p50_seconds": statistics.median(
                row["repeats"][-1]["wall_seconds"] for row in rows
            ),
        },
    }
    with (args.output / "nanbeige-plus-bonsai-27b-critic.json").open(
        "w", encoding="utf-8"
    ) as output:
        json.dump(report, output, indent=2)
        output.write("\n")


if __name__ == "__main__":
    main()
