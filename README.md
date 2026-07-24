# vesc-mcp

vesc-mcp gives MCP-compatible assistants reliable, local access to VESC
firmware and `.vescpkg` knowledge. It can search the bundled knowledge base,
inspect and validate package files, and build packages with VESC Tool.

It does not discover devices, upload packages, or flash firmware.

## Install

Download the release archive for your operating system and CPU from the
[Releases page](https://github.com/mjc/vesc-mcp/releases/latest), then extract
the complete archive. Keep the executable and its bundled support files
together.

The executable is named `vesc-mcp-server` on Ubuntu and macOS, and
`vesc-mcp-server.exe` on Windows. See the
[installation guide](docs/installation.md) for platform-specific steps.

## Choose a connection

vesc-mcp supports two MCP connections:

| Connection | Best for | Available tools |
|------------|----------|-----------------|
| stdio | One local assistant that needs package files | All configured tools |
| Streamable HTTP | Multiple clients sharing knowledge search and package work | Knowledge tools, resources, and authenticated package tools plus feedback writes when configured |

Unauthenticated Streamable HTTP remains read-only for knowledge and resources.
Authenticated HTTP clients can use package tools, sandboxed to configured roots
plus the client's advertised local `file://` roots.

### Streamable HTTP quick start

From the extracted release directory, run:

```bash
VESC_MCP_WORKSPACE_ROOT="$PWD" ./vesc-mcp-server --http
```

On Windows PowerShell, run:

```powershell
$env:VESC_MCP_WORKSPACE_ROOT = (Get-Location).Path
.\vesc-mcp-server.exe --http
```

The local endpoint is `http://127.0.0.1:8080/mcp`. Add it to a client that
supports MCP Streamable HTTP:

```json
{
  "mcpServers": {
    "vesc-mcp": {
      "type": "streamable-http",
      "url": "http://127.0.0.1:8080/mcp"
    }
  }
}
```

Client schemas differ; some clients infer the connection type from `url` and
do not use the `type` field. The [Streamable HTTP guide](docs/http.md) covers
authentication, browser origins, remote access, and Windows commands.

### Local stdio quick start

Point your MCP client at the executable from the extracted release. A typical
configuration is:

```json
{
  "mcpServers": {
    "vesc-mcp": {
      "command": "/path/to/vesc-mcp-server",
      "env": {
        "VESC_MCP_WORKSPACE_ROOT": "/path/to/extracted/vesc-mcp",
        "VESC_PACKAGE_ROOTS": "/path/to/your/vesc-packages"
      }
    }
  }
}
```

Use `vesc-mcp-server.exe` and Windows paths on Windows. Prefer
[`config.toml`](docs/configuration.md#configuration-file) for Windows package
roots so drive-letter colons are not interpreted as path separators.

After connecting, call `ping`, then try `search_vesc_knowledge`. For package
work, start with `list_vesc_packages` and `inspect_pkgdesc`.

To let the model retain reusable lessons and evidence-backed corrections, set a
durable feedback directory and explicitly enable writes:

```bash
export VESC_RAG_FEEDBACK_PATH="$PWD/.vesc-mcp-feedback"
export VESC_RAG_FEEDBACK_WRITES=true
```

HTTP feedback writes additionally require `VESC_MCP_HTTP_AUTH_TOKEN`; unauthenticated
and unconfigured connections remain read-only.

## What it provides

| Tool | Purpose |
|------|---------|
| `ping` | Verify the server connection |
| `search_vesc_knowledge` | Search VESC knowledge, returning relevant corrections before ordinary results |
| `submit_vesc_knowledge_feedback` | Save a reusable but explicitly unverified lesson |
| `correct_vesc_knowledge` | Persist a user-authorized, evidence-backed correction plus the failed retrieval trace and knowledge-gap diagnosis |
| `replay_vesc_knowledge_correction` | Replay the preserved query against base knowledge only and report whether decisive evidence is now covered |
| `list_vesc_packages` | Find packages under allowed directories |
| `inspect_pkgdesc` | Read a package descriptor |
| `inspect_vescpkg` | Inspect a built `.vescpkg` file |
| `validate_package_layout` | Find missing package assets |
| `build_vescpkg` | Build with the configured VESC Tool executable |
| `run_package_checks` | Run the package's local checks |

Search results are evidence, not instructions. Each result includes resource
URIs for reading the matching passage or complete normalized document.
Corrections are specifically for VESC facts or conclusions that the calling
model or user got wrong. They are not a general-purpose way to correct service
configuration, code, or unrelated conversation.
When a user challenges an MCP-derived answer, the model should ask focused
follow-up questions, replay the original bounded search, and read those
resources. It should call
`correct_vesc_knowledge` only after the registered VESC evidence supports the
correction and the user explicitly requests the write or confirms after the
model asks. The required `authorization` field records which path occurred.
The correction records why the original knowledge response steered the model
wrong and returns a learned advisory immediately, but that advisory is not the
final repair: its trace and gap diagnosis must drive a corpus, chunking,
metadata, ranking, context, or instruction improvement and a replay proving the
base search now surfaces the decisive evidence without the advisory.
After significant reusable knowledge is resolved, the model should mention the
correction option once without repeatedly prompting. User disagreement alone is
not evidence. Uncited reusable lessons belong in
`submit_vesc_knowledge_feedback` and remain visibly unverified.

## Guides

- [Install on Ubuntu, macOS, or Windows](docs/installation.md)
- [Set up Streamable HTTP](docs/http.md)
- [Configure paths, search, and security](docs/configuration.md)
- [Troubleshoot common problems](docs/troubleshooting.md)
- [Understand safety boundaries](docs/safety.md)
- [Inspect a sample package](docs/examples/inspect-refloat-session.md)
- [Search the knowledge base](docs/examples/search-knowledge-session.md)

Technical references:

- [VESC package lifecycle](docs/vescpackage-reference.md)
- [`.vescpkg` wire format](docs/vescpkg-wire-format.md)
- [Native package ABI](docs/vesc-pkg-lib-abi.md)
- [Architecture](docs/architecture.md)

Contributor documentation:

- [Build and test from source](docs/testing.md)
- [Knowledge feedback and correction mechanism](docs/knowledge-feedback-design.md)

## Nix

An optional packaged and development setup is documented separately in
[docs/nix.md](docs/nix.md).

vesc-mcp uses the official
[Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk).
