# Build a sample native-library package

Walkthrough for `build_vescpkg` on `tests/fixtures/native-lib-minimal/` using the official `vesc_tool` packer. Production refloat packages follow the same `--buildPkgFromDesc` path (see [inspect-refloat-session.md](inspect-refloat-session.md) and `vesc://catalog/build-recipe/refloat-vesc-tool`).

**Prerequisites**

| Requirement | Notes |
|-------------|-------|
| `VESC_PACKAGE_ROOTS` | Must include `tests/fixtures/` (or the fixture parent path). |
| `VESC_TOOL_PATH` | Path to a `vesc_tool` binary with `--buildPkgFromDesc` support (or `vesc_tool` on PATH). |

The fixture uses nested layout `package/pkgdesc.qml` (vesc_tool dialect). `build_vescpkg` resolves the descriptor via `locate_pkgdesc` and runs `vesc_tool` with the package directory as cwd.

---

## Prompt 1 — validate before build

> Validate layout for `tests/fixtures/native-lib-minimal` before building.

**Tool call** (`validate_package_layout`)

```json
{
  "root": "tests/fixtures/native-lib-minimal"
}
```

**Expected response**

```json
{
  "ok": true
}
```

---

## Prompt 2 — build with vesc_tool

> Build a `.vescpkg` for the native-lib minimal fixture using `build_vescpkg`.

**Tool call**

```json
{
  "root": "tests/fixtures/native-lib-minimal",
  "timeout_secs": 120
}
```

**Expected response**

```json
{
  "ok": true,
  "artifact_path": "/…/tests/fixtures/native-lib-minimal/package/native-lib-minimal.vescpkg",
  "sha256": "<64-character SHA-256>",
  "size_bytes": 406
}
```

The byte count shown is from the committed golden example. A source change can
change the digest or size; compare against the sidecar checksum only when
verifying or intentionally regenerating that golden vector.

On layout, missing `vesc_tool`, or I/O failure the tool returns `{ "ok": false, "error": { "code": "…", "message": "…", "hint": "…" } }`.

---

## Prompt 3 — inspect wire artifact

> Run `inspect_vescpkg` on the artifact from the build step (or on the committed golden file `tests/fixtures/golden/native-lib-minimal.vescpkg`).

**Tool call**

```json
{
  "path": "tests/fixtures/golden/native-lib-minimal.vescpkg"
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

> Fetch `vescpkg://fixture/native-lib-minimal/manifest` and confirm pkgdesc fields before build.

**Resource URI**

```
vescpkg://fixture/native-lib-minimal/manifest
```

**Expected `parsed` excerpt**

```json
{
  "pkg_name": "native-lib minimal fixture",
  "output_name": "native-lib-minimal.vescpkg",
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
export VESC_TOOL_PATH=/path/to/vesc_tool   # or ensure vesc_tool is on PATH
```

Build recipe resource:

- Production (refloat): `vesc://catalog/build-recipe/refloat-vesc-tool`

---

## Related docs

- [inspect-refloat-session.md](inspect-refloat-session.md) — discovery and layout checks with vesc_tool workflow
- [configuration.md](../configuration.md) — `VESC_PACKAGE_ROOTS`, `VESC_TOOL_PATH`
- [tests/fixtures/golden/README.md](../../tests/fixtures/golden/README.md) — regenerate committed golden bytes
