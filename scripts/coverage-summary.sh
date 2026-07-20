#!/usr/bin/env bash
# Per-crate line coverage (lib src/ only) from an existing LCOV report.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT="${1:-$ROOT/lcov.info}"
FLOOR="${COVERAGE_FLOOR:-80}"
CRATES=(vesc-domain vesc-knowledge-index vesc-mcp-adapters vesc-mcp-core)

if [[ $# -gt 1 || ! -f "$REPORT" ]]; then
  echo "usage: $0 [lcov-report]" >&2
  exit 2
fi

python3 - "$REPORT" "$FLOOR" "${CRATES[@]}" <<'PY'
import sys
from pathlib import Path

report, floor, *crates = sys.argv[1:]
floor = float(floor)
lines = {crate: {} for crate in crates}
active = None
source = ""

for row in Path(report).read_text().splitlines():
    if row.startswith("SF:"):
        source = row[3:]
        active = next(
            (crate for crate in crates if f"crates/{crate}/src/" in source),
            None,
        )
        if "/src/bin/" in f"/{source}":
            active = None
    elif active and row.startswith("DA:"):
        line, hits, *_ = row[3:].split(",")
        key = (source, line)
        lines[active][key] = lines[active].get(key, False) or int(hits) > 0

print(f"Per-crate line coverage (floor {floor:g}%, lib src/ only):\n")
failed = False
for crate in crates:
    total = len(lines[crate])
    covered = sum(lines[crate].values())
    percent = 100 * covered / total if total else 0
    status = "ok" if total and percent >= floor else "BELOW FLOOR"
    failed |= status != "ok"
    print(f"  {crate:<24} {covered:6} / {total:<6} lines  {percent:5.1f}%  {status}")
print()
raise SystemExit(failed)
PY
