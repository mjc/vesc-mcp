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
    if len(sys.argv) not in (2, 3):
        print(f"usage: {sys.argv[0]} REPORT.json [SUITE.json]", file=sys.stderr)
        return 2
    report_path = Path(sys.argv[1])
    suite_path = (
        Path(sys.argv[2])
        if len(sys.argv) == 3
        else Path("tests/evaluation/v2/queries.json")
    )
    report = json.loads(report_path.read_text())
    suite = json.loads(suite_path.read_text())
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
    if len(report["candidates"]) != 4:
        raise AssertionError("bake-off report must contain four candidates")
    names = set()
    for candidate in report["candidates"]:
        name = candidate["candidate"]["name"]
        if name in names:
            raise AssertionError(f"duplicate candidate {name}")
        names.add(name)
        verify_report(f"{name}.semantic", candidate["semantic"], suite_queries)
        verify_report(f"{name}.hybrid", candidate["hybrid"], suite_queries)
    print(f"verified {len(report['candidates'])} candidates and lexical control")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
