#!/usr/bin/env python3
"""One-shot generator for tests/fixtures/golden/poc-minimal.vescpkg (br-domain-model-oli.9)."""

from __future__ import annotations

import hashlib
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests/fixtures/poc-native-lib-minimal"
OUT_DIR = ROOT / "tests/fixtures/golden"


def append_string(buf: bytearray, value: str) -> None:
    buf.extend(value.encode("utf-8"))
    buf.append(0)


def append_i32_be(buf: bytearray, value: int) -> None:
    buf.extend(struct.pack(">i", value))


def append_bytes(buf: bytearray, value: bytes) -> None:
    append_i32_be(buf, len(value))
    buf.extend(value)


def append_text_field(buf: bytearray, key: str, value: str) -> None:
    if not value:
        return
    append_string(buf, key)
    append_bytes(buf, value.encode("utf-8"))


def append_bytes_field(buf: bytearray, key: str, value: bytes) -> None:
    if not value:
        return
    append_string(buf, key)
    append_bytes(buf, value)


def parse_import_line(line: str) -> tuple[str, str] | None:
    trimmed = line.lstrip()
    while trimmed.startswith("( "):
        trimmed = trimmed[1:]
    if not trimmed.startswith("(import "):
        return None
    start = trimmed.find('"')
    end = trimmed.rfind('"')
    if start < 0 or end <= start:
        return None
    path = trimmed[start + 1 : end]
    tag = trimmed[end + 1 :].replace("\r", "").replace(" ", "").replace(")", "").replace("'", "")
    if ";" in tag:
        tag = tag[: tag.find(";")]
    if not path or not tag:
        return None
    return path, tag


def pack_lisp_imports(code_str: str, editor_root: Path) -> bytes:
    packed = bytearray()
    packed.extend(struct.pack(">H", 0))
    packed.extend(code_str.encode("utf-8"))
    if not packed or packed[-1] != 0:
        packed.append(0)

    imports: list[tuple[str, bytes]] = []
    for line in code_str.splitlines():
        parsed = parse_import_line(line)
        if parsed is None:
            continue
        rel_path, tag = parsed
        source = editor_root / rel_path
        if not source.is_file():
            source = Path(rel_path)
        file_data = bytearray(source.read_bytes())
        if not file_data or file_data[-1] != 0:
            file_data.append(0)
        imports.append((tag, bytes(file_data)))

    file_table_size = sum(len(tag.encode("utf-8")) + 1 + 8 for tag, _ in imports)
    packed.extend(struct.pack(">h", len(imports)))

    file_offset = len(packed) + file_table_size - 2
    payloads: list[bytes] = []
    for tag, data in imports:
        while file_offset % 4 != 0:
            file_offset += 1
        append_string(packed, tag)
        append_i32_be(packed, file_offset)
        append_i32_be(packed, len(data))
        file_offset += len(data)
        payloads.append(data)

    for data in payloads:
        while (len(packed) - 2) % 4 != 0:
            packed.append(0)
        packed.extend(data)

    return bytes(packed)


def q_compress(data: bytes) -> bytes:
    compressed = zlib.compress(data, level=9)
    return struct.pack(">I", len(data)) + compressed


def build_vesc_package(
    name: str,
    description_md: str,
    lisp_data: bytes,
    qml_file: str,
    pkg_desc_qml: str,
    qml_is_fullscreen: bool,
) -> bytes:
    raw = bytearray()
    append_string(raw, "VESC Packet")
    append_text_field(raw, "name", name)
    append_text_field(raw, "description_md", description_md)
    append_bytes_field(raw, "lispData", lisp_data)
    append_text_field(raw, "qmlFile", qml_file)
    append_text_field(raw, "pkgDescQml", pkg_desc_qml)
    append_string(raw, "qmlIsFullscreen")
    append_i32_be(raw, 1)
    raw.append(1 if qml_is_fullscreen else 0)
    return q_compress(bytes(raw))


def main() -> None:
    package_root = FIXTURE / "package"
    lisp_source = (package_root / "code.lisp").read_text(encoding="utf-8")
    lisp_data = pack_lisp_imports(lisp_source, FIXTURE)
    pkgdesc = (package_root / "pkgdesc.qml").read_text(encoding="utf-8")
    readme = (package_root / "README.md").read_text(encoding="utf-8")

    package = build_vesc_package(
        name="POC native-lib minimal fixture",
        description_md=readme,
        lisp_data=lisp_data,
        qml_file="",
        pkg_desc_qml=pkgdesc,
        qml_is_fullscreen=False,
    )

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    out_path = OUT_DIR / "poc-minimal.vescpkg"
    out_path.write_bytes(package)

    digest = hashlib.sha256(package).hexdigest()
    (OUT_DIR / "poc-minimal.sha256").write_text(f"{digest}  poc-minimal.vescpkg\n", encoding="utf-8")

    readme_path = OUT_DIR / "README.md"
    readme_path.write_text(
        """# Wire format golden vectors

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
""",
        encoding="utf-8",
    )
    print(f"wrote {out_path} ({len(package)} bytes)")
    print(f"sha256 {digest}")


if __name__ == "__main__":
    main()
