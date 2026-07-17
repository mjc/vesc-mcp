# Wire format golden vectors

Deterministic `.vescpkg` bytes for offline domain tests. Golden files are **read-only fixtures** — regenerate only when the upstream `vesc_tool` packer or fixture layout intentionally changes.

| File | Source |
|------|--------|
| `native-lib-minimal.vescpkg` | `tests/fixtures/native-lib-minimal/` layout, packed by `vesc_tool` |
| `native-lib-minimal.sha256` | SHA-256 of `native-lib-minimal.vescpkg` (`5148d649…`) |

## Regenerate

Requires a `vesc_tool` binary with `--buildPkgFromDesc` support (`VESC_TOOL_PATH` or on PATH):

```bash
export VESC_TOOL_PATH=/path/to/vesc_tool   # optional if vesc_tool is on PATH
cd tests/fixtures/native-lib-minimal/package
"$VESC_TOOL_PATH" --buildPkgFromDesc pkgdesc.qml
cp native-lib-minimal.vescpkg ../../golden/native-lib-minimal.vescpkg
shasum -a 256 ../../golden/native-lib-minimal.vescpkg \
  | awk '{print $1 "  native-lib-minimal.vescpkg"}' > ../../golden/native-lib-minimal.sha256
cargo nextest run -p vesc-domain -p vesc-mcp-core -E 'test(golden|build_native_lib)'
```

Alternatively, call MCP `build_vescpkg` on `tests/fixtures/native-lib-minimal` and copy the artifact to `golden/native-lib-minimal.vescpkg`, then update the sidecar SHA-256.
