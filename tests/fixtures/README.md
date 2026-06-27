# Test Fixtures

Synthetic vescpkg workspace trees for vesc-mcp CI. All content is MIT-licensed synthetic data — no GPL code from refloat or bldc is vendored.

| Fixture | Purpose |
|---------|---------|
| `refloat-minimal/` | Valid vesc_tool-style pkgdesc + stub assets |
| `poc-native-lib-minimal/` | Valid vesc_tool-style pkgdesc for native-lib package layout |
| `broken-missing-lisp/` | pkgdesc references absent lisp file |
| `broken-missing-pkgdesc/` | package root with no pkgdesc.qml |
| `broken-bad-magic/` | `.vescpkg` with invalid magic/header |
| `broken-bad-wire/` | Truncated `.vescpkg` bytes |
| `legacy-colon-desc/` | OLDVT colon-format `--buildPkg` descriptor string |
| `golden/` | Deterministic `.vescpkg` wire bytes for offline tests |

Total fixture size is kept under 50KB.
