# VESC package reference

End-to-end guide for VESC custom packages: `pkgdesc.qml` on disk → packer → `.vescpkg` wire bytes → VESC Tool upload → on-device Lisp loader → native library ABI. Use this topic with the split repo docs and sibling MCP resources below.

## Document map

| Repo doc | Scope |
|----------|-------|
| `docs/vescpackage-reference.md` | Master index, lifecycle diagram, sharp edges, MCP integration |
| `docs/vescpkg-wire-format.md` | Byte-level `.vescpkg` spec, `lispData` geometry, failure taxonomy |
| `docs/vesc-pkg-lib-abi.md` | Native loader contract, macros, C vs Rust paths, firmware load sequence |

Related MCP doc topics: `pkgdesc_dialects`, `lisp_imports`, `vesc_c_if`.

## Lifecycle (summary)

1. **Authoring** — `pkgdesc.qml`, loader `.lisp`, optional UI `.qml`, README, native sources under a package root.
2. **Validation** — `validate_package_layout` checks descriptor-relative paths resolve under the root.
3. **Packing** — **Production:** `vesc_tool --buildPkgFromDesc pkgdesc.qml` (refloat Makefile). Authoritative behavior is in VESC Tool `codeloader.cpp`.
4. **Artifact** — `.vescpkg`: Qt `qCompress` (4-byte BE length + zlib) around a `"VESC Packet"` field spine.
5. **Distribution** — VESC Tool upload or MCP `inspect_vescpkg`.
6. **Runtime** — Firmware evaluates `lispData`, resolves `(import …)` embedded binaries, calls `(load-native-lib …)`.
7. **Extensions** — Native `init` registers symbols via `VESC_IF->lbm_add_extension`.

## Sharp edges (read first)

| Edge | Detail |
|------|--------|
| `lisp_editor_path` | In `vesc_tool`, import paths resolve relative to the lisp file directory (`codeloader.cpp`). |
| Legacy POC pkgdesc | Keys like `packageName`, `nativeLibraryPath` are **invalid** — use vesc_tool schema (`pkgName`, `pkgLisp`, …). |
| Empty wire fields | May be **omitted** from the spine, not zero-length placeholders. |
| `pkgOutput` | Output filename on disk only — **not** a wire field. |
| Wire read | `vesc-domain::wire` is read-only; packing is always `vesc_tool`. |

## Wire field spine (vesc_tool order)

| Order | Wire key | QML source |
|-------|----------|------------|
| 1 | `name` | `pkgName` |
| 2 | `description_md` | `pkgDescriptionMd` → file |
| 3 | `lispData` | `pkgLisp` → loader + import table |
| 4 | `qmlFile` | `pkgQml` |
| 5 | `pkgDescQml` | embedded descriptor round-trip |
| 6 | `qmlIsFullscreen` | `pkgQmlIsFullscreen` |

See `lisp_imports` topic for `lispData` binary layout and offset arithmetic.

## MCP tools and fixtures

| Tool | Use |
|------|-----|
| `inspect_pkgdesc` | Parse `pkgdesc.qml` under sandbox roots |
| `inspect_vescpkg` | Decode wire fields and lisp imports |
| `validate_package_layout` | Pre-build asset checks |
| `build_vescpkg` | Spawn `vesc_tool --buildPkgFromDesc` (`VESC_TOOL_PATH`) |

Offline fixtures: `tests/fixtures/refloat-minimal/`, `native-lib-minimal/`, read-only `golden/native-lib-minimal.vescpkg`.
