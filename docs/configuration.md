# Configuration

vesc-mcp reads its configuration when the process starts. Environment
variables override the configuration file, and the configuration file
overrides built-in defaults. Restart the server after a change.

Most users need only a package root for stdio, or no configuration at all for
local Streamable HTTP knowledge search.

## Configuration file

The default file on Ubuntu and macOS is:

```text
$HOME/.config/vesc-mcp/config.toml
```

On Windows, choose a location and set `VESC_MCP_CONFIG` to its full path in
the environment used to start the server. This avoids relying on a Unix-style
home-directory convention.

A practical starting file is:

```toml
[paths]
package_roots = ["/path/to/vesc-packages"]
vesc_tool = "/path/to/vesc_tool"

[features]
enable_flash = false

[knowledge]
mode = "lexical"
```

Windows paths may use forward slashes:

```toml
[paths]
package_roots = ["C:/VESC/packages"]
vesc_tool = "C:/VESC/Tool/vesc_tool.exe"
```

The repository also includes [`config.example.toml`](../config.example.toml).

## Package access

`package_roots` lists the directories the stdio server may scan, inspect,
check, and build. Paths outside these directories are rejected.

| Setting | Environment variable | Default |
|---------|----------------------|---------|
| `[paths] package_roots` | `VESC_PACKAGE_ROOTS` | no allowed package directories |

On Ubuntu and macOS, multiple environment roots may be separated by commas or
colons:

```bash
export VESC_PACKAGE_ROOTS="/path/to/packages,/another/package-root"
```

On Windows, use `package_roots` in `config.toml`; a drive-letter colon is
ambiguous in `VESC_PACKAGE_ROOTS`.

HTTP clients cannot use package tools even when package roots are configured.

## Building packages

`build_vescpkg` calls the official VESC Tool command-line executable.

| Setting | Environment variable | Default |
|---------|----------------------|---------|
| `[paths] vesc_tool` | `VESC_TOOL_PATH` | `vesc_tool` from `PATH` |

Set this only if the executable is not already on `PATH`. This setting does
not grant device access; vesc-mcp uses it only to build a local package.

## Release support files

Release archives contain a `catalog` directory alongside the server. Keep the
archive contents together. If you start the executable directly, set
`VESC_MCP_WORKSPACE_ROOT` to the extracted release directory.

| Variable | Purpose |
|----------|---------|
| `VESC_MCP_WORKSPACE_ROOT` | Directory containing the bundled `catalog` |

Source checkouts are discovered automatically when the server is run from the
project.

## Streamable HTTP

Run `vesc-mcp-server --http` to start a shared endpoint. The default is local
only at `http://127.0.0.1:8080/mcp`.

| Variable | Default | Purpose |
|----------|---------|---------|
| `VESC_MCP_HTTP_BIND` | `127.0.0.1:8080` | Listen address and port |
| `VESC_MCP_HTTP_PATH` | `/mcp` | Endpoint path |
| `VESC_MCP_HTTP_ALLOWED_HOSTS` | `localhost,127.0.0.1,::1` | Accepted Host values |
| `VESC_MCP_HTTP_ALLOWED_ORIGINS` | empty | Accepted browser origins |
| `VESC_MCP_HTTP_AUTH_TOKEN` | unset | Required bearer token when set |

See [http.md](http.md) for complete local and remote examples. Remote access
requires a TLS boundary, authentication, explicit host/origin policy, and a
firewall rule.

## Knowledge search

The default `lexical` mode is local and does not download a model.

| Setting | Environment variable | Default | Purpose |
|---------|----------------------|---------|---------|
| `[knowledge] mode` | `VESC_RAG_MODE` | `lexical` | Retrieval mode |
| `[knowledge] artifact_path` | `VESC_RAG_ARTIFACT` | bundled or embedded corpus | Generated artifact directory |
| `[knowledge.semantic] model_dir` | `VESC_RAG_SEMANTIC_MODEL_DIR` | unset | Pinned local model directory |
| `[knowledge.semantic] model_id` | `VESC_RAG_SEMANTIC_MODEL_ID` | unset | Model identity recorded by the artifact |
| `[knowledge.semantic] model_revision` | `VESC_RAG_SEMANTIC_MODEL_REVISION` | unset | Pinned model revision |
| `[knowledge.semantic] idle_timeout_secs` | `VESC_RAG_SEMANTIC_IDLE_TIMEOUT_SECS` | `300` | Seconds before unloading an idle model |

Supported modes:

| Mode | Behavior |
|------|----------|
| `lexical` | Offline keyword and identifier search; recommended default |
| `legacy` | Compatibility search for older results |
| `auto` | Uses hybrid search when configured; otherwise returns lexical results with a warning |
| `hybrid` | Requires a compatible local vector artifact and model; reports an error if unavailable |

The server never downloads a semantic model at startup. Model directory,
identity, and revision must match the vector artifact manifest.

Search is bounded to a 4 KiB query, 50 results, 8 KiB per passage, and a 64
KiB serialized response by default. These file-only limits can be adjusted:

```toml
[knowledge]
max_limit = 50
max_query_bytes = 4096
max_response_bytes = 65536
max_passage_bytes = 8192
```

## Optional source checkouts

Most users do not need upstream source repositories. They are used only for
catalog validation, knowledge artifact generation, and detailed source
attribution.

| Config key | Environment variable | Purpose |
|------------|----------------------|---------|
| `[paths] refloat_root` | `VESC_REFLOAT_ROOT` | Refloat source checkout |
| `[paths] bldc_root` | `VESC_BLDC_ROOT` | VESC firmware source checkout |
| `[paths] poc_root` | `VESC_POC_ROOT` | Rust proof-of-concept checkout |
| `[paths] vesc_tool_root` | `VESC_VESC_TOOL_ROOT` | VESC Tool source checkout |

Use paths in `config.toml` or environment variables. Do not put personal
absolute paths in shared client configurations or documentation.

## Logging

Set `RUST_LOG=info` for normal diagnostics or a narrower filter such as
`vesc_mcp_core=debug` for detailed troubleshooting. Logs go to stderr so they
do not corrupt stdio MCP messages.

## Reserved flash setting

`VESC_MCP_ENABLE_FLASH` and `[features] enable_flash` are reserved for a
possible future feature. They default to false, and setting them currently
adds no upload or flash tools. See [safety.md](safety.md).
