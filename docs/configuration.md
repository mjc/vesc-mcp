# Configuration

vesc-mcp loads settings once per process. Precedence: **environment variables → `~/.config/vesc-mcp/config.toml` → defaults**.

Copy [`config.example.toml`](../config.example.toml) to `~/.config/vesc-mcp/config.toml` and adjust paths for your machine.

## Environment variables

### Package sandbox

| Variable | Config key | Default | Description |
|----------|------------|---------|-------------|
| `VESC_PACKAGE_ROOTS` | `[paths] package_roots` | *(empty)* | Comma- or colon-separated directories the server may scan, inspect, build, and run checks in. Required for live package paths outside tests. |

Example:

```bash
export VESC_PACKAGE_ROOTS="$HOME/projects/refloat:$HOME/projects/vesc-rust-poc"
```

Tools reject paths outside these roots with `outside configured VESC_PACKAGE_ROOTS`.

### Upstream repository roots

Used for catalog path validation, knowledge indexing, and source attribution. Resolution order per repo: **env override → initialized `vendor/` submodule → sibling default**.

| Variable | Config key | Sibling default |
|----------|------------|-----------------|
| `VESC_REFLOAT_ROOT` | `refloat_root` | `~/projects/refloat` |
| `VESC_BLDC_ROOT` | `bldc_root` | `~/projects/bldc` |
| `VESC_POC_ROOT` | `poc_root` | `~/projects/vesc-rust-poc` |
| `VESC_VESC_TOOL_ROOT` | `vesc_tool_root` | `~/projects/vesc_tool` |

Relative paths in config (e.g. `vendor/refloat`) resolve against the vesc-mcp workspace root.

### Build tooling

| Variable | Config key | Default | Description |
|----------|------------|---------|-------------|
| `VESC_TOOL_PATH` | `vesc_tool` | `vesc_tool` | Path to the `vesc_tool` binary for `build_vescpkg`. |

### Server features

| Variable | Config key | Default | Description |
|----------|------------|---------|-------------|
| `VESC_MCP_ENABLE_FLASH` | `[features] enable_flash` | `false` | Gate flash/upload tools. Accepts `1`, `true`, `yes`, `on` (case-insensitive). **Leave unset in normal development.** |

### Config file location

| Variable | Default | Description |
|----------|---------|-------------|
| `VESC_MCP_CONFIG` | `~/.config/vesc-mcp/config.toml` | Override path to the TOML config file. |

### Workspace discovery

| Variable | Default | Description |
|----------|---------|-------------|
| `VESC_MCP_WORKSPACE_ROOT` | Auto-detect | Force vesc-mcp repo root (directory containing `catalog/` or `flake.nix`). Used to resolve `vendor/…` relative paths. |

### Logging

| Variable | Default | Description |
|----------|---------|-------------|
| `RUST_LOG` | *(none)* | Tracing filter for `vesc-mcp-server` stderr (e.g. `vesc_mcp_core=debug`, `info`). |

### Implicit

| Variable | Used for |
|----------|----------|
| `HOME` | Expanding `~/…` in paths; default config directory |

## Example `config.toml`

```toml
[paths]
package_roots = [
  "~/projects/refloat",
  "~/projects/vesc-rust-poc",
]
refloat_root = "vendor/refloat"
bldc_root = "vendor/bldc"
poc_root = "~/projects/vesc-rust-poc"
vesc_tool_root = "vendor/vesc_tool"
vesc_tool = "vesc_tool"

[features]
enable_flash = false
```

## MCP client wiring

Prefer passing repo roots through the MCP server `env` block so assistants inherit your machine layout:

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
        "VESC_BLDC_ROOT": "${env:VESC_BLDC_ROOT}",
        "VESC_POC_ROOT": "${env:VESC_POC_ROOT}",
        "VESC_VESC_TOOL_ROOT": "${env:VESC_VESC_TOOL_ROOT}"
      }
    }
  }
}
```

Without Nix, set `command` to the absolute path of a release-built `vesc-mcp-server` binary and ensure the same env vars are exported in your shell profile.

## Test / CI defaults

When `VESC_PACKAGE_ROOTS` is unset and the `test-fixtures` feature is enabled (Makefile / CI), the server automatically allows `tests/fixtures/` as the sandbox root. Production MCP sessions should always set explicit roots.

Optional live-repo tests (`cargo nextest run --run-ignored all`) require sibling checkouts via the env vars above — see [testing.md](testing.md).
