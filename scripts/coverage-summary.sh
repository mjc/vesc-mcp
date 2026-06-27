#!/usr/bin/env bash
# Per-crate line coverage (lib src/ only) via cargo llvm-cov.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

FLOOR="${COVERAGE_FLOOR:-80}"
IGNORE=$(grep -v '^#' "$ROOT/.config/coverage-exclude.regex" | head -1)
LLVM_COV_IGNORE=(--ignore-filename-regex "$IGNORE")

parse_crate_lib_lines() {
  local crate="$1"
  cargo llvm-cov report 2>/dev/null | python3 -c "
import sys
crate = sys.argv[1]
cov = tot = 0
full_path_hits = 0
rows = []
for line in sys.stdin:
    if line.startswith('Filename') or line.startswith('-') or line.startswith('TOTAL'):
        continue
    parts = line.split()
    if len(parts) < 9:
        continue
    try:
        total = int(parts[7])
        missed = int(parts[8])
    except ValueError:
        continue
    path = parts[0]
    if '/crates/' in path and f'/crates/{crate}/src/' in path and '/src/bin/' not in path:
        tot += total
        cov += total - missed
        full_path_hits += 1
    elif '.rs' in path and 'nix/store' not in path and '/library/std/' not in path:
        rows.append((total, missed))

if full_path_hits == 0:
    cov = tot = 0
    for total, missed in rows:
        tot += total
        cov += total - missed

print(cov, tot)
" "$crate"
}

run_crate_tests() {
  local crate="$1"
  case "$crate" in
    vesc-mcp-core)
      cargo llvm-cov nextest -p vesc-mcp-core --profile ci \
        --features vesc-mcp-core/test-fixtures "${LLVM_COV_IGNORE[@]}"
      ;;
    *)
      cargo llvm-cov nextest -p "$crate" --profile ci "${LLVM_COV_IGNORE[@]}"
      ;;
  esac
}

echo "Per-crate line coverage (floor ${FLOOR}%, lib src/ only):"
echo ""

fail=0
for crate in vesc-domain vesc-knowledge-index vesc-mcp-adapters vesc-mcp-core; do
  cargo llvm-cov clean >/dev/null 2>&1 || true
  if ! run_crate_tests "$crate" >/dev/null 2>&1; then
    echo "  $crate: TEST RUN FAILED"
    fail=1
    continue
  fi
  read -r cov tot <<< "$(parse_crate_lib_lines "$crate")"
  if [[ "$tot" -eq 0 ]]; then
    echo "  $crate: no lib src lines measured"
    fail=1
    continue
  fi
  pct=$(python3 -c "print(f'{100*$cov/$tot:.1f}')")
  status="ok"
  if python3 -c "import sys; sys.exit(0 if float('$pct') >= $FLOOR else 1)"; then
    :
  else
    status="BELOW FLOOR"
    fail=1
  fi
  printf "  %-24s %6s / %-6s lines  %5s%%  %s\n" "$crate" "$cov" "$tot" "$pct" "$status"
done

echo ""
exit $fail
