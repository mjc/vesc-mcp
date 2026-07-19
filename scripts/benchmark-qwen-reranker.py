#!/usr/bin/env python3
"""Bounded Qwen reranker ceiling benchmark; not a production dependency."""

import json
import os
import platform
import resource
import statistics
import sys
import time

import sentence_transformers
import torch
import transformers
from sentence_transformers import CrossEncoder


REPOSITORIES = {
    "vesc_package_lib": "VescPackageLib",
    "vesc_firmware": "VescFirmware",
    "chibi_os": "ChibiOs",
    "refloat": "Refloat",
}


def camel(value: str) -> str:
    return "".join(part.title() for part in value.split("_"))


def text(identity: dict) -> str:
    return (
        f"repository: {REPOSITORIES[identity['repository']]}\n"
        f"stage: {camel(identity['stage'])}\n"
        f"revision: {identity['revision']}\n"
        f"path: {identity['path']}\n"
        f"symbol: {identity['symbol']}\n"
        f"{identity['content_key']}"
    )


def percentile(samples: list[float], percent: int) -> float:
    samples = sorted(samples)
    index = ((len(samples) - 1) * percent + 99) // 100
    return samples[index]


def cpu_name() -> str | None:
    try:
        with open("/proc/cpuinfo", encoding="utf-8") as source:
            for line in source:
                if line.startswith("model name\t: "):
                    return line.removeprefix("model name\t: ").strip()
    except FileNotFoundError:
        pass
    return platform.processor() or None


def main() -> None:
    if len(sys.argv) != 5:
        raise SystemExit(
            "usage: benchmark-qwen-reranker.py SUITE OUTPUT REVISION REPETITIONS"
        )
    suite_path, output_path, revision, repetitions_arg = sys.argv[1:]
    repetitions = int(repetitions_arg)
    if repetitions < 1 or repetitions > 10:
        raise SystemExit("repetitions must be in 1..=10")
    with open(suite_path, encoding="utf-8") as source:
        suite = json.load(source)
    case = suite["cases"][0]
    identities = case["judgments"] + case["distractors"]
    documents = [text(identity) for identity in identities]
    pairs = [(case["question"], document) for document in documents]

    started = time.perf_counter()
    model = CrossEncoder(
        "Qwen/Qwen3-Reranker-0.6B",
        revision=revision,
        device="cpu",
        max_length=512,
        trust_remote_code=True,
        model_kwargs={"dtype": torch.bfloat16},
    )
    initialization_seconds = time.perf_counter() - started
    model.predict(pairs, batch_size=8, show_progress_bar=False)
    samples = []
    scores = None
    for _ in range(repetitions):
        started = time.perf_counter()
        scores = model.predict(pairs, batch_size=8, show_progress_bar=False)
        samples.append(time.perf_counter() - started)
    assert scores is not None
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    peak_rss_bytes = rss if platform.system() == "Darwin" else rss * 1024
    report = {
        "schema": 1,
        "suite_id": suite["suite_id"],
        "case_id": case["id"],
        "model": {
            "model_id": "Qwen/Qwen3-Reranker-0.6B",
            "model_revision": revision,
            "license": "Apache-2.0",
            "dtype": "bfloat16",
        },
        "provider": "PyTorch CPU",
        "os": platform.system().lower(),
        "arch": platform.machine(),
        "cpu": cpu_name(),
        "max_length": 512,
        "batch_size": 8,
        "prompt_name": "query (model default)",
        "candidate_count": len(pairs),
        "peak_rss_bytes": peak_rss_bytes,
        "runtime": {
            "python": platform.python_version(),
            "sentence_transformers": sentence_transformers.__version__,
            "transformers": transformers.__version__,
            "torch": torch.__version__,
            "torch_threads": torch.get_num_threads(),
        },
        "timing": {
            "initialization_seconds": initialization_seconds,
            "warm_p50_seconds": percentile(samples, 50),
            "warm_p95_seconds": percentile(samples, 95),
            "candidate_pairs_per_second": len(pairs)
            * repetitions
            / sum(samples),
            "repetitions": repetitions,
        },
        "decisions": [
            {"evidence_id": identity["id"], "rerank_score": float(score)}
            for identity, score in zip(identities, scores, strict=True)
        ],
        "warning": (
            "Research ceiling only. The locked suite stores evidence metadata rather "
            "than complete source passages, so this does not establish standalone quality."
        ),
    }
    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    with open(output_path, "w", encoding="utf-8") as output:
        json.dump(report, output, indent=2)
        output.write("\n")


if __name__ == "__main__":
    main()
