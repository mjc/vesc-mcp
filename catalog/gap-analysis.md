# Refloat vs vesc-rust-poc Gap Analysis

Cross-reference matrix for production refloat patterns, authoritative bldc firmware APIs, and the Rust POC experiment. Catalog YAML files provide machine-readable detail; this document captures intentional divergences and integration risks.

## Summary

| Area | Refloat | POC / vesc-mcp handling | Impact |
|------|---------|-------------------------|--------|
| Package packer | vesc_tool CLI | `build_vescpkg` + committed golden bytes | Same packer; golden is read-only offline anchor |
| Descriptor dialect | vesc_tool (`pkgName`, …) | Canonical fixtures; legacy POC fields rejected | One accepted schema |
| Native payload | C via vesc_pkg_lib | Rust staticlib + C shim | Toolchain and symbol audit differ |
| BLE / device test | Manual VESC Tool upload | Host `loopback` CLI | POC has automated hw path; refloat docs none |
| Command protocol | 7 public + 2 internal docs | Indexed catalog resources; absent from POC | POC scope is packaging, not app comms |
| BMS integration | Conditional lisp/bms.lisp | Same pattern in POC fixture | Parity at loader level |

## Descriptor {#descriptor}

**refloat** (`pkgdesc.qml`):

- `pkgName`, `pkgDescriptionMd`, `pkgLisp`, `pkgQml`, `pkgQmlIsFullscreen`, `pkgOutput`
- `isCompatible(fwRxParams)` JavaScript guard

**Historical POC schema** (`fixtures/native-lib-baseline/package/pkgdesc.qml`):

- Uses invented `packageName`, `packageVersion`, `nativeLibraryPath`, `loaderScriptPath`
- These fields are **not** read by `vesc_tool` (`codeloader.cpp` only reads `pkgName`, `pkgDescriptionMd`, `pkgLisp`, `pkgQml`, `pkgQmlIsFullscreen`, `pkgOutput`)

**Gap:** Early POC descriptors used a non-authoritative property naming
scheme. Current vesc-mcp fixtures use the canonical vesc_tool schema.

**Mitigation:** `vesc-domain` rejects legacy POC-only fields with `DomainError::LegacyPocDialect`. The vesc-mcp fixture `native-lib-minimal/` already uses the vesc_tool schema.

## Packer {#packer}

**refloat:** `make` → `vesc_tool --buildPkgFromDesc pkgdesc.qml` (or legacy `--buildPkg` colon string when `OLDVT=1`). This matches official VESC Tool behavior (`codeloader.cpp`).

**vesc-mcp:** `build_vescpkg` spawns `vesc_tool` only. Golden `native-lib-minimal.vescpkg` is a read-only wire reference for offline tests.

**Gap:** CI hosts without `vesc_tool` cannot run live build tests; wire parsing tests use committed golden bytes.

**Mitigation:** Optional golden-stability tests when `VESC_TOOL_PATH` is set; golden regeneration documented in `tests/fixtures/golden/README.md`.

## Native library build {#native}

**refloat:**

- ARM GCC via `vesc_pkg_lib/rules.mk`
- Vendored `vesc_c_if.h` snapshot
- Direct `.bin` import in the default package flow; `conv.py` remains an
  optional byte-array conversion path

**POC:**

- `thumbv7em-none-eabihf` Rust staticlib
- C shim in fixture `src/` with symbol audit gate
- Same `.bin` → package import path conceptually

**Gap:** Rust packages need `symbol_audit` and different link flags; refloat uses pure C examples.

**Mitigation:** `catalog/abi/minimal-test-package-abi.yaml` lists the minimal
12-symbol surface; expand it only as features require.

## Loader script {#loader}

Both import native binary and call `load-native-lib`:

```lisp
(import "src/package_lib.bin" 'package-lib)
(load-native-lib package-lib)
```

**refloat** adds firmware-version gating and optional BMS thread spawn. **POC** baseline loader registers Rust-backed Lisp extensions.

**Gap:** Extension names and registration differ by package; no shared loader library.

## BLE and host testing {#ble}

**POC** documents a full loop: `make check` → `make package` → VESC Tool upload → `vesc-host-cli loopback`.

**refloat** has no equivalent automated BLE smoke in-repo; command docs describe `COMM_CUSTOM_APP_DATA` client protocol instead.

**Gap:** MCP cannot assume POC loopback for refloat-specific behavior without hardware.

## Firmware API surface {#firmware}

**bldc** `vesc_c_if.h` exposes 20+ function groups with append-only FW versioning (6.05 / 6.06 / 7.00 markers).

**POC** intentionally uses ~12 symbols for first proof (`lbm_add_extension`, encode/decode helpers, init macros).

**Gap:** Null-pointer checks required when targeting FW &lt; 6.05; POC does not yet exercise NVM, CAN, or comm handlers.

## Command protocol {#commands}

**refloat** documents 7 public + 2 internal commands under `doc/commands/` with interface id 101.

**POC** has no package command catalog; BLE loopback is transport-level, not Refloat command IDs.

The knowledge corpus keeps Refloat command documents distinct from firmware API
sources through category and source metadata.

## Catalog coverage

| refloat feature | bldc API | POC equivalent |
|-----------------|----------|----------------|
| Build pkgdesc | — | `make package` + `PackageBuildPlan` |
| load-native-lib | `ext_load_native_lib` in lispif_vesc_extensions.c | `code.lisp` + staticlib |
| ext-set-fw-version | `lbm_add_extension` | Rust `ext-rust-add` proof |
| COMM_CUSTOM_APP_DATA | `send_app_data`, `set_app_data_handler` | Not in POC |
| BMS thread | `spawn`, BMS extensions | Conditional `bms.lisp` import |
| XML config codegen | — | Not in POC |
| NVM persistence | `read_nvm`, `write_nvm`, `wipe_nvm` | Not in minimal ABI |

## Implementation status

- Catalog paths are validated with environment-aware tests.
- Domain dialect parsing and POC inspection/build adaptation live in the Rust crates.
- Catalog YAML is exposed through MCP resources.
- The normalized knowledge corpus includes reviewed command documentation.

Track remaining gaps and sequencing in the Lific `VESCM` project rather than
embedding issue IDs here.

## References

- `catalog/refloat/build-flow.yaml`
- `catalog/refloat/lisp-loader.yaml`
- `catalog/bldc/vesc_c_if.yaml`
- `catalog/abi/minimal-test-package-abi.yaml`
- `vesc-rust-poc/docs/package-flow.md`
- `catalog/priorities.json`
