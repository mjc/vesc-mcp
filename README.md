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

Verify the connection with the `ping` tool (optional `message` echoes back). Then use `list_vesc_packages` and `inspect_pkgdesc` on paths under your configured sandbox — offline examples live in [`tests/fixtures/`](tests/fixtures/README.md) and [docs/examples/](docs/examples/).

For multiple MCP clients, run the shared Streamable HTTP service:

```bash
nix develop -c vesc-mcp-server --http
```

It listens on `http://127.0.0.1:8080/mcp` by default and shares the knowledge
index/cache across clients. HTTP exposes health, knowledge search, and
resources; package-tree mutation/build tools remain on stdio until a
per-client authenticated root policy is implemented. Configure bind, Host,
Origin, and bearer authentication with the `VESC_MCP_HTTP_*` variables in
[docs/configuration.md](docs/configuration.md).

Optional CI smoke: `./scripts/docs-smoke.sh` (spawns server, checks `tools/list` count).

## Architecture overview

```
 MCP Client  →  vesc-mcp-server (stdio or Streamable HTTP)  →  vesc-mcp-core
                                              ├─ tools (ping, list, inspect, build, …)
                                              ├─ resources (vesc://catalog/*, vescpkg://…)
                                              ├─ vesc-domain (parse / validate / read vescpkg wire)
                                              ├─ vesc-mcp-adapters (inspect + pkgdesc discovery)
                                              ├─ vesc-knowledge-index (corpus, lexical search, optional vectors)
                                              └─ catalog/ YAML + tests/fixtures/
```

| Crate | Role |
|-------|------|
| `vesc-mcp-server` | MCP binary: stdio by default, Streamable HTTP with `--http` |
| `vesc-mcp-core` | tools, resources, config, MCP service |
| `vesc-domain` | VESC / vescpkg domain types and parsers |
| `vesc-mcp-adapters` | Host-side pkgdesc discovery and `.vescpkg` inspect |
| `vesc-knowledge-index` | Versioned corpus, chunking, lexical retrieval, optional vectors, and artifact lifecycle |

See [docs/architecture.md](docs/architecture.md) for a detailed diagram and data-flow notes.

## Documentation

| Doc | Contents |
|-----|----------|
| [AGENTS.md](AGENTS.md) | MCP tool/resource cheat sheet, TDD workflow |
| [docs/configuration.md](docs/configuration.md) | Environment variables and `config.toml` |
| [docs/architecture.md](docs/architecture.md) | Crate diagram and boundaries |
| [docs/testing.md](docs/testing.md) | Red/green/refactor workflow |
| [docs/vescpackage-reference.md](docs/vescpackage-reference.md) | End-to-end package lifecycle index |
| [docs/vescpkg-wire-format.md](docs/vescpkg-wire-format.md) | `.vescpkg` byte-level wire spec |
| [docs/vesc-pkg-lib-abi.md](docs/vesc-pkg-lib-abi.md) | Native loader ABI (vesc_pkg_lib) |
| [docs/safety.md](docs/safety.md) | Flash/upload gates and device hygiene |
| [docs/rag-threat-model.md](docs/rag-threat-model.md) | Knowledge ingestion and retrieval threat model |
| [docs/troubleshooting.md](docs/troubleshooting.md) | Artifact, mode, and offline fallback diagnosis |
| [docs/examples/search-knowledge-session.md](docs/examples/search-knowledge-session.md) | Copy-paste retrieval and citation sessions |
| [docs/examples/inspect-refloat-session.md](docs/examples/inspect-refloat-session.md) | Agent walkthrough: refloat-minimal fixture |
| [docs/examples/build-native-lib-package-session.md](docs/examples/build-native-lib-package-session.md) | Agent walkthrough: fixture build via `vesc_tool` |

## External repos (optional)

Catalog path validation and knowledge indexing resolve upstream sources in this order: **environment override → initialized `vendor/` submodule → sibling checkout default**. Full reference: [docs/configuration.md](docs/configuration.md).

The Nix release package includes the generated lexical corpus from the pinned
VESC firmware, VESC Tool, and Refloat revisions and selects it by default.
Submodules are not used at runtime or while installing the release package.

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

### Build and inspect knowledge artifacts

The default server remains offline and lexical; explicit legacy mode remains
available for compatibility. To build the
reviewed in-repo corpus, versioned lexical artifacts, and inspect the active
manifest:

```bash
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- build --source-root "$PWD"
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- inspect
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- evaluate --mode lexical --artifact target/knowledge-artifacts
nix develop -c cargo run -p vesc-knowledge-index --bin gen-knowledge-index -- benchmark --artifact target/knowledge-artifacts
nix develop -c cargo run -p vesc-mcp-server -- --benchmark-search --artifact target/knowledge-artifacts
```

Semantic retrieval is optional (`semantic-fastembed`); it requires an
operator-provisioned, pinned local model directory and never downloads one at
server startup. Build it with `--semantic-model-dir`, `--semantic-model-id`,
and `--semantic-model-revision`. The active manifest records source digests,
vendor repository revisions, chunking settings, component versions, and
optional-source diagnostics.

For the supported FastEmbed baseline, provision the model explicitly with the
online-only feature, then use the printed revision and model manifest when
building vectors:

```bash
nix develop -c cargo run -p vesc-knowledge-index --features semantic-fastembed-online --bin gen-knowledge-index -- \
  provision-model --out target/models/bge-small-en-v1.5
```

The provisioner downloads only when this command is run, validates one local
embedding, copies the five required files, and writes SHA-256 hashes to
`target/models/bge-small-en-v1.5/manifest.json`. Evaluate the real hybrid path
by passing that model directory, model ID, model revision, and the semantic
artifact root to `evaluate --mode hybrid`.

## MCP SDK

Uses [rmcp](https://github.com/modelcontextprotocol/rust-sdk) (official Rust MCP SDK) for stdio and Streamable HTTP transports.
