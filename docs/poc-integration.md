# MCP build adapters (fixtures)

**Production packaging** follows refloat and official VESC tooling: `vesc_tool --buildPkgFromDesc pkgdesc.qml` (see [build-flow catalog](../catalog/refloat/build-flow.yaml) and `vesc://catalog/build-recipe/refloat-vesc-tool`).

vesc-mcp does **not** link **vesc-rust-poc**. For **offline CI and MCP sandbox tests only**, `build_vescpkg` with `mode: "rust"` stages fixture files and writes `.vescpkg` via an in-repo **parity writer** in `vesc-domain::wire` that mirrors `vesc_tool` `codeloader.cpp`. That writer is not a supported packaging workflow for real packages.

## Parity writer API (test harness)

| Symbol | Crate | Use |
|--------|-------|-----|
| `VescPackageBuildInput` | vesc-domain | Staged inputs for wire bytes |
| `build_vescpkg_bytes` | vesc-domain | Produce `.vescpkg` bytes |
| `write_vescpkg_file` | vesc-domain | Write artifact to disk |

Implementation: `crates/vesc-domain/src/wire/write.rs`.

## Catalog pointers to external repos

The knowledge index may cite **vesc-rust-poc** for ABI inventory paths. Those are documentation-only — not Cargo dependencies.

`VESC_POC_ROOT` / `poc_root` resolve catalog paths on disk for MCP resources when a sibling checkout exists.

## Sharp edges

1. **`lisp_editor_path` is the package/fixture root**, not the `.lisp` file path.

2. **Pkgdesc dialect.** Fixtures use vesc_tool keys (`pkgName`, `pkgLisp`, `pkgOutput`, …). Legacy keys are rejected with `DomainError::LegacyPocDialect`.

3. **Golden vectors.** `tests/fixtures/golden/poc-minimal.vescpkg` pins parity-writer output for CI. Regenerate: `nix develop -c cargo run -p vesc-mcp-adapters --bin gen-poc-minimal-golden`.

4. **Prefer vesc_tool for real builds.** When `VESC_TOOL_PATH` is available, use `build_vescpkg` with `mode: "vesc_tool"`.
