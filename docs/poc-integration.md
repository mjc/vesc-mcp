# POC catalog and fixtures (no compile-time dependency)

vesc-mcp does **not** link the sibling **vesc-rust-poc** checkout. Wire **read and write** both live in `vesc-domain::wire`; `vesc-mcp-adapters` stages files and calls `write_vescpkg_file`.

## Packaging API (in-repo)

| Symbol | Crate | Use in vesc-mcp |
|--------|-------|-----------------|
| `VescPackageBuildInput` | vesc-domain | Build wire payload from staged files |
| `build_vescpkg_bytes` | vesc-domain | Produce `.vescpkg` bytes |
| `write_vescpkg_file` | vesc-domain | Write artifact to disk |

Implementation: `crates/vesc-domain/src/wire/write.rs` (ported from the historical POC packer; kept in sync with `vesc_tool` / `codeloader.cpp` behavior).

## External POC references (catalog only)

The knowledge index and catalog may still cite **vesc-rust-poc** as a logical repo for ABI entries and gap analysis. Those are documentation pointers — not Cargo path dependencies.

`VESC_POC_ROOT` / `poc_root` in config only matter when resolving catalog paths on disk for MCP resources (e.g. ABI snippets), not for building packages.

## Out of scope (MCP server)

| Crate | Reason |
|-------|--------|
| `vesc-ffi` | `unsafe` / device FFI — not loaded in MCP host |
| `vesc-rust-poc` device crate | `no_std` runtime — not linked here |
| `vesc-protocol` | BLE protocol — tools only unless explicitly added |

## vesc_tool pkgdesc (canonical)

Build adapters read **on-disk** `pkgdesc.qml` using `vesc-domain::parse_pkgdesc_qml` (vesc_tool schema: `pkgName`, `pkgOutput`, …).

## Sharp edges

1. **`lisp_editor_path` is the repo/fixture root**, not the `.lisp` file path. `(import "…")` paths resolve relative to this root; adapters pass the same root as `build_package_from_root` receives.

2. **Read and write in domain.** Wire parsing, field spine checks, layout validation, and packing all live in `vesc-domain`. Adapters orchestrate staging + I/O only.

3. **Pkgdesc dialect.** On-disk fixtures must use vesc_tool keys (`pkgName`, `pkgLisp`, `pkgOutput`, …). Legacy POC keys (`packageName`, `nativeLibraryPath`) are rejected with `DomainError::LegacyPocDialect`.

4. **No FFI in adapters.** Device/runtime crates stay out of the MCP host.

5. **Golden vectors.** Committed `tests/fixtures/golden/poc-minimal.vescpkg` must match adapter output SHA-256. Regenerate via `nix develop -c cargo run -p vesc-mcp-adapters --bin gen-poc-minimal-golden`.

## License

GPL. MCP server is a separate binary; keep license files in sync when distributing.
