# Architecture reference

vesc-mcp is an MCP server for VESC firmware and vescpkg domain knowledge. The
binary serves one stdio client by default or a shared Streamable HTTP endpoint
with `--http`. The host process never loads device FFI; package builds and
inspection remain local, sandboxed stdio operations.

## Crate graph

```mermaid
flowchart TB
  subgraph clients [MCP Clients]
    LOCAL[Local stdio client]
    REMOTE[HTTP clients]
  end

  subgraph server_bin [Binary]
    SRV[vesc-mcp-server]
    HTTP[HTTP session and policy]
  end

  subgraph core [vesc-mcp-core]
    TOOLS[MCP tools]
    RES[MCP resources]
    CFG[config + sandbox]
    CAT[catalog loader]
  end

  subgraph libs [Libraries]
    DOM[vesc-domain]
    ADP[vesc-mcp-adapters]
    IDX[vesc-knowledge-index]
    CORPUS[versioned corpus + artifacts]
    LEX[fielded lexical retrieval]
    SEM[optional local vectors]
  end

  subgraph external [External]
    VTOOL[vesc_tool CLI]
    UPSTREAM[Configured upstream checkouts]
  end

  subgraph data [In-repo data]
    CATALOG[catalog/ YAML]
    FIX[tests/fixtures/]
  end

  LOCAL -->|stdio JSON-RPC: all tools| SRV
  REMOTE -->|Streamable HTTP: ping, search, resources| HTTP
  HTTP --> SRV
  SRV --> core
  TOOLS --> DOM
  TOOLS --> ADP
  TOOLS --> IDX
  IDX --> CORPUS
  CORPUS --> LEX
  CORPUS -. optional .-> SEM
  TOOLS --> CFG
  RES --> CAT
  RES --> DOM
  CAT --> CATALOG
  ADP --> DOM
  TOOLS -->|build_vescpkg| VTOOL
  CORPUS --> UPSTREAM
  CFG --> FIX
  DOM --> FIX
```

## Layer responsibilities

| Layer | Crate / path | Responsibility |
|-------|----------------|----------------|
| Transport | `vesc-mcp-server` | Default stdio session; optional shared Streamable HTTP sessions, Host/Origin policy, and bearer authentication |
| MCP surface | `vesc-mcp-core` | Tool router, resource registry, config, workspace discovery |
| Domain | `vesc-domain` | `pkgdesc.qml` parsing, `.vescpkg` wire read/parse, validation types |
| Build adapter | `vesc-mcp-adapters` | Locate `pkgdesc.qml` and inspect `.vescpkg` wire artifacts |
| Knowledge | `vesc-knowledge-index` | Versioned normalized corpus, deterministic chunking, fielded lexical retrieval, optional local vectors, fusion, and artifact lifecycle |
| Catalog | `catalog/` | Reviewed YAML indexes for build flows, commands, ABI, and doc topics |
| Upstream sources | `vendor/` or configured roots | Optional local reference checkouts used for validation, attribution, and knowledge builds |
| Fixtures | `tests/fixtures/` | Synthetic offline package trees for CI |

## Tool flow (example)

```mermaid
sequenceDiagram
  participant C as MCP Client
  participant S as vesc-mcp-server
  participant T as build_vescpkg
  participant D as vesc-domain
  participant A as vesc-mcp-adapters
  participant V as vesc_tool

  C->>S: tools/call build_vescpkg
  S->>T: root, timeout_secs
  T->>T: validate_sandbox_path
  T->>A: locate_pkgdesc
  T->>D: parse_pkgdesc_qml / validate_package_layout
  T->>V: spawn --buildPkgFromDesc
  T-->>C: JSON artifact path + metadata
```

## Resource flow

Static resources are registered at startup from `catalog/` and fixture metadata. Dynamic reads use URI templates:

- `vescpkg://manifest/{path}` — parse live pkgdesc under sandbox roots
- `vesc://catalog/commands/refloat/{command}` — render markdown from indexed command docs
- `vesc://knowledge/chunk/{id}` — read the bounded normalized passage returned by retrieval
- `vesc://knowledge/document/{id}` — read the complete normalized document assembled from its chunks

Both transports expose the resource registry, including subscriptions. Each
Streamable HTTP MCP session has an isolated current-repository selection, so
many chats can share one server without leaking repository context. HTTP
package-tree tools still require authentication and sandboxed roots.

## Retrieval flow

```mermaid
flowchart LR
  SRC[allowlisted Markdown/YAML/JSON sources] --> ING[normalize + provenance]
  ING --> CH[structure-aware chunks]
  CH --> COR[corpus manifest]
  COR --> LEX[fielded lexical index]
  COR -. optional .-> VEC[pinned local vector artifact]
  Q[bounded MCP query] --> LEX
  Q -. auto/hybrid when available .-> VEC
  LEX --> FUSE[deterministic fusion + diversity + budgets]
  VEC --> FUSE
  FUSE --> MCP[MCP evidence + stable resource URI]
```

`lexical` is the offline default after passing the locked evaluation gate.
`legacy` remains the explicit compatibility mode. The lexical path uses the
normalized in-memory Tantivy index. Hybrid fusion uses RRF with a lexical floor
and bounded adjacent context, so an uncalibrated semantic model cannot displace
trusted lexical evidence; `auto` reports an error when semantic capability is
unavailable. Artifact writes are staged and the active manifest
selector is replaced only after checksum validation. The selector in
`active.json` points to the full generation manifest and carries its checksum;
readers should use the lifecycle inspection API, which also accepts legacy
full-manifest `active.json` files.

The lexical MCP path caches the validated index by immutable generation path,
so a rebuilt generation naturally invalidates the cache. Search responses expose
bounded index diagnostics (corpus digest, counts, source count, component
versions, and diagnostic count) without raw queries or private filesystem paths.

Build-recipe and doc-topic bodies include repository-relative source
attribution. Search responses and resources do not expose private filesystem
paths.

## Boundaries and non-goals

| In scope | Out of scope |
|----------|--------------|
| Package discovery, inspect, validate, build | Rider-facing tuning docs |
| Catalog-backed docs and ABI summaries | Duplicating full POC or refloat internals |
| Sandboxed path access | Default-on flash/upload |
| `vesc_tool` subprocess builds | Loading `vesc-ffi` / BLE protocol in MCP host |
| Read-only wire parsing in `vesc-domain` | In-repo `.vescpkg` packers |
| Shared HTTP knowledge search/resources | Unauthenticated HTTP package access |

## Running the service

The release server runs as one local stdio process or as a shared Streamable
HTTP process. See [installation.md](installation.md) and [http.md](http.md) for
user setup.

## Testing architecture

| Tier | Location |
|------|----------|
| Unit | `#[cfg(test)]` in crate sources |
| Integration | `crates/*/tests/*.rs` |
| MCP harness | `McpTestHarness` in `vesc-mcp-core::test_support` |

See [testing.md](testing.md).
