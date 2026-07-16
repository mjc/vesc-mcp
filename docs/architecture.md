# Architecture

vesc-mcp is a **stdio MCP server** that wraps VESC firmware and vescpkg domain logic for AI assistants. The host process never loads device FFI; builds and inspections run on the developer machine under configurable path sandboxes.

## Crate graph

```mermaid
flowchart TB
  subgraph client [MCP Client]
    IDE[Cursor / Claude / other]
  end

  subgraph server_bin [Binary]
    SRV[vesc-mcp-server]
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
  end

  subgraph data [In-repo data]
    CATALOG[catalog/ YAML]
    FIX[tests/fixtures/]
  end

  IDE -->|stdio JSON-RPC| SRV
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
  CFG --> FIX
  DOM --> FIX
```

## Layer responsibilities

| Layer | Crate / path | Responsibility |
|-------|----------------|----------------|
| Transport | `vesc-mcp-server` | stdio MCP session, tracing to stderr |
| MCP surface | `vesc-mcp-core` | Tool router, resource registry, config, workspace discovery |
| Domain | `vesc-domain` | `pkgdesc.qml` parsing, `.vescpkg` wire read/parse, validation types |
| Build adapter | `vesc-mcp-adapters` | Locate `pkgdesc.qml` and inspect `.vescpkg` wire artifacts |
| Knowledge | `vesc-knowledge-index` | Versioned normalized corpus, deterministic chunking, fielded lexical retrieval, optional local vectors, fusion, and artifact lifecycle |
| Catalog | `catalog/` | YAML indexes (build flows, commands, ABI, doc topics) — no GPL source vendored |
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
trusted lexical evidence; `auto` degrades to lexical when no semantic
capability is active. Artifact writes are staged and the active manifest is
replaced only after checksum validation.

The lexical MCP path caches the validated index by immutable generation path,
so a rebuilt generation naturally invalidates the cache. Search responses expose
bounded index diagnostics (corpus digest, counts, source count, component
versions, and diagnostic count) without raw queries or private filesystem paths.

Build-recipe and doc-topic bodies include **source attribution** footers pointing at resolved repo paths (`VESC_*_ROOT`).

## Boundaries and non-goals

| In scope | Out of scope |
|----------|--------------|
| Package discovery, inspect, validate, build | Rider-facing tuning docs |
| Catalog-backed docs and ABI summaries | Duplicating full POC or refloat internals |
| Sandboxed path access | Default-on flash/upload |
| `vesc_tool` subprocess builds | Loading `vesc-ffi` / BLE protocol in MCP host |
| Read-only wire parsing in `vesc-domain` | In-repo `.vescpkg` packers |

## Testing architecture

| Tier | Location |
|------|----------|
| Unit | `#[cfg(test)]` in crate sources |
| Integration | `crates/*/tests/*.rs` |
| MCP harness | `McpTestHarness` in `vesc-mcp-core::test_support` |

See [testing.md](testing.md).
