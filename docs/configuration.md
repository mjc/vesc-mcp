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

### Streamable HTTP

Run `vesc-mcp-server --http` to serve one shared, long-lived MCP endpoint. The
default is local-only at `http://127.0.0.1:8080/mcp`; stdio remains the default
when `--http` is absent.

| Variable | Default | Description |
|----------|---------|-------------|
| `VESC_MCP_HTTP_BIND` | `127.0.0.1:8080` | Listen address. |
| `VESC_MCP_HTTP_PATH` | `/mcp` | Streamable HTTP endpoint path. |
| `VESC_MCP_HTTP_ALLOWED_HOSTS` | `localhost,127.0.0.1,::1` | Comma-separated rmcp Host allowlist. |
| `VESC_MCP_HTTP_ALLOWED_ORIGINS` | *(empty)* | Comma-separated browser Origin allowlist. |
| `VESC_MCP_HTTP_AUTH_TOKEN` | *(unset)* | Optional bearer token; clients must send `Authorization: Bearer …`. |

HTTP intentionally exposes only `ping` and knowledge search plus the catalog
and knowledge resources. Package-tree tools stay on stdio until an
authenticated per-client package-root policy replaces process-global
`VESC_PACKAGE_ROOTS` assumptions. Remote HTTP exposure requires an explicit
bind address, Host/Origin policy, and authentication boundary.

### Knowledge retrieval rollout

| Variable | Config key | Default | Description |
|----------|------------|---------|-------------|
| `VESC_RAG_MODE` | `[knowledge] mode` | `lexical` | `lexical`, `legacy`, `auto`, or `hybrid`; `auto` degrades to lexical when semantic capability is unavailable, while explicit `hybrid` reports a structured capability error. |
| `VESC_RAG_ARTIFACT` | `[knowledge] artifact_path` | *(none)* | Optional generated artifact path; it is never downloaded or inferred from a private home path. |
| `VESC_RAG_SEMANTIC_MODEL_DIR` | `[knowledge.semantic] model_dir` | *(none)* | Explicit local FastEmbed model directory; no download is attempted. |
| `VESC_RAG_SEMANTIC_MODEL_ID` | `[knowledge.semantic] model_id` | *(none)* | Must match the vector artifact manifest. |
| `VESC_RAG_SEMANTIC_MODEL_REVISION` | `[knowledge.semantic] model_revision` | *(none)* | Must match the vector artifact manifest. |

The knowledge tool bounds queries to 4 KiB, results to 50, each passage to 8 KiB,
and the serialized response to 64 KiB. The default is offline `lexical`; use
explicit `legacy` for compatibility or `auto`/`hybrid` only with a provisioned
semantic capability.
When a request omits `mode`, the resolved `[knowledge]`/environment mode is
used; an explicit request mode takes precedence. Search passages are untrusted
evidence and are available for bounded follow-up reads at
`vesc://knowledge/chunk/{id}`. Use the additive
`vesc://knowledge/document/{id}` URI when a complete normalized source document
is needed for citation context. The response `index` object reports bounded
corpus/artifact diagnostics, including source and optional-rejection counts and
component versions; it never includes the expanded config path.

The lexical artifact cache is keyed by the active immutable generation path.
After `build`, the new corpus digest selects a new generation and the next
request loads that generation automatically.

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

[knowledge]
mode = "lexical"
max_limit = 50
max_query_bytes = 4096
max_response_bytes = 65536
max_passage_bytes = 8192
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

### NixOS deployment

The flake exports `packages.default`, `overlays.default`, and
`nixosModules.default`. Enable the service declaratively:

```nix
services.vesc-mcp = {
  enable = true;
  # bind = "127.0.0.1";
  # port = 8080;
  allowedHosts = [ "localhost" "127.0.0.1" ];
  packageRoots = [ "/srv/vesc-packages" ];
  retrievalMode = "lexical";
};
```

Set `authTokenFile` to a root-readable EnvironmentFile containing
`VESC_MCP_HTTP_AUTH_TOKEN=...` before enabling remote access. The module uses a
dynamic user, explicit state/cache directories, restart-on-failure, and systemd
filesystem/network hardening.

## Test / CI defaults

When `VESC_PACKAGE_ROOTS` is unset and the `test-fixtures` feature is enabled (Makefile / CI), the server automatically allows `tests/fixtures/` as the sandbox root. Production MCP sessions should always set explicit roots.

Optional live-repo tests (`cargo nextest run --run-ignored all`) require sibling checkouts via the env vars above — see [testing.md](testing.md).
