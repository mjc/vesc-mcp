#!/usr/bin/env python3
"""Regenerate tests/fixtures/golden/poc-minimal.vescpkg via vesc-mcp-adapters (br-integrate-poc-5tu.11)."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main() -> None:
    result = subprocess.run(
        [
            "cargo",
            "run",
            "-q",
            "-p",
            "vesc-mcp-adapters",
            "--bin",
            "gen-poc-minimal-golden",
        ],
        cwd=ROOT,
        check=False,
    )
    raise SystemExit(result.returncode)


if __name__ == "__main__":
    main()
