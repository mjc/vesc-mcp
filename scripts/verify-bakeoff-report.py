#!/usr/bin/env python3
"""Independently recompute VESCM-165 quality metrics from a bake-off report."""

from __future__ import annotations

import json
import math
import sys
from pathlib import Path


CATEGORIES = {
    "conceptual_to_implementation",
    "identifier_to_declaration",
    "description_to_obscure_c_function",
    "vesc_tool_to_firmware",
    "refloat_to_control_code",
}
EPSILON = 1e-9


def mean(values: list[float]) -> float:
    return sum(values) / len(values) if values else 0.0


def metrics(queries: list[dict], results: list[dict]) -> dict[str, float]:
    if len(queries) != len(results):
        raise AssertionError("suite/report query counts differ")
    recall5: list[float] = []
    recall10: list[float] = []
    mrr10: list[float] = []
    ndcg10: list[float] = []
    zero: list[float] = []
    identifier: list[float] = []
    for query, result in zip(queries, results):
        if query["id"] != result["id"]:
            raise AssertionError(f"query order differs at {query['id']}")
        relevant = query["relevant"]
        positive = {key for key, grade in relevant.items() if grade > 0}
        returned = result["returned"]
        if any(not item.startswith("chunk-") for item in returned):
            raise AssertionError(f"{query['id']}: ranking contains a non-corpus ID")
        found5 = sum(item in positive for item in returned[:5])
        found10 = sum(item in positive for item in returned[:10])
        denominator = len(positive)
        recall5.append(found5 / denominator if denominator else 0.0)
        recall10.append(found10 / denominator if denominator else 0.0)
        rank = next(
            (index + 1 for index, item in enumerate(returned[:10]) if item in positive),
            None,
        )
        mrr10.append(1.0 / rank if rank else 0.0)
        ideal = sorted((grade for grade in relevant.values() if grade > 0), reverse=True)

        def gain(grade: int, index: int) -> float:
            return (2**grade - 1) / math.log2(index + 2)

        dcg = sum(
            gain(relevant[item], index)
            for index, item in enumerate(returned[:10])
            if item in relevant
        )
        idcg = sum(gain(grade, index) for index, grade in enumerate(ideal[:10]))
        ndcg10.append(dcg / idcg if idcg else 0.0)
        zero.append(float(not returned))
        if query["intent"] == "identifier":
            identifier.append(
                float(bool(returned) and relevant.get(returned[0]) == 2)
            )
    return {
        "query_count": float(len(queries)),
        "recall_at_5": mean(recall5),
        "recall_at_10": mean(recall10),
        "mrr_at_10": mean(mrr10),
        "ndcg_at_10": mean(ndcg10),
        "zero_result_rate": mean(zero),
        "exact_identifier_top_one": mean(identifier),
    }


def assert_close(name: str, actual: float, expected: float) -> None:
    if not math.isclose(actual, expected, rel_tol=EPSILON, abs_tol=EPSILON):
        raise AssertionError(f"{name}: report={actual} recomputed={expected}")


def verify_report(name: str, report: dict, suite_queries: list[dict]) -> None:
    if report["query_count"] != len(suite_queries):
        raise AssertionError(f"{name}: wrong query_count")
    expected = metrics(suite_queries, report["queries"])
    for field, value in expected.items():
        if field == "query_count":
            continue
        assert_close(f"{name}.{field}", report[field], value)
    categories = {
        query["failure_category"] for query in suite_queries
    }
    if categories != CATEGORIES:
        raise AssertionError(f"suite categories differ: {categories}")
    for category in CATEGORIES:
        selected = [
            (query, result)
            for query, result in zip(suite_queries, report["queries"])
            if query["failure_category"] == category
        ]
        category_metrics = metrics(
            [query for query, _ in selected],
            [result for _, result in selected],
        )
        grouped = report["by_failure_category"][category]
        for field in ("recall_at_5", "recall_at_10", "mrr_at_10", "ndcg_at_10"):
            assert_close(
                f"{name}.by_failure_category[{category}].{field}",
                grouped[field],
                category_metrics[field],
            )


