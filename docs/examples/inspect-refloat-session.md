# Example agent session: inspect refloat-minimal

Copy-paste prompts for an MCP client connected to `vesc-mcp-server`. All paths use the in-repo fixture `tests/fixtures/refloat-minimal/` — no live refloat checkout required.

**Prerequisites:** `VESC_PACKAGE_ROOTS` includes the vesc-mcp repo `tests/fixtures/` directory (automatic in CI via `test-fixtures`; for a local MCP session set `VESC_PACKAGE_ROOTS` to your clone's `tests/fixtures` path).

See [AGENTS.md](../../AGENTS.md) for the full tool cheat sheet.

---

## Prompt 1 — discover packages

> List VESC packages under `tests/fixtures/refloat-minimal` using `list_vesc_packages`.

**Tool call**

```json
{
  "roots": ["tests/fixtures/refloat-minimal"]
}
```

**Expected response** (paths vary by machine; shape is stable):

```json
{
  "ok": true,
  "packages": [
    {
      "root": "/…/vesc-mcp/tests/fixtures/refloat-minimal",
      "pkgdesc_path": "/…/vesc-mcp/tests/fixtures/refloat-minimal/pkgdesc.qml",
      "dialect": "vesc_tool"
    }
  ]
}
```

---

## Prompt 2 — parse pkgdesc

> Inspect the pkgdesc at `tests/fixtures/refloat-minimal/pkgdesc.qml` with `inspect_pkgdesc`.

**Tool call**

```json
{
  "path": "tests/fixtures/refloat-minimal/pkgdesc.qml"
}
```

**Expected response**

```json
{
  "ok": true,
  "dialect": "vesc_tool",
  "parsed": {
    "pkg_name": "Refloat Minimal",
    "description_md_path": "package_README-gen.md",
    "lisp_path": "lisp/package.lisp",
    "qml_path": "ui.qml",
    "output_name": "refloat-minimal.vescpkg",
    "qml_is_fullscreen": false
  }
}
```

---

## Prompt 3 — validate on-disk layout

> Run `validate_package_layout` on the refloat-minimal fixture root and confirm all referenced assets exist.

**Tool call**

```json
{
  "root": "tests/fixtures/refloat-minimal"
}
```

**Expected response**

```json
{
  "ok": true
}
```

When assets are missing, `ok` is `false` and an `issues` array lists each problem (see `tests/fixtures/broken-missing-lisp/` in integration tests).

---

## Prompt 4 — fetch fixture manifest resource

> Read MCP resource `vescpkg://fixture/refloat-minimal/manifest` and compare it to the `inspect_pkgdesc` output.

**Resource URI**

```
vescpkg://fixture/refloat-minimal/manifest
```

**Expected body** (JSON block before the `---` attribution footer):

```json
{
  "ok": true,
  "dialect": "vesc_tool",
  "parsed": {
    "pkg_name": "Refloat Minimal",
    "description_md_path": "package_README-gen.md",
    "lisp_path": "lisp/package.lisp",
    "qml_path": "ui.qml",
    "output_name": "refloat-minimal.vescpkg",
    "qml_is_fullscreen": false
  },
  "raw_qml": "import QtQuick 2.15\n\nItem {\n    property string pkgName: \"Refloat Minimal\"\n    …"
}
```

The footer attributes the source file under `tests/fixtures/refloat-minimal/pkgdesc.qml`. The `parsed` object must match `inspect_pkgdesc` on the same file (`resource_manifest_matches_tool_output` in CI).

---

## Next steps

- Build recipe: `vesc://catalog/build-recipe/refloat-vesc-tool`
- Build the native-lib fixture: [build-native-lib-package-session.md](build-native-lib-package-session.md)
- Safety rules for device operations: [../safety.md](../safety.md)
