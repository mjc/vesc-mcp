# Example agent session: build POC native-lib package

Walkthrough for `build_vescpkg` in **rust** mode on `tests/fixtures/poc-native-lib-minimal/`, then `inspect_vescpkg` on the wire artifact.

**Prerequisites**

| Requirement | Notes |
|-------------|-------|
| `VESC_PACKAGE_ROOTS` | Must include `tests/fixtures/` (or the fixture parent path). |
| `VESC_POC_ROOT` | Sibling checkout of **vesc-rust-poc** (default `~/projects/vesc-rust-poc`). The Rust packer is a path dependency on `vesc-pkg-build` — see [poc-integration.md](../poc-integration.md). |
| Build toolchain | `nix develop -c make check` in vesc-mcp; POC checkout must build if you change packer code. |

The fixture uses nested layout `package/pkgdesc.qml` (vesc_tool dialect). Adapters resolve `lisp_editor_path` to the package root — see sharp edges in [poc-integration.md](../poc-integration.md).

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

## Prompt 2 — build with Rust packer

> Build a `.vescpkg` for the POC native-lib minimal fixture using `build_vescpkg` with `mode: "rust"`.

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
  "sha256": "34e95e3628a810efc9bfd3cdf23d80dea193f9a11c65b4a5da6f8a23163b7207",
  "size_bytes": 512
}
```

The SHA-256 must match the golden vector in `tests/fixtures/golden/poc-minimal.sha256`. `size_bytes` is illustrative — only the hash is pinned in CI.

If `VESC_POC_ROOT` is missing or `vesc-pkg-build` fails to link, the tool returns `{ "ok": false, "error": { "code": "…", "message": "…", "hint": "…" } }`.

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
export VESC_POC_ROOT="$HOME/projects/vesc-rust-poc"   # or vendor/sibling path
```

Build recipe resource: `vesc://catalog/build-recipe/poc-rust-packer`

---

## Related docs

- [inspect-refloat-session.md](inspect-refloat-session.md) — discovery and layout checks without POC
- [poc-integration.md](../poc-integration.md) — path dependency and sharp edges
- [configuration.md](../configuration.md) — `VESC_POC_ROOT`, `VESC_PACKAGE_ROOTS`
