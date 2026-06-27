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
  TOOLS --> CFG
  RES --> CAT
  RES --> DOM
  CAT --> CATALOG
  ADP --> DOM
  TOOLS -->|mode vesc_tool| VTOOL
  CFG --> FIX
  DOM --> FIX
```

## Layer responsibilities

| Layer | Crate / path | Responsibility |
|-------|----------------|----------------|
| Transport | `vesc-mcp-server` | stdio MCP session, tracing to stderr |
| MCP surface | `vesc-mcp-core` | Tool router, resource registry, config, workspace discovery |
| Domain | `vesc-domain` | `pkgdesc.qml` parsing, `.vescpkg` wire read/write, validation types |
| Build adapter | `vesc-mcp-adapters` | Stage fixtures; rust mode uses parity writer; production path is `vesc_tool` |
| Knowledge | `vesc-knowledge-index` | Embedded search index over catalog-derived entries |
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
  participant W as vesc-domain::wire

  C->>S: tools/call build_vescpkg
  S->>T: root, mode=rust
  T->>T: validate_sandbox_path
  T->>D: parse_pkgdesc_qml
  alt mode rust
    T->>A: stage + build
    A->>W: write_vescpkg_file
  else mode vesc_tool
    T->>T: spawn VESC_TOOL_PATH subprocess
  end
  T-->>C: JSON artifact path + metadata
```

## Resource flow

Static resources are registered at startup from `catalog/` and fixture metadata. Dynamic reads use URI templates:

- `vescpkg://manifest/{path}` — parse live pkgdesc under sandbox roots
- `vesc://catalog/commands/refloat/{command}` — render markdown from indexed command docs

Build-recipe and doc-topic bodies include **source attribution** footers pointing at resolved repo paths (`VESC_*_ROOT`).

## Boundaries and non-goals

| In scope | Out of scope |
|----------|--------------|
| Package discovery, inspect, validate, build | Rider-facing tuning docs |
| Catalog-backed docs and ABI summaries | Duplicating full POC or refloat internals |
| Sandboxed path access | Default-on flash/upload |
| `vesc_tool` subprocess builds | Loading `vesc-ffi` / BLE protocol in MCP host |

POC integration details: [poc-integration.md](poc-integration.md).

## Testing architecture

| Tier | Location |
|------|----------|
| Unit | `#[cfg(test)]` in crate sources |
| Integration | `crates/*/tests/*.rs` |
| MCP harness | `McpTestHarness` in `vesc-mcp-core::test_support` |

See [testing.md](testing.md).
