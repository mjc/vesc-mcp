#!/usr/bin/env python3
"""Run a tiny deterministic local-planner matrix against the locked suite."""

import argparse
import hashlib
import json
import pathlib
import statistics
import subprocess
import time
import urllib.error
import urllib.request


FACETS = (
    ("package-format", "vesc_package_lib", "package_format", "package"),
    ("generated-entry", "vesc_package_lib", "generated_entry", "package"),
    ("firmware-loader", "vesc_firmware", "firmware_loader", "firmware"),
    ("abi-dispatch", "vesc_firmware", "abi_dispatch", "firmware"),
    ("runtime-module-loading", "chibi_os", "runtime_module_loading", "rtos"),
    ("consumer-invocation", "refloat", "consumer_invocation", "consumer"),
)
OUTPUT_KEYS = {
    "schema",
    "facet_additions",
    "relationship_additions",
    "search_queries",
    "request_critic",
}
REPOSITORIES = {"vesc_package_lib", "vesc_firmware", "chibi_os", "refloat"}
STAGES = {
    "package_format",
    "generated_entry",
    "firmware_loader",
    "abi_dispatch",
    "runtime_module_loading",
    "consumer_invocation",
    "configuration",
}
RELATIONSHIP_KINDS = {
    "produces",
    "loaded_by",
    "dispatches_to",
    "runs_in",
    "invoked_by",
}


def read(path: pathlib.Path) -> dict:
    with path.open(encoding="utf-8") as source:
        return json.load(source)


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def request(port: int, method: str, path: str, body: dict | None = None) -> dict:
    data = None if body is None else json.dumps(body).encode()
    call = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        data=data,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(call, timeout=120) as response:
        return json.load(response)


def wait_ready(port: int, process: subprocess.Popen, timeout: float = 60) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"llama-server exited with {process.returncode}")
        try:
            request(port, "GET", "/health")
            return
        except (OSError, urllib.error.HTTPError):
            time.sleep(0.1)
    raise TimeoutError("llama-server did not become ready")


def prompt(loader_case: dict, bundle: dict, expected: list[str]) -> str:
    revisions = loader_case["revisions"]
    facet_lines = [
        f"- {facet_id}: {repository}/{stage} exact revision {revisions[revision_key]}"
        for facet_id, repository, stage, revision_key in FACETS
    ]
    evidence = "\n".join(f"- {identity}" for identity in bundle["evidence_ids"])
    missing = ", ".join(expected) if expected else "none"
    return f"""You are a bounded investigation planner. Return only one JSON object with exactly these keys: schema, facet_additions, relationship_additions, search_queries, request_critic.

Rules:
- schema is the number 1, never a string.
- facet_additions and relationship_additions are arrays of full requirement objects, or empty arrays.
- search_queries is an array of objects with exactly facet_id and query.
- request_critic is a boolean.
- Never answer or explain the user's question.
- Rust's hard contract is authoritative. Never remove, weaken, or re-add a required facet.
- Emit exactly one concise source-code search query for every listed missing facet and no other query.
- If currently missing facets is none, search_queries must be [].
- A query targets missing source evidence; it never repeats the user's question.
- Use empty addition arrays unless the hard contract genuinely omitted a requirement.
- Set request_critic only when the missing evidence cannot be targeted deterministically.

Question: {loader_case['question']}

Hard required facets:
{chr(10).join(facet_lines)}

Currently missing facets: {missing}

Current evidence identities:
{evidence}
"""


