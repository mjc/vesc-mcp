//! `.vescpkg` wire format reader (mirrors POC `package_format_decode`).

use std::io::Read;
use std::path::Path;

use flate2::read::ZlibDecoder;

use crate::error::DomainError;

/// Magic header inside the decompressed VESC package payload.
pub const MAGIC: &str = "VESC Packet";

/// Field keys in `vesc_tool` wire order (empty text fields may be omitted).
pub const FIELD_SPINE: [&str; 6] = [
    "name",
    "description_md",
    "lispData",
    "qmlFile",
    "pkgDescQml",
    "qmlIsFullscreen",
];

/// Decompressed package fields extracted from a `.vescpkg` blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VescPackageFields {
    pub name: String,
    pub description_md: String,
    pub lisp_data: Vec<u8>,
    pub qml_file: String,
    pub pkg_desc_qml: String,
    pub qml_is_fullscreen: bool,
}

/// One key/value pair from the decompressed wire payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageField {
    pub key: String,
    pub value: Vec<u8>,
}

/// Embedded Lisp import table entry from `lispData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LispImport {
    pub tag: String,
    pub offset: usize,
    pub size: usize,
    pub payload: Vec<u8>,
}

/// Read and parse a `.vescpkg` file from disk.
///
/// # Errors
///
/// Returns [`DomainError::Io`] on read failure or [`DomainError::InvalidWireFormat`]
/// when the bytes are not a valid VESC package.
pub fn read_vescpkg_fields(path: impl AsRef<Path>) -> Result<VescPackageFields, DomainError> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|source| DomainError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_vescpkg_fields(&bytes)
}

/// Decompress a Qt `qCompress`-wrapped `.vescpkg` payload.
///
/// # Errors
///
/// Returns [`DomainError::InvalidWireFormat`] when the wrapper or zlib stream is invalid.
pub fn decompress_vescpkg(package: &[u8]) -> Result<Vec<u8>, DomainError> {
    if package.len() < 4 {
        return Err(DomainError::InvalidWireFormat {
            message: "package shorter than qCompress length prefix".to_string(),
        });
    }

    let declared_len = u32::from_be_bytes(
        package[..4]
            .try_into()
            .map_err(|_| wire_err("invalid qCompress length prefix"))?,
    ) as usize;

    let mut decoder = ZlibDecoder::new(&package[4..]);
    let mut raw = Vec::new();
    decoder
        .read_to_end(&mut raw)
        .map_err(|source| wire_err(format!("zlib decompress failed: {source}")))?;

    if raw.len() != declared_len {
        return Err(DomainError::InvalidWireFormat {
            message: format!(
                "decompressed length {} does not match declared length {declared_len}",
                raw.len()
            ),
        });
    }

    Ok(raw)
}

/// Parse decompressed wire fields from raw `.vescpkg` bytes.
///
/// # Errors
///
/// Returns [`DomainError::InvalidWireFormat`] when magic or field encoding is invalid.
pub fn parse_vescpkg_fields(package: &[u8]) -> Result<VescPackageFields, DomainError> {
    let fields = package_fields(package)?;
    fields_from_pairs(&fields)
}

/// Parse all key/value fields from compressed `.vescpkg` bytes.
///
/// # Errors
///
/// Returns [`DomainError::InvalidWireFormat`] on decode failure.
pub fn package_fields(package: &[u8]) -> Result<Vec<PackageField>, DomainError> {
    let raw = decompress_vescpkg(package)?;
    let mut cursor = raw.as_slice();

    let magic = read_string(&mut cursor)?;
    if magic != MAGIC {
        return Err(DomainError::InvalidWireFormat {
            message: format!("expected magic {MAGIC:?}, got {magic:?}"),
        });
    }

    let mut fields = Vec::new();
    while !cursor.is_empty() {
        let key = read_string(&mut cursor)?;
        let len_i32 = read_i32_be(&mut cursor)?;
        if len_i32 < 0 {
            return Err(wire_err(format!(
                "field {key:?} has negative length {len_i32}"
            )));
        }
        let len = usize_from_i32(len_i32, &format!("field {key:?} length"))?;
        if cursor.len() < len {
            return Err(DomainError::InvalidWireFormat {
                message: format!("field {key:?} length {len} exceeds remaining bytes"),
            });
        }
        let (value, rest) = cursor.split_at(len);
        cursor = rest;
        fields.push(PackageField {
            key,
            value: value.to_vec(),
        });
    }

    Ok(fields)
}

