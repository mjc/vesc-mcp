#!/usr/bin/env python3
"""Capture the VESCM-207 judged baseline through the real MCP HTTP service.

The script deliberately has no third-party dependencies.  It records the
bounded first response, rather than pretending that an in-process benchmark
is equivalent to what a chat client sees.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import statistics
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


def sha256_json(value: Any) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode()
    return "sha256:" + hashlib.sha256(encoded).hexdigest()


def parse_sse(body: bytes) -> list[dict[str, Any]]:
    """Parse JSON ``data:`` events from a streamable HTTP response."""
    events: list[dict[str, Any]] = []
    for line in body.decode("utf-8", errors="replace").splitlines():
        if not line.startswith("data:"):
            continue
        payload = line[5:].strip()
        if not payload:
            continue
        try:
            value = json.loads(payload)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            events.append(value)
    return events


def first_json(value: Any) -> Any:
    """Extract the JSON text payload returned by an MCP tools/call result."""
    if isinstance(value, dict) and "content" in value:
        for item in value["content"]:
            if isinstance(item, dict) and item.get("type") == "text":
                try:
                    return json.loads(item["text"])
                except (KeyError, TypeError, json.JSONDecodeError):
                    pass
    return value


def result_identity(result: dict[str, Any], default_repository: str | None = None) -> dict[str, Any]:
    source = result.get("source") or {}
    provenance = result.get("provenance") or {}
    span = source.get("source_span") or provenance.get("source_span")
    if span is None and any(key in source for key in ("line", "end_line", "start_byte", "end_byte")):
        span = {
            "start_line": source.get("line"),
            "end_line": source.get("end_line"),
            "start_byte": source.get("start_byte"),
            "end_byte": source.get("end_byte"),
        }
    return {
        "id": result.get("id"),
        "chunk_id": result.get("chunk_id"),
        "document_id": result.get("document_id"),
        "repository": source.get("repository") or source.get("repo") or provenance.get("repository") or provenance.get("repo") or default_repository,
        "path": source.get("path") or provenance.get("path"),
        "revision": source.get("revision") or provenance.get("revision"),
        "source_span": span,
        "name": result.get("name"),
        "summary": result.get("summary"),
        "passage": result.get("passage") or provenance.get("passage"),
        "resource_uri": result.get("resource_uri") or provenance.get("resource_uri"),
    }


def normalized_digest(searches: list[dict[str, Any]]) -> str:
    """Digest judged identities/order, excluding timing and transport details."""
    judged = []
    for item in searches:
        judged.append(
            {
                "id": item["id"],
                "mode": item["mode"],
                "detail": item["detail"],
                "snapshot_id": item.get("snapshot_id"),
                "results": item["ordered_results"],
                "warnings": item.get("warning_codes", []),
            }
        )
    return sha256_json(judged)


def _text(result: dict[str, Any]) -> str:
    return " ".join(
        str(result.get(key, ""))
        for key in ("id", "name", "summary", "passage")
    ).lower()


def matches_gold(result: dict[str, Any], gold: dict[str, Any]) -> bool:
    identity = result_identity(result, gold.get("repository"))
    repository = gold.get("repository")
    path = identity.get("path") or ""
    if repository and identity.get("repository") != repository:
        return False
    expected_path = gold.get("path")
    prefix = gold.get("path_prefix")
    if expected_path and path != expected_path:
        return False
    if prefix and not path.startswith(prefix):
        return False
    revision = gold.get("revision")
    if revision and identity.get("revision") != revision:
        return False
    return all(term.lower() in _text(result) for term in gold.get("required_terms", []))


def completeness(query: dict[str, Any], results: list[dict[str, Any]]) -> dict[str, Any]:
    matched = []
    missing = []
    for gold in query.get("gold", []):
        if any(matches_gold(result, gold) for result in results):
            matched.append(gold.get("id", gold))
        else:
            missing.append(gold.get("id", gold))
    return {
        "required": len(query.get("gold", [])),
        "matched": len(matched),
        "matched_ids": matched,
        "missing_ids": missing,
        "complete": not missing,
    }


def trace_fallback(query: dict[str, Any], judged: dict[str, Any], include_observed: bool = True) -> dict[str, Any]:
    observed = query.get("observed_manual_reads")
    if observed is not None and include_observed:
        return {"kind": "observed_trace", "manual_file_reads": len(observed), "paths": observed}
    return {
        "kind": "bounded_response_eval",
        "manual_file_reads_required_by_evaluation": 0 if judged["complete"] else None,
        "reason": "zero when every reviewed decisive span is present in the first response",
    }


class McpHttp:
    def __init__(self, endpoint: str, token: str):
        self.endpoint = endpoint
        self.token = token
        self.session_id: str | None = None
        self.request_id = 0

    def call(self, method: str, params: dict[str, Any] | None = None, notification: bool = False) -> tuple[Any, int, int]:
        self.request_id += 1
        request_id = self.request_id
        payload: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
        if not notification:
            payload["id"] = request_id
        if params is not None:
            payload["params"] = params
        headers = {
            "authorization": "Bearer " + self.token,
            "content-type": "application/json",
            "accept": "application/json, text/event-stream",
        }
        if self.session_id:
            headers["mcp-session-id"] = self.session_id
        request = urllib.request.Request(self.endpoint, data=json.dumps(payload).encode(), headers=headers, method="POST")
        started = time.perf_counter_ns()
        try:
            with urllib.request.urlopen(request, timeout=180) as response:
                body = response.read()
                response_headers = response.headers
        except urllib.error.HTTPError as error:
            body = error.read()
            raise RuntimeError(f"MCP HTTP {error.code}: {body[:500].decode(errors='replace')}") from error
        elapsed_us = (time.perf_counter_ns() - started) // 1_000
        if response_headers.get("mcp-session-id"):
            self.session_id = response_headers["mcp-session-id"]
        if notification:
            return None, len(body), elapsed_us
        events = parse_sse(body)
        for event in events:
            if event.get("id") == request_id:
                if "error" in event:
                    raise RuntimeError(json.dumps(event["error"], sort_keys=True))
                return event.get("result"), len(body), elapsed_us
        raise RuntimeError(f"MCP response did not contain id {request_id}")

    def initialize(self) -> dict[str, Any]:
        result, _, _ = self.call(
            "initialize",
            {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "vescm-207-baseline", "version": "1"}},
        )
        self.call("notifications/initialized", notification=True)
        return result

    def tool(self, name: str, arguments: dict[str, Any]) -> tuple[Any, int, int]:
        result, size, elapsed = self.call("tools/call", {"name": name, "arguments": arguments})
        return first_json(result), size, elapsed


def run(args: argparse.Namespace) -> dict[str, Any]:
    suite = json.loads(Path(args.suite).read_text())
    token = args.token or os.environ.get("VESC_MCP_HTTP_AUTH_TOKEN")
    if not token:
        raise SystemExit("set VESC_MCP_HTTP_AUTH_TOKEN or pass --token")
    client = McpHttp(args.endpoint, token)
    initialize = client.initialize()
    tools, tools_bytes, _ = client.call("tools/list", {})
    capabilities, _, _ = client.tool("get_vesc_knowledge_capabilities", {})
    snapshot_ids: set[str] = set()
    snapshot_metadata: dict[str, Any] | None = None
    searches = []
    modes = tuple(mode for mode in args.modes.split(",") if mode)
    if not modes:
        raise SystemExit("--modes must contain at least one mode")
    for query in suite["queries"]:
        repository = query.get("repository")
        if repository:
            client.tool("set_current_repository", {"repository": repository})
        for mode in modes:
            params = {
                "query": query["query"],
                "detail": query.get("detail", "full"),
                "mode": mode,
                "limit": query.get("limit", 5),
                "max_context_bytes": query.get("max_context_bytes", 8192),
                "max_response_bytes": query.get("max_response_bytes", 65536),
            }
            filters = dict(query.get("filters", {}))
            if repository:
                filters["repository"] = repository
            if filters:
                params["filters"] = filters
            if args.snapshot_id:
                params["snapshot_id"] = args.snapshot_id
            response, response_bytes, elapsed_us = client.tool("search_vesc_knowledge", params)
            response = response if isinstance(response, dict) else {"ok": False, "error": "invalid response"}
            results = response.get("results", [])
            judged = completeness(query, results)
            identities = [result_identity(result, repository) for result in results]
            paths = [identity.get("path") for identity in identities if identity.get("path")]
            contents = [identity.get("passage") or identity.get("summary") or "" for identity in identities]
            distinct_contents = len(set(contents))
            expanded_context_rows = sum(
                isinstance(result.get("explanation"), dict)
                and bool(result["explanation"].get("expansion_reason"))
                for result in results
            )
            snapshot_id = response.get("index", {}).get("snapshot_id") or response.get("snapshot_id")
            if snapshot_metadata is None and isinstance(response.get("index"), dict):
                index = response["index"]
                snapshot_metadata = {
                    key: index.get(key)
                    for key in (
                        "snapshot_id", "snapshot_profile", "repositories", "corpus_version",
                        "corpus_digest", "document_count", "chunk_count", "source_count",
                        "diagnostic_count", "component_versions", "lexical_checksum", "limits",
                    )
                    if key in index
                }
            if snapshot_id:
                snapshot_ids.add(snapshot_id)
            searches.append(
                {
                    "id": f"{query['id']}::{mode}",
                    "query_id": query["id"],
                    "query": query["query"],
                    "repository": repository,
                    "mode": response.get("mode_used", response.get("mode")),
                    "mode_requested": response.get("mode_requested"),
                    "detail": response.get("detail"),
                    "snapshot_id": snapshot_id,
                    "response_bytes": response_bytes,
                    "latency_us": elapsed_us,
                    "result_count": len(results),
                    "distinct_source_paths": len(set(paths)),
                    "distinct_content_rows": distinct_contents,
                    "duplicate_content_rows": len(contents) - distinct_contents,
                    "expanded_context_rows": expanded_context_rows,
                    "context_bytes": sum(len(content.encode("utf-8")) for content in contents),
                    "warning_codes": response.get("warning_codes", []),
                    "ordered_results": identities,
                    "judged": judged,
                    "fallback": trace_fallback(query, judged, include_observed=mode == "auto"),
                }
            )
    return {
        "schema": 1,
        "ticket": "VESCM-207",
        "captured_at": args.captured_at or time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "endpoint": "local authenticated MCP HTTP service",
        "service": initialize.get("serverInfo", {}),
        "suite": suite.get("schema"),
        "tools_list_digest": sha256_json(tools),
        "tools_list_bytes": tools_bytes,
        "capabilities": capabilities,
        "snapshot_metadata": snapshot_metadata,
        "snapshot_ids": sorted(snapshot_ids),
        "searches": searches,
        "judged_output_digest": normalized_digest(searches),
        "summary": {
            "queries": len(searches),
            "query_cases": len(suite["queries"]),
            "requested_modes": list(modes),
            "complete_queries": sum(item["judged"]["complete"] for item in searches),
            "first_response_completeness": sum(item["judged"]["matched"] for item in searches),
            "required_evidence": sum(item["judged"]["required"] for item in searches),
            "observed_manual_reads": sum(item["fallback"].get("manual_file_reads", 0) for item in searches),
            "bounded_response_manual_reads_required": sum(item["fallback"].get("manual_file_reads_required_by_evaluation", 0) or 0 for item in searches),
            "latency_us": {
                "p50": int(statistics.median(item["latency_us"] for item in searches)) if searches else 0,
                "max": max((item["latency_us"] for item in searches), default=0),
            },
            "response_bytes": {
                "p50": int(statistics.median(item["response_bytes"] for item in searches)) if searches else 0,
                "max": max((item["response_bytes"] for item in searches), default=0),
            },
            "candidate_count": sum(item["result_count"] for item in searches),
            "expanded_context_rows": sum(item["expanded_context_rows"] for item in searches),
            "context_bytes": sum(item["context_bytes"] for item in searches),
            "duplicate_content_rows": sum(item["duplicate_content_rows"] for item in searches),
            "service_memory": {
                "available": False,
                "reason": "authenticated HTTP MCP does not expose server RSS; use the in-process benchmark for retained-memory measurements",
            },
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", default="tests/evaluation/v4/queries.json")
    parser.add_argument("--output", required=True)
    parser.add_argument("--endpoint", default="http://127.0.0.1:8444/mcp")
    parser.add_argument("--token")
    parser.add_argument("--snapshot-id")
    parser.add_argument("--captured-at")
    parser.add_argument("--modes", default="lexical,hybrid,auto", help="comma-separated MCP search modes")
    args = parser.parse_args()
    report = run(args)
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps(report["summary"], sort_keys=True))


if __name__ == "__main__":
    main()
