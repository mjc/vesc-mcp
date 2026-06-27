# lispData import table

The `lispData` field in a `.vescpkg` wire payload embeds the package Lisp loader source plus a binary import table for native payloads (`.bin` libraries, assets, etc.). `vesc-domain::parse_lisp_imports` decodes the layout shared with `vesc_tool` and the Rust POC packer.

## Wire layout

After decompression, `lispData` is a binary blob:

1. **Header** — big-endian `i16` `0`
2. **Code** — length-prefixed UTF-8 Lisp source (the loader script)
3. **Import count** — big-endian `i16` ≥ 0
4. **Import entries** — repeated `import_count` times:
   - **Tag** — length-prefixed UTF-8 symbol (e.g. `package-lib`)
   - **Offset** — big-endian `i32` byte offset from the start of `lispData` (must be 4-byte aligned in POC builds)
   - **Size** — big-endian `i32` payload byte length
5. **Embedded payloads** — raw bytes at each `(offset, size)` range inside `lispData`

Typical loader code imports a native library and registers it:

```lisp
(import "src/package_lib.bin" 'package-lib)
(load-native-lib package-lib)
```

The `(import … 'tag)` form binds `tag` to the embedded bytes at the recorded offset/size. POC characterization tests assert that trailing NUL padding after the native blob is allowed.

## Field spine context

`lispData` is the third key in the vesc_tool field spine (`name`, `description_md`, `lispData`, `qmlFile`, `pkgDescQml`, `qmlIsFullscreen`). Empty text fields may be omitted from the on-disk package.