def validate(content: str, known_facets: set[str]) -> tuple[dict | None, str | None]:
    try:
        proposal = json.loads(content)
    except json.JSONDecodeError as error:
        return None, f"invalid JSON: {error.msg}"
    if not isinstance(proposal, dict) or set(proposal) != OUTPUT_KEYS:
        return None, "output keys do not match the strict schema"
    if type(proposal["schema"]) is not int or proposal["schema"] != 1:
        return None, "schema must be integer 1"
    if type(proposal["request_critic"]) is not bool:
        return None, "request_critic must be boolean"
    for key in ("facet_additions", "relationship_additions", "search_queries"):
        if not isinstance(proposal[key], list) or len(proposal[key]) > 8:
            return None, f"{key} is not a bounded array"
    for facet in proposal["facet_additions"]:
        if not isinstance(facet, dict) or set(facet) != {
            "id",
            "repository",
            "stage",
            "era",
        }:
            return None, "facet addition shape is invalid"
        era = facet["era"]
        if not isinstance(era, dict) or (
            set(era) == {"kind"} and era.get("kind") == "any"
        ) is False and (
            set(era) == {"kind", "revision"}
            and era.get("kind") == "exact"
            and isinstance(era.get("revision"), str)
            and bool(era["revision"])
        ) is False:
            return None, "facet era is invalid"
        if (
            not isinstance(facet["id"], str)
            or not facet["id"]
            or facet["repository"] not in REPOSITORIES
            or facet["stage"] not in STAGES
        ):
            return None, "facet addition violates the strict schema"
    for relationship in proposal["relationship_additions"]:
        if not isinstance(relationship, dict) or set(relationship) != {
            "id",
            "from_facet",
            "to_facet",
            "kind",
        }:
            return None, "relationship addition shape is invalid"
        if (
            relationship["kind"] not in RELATIONSHIP_KINDS
            or any(
                not isinstance(relationship[key], str) or not relationship[key]
                for key in ("id", "from_facet", "to_facet")
            )
        ):
            return None, "relationship addition violates the strict schema"
    seen = set()
    for query in proposal["search_queries"]:
        if not isinstance(query, dict) or set(query) != {"facet_id", "query"}:
            return None, "search query shape is invalid"
        facet_id, text = query["facet_id"], query["query"]
        if (
            not isinstance(facet_id, str)
            or facet_id not in known_facets
            or not isinstance(text, str)
            or not text.strip()
            or len(text.encode()) > 256
            or any(ord(character) < 32 for character in text)
            or (facet_id, text) in seen
        ):
            return None, "search query violates the Rust boundary"
        seen.add((facet_id, text))
    return proposal, None


def score(content: str, expected: list[str], keywords: dict, question: str) -> dict:
    proposal, error = validate(content, {facet[0] for facet in FACETS})
    if proposal is None:
        return {
            "schema_valid": False,
            "error": error,
            "query_facets_exact": False,
            "query_quality": False,
            "false_requirements": None,
            "answer_leak": question.lower() in content.lower(),
        }
    returned = [query["facet_id"] for query in proposal["search_queries"]]
    false_requirements = len(proposal["facet_additions"]) + len(
        proposal["relationship_additions"]
    )
    answer_leak = question.lower() in content.lower()
    useful = True
    for query in proposal["search_queries"]:
        terms = keywords.get(query["facet_id"], [])
        if terms and sum(term in query["query"].lower() for term in terms) < 2:
            useful = False
        if query["query"].strip().lower() == question.strip().lower():
            useful = False
    return {
        "schema_valid": True,
        "error": None,
        "query_facets_exact": returned == expected,
        "query_quality": useful
        and returned == expected
        and false_requirements == 0
        and not answer_leak,
        "false_requirements": false_requirements,
        "answer_leak": answer_leak,
    }


def summarize(rows: list[dict]) -> dict:
    repetitions = [repeat for row in rows for repeat in row["repeats"]]
    return {
        "schema_valid_rate": statistics.mean(
            repeat["score"]["schema_valid"] for repeat in repetitions
        ),
        "query_facets_exact_rate": statistics.mean(
            repeat["score"]["query_facets_exact"] for repeat in repetitions
        ),
        "query_quality_rate": statistics.mean(
            repeat["score"]["query_quality"] for repeat in repetitions
        ),
        "false_requirement_output_rate": statistics.mean(
            (repeat["score"]["false_requirements"] or 0) > 0
            for repeat in repetitions
        ),
        "answer_leak_rate": statistics.mean(
            repeat["score"]["answer_leak"] for repeat in repetitions
        ),
        "repeatability_rate": statistics.mean(row["repeatable"] for row in rows),
    }


