# Wire format golden vectors

Deterministic `.vescpkg` bytes for offline domain tests. Golden files are **read-only fixtures** — regenerate only when the upstream `vesc_tool` packer or fixture layout intentionally changes.

| File | Source |
|------|--------|
| `poc-minimal.vescpkg` | `tests/fixtures/poc-native-lib-minimal/` layout, packed by `vesc_tool` |
| `poc-minimal.sha256` | SHA-256 of `poc-minimal.vescpkg` (`5148d649…`) |

## Regenerate

Requires a `vesc_tool` binary with `--buildPkgFromDesc` support (`VESC_TOOL_PATH` or on PATH):

```bash
export VESC_TOOL_PATH=/path/to/vesc_tool   # optional if vesc_tool is on PATH
cd tests/fixtures/poc-native-lib-minimal/package
"$VESC_TOOL_PATH" --buildPkgFromDesc pkgdesc.qml
cp poc-native-lib-minimal.vescpkg ../../golden/poc-minimal.vescpkg
shasum -a 256 ../../golden/poc-minimal.vescpkg \
  | awk '{print $1 "  poc-minimal.vescpkg"}' > ../../golden/poc-minimal.sha256
nix develop -c cargo nextest run -p vesc-domain -p vesc-mcp-core -E 'test(golden|build_poc)'
```

Alternatively, call MCP `build_vescpkg` on `tests/fixtures/poc-native-lib-minimal` and copy the artifact to `golden/poc-minimal.vescpkg`, then update the sidecar SHA-256.
