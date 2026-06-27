# `.vescpkg` wire format

Byte-level specification for VESC custom package wire artifacts. Canonical behavior is implemented in `vesc-domain::wire` (reader) and mirrored by `vesc-pkg-build::package_format` (writer) and `vesc_tool` `codeloader.cpp` (reference writer).

## File anatomy

```
┌─────────────────────────────────────────┐
│ qCompress wrapper (on disk)             │
│   u32 BE  declared_decompressed_length  │
│   zlib    compressed payload            │
└─────────────────────────────────────────┘
                    │ decompress
                    ▼
┌─────────────────────────────────────────┐
│ Decompressed payload                    │
│   cstring  magic = "VESC Packet\0"      │
│   repeat until EOF:                     │
│     cstring  field_key\0                │
│     i32 BE   value_len (≥ 0)            │
│     bytes    value[value_len]           │
└─────────────────────────────────────────┘
```

Within `lispData` values, a nested binary layout embeds Lisp source and native payloads — see [lispData structure](#lispdata-binary-structure).

## Outer container (Qt qCompress)

| Offset | Type | Field |
|--------|------|-------|
| 0 | u32 BE | Declared decompressed length |
| 4 | zlib | Compressed bytes (flate2-compatible) |

Reference: `vesc-domain::decompress_vescpkg`.

Rules:

- File must be ≥ 4 bytes or parsing fails with *"package shorter than qCompress length prefix"*.
- After zlib inflate, actual length must equal declared length or parsing fails with a length mismatch error.
- Truncated zlib streams fail with *"zlib decompress failed"*.

## Decompressed payload layout

After decompression, the payload is sequential:

```
[ magic: null-terminated UTF-8 "VESC Packet" ]
[ repeat until buffer exhausted: ]
    key:   null-terminated UTF-8 string
    len:   big-endian i32 byte count (must be ≥ 0)
    value: exactly `len` raw bytes
```

Properties:

- **No explicit field count** — parser reads until the buffer is exhausted.
- **Strings** in text-valued slots are UTF-8 **without** a trailing NUL unless the packer added one inside the value bytes.
- **Bool** `qmlIsFullscreen`: value length 1, byte `0` = false, any non-zero = true (`optional_bool_field`).

Magic mismatch yields *"expected magic \"VESC Packet\", got …"*.

## vesc_tool field spine (canonical order)

Wire keys in pack order (`vesc-domain::FIELD_SPINE`):

| Order | Wire key | Semantics | Required on wire? |
|-------|----------|-----------|-------------------|
| 1 | `name` | Package display name (`pkgName`) | yes |
| 2 | `description_md` | README / markdown body | optional (may omit if empty) |
| 3 | `lispData` | Lisp source + import table + embedded binaries | optional |
| 4 | `qmlFile` | UI QML source text | optional (may omit if empty) |
| 5 | `pkgDescQml` | Original `pkgdesc.qml` text (round-trip) | optional |
| 6 | `qmlIsFullscreen` | Single-byte bool | optional (default false) |

**`pkgOutput` is not a wire field** — it only names the output filename during build.

### Golden fixture spine

`tests/fixtures/golden/poc-minimal.vescpkg` decodes to keys:

```
name, description_md, lispData, pkgDescQml, qmlIsFullscreen
```

Empty `qmlFile` is omitted — not written as a zero-length field. Test: `package_fields_follow_vesc_tool_spine`.

Parity tests:

- `vesc-pkg-build`: `package_uses_the_vesc_tool_field_spine`
- `vesc-mcp-adapters`: `characterization_package_uses_vesc_tool_field_spine`

## lispData binary structure

The `lispData` wire value is a self-contained binary blob:

```
Offset  Type        Field
------  ----------  -----
+0      i16 BE      header (must be 0)
+2      cstring     Lisp source code (null-terminated)
        i16 BE      import_count (≥ 0)
        repeat import_count times:
          cstring   import[i].tag (null-terminated symbol name)
          i32 BE    import[i].offset
          i32 BE    import[i].size
        [padding to 4-byte align payload region]
        bytes       embedded payloads at recorded offsets
```

Reference: `vesc-domain::parse_lisp_imports`, `vesc-pkg-build::pack_lisp_imports`.

### Payload placement rule (critical)

Embedded file bytes live **inside the same `lispData` buffer**.

- Import `offset` is measured from the **start of lispData** (byte 0).
- Payload slice for decoding uses: `start = 2 + offset`, `end = start + size`.
- The `+2` skips the leading i16 header when locating payload bytes.
- Payloads are **4-byte aligned** in POC/vesc_tool builds; trailing NUL padding after native `.bin` content is allowed.

Example from characterization test (`characterization_lisp_imports_embed_native_payload_bytes`):

- Native bytes: `[0, 1, 2, 3, 0xff]` (5 bytes)
- Stored size: **6** (one trailing NUL pad)
- Import offset: **100** (4-byte aligned)
- Tag: `package-lib`

Validation helper: `payload_matches_native_with_only_nul_tail` — true when payload equals native bytes followed by zero padding only.

### Lisp source side

Typical loader (`tests/fixtures/poc-native-lib-minimal/package/code.lisp`):

```lisp
(import "src/package_lib.bin" 'package-lib)
(load-native-lib package-lib)
```

| Concept | Rule |
|---------|------|
| Import path | Resolved relative to **`lisp_editor_path`** (package root), not the `.lisp` file directory |
| Tag (`package-lib`) | Becomes the import table entry name; argument to `(load-native-lib …)` |
| `(load-native-lib …)` | Firmware extension — see [vesc-pkg-lib-abi.md](vesc-pkg-lib-abi.md) |

Packer algorithm (`pack_lisp_imports`):

1. Write header `i16` 0 + null-terminated source.
2. Scan source lines for `(import "path" 'tag)` via `parse_import_line`.
3. Read each path relative to `lisp_editor_path`; append trailing NUL to file bytes if missing.
4. Write import count, then each tag/offset/size; align and append payload bytes.

## Wire format failure taxonomy

All wire parse failures surface as `DomainError::InvalidWireFormat { message }`. MCP tools return this in JSON error payloads.

| Symptom / message | Cause | Fixture |
|-------------------|-------|---------|
| shorter than qCompress length prefix | Truncated file (< 4 bytes) | `broken-bad-wire/truncated.vescpkg` |
| zlib decompress failed | Corrupt compressed stream | truncated fixture |
| decompressed length N does not match declared M | Length prefix lie | crafted / truncated |
| expected magic "VESC Packet" | Bad magic after decompress | `broken-bad-magic/bad-magic.vescpkg` |
| missing required field name | Spine missing `name` | malformed craft |
| field key has negative length | corrupt length i32 | malformed craft |
| field key length N exceeds remaining bytes | overrun | truncated inner payload |
| unexpected lispData header H (H ≠ 0) | bad lispData prefix | malformed craft |
| negative Lisp import count | corrupt import table | malformed craft |
| import tag has negative offset or size | corrupt entry | malformed craft |
| import tag range [start, end) exceeds lispData length | offset/size lie | malformed craft |
| field key is not valid UTF-8 | binary in text slot | malformed craft |

Integration tests: `tool_inspect_vescpkg_rejects_bad_magic`, `tool_inspect_vescpkg_rejects_truncated_wire`, `extract_vescpkg_rejects_*` in `vesc-domain`.

## Packer implementations

| Aspect | vesc_tool | vesc-pkg-build (POC) |
|--------|-----------|----------------------|
| Entry | `--buildPkgFromDesc pkgdesc.qml` | `build_vesc_package(&VescPackageInput)` |
| Descriptor | Reads QML properties live | Reads staged files + embeds pkgdesc text |
| Native embed | `lispPackImports(…, editorPath, …)` | `pack_lisp_imports(code, editor_path)` |
| Compression | Qt `qCompress` | `q_compress` (same layout) |
| Legacy | `--buildPkg` colon format (`OLDVT=1`) | Not supported |

### Upstream citations

| Repo | Path | Lines | Content |
|------|------|-------|---------|
| vesc_tool | `codeloader.cpp` | 817–864 | `packVescPackage` — magic + fields |
| vesc_tool | `codeloader.cpp` | 879–916 | Unpack mirror |
| vesc_tool | `codeloader.cpp` | 173–252 | `lispPackImports` |
| vesc_tool | `codeloader.cpp` | 1174–1252 | pkgdesc QML property reads |
| vesc-rust-poc | `crates/vesc-pkg-build/src/package_format.rs` | 20–37 | `build_vesc_package` spine |
| vesc-rust-poc | `crates/vesc-pkg-build/src/package_format.rs` | 111–180 | `pack_lisp_imports` |
| vesc-mcp | `crates/vesc-domain/src/wire/mod.rs` | full module | Reader + tests |

Set `$VESC_POC_ROOT`, `$VESC_TOOL_ROOT` (or sibling checkout paths) per [configuration.md](configuration.md).

## Parity strategy

1. **Golden vector** — `tests/fixtures/golden/poc-minimal.vescpkg` SHA-256: `34e95e3628a810efc9bfd3cdf23d80dea193f9a11c65b4a5da6f8a23163b7207`
2. **Characterization** — `characterization_matches_golden_sha256` builds from `poc-native-lib-minimal` fixture
3. **Field spine** — built packages must expose keys in `FIELD_SPINE` order when all fields present
4. **Import geometry** — offset/size/payload round-trip via `parse_lisp_imports`

Regenerate golden after POC packer changes:

```bash
nix develop -c cargo run -p vesc-mcp-adapters --bin gen-poc-minimal-golden
```

## Import table geometry (diagram)

```
lispData[0..1]     i16 header = 0
lispData[2..]      null-terminated Lisp source
                   i16 import_count
                   ┌─ tag₀ \0  offset₀(i32)  size₀(i32)
                   ├─ tag₁ \0  offset₁(i32)  size₁(i32)
                   └─ ...
                   [align to 4 bytes from byte 2]
lispData[2+offset₀]  payload₀ (size₀ bytes, may include NUL pad)
```

When hand-decoding: always verify `2 + offset + size ≤ lispData.len()`.

## Related documents

- Master index: [vescpackage-reference.md](vescpackage-reference.md)
- Native load path: [vesc-pkg-lib-abi.md](vesc-pkg-lib-abi.md)
- MCP snippet: resource `vesc://catalog/doc/topic/lisp_imports`
