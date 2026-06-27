# AGENTS.md — vesc-mcp cheat sheet

Guide for AI assistants working in this repo. Tool and resource names match `VescMcpService` / `McpTestHarness::list_tool_names()` and the default resource registry (see `crates/vesc-mcp-core/tests/resource_service.rs`).

## Dev shell and checks

```bash
nix develop -c make check
nix develop -c cargo nextest run -p vesc-mcp-core -E 'test(tool_)'
```

See [docs/testing.md](docs/testing.md) for the red → green → refactor workflow.

## MCP tools

All tools return JSON text payloads.

| Tool | Purpose | Key params |
|------|---------|------------|
| `ping` | Health check | `message` (optional echo) |
| `list_vesc_packages` | Discover package roots (`pkgdesc.qml`) | `roots` (optional; defaults to `VESC_PACKAGE_ROOTS`) |
| `inspect_pkgdesc` | Parse `pkgdesc.qml` | `path` — file path |
| `inspect_vescpkg` | Read `.vescpkg` wire artifact | `path` — file path |
| `validate_package_layout` | Check pkgdesc asset references exist | `root` — package directory |
| `build_vescpkg` | Build `.vescpkg` | `root`, `mode` (`rust` \| `vesc_tool`), `timeout_secs` (default 120) |
| `run_package_checks` | Run fmt/clippy/test in package sandbox | `root` |
| `search_vesc_knowledge` | Search embedded knowledge index | `query`, `category` (optional), `limit` (default 10) |

### Path sandbox

Tools that read or write package trees require paths under **`VESC_PACKAGE_ROOTS`** (comma- or colon-separated). In tests, fixtures under `tests/fixtures/` are allowed automatically via the `test-fixtures` feature.

### Offline fixture examples

| Fixture | Use with |
|---------|----------|
| `tests/fixtures/refloat-minimal/` | `inspect_pkgdesc`, `validate_package_layout`, `build_vescpkg` |
| `tests/fixtures/poc-native-lib-minimal/` | native-lib layout, rust packer |
| `tests/fixtures/broken-*` | negative / error-path tests |
| `tests/fixtures/golden/` | deterministic wire bytes |

Helpers: `vesc_mcp_core::test_support::{fixture_path, read_fixture_file, McpTestHarness, TempWorkspace}` — see [tests/fixtures/README.md](tests/fixtures/README.md).

## MCP resources

### Static resources (`resources/list`)

**Build recipes** (`text/markdown`):

| URI | Description |
|-----|-------------|
| `vesc://catalog/build-recipe/refloat-vesc-tool` | Refloat Makefile + vesc_tool build flow |
| `vesc://catalog/build-recipe/poc-rust-packer` | POC `make package` + Rust packer |

**Doc topics** (`text/markdown`):

| URI | Description |
|-----|-------------|
| `vesc://catalog/doc/topic/pkgdesc_dialects` | vesc_tool vs legacy POC pkgdesc schemas |
| `vesc://catalog/doc/topic/vesc_c_if` | LBM `vesc_c_if` extension surface |
| `vesc://catalog/doc/topic/lisp_imports` | `lispData` native import wire format |

**ABI inventory** (`application/json`):

| URI | Description |
|-----|-------------|
| `vesc://catalog/abi/minimal-test-package` | Symbols for minimal test package (e.g. `lbm_add_extension`) |

**Fixture manifests** (`application/json`):

| URI | Description |
|-----|-------------|
| `vescpkg://fixture/refloat-minimal/manifest` | Parsed pkgdesc for refloat-minimal fixture |
| `vescpkg://fixture/poc-native-lib-minimal/manifest` | Parsed pkgdesc for POC native-lib fixture |

**Refloat command docs** (`text/markdown`, from `catalog/refloat/commands.yaml`):

| URI | Summary |
|-----|---------|
| `vesc://catalog/commands/refloat/INFO` | Versioned handshake |
| `vesc://catalog/commands/refloat/LIGHTS_CONTROL` | Lights control |
| `vesc://catalog/commands/refloat/REMOTE` | Remote tilt/drive input |
| `vesc://catalog/commands/refloat/REALTIME_DATA` | Selectable realtime fields |
| `vesc://catalog/commands/refloat/DATA_RECORD` | Data recording control |
| `vesc://catalog/commands/refloat/ALERTS_LIST` | Alerts list/history |
| `vesc://catalog/commands/refloat/ALERTS_CONTROL` | Alerts control |
| `vesc://catalog/commands/refloat/REALTIME_DATA_INTERNAL` | Internal realtime (unstable) |
| `vesc://catalog/commands/refloat/REALTIME_DATA_INTERNAL_IDS` | Internal field ID strings |

### Resource templates (`resources/templates/list`)

| Template | Use |
|----------|-----|
| `vescpkg://manifest/{path}` | Live pkgdesc under a sandboxed package root (`{path}` = package root) |
| `vesc://catalog/commands/refloat/{command}` | Any command name indexed in `catalog/refloat/commands.yaml` |

## Safety rules

- **Flash/upload tools are gated** — default off. Enable only with `VESC_MCP_ENABLE_FLASH=1` or `[features] enable_flash = true` in config. Never assume flash is available. See [docs/safety.md](docs/safety.md).
- **Sandbox all paths** — reject reads/writes outside `VESC_PACKAGE_ROOTS`.
- **Prefer fixtures offline** — use `tests/fixtures/` and `vescpkg://fixture/…` URIs before pointing at live sibling repos.
- **No hardcoded home paths** in prompts or commits — use env vars or `config.toml`.

## TDD checklist

1. **RED** — Add a failing test naming the behavior (e.g. `inspect_pkgdesc_returns_json_for_refloat_fixture`).
2. **GREEN** — Minimal implementation; `nix develop -c cargo nextest run --workspace`.
3. **REFACTOR** — Extract shared logic; keep tests green.
4. Commit with `test(...)` / `feat(...)` / `docs(...)` and reference the Beads task id.

Integration tests use `McpTestHarness::call_tool(name, json!({...}))` — same handlers as the live MCP server.

## Related docs

- [docs/configuration.md](docs/configuration.md) — env vars
- [docs/architecture.md](docs/architecture.md) — crate boundaries
- [docs/safety.md](docs/safety.md) — flash/upload gates
- [docs/examples/](docs/examples/) — copy-paste agent sessions
- [catalog/gap-analysis.md](catalog/gap-analysis.md) — known coverage gaps
