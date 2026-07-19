#!/usr/bin/env python3
"""Fail closed when the committed VESCM-196 bakeoff is inconsistent."""

import json
import pathlib
import sys


ETTIN = (
    "ettin-reranker-17m-v1-qint8-avx2",
    "ettin-reranker-32m-v1-qint8-avx2",
    "ettin-reranker-68m-v1-qint8-avx2",
)


def read(path: pathlib.Path) -> dict:
    with path.open(encoding="utf-8") as source:
        return json.load(source)


def main() -> None:
    root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "release/benchmarks/vescm-196")
    reports = [read(root / f"{name}.json") for name in ETTIN]
    qwen = read(root / "qwen3-reranker-0.6b-bf16.json")
    candidate_hashes = {report["candidate_set_sha256"] for report in reports}
    assert len(candidate_hashes) == 1
    expected_ids = {row["evidence_id"] for row in reports[0]["decisions"]}
    assert all({row["evidence_id"] for row in report["decisions"]} == expected_ids for report in reports)
    assert {row["evidence_id"] for row in qwen["decisions"]} == expected_ids
    assert all(report["candidate_count"] == 9 for report in reports)
    assert qwen["candidate_count"] == 9
    assert all(report["max_length"] == 512 and report["batch_size"] == 8 for report in reports)
    assert qwen["max_length"] == 512 and qwen["batch_size"] == 8
    for report in reports:
        comparison = report["comparison"]
        assert comparison["no_reranker_global"]["path_complete_at_n"] == 1.0
        assert comparison["reranker_global"]["path_complete_at_n"] == 0.0
        assert comparison["reranker_per_facet"]["path_complete_at_n"] == 1.0
        assert report["peak_rss_bytes"] > 0
    assert qwen["comparison"]["reranker_global"]["path_complete_at_n"] == 0.0
    assert qwen["comparison"]["reranker_per_facet"]["path_complete_at_n"] == 1.0
    assert qwen["peak_rss_bytes"] > 0
    print("VESCM-196 Ryzen bakeoff verified: default=no-reranker")


if __name__ == "__main__":
    main()
