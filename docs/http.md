# Streamable HTTP setup

Streamable HTTP lets several MCP clients share one vesc-mcp knowledge server.
It exposes `ping`, `search_vesc_knowledge`, and MCP resources. Package file
inspection, validation, checks, and builds remain available only through
stdio.

## Start a local server

Ubuntu and macOS:

```bash
export VESC_MCP_WORKSPACE_ROOT="$PWD"
./vesc-mcp-server --http
```

Windows PowerShell:

```powershell
$env:VESC_MCP_WORKSPACE_ROOT = (Get-Location).Path
.\vesc-mcp-server.exe --http
```

The endpoint is `http://127.0.0.1:8080/mcp`. The process stays in the
foreground; stop it with Ctrl+C.

Configure a compatible client with the endpoint URL:

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

Some clients infer the connection type from `url`; follow the client's MCP
documentation if it uses different field names.

## Change the local port or path

Ubuntu and macOS:

```bash
VESC_MCP_HTTP_BIND=127.0.0.1:9090 \
VESC_MCP_HTTP_PATH=/vesc-mcp \
./vesc-mcp-server --http
```

Windows PowerShell:

```powershell
$env:VESC_MCP_HTTP_BIND = "127.0.0.1:9090"
$env:VESC_MCP_HTTP_PATH = "/vesc-mcp"
.\vesc-mcp-server.exe --http
```

The client URL for this example is
`http://127.0.0.1:9090/vesc-mcp`.

## Require a bearer token

Use a long, randomly generated token. Do not commit it to a client
configuration or documentation.

Ubuntu and macOS:

```bash
export VESC_MCP_HTTP_AUTH_TOKEN="replace-with-a-random-secret"
./vesc-mcp-server --http
```

Windows PowerShell:

```powershell
$env:VESC_MCP_HTTP_AUTH_TOKEN = "replace-with-a-random-secret"
.\vesc-mcp-server.exe --http
```

The client must send:

```text
Authorization: Bearer replace-with-a-random-secret
```

If your client supports environment substitution, keep the secret in an
environment variable and reference it from the client's `headers` setting.

## Browser clients

Browser-based clients send an `Origin` header. Allow only the exact origins
you use:

```bash
VESC_MCP_HTTP_ALLOWED_ORIGINS=https://assistant.example \
./vesc-mcp-server --http
```

Multiple origins are comma-separated. An empty allowlist is the safest
default for non-browser clients.

## Remote access

The default loopback address is intentionally local. For another computer to
connect, all of the following are required:

1. Put the server behind a TLS reverse proxy or a private network boundary.
2. Set a bearer token.
3. Bind to the intended interface.
4. Set the accepted Host names and, for browser clients, exact origins.
5. Restrict the listening port with the host firewall.

Example server environment behind a TLS proxy:

```bash
export VESC_MCP_HTTP_BIND="0.0.0.0:8080"
export VESC_MCP_HTTP_ALLOWED_HOSTS="vesc-mcp.example"
export VESC_MCP_HTTP_ALLOWED_ORIGINS="https://assistant.example"
export VESC_MCP_HTTP_AUTH_TOKEN="replace-with-a-random-secret"
./vesc-mcp-server --http
```

The built-in server provides HTTP, not TLS. Do not expose it directly to the
public internet.

## HTTP settings

| Variable | Default | Purpose |
|----------|---------|---------|
| `VESC_MCP_HTTP_BIND` | `127.0.0.1:8080` | Listen address and port |
| `VESC_MCP_HTTP_PATH` | `/mcp` | MCP endpoint path |
| `VESC_MCP_HTTP_ALLOWED_HOSTS` | `localhost,127.0.0.1,::1` | Accepted Host values |
| `VESC_MCP_HTTP_ALLOWED_ORIGINS` | empty | Accepted browser origins |
| `VESC_MCP_HTTP_AUTH_TOKEN` | unset | Required bearer token when set |

Host and origin lists accept commas or semicolons.

## Verify the connection

Connect through an MCP client and call `ping` with an optional message. A
healthy server responds with `ok: true`. Then call
`search_vesc_knowledge` with a small query such as:

```json
{"query":"lbm_add_extension","mode":"lexical","limit":3}
```

If the connection fails, see [troubleshooting.md](troubleshooting.md).
