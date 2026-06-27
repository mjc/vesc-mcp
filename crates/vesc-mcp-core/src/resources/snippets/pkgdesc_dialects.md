# pkgdesc.qml dialects

VESC packages ship a Qt Quick descriptor (`pkgdesc.qml`) that `vesc_tool` reads when building a `.vescpkg`. Two property naming schemes appear in the wild; only one is authoritative for modern tooling.

## vesc_tool schema (canonical)

Used by **refloat** and current **vesc-rust-poc** fixtures. Required properties:

| Property | Role |
|----------|------|
| `pkgName` | Display / package name |
| `pkgDescriptionMd` | Path to markdown readme |
| `pkgLisp` | Path to loader Lisp source |
| `pkgQml` | Path to UI QML |
| `pkgQmlIsFullscreen` | Full-screen UI flag |
| `pkgOutput` | Output `.vescpkg` filename |

Optional JavaScript guards such as `isCompatible(fwRxParams)` are preserved in the wire `pkgDescQml` field but are not parsed by `vesc-domain`.

## Legacy POC schema (rejected)

Early **vesc-rust-poc** experiments used invented keys that `vesc_tool` never reads (`codeloader.cpp` only loads the vesc_tool properties above):

| Property | Notes |
|----------|-------|
| `packageName` | Non-authoritative alias |
| `packageVersion` | Not consumed by vesc_tool |
| `nativeLibraryPath` | Use `pkgLisp` import table instead |
| `loaderScriptPath` | Use `pkgLisp` instead |

`vesc-domain::parse_pkgdesc_qml` returns `DomainError::LegacyPocDialect` when any legacy POC-only field is present. Migrate packages to the vesc_tool schema before building with `vesc_tool --buildPkgFromDesc`.

## Related build modes

Refloat also supports `OLDVT=1` colon-format `--buildPkg` strings for legacy vesc_tool versions; that is a separate wire/build path from pkgdesc.qml dialects. See `catalog/refloat/build-flow.yaml` for Makefile targets.