def run_candidate(args: argparse.Namespace, spec: dict, loader_case: dict, cases: dict) -> dict:
    model_path = args.model_root / spec["file"]
    if model_path.stat().st_size != spec["bytes"] or sha256(model_path) != spec["sha256"]:
        raise RuntimeError(f"pinned model artifact mismatch: {model_path}")
    log_path = args.output / f"{spec['name']}.server.log"
    log = log_path.open("w", encoding="utf-8")
    command = [
        "llama-server",
        "-m",
        str(model_path),
        "--device",
        args.device,
        "--gpu-layers",
        "999",
        "--fit",
        "off",
        "--ctx-size",
        "8192",
        "--parallel",
        "1",
        "--threads",
        str(args.threads),
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
        wait_ready(args.port, process)
        initialization_seconds = time.monotonic() - started
        rows = []
        bundles = {bundle["id"]: bundle for bundle in loader_case["adversarial_bundles"]}
        for case in cases["cases"]:
            bundle = bundles[case["bundle_id"]]
            case_prompt = prompt(loader_case, bundle, case["expected_query_facets"])
            repeats = []
            for _ in range(args.repetitions):
                wall_started = time.monotonic()
                response = request(
                    args.port,
                    "POST",
                    "/v1/chat/completions",
                    {
                        "model": "local",
                        "messages": [
                            {
                                "role": "system",
                                "content": "Return only bounded JSON. Never answer the user's question.",
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
                        "score": score(
                            content,
                            case["expected_query_facets"],
                            case["keywords"],
                            loader_case["question"],
                        ),
                    }
                )
            rows.append(
                {
                    "case_id": case["id"],
                    "expected_query_facets": case["expected_query_facets"],
                    "prompt_sha256": hashlib.sha256(case_prompt.encode()).hexdigest(),
                    "repeatable": len({row["content"] for row in repeats}) == 1,
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
    summary = summarize(rows)
    summary.update(
        {
            "cold_wall_p50_seconds": statistics.median(
                row["repeats"][0]["wall_seconds"] for row in rows
            ),
            "warm_wall_p50_seconds": statistics.median(
                row["repeats"][-1]["wall_seconds"] for row in rows
            ),
            "cached_prompt_tokens": [
                row["repeats"][-1]["usage"]["prompt_tokens_details"]["cached_tokens"]
                for row in rows
            ],
        }
    )
    return {
        "schema": 1,
        "suite_id": cases["suite_id"],
        "candidate": spec,
        "runtime": {
            "llama_cpp_commit": "6f4f53f",
            "llama_cpp_package_version": 9842,
            "provider": "llama.cpp Vulkan",
            "device": args.device,
            "gpu_layers": 999,
            "context_size": 8192,
            "threads": args.threads,
            "temperature": 0,
            "seed": 42,
            "schema_grammar": "rejected by llama.cpp sampler; strict post-validation used",
            "device_inventory": subprocess.check_output(
                ["llama-server", "--list-devices"], text=True
            ).strip(),
        },
        "initialization_seconds": initialization_seconds,
        "cases": rows,
        "summary": summary,
    }


def hard_coded_baseline(loader_case: dict, cases: dict) -> dict:
    revisions = loader_case["revisions"]
    facets = {
        facet_id: f"{repository} {stage} {revisions[revision_key]}"
        for facet_id, repository, stage, revision_key in FACETS
    }
    rows = []
    for case in cases["cases"]:
        proposal = {
            "schema": 1,
            "facet_additions": [],
            "relationship_additions": [],
            "search_queries": [
                {"facet_id": facet_id, "query": facets[facet_id]}
                for facet_id in case["expected_query_facets"]
            ],
            "request_critic": False,
        }
        content = json.dumps(proposal, separators=(",", ":"))
        rows.append(
            {
                "case_id": case["id"],
                "expected_query_facets": case["expected_query_facets"],
                "repeatable": True,
                "repeats": [
                    {
                        "content": content,
                        "score": score(
                            content,
                            case["expected_query_facets"],
                            case["keywords"],
                            loader_case["question"],
                        ),
                    }
                ],
            }
        )
    return {
        "schema": 1,
        "suite_id": cases["suite_id"],
        "candidate": {"name": "hard-coded-contract-only", "model_id": None},
        "runtime": {"provider": "deterministic Rust rules", "model_calls": 0},
        "cases": rows,
        "summary": summarize(rows),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--models", type=pathlib.Path, required=True)
    parser.add_argument("--model-root", type=pathlib.Path, required=True)
    parser.add_argument("--loader-suite", type=pathlib.Path, required=True)
    parser.add_argument("--cases", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--candidate", action="append")
    parser.add_argument("--device", default="Vulkan0")
    parser.add_argument("--threads", type=int, default=6)
    parser.add_argument("--repetitions", type=int, default=2)
    parser.add_argument("--port", type=int, default=8097)
    args = parser.parse_args()
    if not 1 <= args.repetitions <= 3:
        raise SystemExit("repetitions must be in 1..=3")
    args.output.mkdir(parents=True, exist_ok=True)
    models = read(args.models)
    loader_case = read(args.loader_suite)["cases"][0]
    cases = read(args.cases)
    baseline = hard_coded_baseline(loader_case, cases)
    with (args.output / "hard-coded-contract-only.json").open(
        "w", encoding="utf-8"
    ) as output:
        json.dump(baseline, output, indent=2)
        output.write("\n")
    selected = set(args.candidate or [model["name"] for model in models["candidates"]])
    for spec in models["candidates"]:
        if spec["name"] not in selected:
            continue
        report = run_candidate(args, spec, loader_case, cases)
        with (args.output / f"{spec['name']}.json").open("w", encoding="utf-8") as output:
            json.dump(report, output, indent=2)
            output.write("\n")


if __name__ == "__main__":
    main()
