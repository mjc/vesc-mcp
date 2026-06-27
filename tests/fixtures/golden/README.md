# Wire format golden vectors

Deterministic `.vescpkg` bytes for offline domain tests (no live POC build in CI).

| File | Source |
|------|--------|
| `poc-minimal.vescpkg` | `tests/fixtures/poc-native-lib-minimal/` layout |
| `poc-minimal.sha256` | SHA-256 of `poc-minimal.vescpkg` |

## Regenerate

```bash
python3 scripts/gen_poc_minimal_golden.py
nix develop -c cargo nextest run -p vesc-domain
```
