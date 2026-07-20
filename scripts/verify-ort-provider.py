#!/usr/bin/env python3
"""Reject accelerator benchmark logs that do not prove accelerator execution."""

import argparse
import json
import re
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("log", type=Path)
    parser.add_argument("--provider", required=True)
    parser.add_argument("--allow-cpu-nodes", action="store_true")
    args = parser.parse_args()

    text = args.log.read_text()
    match = re.search(r"semantic-runtime: (\{[^\n]+\})", text)
    if match is None:
        raise SystemExit("missing semantic-runtime diagnostics")
    diagnostics = json.loads(match.group(1))
    if diagnostics["selected_provider"] != args.provider:
        raise SystemExit(
            f"selected {diagnostics['selected_provider']}, expected {args.provider}"
        )
    availability = f"{args.provider}=true"
    if availability not in diagnostics["provider_availability"]:
        raise SystemExit(f"provider is unavailable: {availability}")

    # ORT verbose logs name the provider for every assigned graph node. An
    # accelerator label without assignment evidence only proves registration.
    assigned = re.findall(
        r"(?:assigned to|placed on)\s+(\w+ExecutionProvider)", text, re.IGNORECASE
    )
    if not assigned:
        raise SystemExit("missing ONNX Runtime node-placement evidence")
    if args.provider not in assigned:
        raise SystemExit(f"no graph nodes assigned to {args.provider}")
    cpu_nodes = sum(provider == "CPUExecutionProvider" for provider in assigned)
    if cpu_nodes and not args.allow_cpu_nodes:
        raise SystemExit(f"invalid accelerator result: {cpu_nodes} CPU node assignments")

    print(json.dumps({"selected_provider": args.provider, "cpu_nodes": cpu_nodes}))


if __name__ == "__main__":
    main()
