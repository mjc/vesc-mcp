# vesc-mcp

MCP server that exposes VESC firmware and **vescpkg** knowledge to AI assistants — package discovery, manifest inspection, builds, and diagnostics.

## Quick start

```bash
git clone --recurse-submodules <repo-url>
cd vesc-mcp
direnv allow          # or: nix develop
make check            # fmt, clippy, nextest, doc
cargo run -p vesc-mcp-server
```

If you already cloned without submodules:

```bash
git submodule update --init --recursive
```

## Workspace layout

| Crate | Role |
|-------|------|
| `vesc-mcp-server` | stdio MCP binary |
| `vesc-mcp-core` | tools, config, errors |
| `vesc-domain` | VESC / vescpkg domain types |

## External repos (optional env vars)

Catalog path validation and knowledge indexing resolve upstream sources in this order: **environment override → initialized `vendor/` submodule → sibling checkout default**.

| Variable | Default (when unset) |
|----------|----------------------|
| `VESC_REFLOAT_ROOT` | `vendor/refloat` if submodule initialized, else `~/projects/refloat` |
| `VESC_BLDC_ROOT` | `vendor/bldc` if submodule initialized, else `~/projects/bldc` |
| `VESC_POC_ROOT` | `~/projects/vesc-rust-poc` |
| `VESC_VESC_TOOL_ROOT` | `vendor/vesc_tool` if submodule initialized, else `~/projects/vesc_tool` |
| `VESC_TOOL_PATH` | `vesc_tool` (binary on PATH for subprocess builds) |

Vendor submodules (optional but recommended for catalog validation):

| Submodule | Upstream |
|-----------|----------|
| `vendor/bldc` | [vedderb/bldc](https://github.com/vedderb/bldc) |
| `vendor/refloat` | [lukash/refloat](https://github.com/lukash/refloat) |
| `vendor/vesc_tool` | [vedderb/vesc_tool](https://github.com/vedderb/vesc_tool) |

## Beads backlog

```bash
br --project-db-root /Users/mjc/cfg/beads ready
br --project-db-root /Users/mjc/cfg/beads show br-foundation-qhu
```

Coordination epic: `br-flj` (multitask delivery waves).

See [docs/poc-integration.md](docs/poc-integration.md) for vesc-rust-poc path dependency strategy.

## MCP SDK

Uses [rmcp](https://github.com/modelcontextprotocol/rust-sdk) (official Rust MCP SDK) with stdio transport.
