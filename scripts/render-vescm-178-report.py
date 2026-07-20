#!/usr/bin/env python3
"""Render the tracked VESCM-178 JSON evidence as Markdown."""

import argparse
import json
import pathlib
import statistics
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
EVIDENCE = ROOT / "release/benchmarks/vescm-178/report.json"
OUTPUT = ROOT / "release/benchmarks/vescm-178/report.md"


def table(headers, rows):
    lines = ["| " + " | ".join(headers) + " |", "|" + "|".join("---" for _ in headers) + "|"]
    lines.extend("| " + " | ".join(map(str, row)) + " |" for row in rows)
    return "\n".join(lines)


def timing(value):
    if value is None:
        return "n/a"
    return f"{value:.3f}"


def median_range(samples, field, formatter=timing):
    values = [sample[field] for sample in samples]
    return f"{formatter(statistics.median(values))} [{formatter(min(values))}–{formatter(max(values))}]"


def render(data):
    identity = data["identity"]
    gates = data["quality_gates"]
    lines = [
        "# VESCM-178 integrated RAG performance report",
        "",
        f"Status: **{data['status']}**",
        "",
        data["decision"],
        "",
        "## Pinned identity",
        "",
        f"- Host: {identity['host']}",
        f"- Corpus: `{identity['corpus_digest']}` ({identity['documents']} documents / {identity['chunks']} chunks)",
        f"- Model: `{identity['model']}` at `{identity['model_revision']}`",
        f"- Provider: {identity['provider']}; batch {identity['batch_size']}; {identity['intra_op_threads']} intra-op threads",
        f"- Runtime/toolchain: ONNX Runtime {identity['onnx_runtime']}; rustc {identity['rustc']}; LLVM {identity['llvm']}; `{identity['target']}`",
        f"- Release profile: LTO `{str(identity['release_lto']).lower()}`; {identity['release_codegen_units']} codegen unit; no `target-cpu=native` override",
        "- Sources: " + ", ".join(f"`{name}@{revision}`" for name, revision in identity["sources"].items()),
        "",
        "## Lifecycle before/after",
        "",
        table(
            ["Run", "n", "Build s median [range]", "External s median [range]", "Peak RSS B median [range]", "Ingest s median [range]", "Chunk s median [range]", "Lexical s median [range]", "Validate s median [range]"],
            [[row["label"], len(row["samples"]), median_range(row["samples"], "build_seconds"), median_range(row["samples"], "external_seconds"), median_range(row["samples"], "peak_rss_bytes", str), median_range(row["samples"], "ingestion_seconds"), median_range(row["samples"], "chunking_seconds"), median_range(row["samples"], "lexical_seconds"), median_range(row["samples"], "validation_seconds")] for row in data["lifecycle"]],
        ),
        "",
        "## Full semantic builds",
        "",
        table(
            ["Run", "Build s", "Provider s", "Chunks/s", "External s", "Peak RSS KiB"],
            [[row["run"], timing(row["build_seconds"]), timing(row["provider_seconds"]), timing(row["throughput_chunks_per_second"]), timing(row["external_seconds"]), row["peak_rss_kib"]] for row in data["semantic_builds"]],
        ),
        "",
        "Semantic build median [range]: "
        + ", ".join(
            [
                "build " + median_range(data["semantic_builds"], "build_seconds") + " s",
                "provider " + median_range(data["semantic_builds"], "provider_seconds") + " s",
                "external " + median_range(data["semantic_builds"], "external_seconds") + " s",
                "peak RSS " + median_range(data["semantic_builds"], "peak_rss_kib", str) + " KiB",
            ]
        )
        + ".",
        "",
        "Run 3 phase isolation: "
        + ", ".join(
            f"{name.replace('_seconds', '').replace('_', ' ')} {timing(value)} s"
            for name, value in data["semantic_phase_breakdown"].items()
            if name != "run"
        )
        + ".",
        "",
        "## Quality",
        "",
        table(
            ["Mode", "R@5", "R@10", "MRR@10", "nDCG@10", "Exact ID top-1"],
            [[row["mode"], f"{row['recall_at_5']:.4f}", f"{row['recall_at_10']:.4f}", f"{row['mrr_at_10']:.4f}", f"{row['ndcg_at_10']:.4f}", f"{row['identifier_top_1']:.4f}"] for row in data["quality"]],
        ),
        "",
        f"Locked hybrid gates: R@5 ≥ {gates['recall_at_5']:.2f}, MRR@10 ≥ {gates['mrr_at_10']:.2f}, nDCG@10 ≥ {gates['ndcg_at_10']:.2f}, exact-ID top-1 = {gates['identifier_top_1']:.1f}. Result: **{'PASS' if gates['passed'] else 'FAIL'}**.",
        "",
        "## Query latency",
        "",
    ]
    query = data.get("query_benchmark")
    if query:
        lines.extend([
            f"Cold initialization: {query['cold_initialization_us']} µs. First query: {query['first_query_us']} µs. Warm embedding p50/p95: {query['embedding_p50_us']}/{query['embedding_p95_us']} µs ({query['samples']} samples, {query['warmup_iterations']} warmup, {query['repetitions']} repetitions across {query['query_count']} queries and {query['vector_count']} × {query['vector_dimension']}-dimensional vectors).",
            f"Retained RSS delta: {query['rss_retained_delta_bytes']} B ({query['rss_before_queries_bytes']} B before / {query['rss_after_queries_bytes']} B after).",
            "",
            table(["K", "Samples", "Min µs", "p50 µs", "p95 µs", "Max µs"], [[row["k"], row["samples"], row["min_us"], row["p50_us"], row["p95_us"], row["max_us"]] for row in query["exact_search"]]),
        ])
    else:
        lines.append("Not recorded.")
    lines.extend([
        "",
        "## Batch and thread sweeps",
        "",
        table(["Order", "Batch", "Seconds", "Chunks/s", "Padding %", "Peak RSS GiB"], [[row["order"], row["batch"], row["seconds"], row["throughput"], row["padding_percent"], row["peak_rss_gib"]] for row in data["batch_sweep"]]),
        "",
        table(["Threads", "Seconds", "Chunks/s", "Peak RSS GiB"], [[row["threads"], row["seconds"], row["throughput"], row["peak_rss_gib"]] for row in data["thread_sweep"]]),
        "",
        "## Provider matrix",
        "",
        table(["Provider", "Seconds", "Chunks/s", "Peak RSS KiB", "Usable"], [[row["provider"], timing(row["seconds"]), timing(row["throughput"]), row["peak_rss_kib"] if row["peak_rss_kib"] is not None else "n/a", row["usable"]] for row in data["provider_matrix"]]),
        "",
        "Git ingestion repetitions: " + ", ".join(f"{value:.3f} ms" for value in data["git_ingestion"]["git_ingestion_milliseconds"]) + f"; full warm builds: {data['git_ingestion']['total_seconds']} s; byte-identical lexical artifact: {data['git_ingestion']['lexical_artifact_byte_identical']}.",
        "",
        "## Deterministic artifacts",
        "",
        table(["Artifact", "Bytes", "SHA-256", "Repeat identical"], [[row["name"], row["bytes"], f"`{row['sha256']}`", row["byte_identical_across_final_builds"]] for row in data["artifacts"]]),
        "",
        "## Retained and rejected attempts",
        "",
        table(["Attempt", "Decision", "Reason"], [[row["name"], row["decision"], row["reason"]] for row in data["attempts"]]),
        "",
        "## Remaining top costs",
        "",
        *[f"- {item}" for item in data["top_costs"]],
        "",
        "## Acceptance gaps",
        "",
        *([f"- {item}" for item in data["acceptance_gaps"]] or ["None."]),
        "",
        "Evidence commits: " + ", ".join(f"`{commit}`" for commit in data["evidence_commits"]),
        "",
    ])
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    rendered = render(json.loads(EVIDENCE.read_text()))
    if args.check:
        if not OUTPUT.exists() or OUTPUT.read_text() != rendered:
            print(f"{OUTPUT.relative_to(ROOT)} is stale", file=sys.stderr)
            return 1
    else:
        OUTPUT.write_text(rendered)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
