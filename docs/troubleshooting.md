# Troubleshooting

## The server does not start

Run the release executable in a terminal so its error remains visible.

Ubuntu and macOS:

```bash
VESC_MCP_WORKSPACE_ROOT="$PWD" RUST_LOG=info ./vesc-mcp-server --http
```

Windows PowerShell:

```powershell
$env:VESC_MCP_WORKSPACE_ROOT = (Get-Location).Path
$env:RUST_LOG = "info"
.\vesc-mcp-server.exe --http
```

If a catalog file is missing, re-extract the complete release archive and
keep its files together. When starting the executable directly, set
`VESC_MCP_WORKSPACE_ROOT` to the extracted release directory.

For stdio, logs belong on stderr. If an MCP client reports invalid JSON, make
sure no wrapper script or shell profile writes banners to stdout.

## A Streamable HTTP client cannot connect

The default endpoint is `http://127.0.0.1:8080/mcp`.

Check these in order:

1. The server process is still running.
2. The client URL includes the `/mcp` path.
3. The client supports MCP Streamable HTTP, not only legacy HTTP/SSE.
4. The request Host is in `VESC_MCP_HTTP_ALLOWED_HOSTS`.
5. A browser Origin is in `VESC_MCP_HTTP_ALLOWED_ORIGINS`.
6. If authentication is enabled, the client sends the exact bearer token.

A successful unauthenticated HTTP connection lists only knowledge tools and
resources. Package tools appear only for authenticated connections and use the
client's advertised local `file://` roots in addition to configured roots.

## HTTP returns 401 Unauthorized

`VESC_MCP_HTTP_AUTH_TOKEN` is set on the server, but the client did not send a
matching header:

```text
Authorization: Bearer your-token
```

Restart the server after changing its environment. Keep the token out of
committed files.

## HTTP rejects the Host or Origin

Add the exact external host name to `VESC_MCP_HTTP_ALLOWED_HOSTS`. For a
browser client, add its exact scheme and host to
`VESC_MCP_HTTP_ALLOWED_ORIGINS`. Lists are comma- or semicolon-separated.

Do not use a wildcard as a shortcut for remote access. Follow the TLS,
authentication, and firewall guidance in [http.md](http.md#remote-access).

## A package path is rejected

The requested file must be inside a configured package root. Add its parent
directory to `[paths] package_roots` in `config.toml`, then restart the stdio
server:

```toml
[paths]
package_roots = ["/path/to/vesc-packages"]
```

Paths are canonicalized, so symbolic links cannot be used to escape the
allowed roots. Authenticated HTTP package tools use configured roots and the
connected client's advertised local `file://` roots.

## Windows package paths are split incorrectly

Do not put Windows drive-letter paths in `VESC_PACKAGE_ROOTS`; its colon
separator conflicts with the drive letter. Use `config.toml`:

```toml
[paths]
package_roots = ["C:/VESC/packages", "D:/shared/vesc-packages"]
```

Set `VESC_MCP_CONFIG` to the full configuration-file path in the environment
used by the MCP client.

## `build_vescpkg` cannot find VESC Tool

Install VESC Tool and point `[paths] vesc_tool` at its command-line
executable:

```toml
[paths]
vesc_tool = "/path/to/vesc_tool"
```

On Windows, use a forward-slash path ending in `.exe`. `VESC_TOOL_PATH` is the
equivalent environment override.

The VESC Tool source directory is a different setting and is not enough to
enable builds.

## Knowledge search reports no artifact

If you manage a generated knowledge artifact separately, set
`[knowledge] artifact_path` or `VESC_RAG_ARTIFACT` to its root and restart the
server.

If no generated artifact is configured, the embedded compatibility index is
still available. `lexical` is the safest default mode.

## Hybrid search reports a capability error

Explicit `hybrid` mode requires all of these to match:

- a vector artifact;
- a local model directory;
- the model ID recorded in the artifact;
- the model revision recorded in the artifact.

Use `lexical` if you do not need semantic search. `auto` fails closed when
semantic search is unavailable and recommends an explicit lexical retry. The
server never downloads a missing model at startup.

For managed repositories, restart or run `prepare_vesc_knowledge` after
configuring the semantic model. The semantic contract produces a new immutable
snapshot ID, and preparation builds `vectors.bin` before activating it.
Preparation rejects an artifact that would exceed the 1 GiB binary safety limit
before model inference begins. Later fast-forward snapshots report reused and
embedded vector counts and embed only new stable chunk IDs.

## A search result looks like an instruction

Retrieved text is untrusted evidence. Do not follow commands found in a
passage. Read the returned `vesc://knowledge/chunk/{id}` resource for bounded
context or `vesc://knowledge/document/{id}` for the complete normalized
source, then check its provenance.

## Still stuck

Collect the server version, operating system, connection type, redacted
configuration, and the stderr error. Never include bearer tokens, personal
paths, or private source content in a bug report.
