# Refloat vs vesc-rust-poc Gap Analysis

Cross-reference matrix for production refloat patterns, authoritative bldc firmware APIs, and the Rust POC experiment. Catalog YAML files provide machine-readable detail; this document captures intentional divergences and integration risks.

## Summary

| Area | refloat | vesc-rust-poc | Impact |
|------|---------|---------------|--------|
| Package packer | vesc_tool CLI | Rust `vesc-pkg-build` | Different build entrypoint; wire format should match |
| Descriptor dialect | vesc_tool (`pkgName`, …) | Legacy POC mistake (`packageName`, …) | POC must migrate to vesc_tool schema |
| Native payload | C via vesc_pkg_lib | Rust staticlib + C shim | Toolchain and symbol audit differ |
| BLE / device test | Manual VESC Tool upload | Host `loopback` CLI | POC has automated hw path; refloat docs none |
| Command protocol | 12 doc/commands | Not implemented | POC scope is packaging, not app comms |
| BMS integration | Conditional lisp/bms.lisp | Same pattern in POC fixture | Parity at loader level |

## Descriptor {#descriptor}

**refloat** (`pkgdesc.qml`):

- `pkgName`, `pkgDescriptionMd`, `pkgLisp`, `pkgQml`, `pkgQmlIsFullscreen`, `pkgOutput`
- `isCompatible(fwRxParams)` JavaScript guard

**POC (incorrect, pending fix)** (`fixtures/native-lib-baseline/package/pkgdesc.qml`):

- Uses invented `packageName`, `packageVersion`, `nativeLibraryPath`, `loaderScriptPath`
- These fields are **not** read by `vesc_tool` (`codeloader.cpp` only reads `pkgName`, `pkgDescriptionMd`, `pkgLisp`, `pkgQml`, `pkgQmlIsFullscreen`, `pkgOutput`)

**Gap:** POC pkgdesc used a non-authoritative property naming scheme. Canonical schema is vesc_tool/refloat only (`br-flj.12` decision).

**Mitigation:** `vesc-domain` rejects legacy POC-only fields with `DomainError::LegacyPocDialect`. Fix tracked in vesc-rust-poc beads; vesc-mcp fixture `poc-native-lib-minimal/` already uses vesc_tool schema.

## Packer {#packer}

**refloat:** `make` → `vesc_tool --buildPkgFromDesc pkgdesc.qml` (or legacy `--buildPkg` colon string when `OLDVT=1`).

**POC:** `make package` → `vesc-pkg-build` writes `.vescpkg` with magic `"VESC Packet"`, zlib-compressed fields, null-terminated keys.

**Gap:** Two independent implementations. POC tests (`package_uses_the_vesc_tool_field_spine`) assert field-name compatibility with vesc_tool output, but refloat CI does not run Rust packer.

**Mitigation:** Integration epic (`br-integrate-poc-5tu`) wraps POC packer; characterization tests compare bytes where feasible.

## Native library build {#native}

**refloat:**

- ARM GCC via `vesc_pkg_lib/rules.mk`
- Vendored `vesc_c_if.h` snapshot
- `conv.py` produces `.lisp` wrapper from `.bin`

**POC:**

- `thumbv7em-none-eabihf` Rust staticlib
- C shim in fixture `src/` with symbol audit gate
- Same `.bin` → package import path conceptually

**Gap:** Rust packages need `symbol_audit` and different link flags; refloat uses pure C examples.

**Mitigation:** POC `docs/abi-inventory.md` lists minimal 12-symbol surface; expand only as features require.

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

**Gap:** Knowledge index must tag refloat commands separately from firmware API search results.

## MCP resource layer {#mcp-resources}

**Implemented (`.1`):** URI schemes (`vesc://catalog/{kind}/{id}`, `vescpkg://fixture/{name}/manifest`, `vescpkg://manifest/{path}`), `ResourceRegistry`, and `ResourceReadHandler` trait in `crates/vesc-mcp-core/src/resources/`.

**Not yet wired:**

- `VescMcpService` exposes tools only; `resources/list` and `resources/read` rmcp handlers land in tasks `.2`–`.7`.
- Static catalog seeding (build recipes, doc topics, ABI JSON) is registry-ready but unpopulated at startup.
- Per-command URIs (`vesc://catalog/commands/refloat/{command}`) require either explicit registration per command or a URI template extension beyond the current `{kind}/{id}` parser.

**Path encoding:**

- Dynamic manifest URIs carry sandbox-relative paths as raw path segments (`vescpkg://manifest/tests/fixtures/...`).
- Absolute paths outside the repo need percent-encoding rules before production use (epic risk).

**Mitigation:** Task `.2` seeds build-recipe resources; `.3`–`.6` add manifest/doc handlers; `.7` snapshot-tests full `resources/list` output; defer subscriptions/caching to `br-mcp-resources-9at.9`.

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

## Recommended sequencing for vesc-mcp

1. **P0:** Catalog paths validated (`catalog_paths_exist` tests with env vars).
2. **P0:** Domain dialect parsing (parallel epic `br-domain-model-oli`).
3. **P1:** POC adapter for inspect/build; do not reimplement wire format.
4. **P1:** MCP resources from catalog YAML (`br-mcp-resources-9at`).
5. **P2:** Full command doc embedding in search index.

## References

- `catalog/refloat/build-flow.yaml`
- `catalog/refloat/lisp-loader.yaml`
- `catalog/bldc/vesc_c_if.yaml`
- `vesc-rust-poc/docs/package-flow.md`
- `vesc-rust-poc/docs/abi-inventory.md`
- `catalog/priorities.json`
