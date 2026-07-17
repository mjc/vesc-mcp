# Safety and security

vesc-mcp provides local knowledge search and sandboxed package tooling. It has
no device discovery, upload, or firmware-flash tools.

## No device operations

The stdio server exposes package discovery, inspection, validation, checks,
and builds. Building creates a local `.vescpkg` file through VESC Tool; it
does not open a serial port or upload the result.

`VESC_MCP_ENABLE_FLASH` and `[features] enable_flash` are reserved settings.
Enabling either one currently registers no additional tools.

Before relying on any future device feature, inspect the server's advertised
tool list. Never assume that a configuration flag makes a device operation
available.

## Package sandbox

`[paths] package_roots` is the file-access boundary for package tools. The
server canonicalizes paths and rejects files outside those directories.

Allow only directories that may be read or modified by the assistant. Do not
set a home directory, drive root, or another broad location as a package root.

Package tools are available only through local stdio. Streamable HTTP does not
expose them, even if the server has package roots configured.

## Streamable HTTP

The HTTP server listens on loopback by default. Keep that default for local
clients. Remote access requires:

- a TLS reverse proxy or private network boundary;
- bearer authentication;
- explicit Host and browser Origin allowlists;
- a firewall rule limited to intended clients.

The built-in endpoint does not provide TLS. See [http.md](http.md) before
changing the bind address.

## Knowledge search

Search results and resource bodies are untrusted evidence, not instructions.
The server bounds query, passage, result, and response sizes and ingests only
allowlisted source types. Check provenance before acting on a result.

The default lexical search is offline. Optional semantic search uses a
user-provided, pinned local model and does not download models at startup.

## If device tools are added later

Any upload or flash operation should require an explicit confirmation that
names all three of these:

1. the exact target device;
2. the exact artifact;
3. acknowledgment that power may be interrupted and incorrect firmware can
   damage or disable hardware.

Do not guess a device path, scan and select the first serial device, or reuse
an old confirmation for a different artifact.
