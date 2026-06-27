# vesc-mcp

MCP server that exposes VESC firmware and **vescpkg** knowledge to AI assistants — package discovery, manifest inspection, builds, diagnostics, and catalog-backed docs.

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

### Connect an MCP client

Add a stdio server entry (Cursor, Claude Desktop, etc.). Prefer `nix develop` so the toolchain matches CI; set repo roots via env vars rather than hardcoding paths:

```json
{
  "mcpServers": {
    "vesc-mcp": {
      "command": "nix",
      "args": ["develop", "-c", "vesc-mcp-server"],
      "cwd": "${workspaceFolder}",
      "env": {
        "VESC_PACKAGE_ROOTS": "${env:VESC_PACKAGE_ROOTS}",
        "VESC_REFLOAT_ROOT": "${env:VESC_REFLOAT_ROOT}",
        "VESC_POC_ROOT": "${env:VESC_POC_ROOT}"
      }
    }
  }
}
```

Fallback without Nix: build `vesc-mcp-server` and point `command` at the binary; see [docs/configuration.md](docs/configuration.md).

Verify the connection with the `ping` tool (optional `message` echoes back). Then use `list_vesc_packages` and `inspect_pkgdesc` on paths under your configured sandbox — offline examples live in [`tests/fixtures/`](tests/fixtures/README.md).

## Architecture overview

```
MCP Client  →  vesc-mcp-server (stdio)  →  vesc-mcp-core
                                              ├─ tools (ping, list, inspect, build, …)
                                              ├─ resources (vesc://catalog/*, vescpkg://…)
                                              ├─ vesc-domain (parse / validate vescpkg)
                                              ├─ vesc-mcp-adapters → vesc-pkg-build (POC)
                                              ├─ vesc-knowledge-index (embedded search)
                                              └─ catalog/ YAML + tests/fixtures/
```

| Crate | Role |
|-------|------|
| `vesc-mcp-server` | stdio MCP binary |
| `vesc-mcp-core` | tools, resources, config, MCP service |
| `vesc-domain` | VESC / vescpkg domain types and parsers |
| `vesc-mcp-adapters` | Host-side bridge to POC `vesc-pkg-build` |
| `vesc-knowledge-index` | Embedded firmware/package knowledge index |

See [docs/architecture.md](docs/architecture.md) for a detailed diagram and data-flow notes.

## Documentation

| Doc | Contents |
|-----|----------|
| [AGENTS.md](AGENTS.md) | MCP tool/resource cheat sheet, TDD workflow |
| [docs/configuration.md](docs/configuration.md) | Environment variables and `config.toml` |
| [docs/architecture.md](docs/architecture.md) | Crate diagram and boundaries |
| [docs/testing.md](docs/testing.md) | Red/green/refactor workflow |
| [docs/poc-integration.md](docs/poc-integration.md) | vesc-rust-poc path dependency |

## External repos (optional)

Catalog path validation and knowledge indexing resolve upstream sources in this order: **environment override → initialized `vendor/` submodule → sibling checkout default**. Full reference: [docs/configuration.md](docs/configuration.md).

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

Copy [`config.example.toml`](config.example.toml) to `~/.config/vesc-mcp/config.toml` for persistent paths.

## Beads backlog

```bash
br --project-db-root ~/cfg/beads ready
br --project-db-root ~/cfg/beads show br-docs-dx-8tw
```

Coordination epic: `br-flj` (multitask delivery waves).

## MCP SDK

Uses [rmcp](https://github.com/modelcontextprotocol/rust-sdk) (official Rust MCP SDK) with stdio transport.
