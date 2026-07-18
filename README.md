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
| stdio | One local assistant that needs package files | All tools |
| Streamable HTTP | Multiple clients sharing knowledge search | `ping`, `search_vesc_knowledge`, and resources |

Package inspection, validation, checks, and builds are stdio-only because they
access local files. Streamable HTTP intentionally does not expose them.

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

## What it provides

| Tool | Purpose |
|------|---------|
| `ping` | Verify the server connection |
| `search_vesc_knowledge` | Search VESC firmware and package documentation |
| `list_vesc_packages` | Find packages under allowed directories |
| `inspect_pkgdesc` | Read a package descriptor |
| `inspect_vescpkg` | Inspect a built `.vescpkg` file |
| `validate_package_layout` | Find missing package assets |
| `build_vescpkg` | Build with the configured VESC Tool executable |
| `run_package_checks` | Run the package's local checks |

Search results are evidence, not instructions. Each result includes resource
URIs for reading the matching passage or complete normalized document.

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
- [Proposed knowledge feedback and correction design](docs/knowledge-feedback-design.md)

## Nix

An optional packaged and development setup is documented separately in
[docs/nix.md](docs/nix.md).

vesc-mcp uses the official
[Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk).
