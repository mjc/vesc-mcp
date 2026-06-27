# Safety: flash, upload, and device access

vesc-mcp defaults to **read-only, sandboxed** package tooling. Operations that write firmware or packages to hardware are gated and deferred.

See also the summary in [AGENTS.md](../AGENTS.md#safety-rules).

---

## Flash and upload tools are off by default

Flash and upload MCP tools are **not registered** unless explicitly enabled:

| Enable via | Value |
|------------|-------|
| Environment | `VESC_MCP_ENABLE_FLASH=1` (also `true`, `yes`, `on`) |
| Config file | `[features] enable_flash = true` in `~/.config/vesc-mcp/config.toml` |

Precedence and full env reference: [configuration.md](configuration.md).

**Normal development:** leave `VESC_MCP_ENABLE_FLASH` unset and `enable_flash = false`.

Wave 1 (`br-mcp-tools-ief`) shipped discovery, inspect, validate, and build tools only. Phase-2 device tools (upload `.vescpkg`, flash firmware, BLE pairing helpers) remain **out of scope** until a dedicated epic lands with the same gate.

---

## Agent and human confirmation rules

When flash/upload tools eventually ship:

1. **Never assume availability** — call `tools/list` and confirm the upload/flash tool names exist before proposing device steps.
2. **Require explicit human confirmation** in the agent prompt before any upload or flash, including:
   - Target device identity (serial, CAN ID, or known `/dev/tty*` path the user provided)
   - Exact artifact path under `VESC_PACKAGE_ROOTS`
   - Acknowledgment that motor power may cut and incorrect images can brick hardware
3. **Do not proceed** if the user has not named the device or artifact in the current session.

Example confirmation block for future upload tools:

> I will upload `refloat-minimal.vescpkg` to the VESC on `/dev/ttyACM0` that you confirmed. Reply **yes** to proceed.

---

## Device path hygiene

| Rule | Rationale |
|------|-----------|
| Never upload to unknown or guessed device paths | Wrong port can disrupt unrelated USB serial devices |
| Do not scan `/dev` and pick the first match | Multiple VESCs, BMS, or debug adapters may be present |
| Prefer user-supplied paths or VESC Tool–verified connections | MCP has no hardware discovery in v1 |
| Treat `VESC_TOOL_PATH` subprocess builds separately from device I/O | `build_vescpkg` only writes local `.vescpkg` files via `vesc_tool` |

Sandbox rules for **package trees** (`VESC_PACKAGE_ROOTS`) are independent of device gates — see [configuration.md](configuration.md).

---

## What is safe without the flash gate

These tools are always registered (Wave 3 baseline):

- `ping`, `list_vesc_packages`, `inspect_pkgdesc`, `inspect_vescpkg`
- `validate_package_layout`, `build_vescpkg`, `run_package_checks`
- `search_vesc_knowledge`

They operate on configured directories and catalog resources under `tests/fixtures/` or user-declared package roots. They do **not** open serial ports or initiate VESC Tool uploads.

Offline walkthroughs: [examples/inspect-refloat-session.md](examples/inspect-refloat-session.md), [examples/build-poc-package-session.md](examples/build-poc-package-session.md).

---

## Checklist for operators

- [ ] `VESC_MCP_ENABLE_FLASH` unset in MCP client config
- [ ] `enable_flash = false` in `config.toml`
- [ ] Agents instructed to use fixtures before live repos
- [ ] Any future upload request includes user-confirmed device path and artifact hash or path
