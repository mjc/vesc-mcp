#!/usr/bin/env python3
"""Combine isolated one-candidate VESCM-165 reports into one report."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def load(path: Path) -> dict:
    value = json.loads(path.read_text())
    if value.get("schema") != 1:
        raise ValueError(f"{path}: unsupported report schema")
    candidates = value.get("candidates")
    if not isinstance(candidates, list) or len(candidates) != 1:
        raise ValueError(f"{path}: expected exactly one candidate")
    return value


def combine(reports: list[dict], peak_rss: dict[str, int] | None = None) -> dict:
    if not reports:
        raise ValueError("at least one report is required")
    first = reports[0]
    shared = (
        "suite_id",
        "corpus_digest",
        "corpus_documents",
        "corpus_chunks",
        "evaluated_chunks",
    )
    for report in reports[1:]:
        for field in shared:
            if report[field] != first[field]:
                raise ValueError(f"report {field} differs")
    candidates = []
    names = set()
    warnings: list[str] = []
    for report in reports:
        candidate = report["candidates"][0]
        name = candidate["candidate"]["name"]
        if name in names:
            raise ValueError(f"duplicate candidate {name}")
        names.add(name)
        if peak_rss is not None and name in peak_rss:
            candidate["benchmark"]["peak_rss_bytes"] = peak_rss[name]
        candidates.append(candidate)
        warnings.extend(report.get("warnings", []))
    if peak_rss is not None and all(
        candidate["benchmark"].get("peak_rss_bytes") is not None
        for candidate in candidates
    ):
        warnings = [warning for warning in warnings if not warning.startswith("peak RSS is")]
    return {
        "schema": 1,
        "suite_id": first["suite_id"],
        "corpus_digest": first["corpus_digest"],
        "corpus_documents": first["corpus_documents"],
        "corpus_chunks": first["corpus_chunks"],
        "evaluated_chunks": first["evaluated_chunks"],
        "lexical": first["lexical"],
        "candidates": candidates,
        "machine": first["machine"],
        "warnings": list(dict.fromkeys(warnings)),
    }


def markdown(report: dict) -> str:
    lines = [
        "# Embedding bake-off",
        "",
        f"- Suite: `{report['suite_id']}`",
        f"- Corpus: `{report['corpus_digest']}`",
        f"- Documents / chunks: {report['corpus_documents']} / {report['corpus_chunks']}",
        f"- Evaluated chunks: {report['evaluated_chunks']}",
        "",
        "| Candidate | Provider (s) | Chunks/s | Peak RSS (bytes) | Semantic R@5 | Hybrid R@5 | Hybrid MRR@10 |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]
    for candidate in report["candidates"]:
        benchmark = candidate["benchmark"]
        provider = benchmark["provider_inference"]["p50_us"] / 1_000_000
        throughput = benchmark.get("throughput_chunks_per_second")
        if throughput is None and provider > 0:
            throughput = report["evaluated_chunks"] / provider
        peak_rss = benchmark.get("peak_rss_bytes")
        lines.append(
            "| {name} | {provider:.3f} | {throughput} | {rss} | {semantic:.4f} | "
            "{hybrid:.4f} | {mrr:.4f} |".format(
                name=candidate["candidate"]["name"],
                provider=provider,
                throughput=(f"{throughput:.3f}" if throughput is not None else "—"),
                rss=(str(peak_rss) if peak_rss is not None else "—"),
                semantic=candidate["semantic"]["recall_at_5"],
                hybrid=candidate["hybrid"]["recall_at_5"],
                mrr=candidate["hybrid"]["mrr_at_10"],
            )
        )
    lines.extend(
        [
            "",
            "Peak RSS is an externally measured process maximum; retained RSS "
            "deltas remain separate benchmark fields.",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("reports", nargs="+", type=Path)
    parser.add_argument("--json-out", required=True, type=Path)
    parser.add_argument("--markdown-out", required=True, type=Path)
    parser.add_argument(
        "--peak-rss",
        action="append",
        default=[],
        metavar="NAME=BYTES",
        help="attach externally measured peak RSS to a candidate (repeatable)",
    )
    args = parser.parse_args()
    peak_rss: dict[str, int] = {}
    for entry in args.peak_rss:
        name, separator, value = entry.partition("=")
        if not separator or not name or not value:
            raise ValueError("--peak-rss entries must be NAME=BYTES")
        if name in peak_rss:
            raise ValueError(f"duplicate peak RSS entry for {name}")
        peak_rss[name] = int(value)
    report = combine([load(path) for path in args.reports], peak_rss)
    args.json_out.write_text(json.dumps(report, indent=2) + "\n")
    args.markdown_out.write_text(markdown(report))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
