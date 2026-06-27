# VESC package reference

Textbook-depth reference for VESC custom packages: from `pkgdesc.qml` on disk through packer, `.vescpkg` wire bytes, VESC Tool upload, on-device Lisp loader, and native library ABI. Target audience: AI assistants and package/firmware developers who must implement or debug packages without spelunking four repos.

## Document map

| Document | Scope |
|----------|-------|
| [vescpkg-wire-format.md](vescpkg-wire-format.md) | Byte-level `.vescpkg` spec, `lispData` geometry, failure taxonomy, [golden hex appendix](vescpkg-wire-format.md#appendix--annotated-golden-hex-walkthrough) |
| [vesc-pkg-lib-abi.md](vesc-pkg-lib-abi.md) | Native loader contract, macros, C vs Rust paths, firmware load sequence |
| [poc-integration.md](poc-integration.md) | vesc-mcp ↔ vesc-rust-poc adapter boundaries and sharp edges |
| [configuration.md](configuration.md) | Env vars (`VESC_REFLOAT_ROOT`, `VESC_BLDC_ROOT`, `VESC_POC_ROOT`, …) |
| [safety.md](safety.md) | Flash/upload gates (default off) |

Related MCP resources: `vesc://catalog/doc/topic/vescpackage_reference`, `vesc://catalog/doc/topic/pkgdesc_dialects`, `vesc://catalog/doc/topic/lisp_imports`, `vesc://catalog/doc/topic/vesc_c_if`.

## End-to-end lifecycle

```mermaid
flowchart TB
  subgraph Authoring
    A1[pkgdesc.qml + assets]
    A2[code.lisp imports]
    A3[optional native .bin build]
  end
  subgraph Validate
    V1[validate_package_layout]
  end
  subgraph Pack
    P1[vesc_tool --buildPkgFromDesc]
    P2[vesc-pkg-build Rust POC]
  end
  subgraph Artifact
    W1[.vescpkg qCompress + zlib]
  end
  subgraph Host
    H1[inspect_vescpkg / VESC Tool upload]
  end
  subgraph Runtime
    R1[firmware decompress fields]
    R2[Lisp VM runs lispData]
    R3["(load-native-lib tag)"]
    R4[INIT_FUN + lbm_add_extension]
  end
  A1 --> V1
  A2 --> V1
  A3 --> P1
  A3 --> P2
  V1 --> P1
  V1 --> P2
  P1 --> W1
  P2 --> W1
  W1 --> H1
  H1 --> R1
  R1 --> R2
  R2 --> R3
  R3 --> R4
```

### Pipeline steps

1. **Authoring** — Developer edits `pkgdesc.qml`, loader `.lisp`, optional UI `.qml`, README markdown, and native sources under a package root.
2. **Validation** — `validate_package_layout` checks that descriptor-relative paths (`pkgDescriptionMd`, `pkgLisp`, `pkgQml`) resolve to existing files under the root.
3. **Packing** — **refloat** uses `vesc_tool --buildPkgFromDesc pkgdesc.qml`; **POC** uses `vesc-pkg-build::build_vesc_package`. Both emit the same wire dialect when configured correctly.
4. **Artifact** — On-disk `.vescpkg`: Qt `qCompress` wrapper (4-byte BE length + zlib) around a `"VESC Packet"` field spine.
5. **Distribution** — VESC Tool upload to ESC, or offline inspection via MCP `inspect_vescpkg`.
6. **Runtime** — Firmware stores fields, evaluates `lispData` Lisp source, resolves `(import …)` embedded binaries, calls `(load-native-lib …)`.
7. **Extensions** — Native `init` registers symbols via `VESC_IF->lbm_add_extension`; app-level protocols (e.g. refloat commands) are a separate layer.

## Sharp edges (read first)

| Edge | Detail |
|------|--------|
| `lisp_editor_path` | Package **root**, not the `.lisp` file path. Import paths in `(import "src/foo.bin" …)` resolve relative to this root. See `vesc-mcp-adapters` build path and POC `VescPackageInput::lisp_editor_path`. |
| Legacy POC pkgdesc | Keys like `packageName`, `nativeLibraryPath` are **invalid**. `vesc-domain` returns `DomainError::LegacyPocDialect`. Use vesc_tool schema only (`pkgName`, `pkgLisp`, …). |
| Empty wire fields | May be **omitted** from the spine, not written as zero-length placeholders. Golden fixture omits empty `qmlFile`. |
| `pkgOutput` | Names the output **file on disk during build** only — not a wire field. |
| Read vs write | Wire parsing lives in `vesc-domain`; packing calls `vesc-pkg-build`. Do not reimplement layout in adapters. |

## On-disk layout and pkgdesc

### vesc_tool QML schema (canonical)

| QML property | Wire field | Notes |
|--------------|------------|-------|
| `pkgName` | `name` | Required; sanitized for artifact naming |
| `pkgDescriptionMd` | `description_md` | Relative path → markdown file (not inline) |
| `pkgLisp` | (inside `lispData`) | Relative path → loader Lisp |
| `pkgQml` | `qmlFile` | Relative path → UI QML; may be empty string |
| `pkgQmlIsFullscreen` | `qmlIsFullscreen` | Single-byte bool in wire |
| `pkgOutput` | — | Output filename only, e.g. `refloat.vescpkg` |

Optional refloat-only: `isCompatible(fwRxParams)` JavaScript guard — evaluated by vesc_tool; preserved in wire `pkgDescQml`; **not** parsed by `vesc-domain`.

### Fixture directory trees

**refloat-minimal** (`tests/fixtures/refloat-minimal/`):

```
refloat-minimal/
  pkgdesc.qml
  README.md          ← referenced as package_README-gen.md in production refloat
  code.lisp          ← fixture uses simplified paths vs production lisp/package.lisp
  ui.qml
```

**poc-native-lib-minimal** (`tests/fixtures/poc-native-lib-minimal/`):

```
poc-native-lib-minimal/
  package/
    pkgdesc.qml
    code.lisp
    README.md
  src/
    package_lib.bin    ← embedded via lispData import table
```

`locate_pkgdesc` searches `pkgdesc.qml` and `package/pkgdesc.qml` under a sandbox root.

### Layout validation

`validate_package_layout(root, desc)` mirrors `LayoutIssue::MissingAsset`:

- Missing readme (`pkgDescriptionMd` path)
- Missing lisp (`pkgLisp` path)
- Missing QML when `pkgQml` is non-empty

Negative fixtures: `tests/fixtures/broken-missing-lisp/`.

## Build recipes (summary)

Full Makefile detail lives in `catalog/refloat/build-flow.yaml` and MCP resource `vesc://catalog/build-recipe/refloat-vesc-tool`.

| Mode | Command | When |
|------|---------|------|
| Modern pkgdesc | `vesc_tool --buildPkgFromDesc pkgdesc.qml` | Default (`OLDVT=0`) |
| Legacy colon | `vesc_tool --buildPkg "out:lisp:qml:fs:readme:name"` | `OLDVT=1` on old vesc_tool |
| Native dep | `make -C src` | Before pack; produces `package_lib.bin` |
| POC Rust | `make package` / `build_vescpkg` mode `rust` | vesc-pkg-build in sibling POC |

Makefile variables: `VESC_TOOL`, `MINIFY_QML`, `OLDVT` — see build-flow catalog.

## Packer comparison

| Aspect | vesc_tool | vesc-pkg-build (POC) |
|--------|-----------|----------------------|
| Entry | `--buildPkgFromDesc` | `build_vesc_package(&VescPackageInput)` |
| Descriptor | Reads QML properties live | Staged files + embedded pkgdesc text |
| Native embed | `lispPackImports` from disk | `pack_lisp_imports` — same offset algorithm |
| Legacy colon | `--buildPkg` | Not supported |
| Parity anchor | — | Golden SHA-256 `34e95e36…` on `poc-minimal.vescpkg` |

Upstream writers: `$VESC_TOOL_ROOT/codeloader.cpp` (pack/unpack); in-repo reader: `crates/vesc-domain/src/wire/mod.rs`.

## Ground truth and test anchors

| Anchor | Use |
|--------|-----|
| `tests/fixtures/golden/poc-minimal.vescpkg` + `.sha256` | Byte-identical pack output |
| `vesc-domain` wire tests | Parser behavior, import geometry |
| `vesc-mcp-adapters/tests/characterization.rs` | Packer parity, offset 100 example |
| `tests/fixtures/broken-*` | Wire error taxonomy |
| `catalog/*.yaml` | Structured citations with env vars |

Regenerate golden:

```bash
nix develop -c cargo run -p vesc-mcp-adapters --bin gen-poc-minimal-golden
```

## MCP / assistant integration

Use this reference alongside live MCP tools (offline fixtures first):

| Tool | Use |
|------|-----|
| `inspect_pkgdesc` | Parse `pkgdesc.qml` under sandbox roots |
| `inspect_vescpkg` | Decode wire fields and lisp imports from `.vescpkg` |
| `validate_package_layout` | Pre-build asset checks |
| `build_vescpkg` | `mode: "rust"` on fixtures; `vesc_tool` when binary available |

| Resource URI | Topic |
|--------------|-------|
| `vescpkg://fixture/refloat-minimal/manifest` | Parsed refloat fixture |
| `vescpkg://fixture/poc-native-lib-minimal/manifest` | Parsed POC fixture |
| `vesc://catalog/build-recipe/poc-rust-packer` | POC `make package` flow |
| `vesc://catalog/abi/minimal-test-package` | 12-symbol POC ABI JSON |

Env vars: see [configuration.md](configuration.md). Flash/upload tools remain gated — see [safety.md](safety.md).

## Part 9 — annotated source walkthroughs

Each table lists **file path**, **line range**, **what to read**, and **connection to the next step**. Line numbers verified against in-repo `vendor/` submodules (2026-06); sibling checkouts use the same paths via `$VESC_REFLOAT_ROOT`, `$VESC_BLDC_ROOT`, `$VESC_TOOL_ROOT`, `$VESC_POC_ROOT` (see [configuration.md](configuration.md)).

### Example A — Refloat production package (authoring → artifact)

| Step | Path | Lines | What it proves | Next |
|------|------|-------|----------------|------|
| 1. Descriptor | `$VESC_REFLOAT_ROOT/pkgdesc.qml` | 1–18 | Canonical vesc_tool QML: `pkgName`, `pkgDescriptionMd`, `pkgLisp`, `pkgQml`, `pkgQmlIsFullscreen`, `pkgOutput`; `isCompatible` guard | → Makefile pack target |
| 2. Build entry | `$VESC_REFLOAT_ROOT/Makefile` | 10–15 | `vesc_tool --buildPkgFromDesc pkgdesc.qml`; L12 legacy `--buildPkg` when `OLDVT=1` | → native dep |
| 3. Native dep | `$VESC_REFLOAT_ROOT/Makefile` | 17–18 | `make -C src` builds `package_lib.bin` before pack | → Lisp loader |
| 4. Lisp loader | `$VESC_REFLOAT_ROOT/lisp/package.lisp` | 1–2 | `(import "src/package_lib.bin" 'package-lib)` + `(load-native-lib package-lib)` | → firmware runtime (Example C) |
| 5. BMS conditional | `$VESC_REFLOAT_ROOT/lisp/package.lisp` | 7–16 | Firmware-gated `(import "bms.lisp" 'bms)` — refloat-specific | — |
| 6. HEADER | `$VESC_REFLOAT_ROOT/src/main.c` | 58 | `.program_ptr` section word | → INIT_FUN |
| 7. INIT_FUN | `$VESC_REFLOAT_ROOT/src/main.c` | 2665–2677 | `init(lib_info)`: `INIT_START`, `info->arg`, `info->stop_fun` | → extensions |
| 8. Extensions | `$VESC_REFLOAT_ROOT/src/main.c` | 2708–2709 | `VESC_IF->lbm_add_extension` after native load | → Example C step 4 |
| 9. Native build | `$VESC_REFLOAT_ROOT/src/Makefile` | 7–22 | `TARGET=package_lib`; `VESC_C_LIB_PATH=../vesc_pkg_lib/`; includes `rules.mk` | → bin output |
| 10. Output | `$VESC_REFLOAT_ROOT/vesc_pkg_lib/rules.mk` | 62–66 | `package_lib.bin` → consumed by lisp import | → wire packer (Example D) |

### Example B — vesc_pkg_lib toolchain

| Step | Path | Lines | What it proves | Next |
|------|------|-------|----------------|------|
| 1. Header (refloat) | `$VESC_REFLOAT_ROOT/vesc_pkg_lib/vesc_c_if.h` | 675–704 | `lib_info`, `VESC_IF`, `HEADER`, `INIT_FUN`, `INIT_START`, `PROG_ADDR`, `ARG` | → link script |
| 2. Header (bldc canonical) | `$VESC_BLDC_ROOT/lispBM/c_libs/vesc_c_if.h` | 675–704 | Same macros — firmware source of truth | → compile |
| 3. Link script | `$VESC_REFLOAT_ROOT/vesc_pkg_lib/link.ld` | 11–21 | `.program_ptr` and `.init_fun` at MEM start | → flags |
| 4. Compile flags | `$VESC_REFLOAT_ROOT/vesc_pkg_lib/rules.mk` | 35–49 | cortex-m4, `-DIS_VESC_LIB`, `--undefined=init` | → bin output |
| 5. bin output | `$VESC_REFLOAT_ROOT/vesc_pkg_lib/rules.mk` | 62–66 | objcopy → `package_lib.bin`; `conv.py` → `.lisp` | → import or conv path |
| 6. conv.py | `$VESC_REFLOAT_ROOT/vesc_pkg_lib/conv.py` | 13–31 | Alternate: `(def name [0x..])` byte array embedding | Contrast: refloat default uses `.bin` import |

### Example C — bldc firmware runtime (`load-native-lib`)

| Step | Path | Lines | What it proves | Next |
|------|------|-------|----------------|------|
| 1. Register ext | `$VESC_BLDC_ROOT/lispBM/lispif_vesc_extensions.c` | 6681–6682 | `load-native-lib` / `unload-native-lib` registered | → entry |
| 2. Entry | `$VESC_BLDC_ROOT/lispBM/lispif_c_lib.c` | 738–743 | `ext_load_native_lib`: one byte-array arg | → CIF table |
| 3. CIF table | `$VESC_BLDC_ROOT/lispBM/lispif_c_lib.c` | 747–1082 | First load fills `cif.cif` for native lib | → load sequence |
| 4. Load sequence | `$VESC_BLDC_ROOT/lispBM/lispif_c_lib.c` | 1084–1099 | `addr = array->data`; `addr += 4` skip prog_ptr; `addr \|= 1` thumb; call init | → result |
| 5. Result | `$VESC_BLDC_ROOT/lispBM/lispif_c_lib.c` | 1101–1115 | `SYM_TRUE` or *"Library init failed"* | → unload |
| 6. Unload | `$VESC_BLDC_ROOT/lispBM/lispif_c_lib.c` | 1120–1144 | `stop_fun` cleanup | — |
| 7. pkg import docs | `$VESC_BLDC_ROOT/lispBM/README.md` | 6070+ | `(import "pkg@path.vescpkg" 'name)` syntax | — |

**Swimlane:** `lispData` embedded `.bin` → Lisp byte array → skip 4-byte `prog_ptr` → `INIT_FUN` (cross-link Example A step 7).

### Example D — vesc_tool wire writer (packer; not in bldc)

| Step | Path | Lines | What it proves | Next |
|------|------|-------|----------------|------|
| 1. pkgdesc | `$VESC_TOOL_ROOT/codeloader.cpp` | 1177–1256 | QML property reads: `pkgName`, `pkgDescriptionMd`, `pkgLisp`, `pkgQml`, `pkgQmlIsFullscreen`, `pkgOutput` | → lisp pack |
| 2. lisp pack call | `$VESC_TOOL_ROOT/codeloader.cpp` | 1217 | `lispPackImports(..., fi.canonicalPath(), …)` — **editor path is lisp file dir** | → algorithm |
| 3. lispPackImports | `$VESC_TOOL_ROOT/codeloader.cpp` | 173–252 | flags i16=0, parse imports, `editorPath + "/" + path` resolve | → spine |
| 4. packVescPackage | `$VESC_TOOL_ROOT/codeloader.cpp` | 817–868 | `"VESC Packet"` + wire fields + `qCompress` | → unpack |
| 5. unpack | `$VESC_TOOL_ROOT/codeloader.cpp` | 871–929 | Parser mirror for `vesc-domain` parity | → in-repo reader |

In-repo reader: `crates/vesc-domain/src/wire/mod.rs` (`FIELD_SPINE`, `package_fields`, `parse_lisp_imports`).

### Example E — In-repo fixtures (offline)

| Fixture | Path | Maps to | MCP / test anchor |
|---------|------|---------|-------------------|
| refloat-minimal | `tests/fixtures/refloat-minimal/pkgdesc.qml` | Example A descriptor | `inspect_pkgdesc`, `vescpkg://fixture/refloat-minimal/manifest` |
| poc-native-lib-minimal | `tests/fixtures/poc-native-lib-minimal/package/` | nested layout + native embed | `build_vescpkg` mode `rust` |
| golden | `tests/fixtures/golden/poc-minimal.vescpkg` | annotated hex — [wire appendix](vescpkg-wire-format.md#appendix--annotated-golden-hex-walkthrough) | `inspect_vescpkg`, SHA-256 parity |
| domain | `crates/vesc-domain/src/wire/mod.rs` | reader impl | wire unit tests |
| adapter | `crates/vesc-mcp-adapters/src/build.rs` | 62–63 | `lisp_editor_path` = fixture **root**, not `.lisp` dir |

### Example F — POC Rust packer cross-check

| Path | Lines | What it proves |
|------|-------|----------------|
| `$VESC_POC_ROOT/crates/vesc-pkg-build/src/package_format.rs` | ~337 | `lisp_imports_embed_native_payload_bytes` — offset/size/payload geometry |
| `$VESC_POC_ROOT/crates/vesc-pkg-build/src/package_format.rs` | ~408 | `package_uses_the_vesc_tool_field_spine` |
| `$VESC_POC_ROOT/docs/abi-inventory.md` | — | 12-symbol ABI prose |
| `catalog/poc/minimal-test-package-abi.yaml` | — | MCP resource `vesc://catalog/abi/minimal-test-package` |
| `crates/vesc-mcp-adapters/tests/characterization.rs` | 11–53 | Parity test mirroring L337 (offset 100, size 6 example) |

## Further reading

- Wire bytes: [vescpkg-wire-format.md](vescpkg-wire-format.md)
- Native ABI: [vesc-pkg-lib-abi.md](vesc-pkg-lib-abi.md)
- Gap matrix: [catalog/gap-analysis.md](../catalog/gap-analysis.md)
- Example sessions: [docs/examples/](examples/)
