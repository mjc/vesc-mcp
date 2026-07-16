# `.vescpkg` wire format

Byte-level specification for VESC custom package wire artifacts. Canonical packing behavior is in `vesc_tool` `codeloader.cpp`; `vesc-domain::wire` implements the read-side parser and tests.

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

`tests/fixtures/golden/native-lib-minimal.vescpkg` decodes to keys:

```
name, description_md, lispData, pkgDescQml, qmlIsFullscreen
```

Empty `qmlFile` is omitted — not written as a zero-length field. Test: `package_fields_follow_vesc_tool_spine`.

Golden stability tests:

- `vesc-domain::wire`: golden round-trip and field-spine tests on `tests/fixtures/golden/native-lib-minimal.vescpkg`
- `vesc-mcp-core`: optional `tool_build_native_lib_minimal_matches_golden_when_vesc_tool_available` when `VESC_TOOL_PATH` is set

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

Reference: `vesc-domain::parse_lisp_imports`; authoritative writer is `vesc_tool` `lispPackImports` in `codeloader.cpp`.

### Payload placement rule (critical)

Embedded file bytes live **inside the same `lispData` buffer**.

- Import `offset` is measured from the **start of lispData** (byte 0).
- Payload slice for decoding uses: `start = 2 + offset`, `end = start + size`.
- The `+2` skips the leading i16 header when locating payload bytes.
- Payloads are **4-byte aligned** in POC/vesc_tool builds; trailing NUL padding after native `.bin` content is allowed.

Example from golden fixture (`tests/fixtures/golden/native-lib-minimal.vescpkg`):

- Native bytes: `[0, 1, 2, 3, 0xff]` (5 bytes)
- Stored size: **6** (one trailing NUL pad)
- Import offset: **100** (4-byte aligned)
- Tag: `package-lib`

Validation helper: `payload_matches_native_with_only_nul_tail` — true when payload equals native bytes followed by zero padding only.

### Lisp source side

Typical loader (`tests/fixtures/native-lib-minimal/package/code.lisp`):

```lisp
(import "src/package_lib.bin" 'package-lib)
(load-native-lib package-lib)
```

| Concept | Rule |
|---------|------|
| Import path | Resolved by `vesc_tool` relative to lisp file directory (`codeloader.cpp`) |
| Tag (`package-lib`) | Becomes the import table entry name; argument to `(load-native-lib …)` |
| `(load-native-lib …)` | Firmware extension — see [vesc-pkg-lib-abi.md](vesc-pkg-lib-abi.md) |

Packer algorithm (`vesc_tool` `lispPackImports` in `codeloader.cpp`):

1. Write header `i16` 0 + null-terminated source.
2. Scan source lines for `(import "path" 'tag)`.
3. Read each path relative to the lisp file directory; append trailing NUL to file bytes if missing.
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

**Authoritative:** `vesc_tool` / `codeloader.cpp`. Refloat packages are built with `--buildPkgFromDesc`.

| Aspect | vesc_tool |
|--------|-----------|
| Entry | `--buildPkgFromDesc pkgdesc.qml` |
| Descriptor | Reads QML properties live |
| Native embed | `lispPackImports(…, editorPath, …)` |
| Compression | Qt `qCompress` |
| Legacy | `--buildPkg` colon format (`OLDVT=1`) |

In-repo reader: `crates/vesc-domain/src/wire/mod.rs`.

Set `$VESC_VESC_TOOL_ROOT` per [configuration.md](configuration.md).

## Golden stability strategy

1. **Golden vector** — `tests/fixtures/golden/native-lib-minimal.vescpkg` SHA-256: `5148d649a6da7abb8deb5a4bdca38f9fe7bd1b9d918f9e06001e0f20e2cedba9`
2. **Field spine** — packages must expose keys in `FIELD_SPINE` order when all fields present
3. **Import geometry** — offset/size/payload round-trip via `parse_lisp_imports`
4. **Optional live build** — when `VESC_TOOL_PATH` is set, `build_vescpkg` + golden-stability tests compare against golden

Regenerate golden with `vesc_tool` — see `tests/fixtures/golden/README.md`.

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

## Appendix — annotated golden hex walkthrough

Hand-decode anchor: `tests/fixtures/golden/native-lib-minimal.vescpkg` (406 bytes on disk, SHA-256 `5148d649…`). Produced by `vesc_tool --buildPkgFromDesc` from `tests/fixtures/native-lib-minimal/package/pkgdesc.qml`.