/// Return the raw value for a named field, if present.
#[must_use]
pub fn extract_field<'a>(fields: &'a [PackageField], key: &str) -> Option<&'a [u8]> {
    fields
        .iter()
        .find(|field| field.key == key)
        .map(|field| field.value.as_slice())
}

/// Parse the Lisp import table embedded in `lispData`.
///
/// # Errors
///
/// Returns [`DomainError::InvalidWireFormat`] when the import table is malformed.
pub fn parse_lisp_imports(lisp_data: &[u8]) -> Result<(String, Vec<LispImport>), DomainError> {
    let mut cursor = lisp_data;
    let header = read_i16_be(&mut cursor)?;
    if header != 0 {
        return Err(wire_err(format!("unexpected lispData header {header}")));
    }

    let code = read_string(&mut cursor)?;
    let import_count = read_i16_be(&mut cursor)?;
    if import_count < 0 {
        return Err(wire_err("negative Lisp import count"));
    }
    let import_count = usize_from_i16(import_count, "Lisp import count")?;

    let mut imports = Vec::with_capacity(import_count);
    for _ in 0..import_count {
        let tag = read_string(&mut cursor)?;
        let offset_i32 = read_i32_be(&mut cursor)?;
        let size_i32 = read_i32_be(&mut cursor)?;
        if offset_i32 < 0 || size_i32 < 0 {
            return Err(wire_err(format!(
                "import {tag:?} has negative offset or size"
            )));
        }
        let offset = usize_from_i32(offset_i32, &format!("import {tag:?} offset"))?;
        let size = usize_from_i32(size_i32, &format!("import {tag:?} size"))?;
        let start = 2usize.saturating_add(offset);
        let end = start.saturating_add(size);
        if end > lisp_data.len() {
            return Err(wire_err(format!(
                "import {tag:?} range [{start}, {end}) exceeds lispData length {}",
                lisp_data.len()
            )));
        }
        imports.push(LispImport {
            tag,
            offset,
            size,
            payload: lisp_data[start..end].to_vec(),
        });
    }

    Ok((code, imports))
}

/// True when `payload` equals `native` followed by zero padding only.
#[must_use]
pub fn payload_matches_native_with_only_nul_tail(payload: &[u8], native: &[u8]) -> bool {
    payload.starts_with(native) && payload[native.len()..].iter().all(|byte| *byte == 0)
}

fn fields_from_pairs(fields: &[PackageField]) -> Result<VescPackageFields, DomainError> {
    let name = required_text_field(fields, "name")?;
    let description_md = optional_text_field(fields, "description_md");
    let lisp_data = optional_bytes_field(fields, "lispData");
    let qml_file = optional_text_field(fields, "qmlFile");
    let pkg_desc_qml = optional_text_field(fields, "pkgDescQml");
    let qml_is_fullscreen = optional_bool_field(fields, "qmlIsFullscreen").unwrap_or(false);

    Ok(VescPackageFields {
        name,
        description_md,
        lisp_data,
        qml_file,
        pkg_desc_qml,
        qml_is_fullscreen,
    })
}

fn required_text_field(fields: &[PackageField], key: &str) -> Result<String, DomainError> {
    let value = extract_field(fields, key).ok_or_else(|| DomainError::InvalidWireFormat {
        message: format!("missing required field {key}"),
    })?;
    decode_text(key, value)
}

fn optional_text_field(fields: &[PackageField], key: &str) -> String {
    extract_field(fields, key)
        .and_then(|value| decode_text(key, value).ok())
        .unwrap_or_default()
}

fn optional_bytes_field(fields: &[PackageField], key: &str) -> Vec<u8> {
    extract_field(fields, key)
        .map(<[u8]>::to_vec)
        .unwrap_or_default()
}

fn optional_bool_field(fields: &[PackageField], key: &str) -> Option<bool> {
    let value = extract_field(fields, key)?;
    if value.is_empty() {
        return None;
    }
    Some(value[0] != 0)
}

fn decode_text(key: &str, value: &[u8]) -> Result<String, DomainError> {
    std::str::from_utf8(value)
        .map(std::string::ToString::to_string)
        .map_err(|_| wire_err(format!("field {key:?} is not valid UTF-8")))
}

