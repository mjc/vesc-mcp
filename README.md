# vesc-mcp

MCP server that exposes VESC firmware and **vescpkg** knowledge to AI assistants — package discovery, manifest inspection, builds, and diagnostics.

## Quick start

```bash
direnv allow          # or: nix develop
make check            # fmt, clippy, nextest, doc
cargo run -p vesc-mcp-server
```

## Workspace layout

| Crate | Role |
|-------|------|
| `vesc-mcp-server` | stdio MCP binary |
| `vesc-mcp-core` | tools, config, errors |
| `vesc-domain` | VESC / vescpkg domain types |

## External repos (optional env vars)

| Variable | Default |
|----------|---------|
| `VESC_REFLOAT_ROOT` | `~/projects/refloat` |
| `VESC_BLDC_ROOT` | `~/projects/bldc` |
| `VESC_POC_ROOT` | `~/projects/vesc-rust-poc` |

## Beads backlog

```bash
br --project-db-root /Users/mjc/cfg/beads ready
br --project-db-root /Users/mjc/cfg/beads show br-foundation-qhu
```

Coordination epic: `br-flj` (multitask delivery waves).

## MCP SDK

Uses [rmcp](https://github.com/modelcontextprotocol/rust-sdk) (official Rust MCP SDK) with stdio transport.
