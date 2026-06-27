//! `.vescpkg` wire format writer (mirrors `vesc_tool` / POC packer behavior).

use std::io::Write;
use std::path::Path;

use flate2::Compression;
use flate2::write::ZlibEncoder;

use super::MAGIC;
use crate::DomainError;

/// Inputs for building a `.vescpkg` wire artifact.
#[derive(Debug, Clone)]
pub struct VescPackageBuildInput<'a> {
    pub name: &'a str,
    pub description_md: &'a str,
    pub lisp_source: &'a str,
    pub lisp_editor_path: &'a Path,
    pub qml_file: &'a str,
    pub pkg_desc_qml: &'a str,
    pub qml_is_fullscreen: bool,
}

/// Build compressed `.vescpkg` bytes from staged package inputs.
///
/// # Errors
///
/// Returns [`DomainError::Io`] on import file read failure or
/// [`DomainError::InvalidWireFormat`] when field sizes exceed wire limits.
pub fn build_vescpkg_bytes(input: &VescPackageBuildInput<'_>) -> Result<Vec<u8>, DomainError> {
    let lisp_data = pack_lisp_imports(input.lisp_source, input.lisp_editor_path)?;

    let mut data = Vec::new();
    append_string(&mut data, MAGIC);

    append_text_field(&mut data, "name", input.name)?;
    append_text_field(&mut data, "description_md", input.description_md)?;
    append_bytes_field(&mut data, "lispData", &lisp_data)?;
    append_text_field(&mut data, "qmlFile", input.qml_file)?;
    append_text_field(&mut data, "pkgDescQml", input.pkg_desc_qml)?;

    append_string(&mut data, "qmlIsFullscreen");
    append_i32_be(&mut data, 1);
    data.push(u8::from(input.qml_is_fullscreen));

    q_compress(&data)
}

/// Write a `.vescpkg` file and return the compressed bytes written.
///
/// # Errors
///
/// Same as [`build_vescpkg_bytes`], plus [`DomainError::Io`] on output write failure.
pub fn write_vescpkg_file(
    output_path: impl AsRef<Path>,
    input: &VescPackageBuildInput<'_>,
) -> Result<Vec<u8>, DomainError> {
    let output_path = output_path.as_ref();
    let bytes = build_vescpkg_bytes(input)?;

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| DomainError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(output_path, &bytes).map_err(|source| DomainError::Io {
        path: output_path.to_path_buf(),
        source,
    })?;
    Ok(bytes)
}

fn append_text_field(buf: &mut Vec<u8>, key: &str, value: &str) -> Result<(), DomainError> {
    if value.is_empty() {
        return Ok(());
    }
    append_string(buf, key);
    append_bytes(buf, value.as_bytes())?;
    Ok(())
}

fn append_bytes_field(buf: &mut Vec<u8>, key: &str, value: &[u8]) -> Result<(), DomainError> {
    if value.is_empty() {
        return Ok(());
    }
    append_string(buf, key);
    append_bytes(buf, value)?;
    Ok(())
}

fn append_bytes(buf: &mut Vec<u8>, value: &[u8]) -> Result<(), DomainError> {
    let len = i32::try_from(value.len()).map_err(|_| DomainError::InvalidWireFormat {
        message: "package field exceeds the VESC packet length limit".into(),
    })?;
    append_i32_be(buf, len);
    buf.extend_from_slice(value);
    Ok(())
}

fn append_string(buf: &mut Vec<u8>, value: &str) {
    buf.extend_from_slice(value.as_bytes());
    buf.push(0);
}

fn append_i32_be(buf: &mut Vec<u8>, value: i32) {
    buf.extend_from_slice(&value.to_be_bytes());
}

fn q_compress(data: &[u8]) -> Result<Vec<u8>, DomainError> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(data).map_err(|source| DomainError::Io {
        path: Path::new("<compress>").to_path_buf(),
        source,
    })?;
    let compressed = encoder.finish().map_err(|source| DomainError::Io {
        path: Path::new("<compress>").to_path_buf(),
        source,
    })?;

    let uncompressed_len =
        u32::try_from(data.len()).map_err(|_| DomainError::InvalidWireFormat {
            message: "package payload exceeds the Qt qCompress length limit".into(),
        })?;

    let mut output = Vec::with_capacity(4 + compressed.len());
    output.extend_from_slice(&uncompressed_len.to_be_bytes());
    output.extend_from_slice(&compressed);
    Ok(output)
}

fn pack_lisp_imports(code_str: &str, editor_path: &Path) -> Result<Vec<u8>, DomainError> {
    let mut packed = Vec::new();
    packed.extend_from_slice(&0u16.to_be_bytes());
    packed.extend_from_slice(code_str.as_bytes());
    if packed.last().copied() != Some(0) {
        packed.push(0);
    }

    let mut imports = Vec::new();
    for line in code_str.lines() {
        let Some((path, tag)) = parse_import_line(line) else {
            continue;
        };

        let source_path = resolve_import_path(editor_path, &path);
        let mut file_data = std::fs::read(&source_path).map_err(|source| DomainError::Io {
            path: source_path,
            source,
        })?;
        if file_data.last().copied() != Some(0) {
            file_data.push(0);
        }
        imports.push((tag, file_data));
    }

    let file_table_size = imports
        .iter()
        .fold(0usize, |acc, (tag, _)| acc + tag.len() + 9);
    let num_imports = i16::try_from(imports.len()).map_err(|_| DomainError::InvalidWireFormat {
        message: "too many Lisp imports for a VESC package".into(),
    })?;
    packed.extend_from_slice(&num_imports.to_be_bytes());

    let mut file_offset = packed.len() + file_table_size - 2;
    for (tag, data) in &imports {
        while file_offset % 4 != 0 {
            file_offset += 1;
        }

        append_string(&mut packed, tag);
        append_i32_be(
            &mut packed,
            i32::try_from(file_offset).map_err(|_| DomainError::InvalidWireFormat {
                message: "Lisp import offset exceeds the VESC package limit".into(),
            })?,
        );
        append_i32_be(
            &mut packed,
            i32::try_from(data.len()).map_err(|_| DomainError::InvalidWireFormat {
                message: "Lisp import payload exceeds the VESC package limit".into(),
            })?,
        );
        file_offset += data.len();
    }

    for (_, data) in &imports {
        while (packed.len() - 2) % 4 != 0 {
            packed.push(0);
        }
        packed.extend_from_slice(data);
    }

    Ok(packed)
}

fn resolve_import_path(editor_path: &Path, import_path: &str) -> std::path::PathBuf {
    let relative_candidate = editor_path.join(import_path);
    if relative_candidate.exists() {
        return relative_candidate;
    }
    std::path::PathBuf::from(import_path)
}

fn parse_import_line(line: &str) -> Option<(String, String)> {
    let mut trimmed = line.trim_start();
    while trimmed.starts_with("( ") {
        trimmed = &trimmed[1..];
    }

    if !trimmed.starts_with("(import ") {
        return None;
    }

    let start = trimmed.find('"')?;
    let end = trimmed.rfind('"')?;
    if end <= start {
        return None;
    }

    let path = trimmed[start + 1..end].to_owned();
    let mut tag = trimmed[end + 1..].replace(['\r', ' ', ')', '\''], "");
    if let Some(index) = tag.find(';') {
        tag.truncate(index);
    }

    if path.is_empty() || tag.is_empty() {
        return None;
    }

    Some((path, tag))
}
