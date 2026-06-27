# vesc-rust-poc integration

vesc-mcp reuses **packaging write** logic from the sibling POC while **read/validate** lives in `vesc-domain`.

## Path dependency

Default (sibling checkout):

```toml
# crates/vesc-mcp-adapters/Cargo.toml
vesc-pkg-build = { path = "../../../vesc-rust-poc/crates/vesc-pkg-build" }
```

Override checkout root with `VESC_POC_ROOT` (see `vesc-mcp-core::catalog::CatalogRepo::Poc`).

## In-scope POC API (v1)

| Symbol | Crate | Use in vesc-mcp |
|--------|-------|-----------------|
| `VescPackageInput` | vesc-pkg-build | Build wire payload from staged files |
| `build_vesc_package` | vesc-pkg-build | Produce `.vescpkg` bytes |
| `write_vesc_package` | vesc-pkg-build | Write artifact to disk |
| `build_lisp_data` | vesc-pkg-build | Lisp import embedding |

## Out of scope (MCP server)

| Crate | Reason |
|-------|--------|
| `vesc-ffi` | `unsafe` / device FFI — not loaded in MCP host |
| `vesc-rust-poc` | `no_std` device crate |
| `vesc-protocol` | BLE protocol — tools only unless explicitly added |

## vesc_tool pkgdesc (canonical)

Build adapters read **on-disk** `pkgdesc.qml` using `vesc-domain::parse_pkgdesc_qml` (vesc_tool schema: `pkgName`, `pkgOutput`, …). We do **not** use POC `PackageBuildPlan::render_descriptor()` — it emitted the legacy `packageName` dialect.

POC `PackageBuildPlan` remains documented for native-lib-baseline workflows inside vesc-rust-poc (`br-integrate-poc-5tu.10`).

## Pin policy

1. **Local dev:** path dep to sibling checkout; run `make check` before commits.
2. **API break:** pin POC git rev in this doc + open Beads task; avoid silent breakage.
3. **Future:** optional git submodule or crates.io publish — not v1.

## Sharp edges

1. **`lisp_editor_path` is the repo/fixture root**, not the `.lisp` file path. POC `build_vesc_package` resolves `(import "…")` paths relative to this root; vesc-mcp adapters pass the same root as `build_package_from_root` receives.

2. **Read vs write split.** Wire parsing, field spine checks, and layout validation live in `vesc-domain`. Packing and native-payload embedding call into `vesc-pkg-build`. Do not reimplement wire layout in adapters.

3. **Pkgdesc dialect.** On-disk fixtures must use vesc_tool keys (`pkgName`, `pkgLisp`, `pkgOutput`, …). Legacy POC keys (`packageName`, `nativeLibraryPath`) are rejected by `vesc-domain` with `DomainError::LegacyPocDialect`. Cross-repo migration is tracked in `br-integrate-poc-5tu.10`.

4. **No FFI in adapters.** `vesc-ffi` and device/runtime crates stay out of the MCP host. Adapters only link `vesc-pkg-build` for deterministic pack writes.

5. **No cross-compile in adapters.** Builds assume host-native `vesc-pkg-build`; target triple selection belongs in POC/device workflows, not MCP tooling.

6. **Golden vectors.** Committed `tests/fixtures/golden/poc-minimal.vescpkg` must match adapter output SHA-256. Regenerate via `nix develop -c cargo run -p vesc-mcp-adapters --bin gen-poc-minimal-golden` (or `python3 scripts/gen_poc_minimal_golden.py`, which delegates to the same Rust path).

## License

Both repos are GPL. MCP server is a separate binary linking POC as a library; keep license files in sync when distributing.
