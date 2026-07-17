# Installation

Release archives are the supported installation method for people using
vesc-mcp. Building with Cargo is covered separately in
[testing.md](testing.md) for contributors.

## Before you download

Choose the archive matching both your operating system and CPU from the
[Releases page](https://github.com/mjc/vesc-mcp/releases/latest). Verify its
published checksum, then extract the complete archive. The archive contains
the server executable and the data it needs at runtime; keep those files
together.

| System | Archive suffix |
|--------|----------------|
| Ubuntu on Intel or AMD | `ubuntu-x86_64.tar.gz` |
| Ubuntu on 64-bit Arm | `ubuntu-aarch64.tar.gz` |
| macOS on Intel | `macos-x86_64.tar.gz` |
| macOS on Apple Silicon | `macos-aarch64.tar.gz` |
| Windows on Intel or AMD | `windows-x86_64.zip` |

VESC Tool is optional. You only need its command-line executable if you want
the `build_vescpkg` tool. Knowledge search and package inspection work without
it.

## Ubuntu

1. Extract the downloaded archive.
2. Open a terminal in the extracted directory.
3. Make the server executable and verify it starts:

   ```bash
   chmod +x ./vesc-mcp-server
   VESC_MCP_WORKSPACE_ROOT="$PWD" ./vesc-mcp-server --http
   ```

4. Connect your MCP client to `http://127.0.0.1:8080/mcp`, or stop the server
   and configure the same executable as a local stdio server.

You may move the extracted directory to a stable location. If your client
starts the executable, set `VESC_MCP_WORKSPACE_ROOT` to that directory so the
bundled catalog can be found.

## macOS

1. Extract the archive for Apple Silicon or Intel, as appropriate.
2. Open Terminal in the extracted directory.
3. Make the server executable and verify it starts:

   ```bash
   chmod +x ./vesc-mcp-server
   VESC_MCP_WORKSPACE_ROOT="$PWD" ./vesc-mcp-server --http
   ```

4. Connect your MCP client to `http://127.0.0.1:8080/mcp`, or configure the
   executable as a local stdio server.

Keep the whole extracted directory together. If the operating system blocks
the executable, use Finder's standard Open confirmation only after verifying
the release checksum.

## Windows

1. Download the Windows archive and choose **Extract All**. Do not run the
   executable from inside the ZIP file.
2. Open PowerShell in the extracted directory.
3. Start the server:

   ```powershell
   $env:VESC_MCP_WORKSPACE_ROOT = (Get-Location).Path
   .\vesc-mcp-server.exe --http
   ```

4. Connect your MCP client to `http://127.0.0.1:8080/mcp`, or configure
   `vesc-mcp-server.exe` as a local stdio server.

For stdio package tools, use a configuration file rather than
`VESC_PACKAGE_ROOTS`. This avoids ambiguity between Windows drive-letter
colons and the environment variable's path separator:

```powershell
$configDir = Join-Path $env:APPDATA "vesc-mcp"
New-Item -ItemType Directory -Force $configDir | Out-Null
$env:VESC_MCP_CONFIG = Join-Path $configDir "config.toml"
```

Create that `config.toml` using the example in
[configuration.md](configuration.md#configuration-file). Set
`VESC_MCP_CONFIG` in the environment used by your MCP client as well.

## Connect over Streamable HTTP

The default server listens only on the current computer. Use this MCP client
shape when your client supports Streamable HTTP:

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

See [http.md](http.md) before changing the listen address or allowing remote
clients.

## Connect over stdio

Use stdio when the assistant needs package files or builds. Configure the
absolute path to the release executable and allow only the directories the
assistant should access. In the MCP client's environment, set
`VESC_MCP_WORKSPACE_ROOT` to the extracted release directory so the executable
can find its bundled catalog.

```toml
[paths]
package_roots = ["/path/to/vesc-packages"]
vesc_tool = "/path/to/vesc_tool"
```

The package roots are a security boundary. The server rejects package paths
outside them. Omit `vesc_tool` if you do not need builds.

## Update or remove

To update, stop the server, download the newer release, verify its checksum,
and replace the extracted directory. Keep your configuration file outside the
release directory.

To remove vesc-mcp, stop it and delete the extracted release directory. Delete
your configuration and optional knowledge/model directories separately only
if you no longer need them.
