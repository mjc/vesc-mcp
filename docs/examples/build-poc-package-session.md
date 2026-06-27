# Example agent session: build fixture package (offline)

Walkthrough for `build_vescpkg` in **rust** mode on `tests/fixtures/poc-native-lib-minimal/` — an **MCP/CI parity path only**. Production refloat packages use `vesc_tool --buildPkgFromDesc` (see [inspect-refloat-session.md](inspect-refloat-session.md) and `vesc://catalog/build-recipe/refloat-vesc-tool`).

**Prerequisites**

| Requirement | Notes |
|-------------|-------|
| `VESC_PACKAGE_ROOTS` | Must include `tests/fixtures/` (or the fixture parent path). |
| Build toolchain | `nix develop -c make check` in vesc-mcp. No sibling repos required. |

The fixture uses nested layout `package/pkgdesc.qml` (vesc_tool dialect). Adapters resolve `lisp_editor_path` to the package root — see [poc-integration.md](../poc-integration.md).

When `VESC_TOOL_PATH` is set, prefer `mode: "vesc_tool"` for the same layout using the official packer.

---

## Prompt 1 — validate before build

> Validate layout for `tests/fixtures/poc-native-lib-minimal` before building.

**Tool call** (`validate_package_layout`)

```json
{
  "root": "tests/fixtures/poc-native-lib-minimal"
}
```

**Expected response**

```json
{
  "ok": true
}
```

---

## Prompt 2 — build with parity writer (fixtures)

> Build a `.vescpkg` for the native-lib minimal fixture using `build_vescpkg` with `mode: "rust"`.

**Tool call**

```json
{
  "root": "tests/fixtures/poc-native-lib-minimal",
  "mode": "rust",
  "timeout_secs": 120
}
```

**Expected response**

```json
{
  "ok": true,
  "artifact_path": "/…/tests/fixtures/poc-native-lib-minimal/package/poc-native-lib-minimal.vescpkg",
  "sha256": "5148d649a6da7abb8deb5a4bdca38f9fe7bd1b9d918f9e06001e0f20e2cedba9",
  "size_bytes": 406
}
```

The SHA-256 must match the golden vector in `tests/fixtures/golden/poc-minimal.sha256`.

On layout or I/O failure the tool returns `{ "ok": false, "error": { "code": "…", "message": "…", "hint": "…" } }`.

---

## Prompt 3 — inspect wire artifact

> Run `inspect_vescpkg` on the artifact from the build step (or on the committed golden file `tests/fixtures/golden/poc-minimal.vescpkg`).

**Tool call**

```json
{
  "path": "tests/fixtures/golden/poc-minimal.vescpkg"
}
```

**Expected response**

```json
{
  "ok": true,
  "inspection": {
    "name": "POC native-lib minimal fixture",
    "lisp_import_count": 1,
    "lisp_editor_path": "package-lib"
  }
}
```

Additional wire fields may appear as the inspector grows; the fields above are asserted in `tool_inspect_vescpkg_reads_name`.

---

## Prompt 4 — compare manifest resource (optional)

> Fetch `vescpkg://fixture/poc-native-lib-minimal/manifest` and confirm pkgdesc fields before build.

**Resource URI**

```
vescpkg://fixture/poc-native-lib-minimal/manifest
```

**Expected `parsed` excerpt**

```json
{
  "pkg_name": "POC native-lib minimal fixture",
  "output_name": "poc-native-lib-minimal.vescpkg",
  "description_md_path": "README.md",
  "lisp_path": "code.lisp",
  "qml_path": "",
  "qml_is_fullscreen": false
}
```

---

## Environment reference

```bash
export VESC_PACKAGE_ROOTS="$PWD/tests/fixtures"
# Optional — use official packer instead of rust parity mode:
# export VESC_TOOL_PATH=/path/to/vesc_tool
```

Build recipe resources:

- Production (refloat): `vesc://catalog/build-recipe/refloat-vesc-tool`
- Fixture parity (CI): `vesc://catalog/build-recipe/poc-rust-packer`

---

## Related docs

- [inspect-refloat-session.md](inspect-refloat-session.md) — discovery and layout checks with vesc_tool workflow
- [poc-integration.md](../poc-integration.md) — parity writer vs production vesc_tool
- [configuration.md](../configuration.md) — `VESC_PACKAGE_ROOTS`, `VESC_TOOL_PATH`
