# Test Fixtures

Synthetic vescpkg workspace trees for vesc-mcp CI. All content is MIT-licensed synthetic data — no GPL code from refloat or bldc is vendored.

| Fixture | Purpose |
|---------|---------|
| `refloat-minimal/` | Valid vesc_tool-style pkgdesc + stub assets |
| `poc-native-lib-minimal/` | Valid POC native-lib baseline layout |
| `broken-missing-lisp/` | pkgdesc references absent lisp file |
| `broken-bad-magic/` | `.vescpkg` with invalid magic/header |
| `broken-bad-wire/` | Truncated `.vescpkg` bytes |
| `legacy-colon-desc/` | OLDVT colon-format `--buildPkg` descriptor string |

Total fixture size is kept under 50KB.