### Outer container (bytes 0–405)

| File offset | Bytes (hex) | Meaning |
|-------------|-------------|---------|
| `0x0000`–`0x0003` | `00 00 02 eb` | qCompress declared decompressed length = **747** (0x2eb) |
| `0x0004`–`0x0195` | `78 da …` | zlib deflate stream (402 bytes); inflates to exactly 747 bytes |

Verify: `declared == len(zlib.decompress(raw[4:]))` — mismatch is a hard parse error.

### Decompressed payload field map

After inflate, offsets are **within the 747-byte payload** (not the on-disk file):

| Decomp offset | Region | Content |
|---------------|--------|---------|
| `0x0000`–`0x000b` | magic | `56 45 53 43 20 50 61 63 6b 65 74 00` → `"VESC Packet\0"` |
| `0x000c`–`0x0032` | field `name` | key + `i32 BE 0x0000001e` + 30 UTF-8 bytes → `POC native-lib minimal fixture` |
| `0x0033`–`0x00bf` | field `description_md` | key + `i32 BE 0x0000007a` (122) + README markdown body |
| `0x00c0`–`0x0184` | field `lispData` | key + `i32 BE 0x000000b8` (184) + binary blob (see below) |
| `0x0185`–`0x02d5` | field `pkgDescQml` | key + `i32 BE 0x00000142` (322) + original pkgdesc.qml text |
| `0x02d6`–`0x02ea` | field `qmlIsFullscreen` | key + `i32 BE 0x00000001` + single byte `0x00` (false) |

**Omitted field:** `qmlFile` is absent (empty QML path in fixture pkgdesc) — not written as a zero-length placeholder. Spine ends with `qmlIsFullscreen`.

### `lispData` interior (184 bytes at decomp `0x00cd`)

```
+0   i16 BE 00 00          header (must be 0)
+2   cstring               Lisp source (139 chars + NUL)
     … "(import \"src/package_lib.bin\" 'package-lib)\n(load-native-lib package-lib)\n"
+141 i16 BE 00 01          import_count = 1
+143 cstring "package-lib" tag + NUL
+155 i32 BE 00 00 00 a4     offset = 164 (from start of lispData)
+159 i32 BE 00 00 00 12     size = 18 (17 payload bytes + 1 NUL pad)
+163 [align pad to 4 bytes from byte +2]
+166 bytes                  embedded native payload
     50 4f 43 5f 46 49 58 54 55 52 45 5f 53 54 55 42 0a 00
     → ASCII "POC_FIXTURE_STUB\n" + NUL pad
```

**Geometry check:** payload slice uses `start = 2 + offset = 166`, `end = start + size = 184` — equals `lispData.len()`. Tag `package-lib` matches `(load-native-lib package-lib)` in source.

Representative hex (lispData only):

```
+000: 00 00 3b 20 4d 69 6e 69 6d 61 6c 20 50 4f 43 20  ..; Minimal POC
+128: 70 61 63 6b 61 67 65 2d 6c 69 62 29 0a 00 00 01  package-lib)....
+144: 70 61 63 6b 61 67 65 2d 6c 69 62 00 00 00 00 a4  package-lib.....
+160: 00 00 00 12 00 00 50 4f 43 5f 46 49 58 54 55 52  ......POC_FIXTUR
+176: 45 5f 53 54 55 42 0a 00                          E_STUB..
```

### Decode checklist

1. Read BE u32 at file start → expected **747**.
2. zlib-decompress remainder → length must match.
3. Pop `"VESC Packet\0"` magic.
4. Loop key\0 + i32 len + value until EOF.
5. For `lispData`: verify header 0, parse source cstring, read import table, confirm `2 + offset + size ≤ lispData.len()`.
6. Confirm golden SHA-256 after any packer change.

Offline tools: MCP `inspect_vescpkg` on this path; `vesc-domain` wire tests; regenerate via [tests/fixtures/golden/README.md](../tests/fixtures/golden/README.md).

## Related documents

- Master index: [vescpackage-reference.md](vescpackage-reference.md)
- Native load path: [vesc-pkg-lib-abi.md](vesc-pkg-lib-abi.md)
- MCP snippet: resource `vesc://catalog/doc/topic/lisp_imports`
