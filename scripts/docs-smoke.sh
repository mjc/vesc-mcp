#!/usr/bin/env bash
# Optional docs/CI smoke: spawn vesc-mcp-server and assert tools/list >= DOCS_SMOKE_MIN_TOOLS (default 7).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

export DOCS_SMOKE_MIN_TOOLS="${DOCS_SMOKE_MIN_TOOLS:-7}"

exec nix develop -c cargo nextest run -p vesc-mcp-server --test smoke_stdio smoke_tools_list_count_at_least_seven