def main() -> int:
    if len(sys.argv) not in (2, 3, 4):
        print(
            f"usage: {sys.argv[0]} REPORT.json [SUITE.json] [CANDIDATES.json]",
            file=sys.stderr,
        )
        return 2
    report_path = Path(sys.argv[1])
    suite_path = (
        Path(sys.argv[2])
        if len(sys.argv) >= 3
        else Path("tests/evaluation/v2/queries.json")
    )
    config_path = (
        Path(sys.argv[3])
        if len(sys.argv) == 4
        else Path("tests/benchmark/bakeoff-models.json")
    )
    report = json.loads(report_path.read_text())
    suite = json.loads(suite_path.read_text())
    config = json.loads(config_path.read_text())
    if report["schema"] != 1:
        raise AssertionError("unsupported bake-off report schema")
    if report["suite_id"] != suite["suite_id"]:
        raise AssertionError("report/suite identity differs")
    if report["corpus_digest"] != suite["corpus_digest"]:
        raise AssertionError("report/suite corpus digest differs")
    if report["corpus_documents"] != suite["corpus_documents"]:
        raise AssertionError("report/suite document count differs")
    if report["corpus_chunks"] != suite["corpus_chunks"]:
        raise AssertionError("report/suite chunk count differs")
    suite_queries = suite["queries"]
    verify_report("lexical", report["lexical"], suite_queries)
    expected_candidates = {
        candidate["name"]: candidate for candidate in config["candidates"]
    }
    if len(report["candidates"]) != len(expected_candidates):
        raise AssertionError("bake-off report candidate count differs from config")
    if set(expected_candidates) != {
        candidate["candidate"]["name"] for candidate in report["candidates"]
    }:
        raise AssertionError("report candidates differ from the pinned candidate config")
    names = set()
    shared_measurement: tuple[object, ...] | None = None
    shared_machine: dict | None = None
    for candidate in report["candidates"]:
        identity = candidate["candidate"]
        name = identity["name"]
        if name in names:
            raise AssertionError(f"duplicate candidate {name}")
        names.add(name)
        expected = expected_candidates[name]
        for field in (
            "model_id",
            "model_revision",
            "directory",
            "license",
            "production_eligible",
            "quantization",
            "onnx_sha256",
            "onnx_bytes",
        ):
            if identity[field] != expected[field]:
                raise AssertionError(
                    f"{name}: {field} differs from pinned candidate config"
                )
        benchmark = candidate["benchmark"]
        measurement = (
            benchmark["outer_batch_size"],
            benchmark.get("intra_threads"),
            benchmark.get("length_bucketed", False),
        )
        if shared_measurement is None:
            shared_measurement = measurement
            shared_machine = benchmark["machine"]
        elif measurement != shared_measurement:
            raise AssertionError(f"{name}: benchmark settings differ")
        elif benchmark["machine"] != shared_machine:
            raise AssertionError(f"{name}: machine identity differs")
        if benchmark["outer_batch_size"] != 8:
            raise AssertionError(f"{name}: final bake-off batch size must be 8")
        statistics = benchmark.get("token_statistics")
        if statistics is None or statistics["truncated_chunks"] != 0:
            raise AssertionError(f"{name}: report contains truncated provider input")
        if benchmark.get("peak_rss_bytes") is None:
            raise AssertionError(f"{name}: external peak RSS is missing")
        if not benchmark.get("vector_artifact_sha256"):
            raise AssertionError(f"{name}: vector artifact digest is missing")
        if benchmark["model_id"] != identity["model_id"]:
            raise AssertionError(f"{name}: benchmark model identity differs")
        if benchmark["model_revision"] != identity["model_revision"]:
            raise AssertionError(f"{name}: benchmark revision differs")
        if benchmark["corpus_digest"] != report["corpus_digest"]:
            raise AssertionError(f"{name}: benchmark corpus digest differs")
        if benchmark["corpus_chunks"] != report["evaluated_chunks"]:
            raise AssertionError(f"{name}: benchmark sample count differs")
        if benchmark["vector_count"] != report["evaluated_chunks"]:
            raise AssertionError(f"{name}: vector count does not cover the evaluated sample")
        verify_report(f"{name}.semantic", candidate["semantic"], suite_queries)
        verify_report(f"{name}.hybrid", candidate["hybrid"], suite_queries)
    print(f"verified {len(report['candidates'])} candidates and lexical control")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