fn read_string(cursor: &mut &[u8]) -> Result<String, DomainError> {
    let end = cursor
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| wire_err("missing nul terminator"))?;
    let value = std::str::from_utf8(&cursor[..end])
        .map_err(|_| wire_err("invalid UTF-8 in nul-terminated string"))?
        .to_owned();
    *cursor = &cursor[end + 1..];
    Ok(value)
}

fn read_i32_be(cursor: &mut &[u8]) -> Result<i32, DomainError> {
    if cursor.len() < 4 {
        return Err(wire_err("unexpected end of payload reading i32"));
    }
    let (bytes, rest) = cursor.split_at(4);
    *cursor = rest;
    Ok(i32::from_be_bytes(
        bytes
            .try_into()
            .map_err(|_| wire_err("invalid i32 bytes"))?,
    ))
}

fn read_i16_be(cursor: &mut &[u8]) -> Result<i16, DomainError> {
    if cursor.len() < 2 {
        return Err(wire_err("unexpected end of payload reading i16"));
    }
    let (bytes, rest) = cursor.split_at(2);
    *cursor = rest;
    Ok(i16::from_be_bytes(
        bytes
            .try_into()
            .map_err(|_| wire_err("invalid i16 bytes"))?,
    ))
}

fn wire_err(message: impl Into<String>) -> DomainError {
    DomainError::InvalidWireFormat {
        message: message.into(),
    }
}

fn usize_from_i32(value: i32, context: &str) -> Result<usize, DomainError> {
    usize::try_from(value).map_err(|_| wire_err(format!("{context} out of range: {value}")))
}

fn usize_from_i16(value: i16, context: &str) -> Result<usize, DomainError> {
    usize::try_from(value).map_err(|_| wire_err(format!("{context} out of range: {value}")))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixtures_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
    }

    #[test]
    fn extract_vescpkg_name_field() {
        let path = fixtures_root().join("golden/poc-minimal.vescpkg");
        let fields = read_vescpkg_fields(&path).expect("read golden package");
        assert_eq!(fields.name, "POC native-lib minimal fixture");
        assert!(fields.description_md.contains("native-lib-baseline"));
        assert!(!fields.lisp_data.is_empty());
    }

    #[test]
    fn extract_vescpkg_rejects_bad_magic() {
        let path = fixtures_root().join("broken-bad-magic/bad-magic.vescpkg");
        let bytes = std::fs::read(&path).expect("read bad magic fixture");
        let err = parse_vescpkg_fields(&bytes).expect_err("bad magic should fail");
        assert!(matches!(err, DomainError::InvalidWireFormat { .. }));
    }

    #[test]
    fn extract_vescpkg_rejects_truncated() {
        let path = fixtures_root().join("broken-bad-wire/truncated.vescpkg");
        let bytes = std::fs::read(&path).expect("read truncated fixture");
        let err = parse_vescpkg_fields(&bytes).expect_err("truncated should fail");
        assert!(matches!(err, DomainError::InvalidWireFormat { .. }));
    }

    #[test]
    fn lisp_imports_embed_native_payload_bytes() {
        let path = fixtures_root().join("golden/poc-minimal.vescpkg");
        let fields = read_vescpkg_fields(&path).expect("read golden package");
        let (code, imports) = parse_lisp_imports(&fields.lisp_data).expect("parse imports");

        assert!(code.contains("(import \"src/package_lib.bin\" 'package-lib)"));
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].tag, "package-lib");
        assert_eq!(imports[0].offset % 4, 0);

        let native =
            std::fs::read(fixtures_root().join("poc-native-lib-minimal/src/package_lib.bin"))
                .expect("read native payload");
        assert!(payload_matches_native_with_only_nul_tail(
            &imports[0].payload,
            &native
        ));
    }

    #[test]
    fn package_fields_follow_vesc_tool_spine() {
        let path = fixtures_root().join("golden/poc-minimal.vescpkg");
        let bytes = std::fs::read(&path).expect("read golden package");
        let fields = package_fields(&bytes).expect("parse fields");
        let keys: Vec<_> = fields.iter().map(|field| field.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "name",
                "description_md",
                "lispData",
                "pkgDescQml",
                "qmlIsFullscreen"
            ]
        );
    }
}
