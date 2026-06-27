//! `QtQuick` property extraction from pkgdesc.qml files.

use std::collections::HashMap;
use std::path::Path;

use crate::error::DomainError;

use super::dialect::ParsedPkgDesc;
use super::newtypes::{OutputFileName, PkgName, RelativeAssetPath};
use super::vesc_tool::PkgDescVescTool;

const LEGACY_POC_PROPERTIES: [&str; 4] = [
    "packageName",
    "packageVersion",
    "nativeLibraryPath",
    "loaderScriptPath",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum PropertyValue {
    String(String),
    Bool(bool),
}

/// Parse pkgdesc.qml text and return a parsed descriptor.
///
/// # Errors
///
/// Returns [`DomainError`] when required properties are missing, legacy POC-only
/// fields are present, or duplicate property declarations are found.
pub fn parse_pkgdesc_qml(
    content: &str,
    path: impl AsRef<Path>,
) -> Result<ParsedPkgDesc, DomainError> {
    let path = path.as_ref().to_path_buf();
    let properties = extract_properties(content, &path)?;
    validate_dialect(&properties, &path)?;
    Ok(ParsedPkgDesc::VescTool(build_vesc_tool(
        &properties,
        &path,
    )?))
}

fn extract_properties(
    content: &str,
    path: &Path,
) -> Result<HashMap<String, PropertyValue>, DomainError> {
    let mut properties = HashMap::new();
    for (line_no, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let Some((name, value)) = parse_property_line(trimmed) else {
            continue;
        };
        if properties.insert(name.clone(), value).is_some() {
            return Err(DomainError::InvalidProperty {
                property: name,
                path: path.to_path_buf(),
                message: format!("duplicate property on line {}", line_no + 1),
            });
        }
    }
    Ok(properties)
}

fn parse_property_line(line: &str) -> Option<(String, PropertyValue)> {
    let rest = line.strip_prefix("property ")?;
    let (kind, rest) = rest.split_once(' ')?;
    let (name, rest) = rest.split_once(':')?;
    let name = name.trim().to_string();
    let value_text = rest.trim().trim_end_matches(';').trim();

    match kind {
        "string" => {
            let value = parse_quoted_string(value_text)?;
            Some((name, PropertyValue::String(value)))
        }
        "bool" => {
            let value = match value_text {
                "true" => true,
                "false" => false,
                _ => return None,
            };
            Some((name, PropertyValue::Bool(value)))
        }
        _ => None,
    }
}

fn parse_quoted_string(value_text: &str) -> Option<String> {
    let value_text = value_text.strip_prefix('"')?;
    let value_text = value_text.strip_suffix('"')?;
    Some(value_text.to_string())
}

fn validate_dialect(
    properties: &HashMap<String, PropertyValue>,
    path: &Path,
) -> Result<(), DomainError> {
    let has_legacy_poc = LEGACY_POC_PROPERTIES
        .iter()
        .any(|key| properties.contains_key(*key));
    if has_legacy_poc {
        return Err(DomainError::LegacyPocDialect {
            path: path.to_path_buf(),
        });
    }
    if properties.contains_key("pkgName") {
        return Ok(());
    }
    Err(DomainError::UnknownDialect {
        path: path.to_path_buf(),
    })
}

fn build_vesc_tool(
    properties: &HashMap<String, PropertyValue>,
    path: &Path,
) -> Result<PkgDescVescTool, DomainError> {
    Ok(PkgDescVescTool::new(
        string_property(properties, "pkgName", path)?,
        string_path_property(properties, "pkgDescriptionMd", path)?,
        string_path_property(properties, "pkgLisp", path)?,
        string_path_property(properties, "pkgQml", path)?,
        string_output_property(properties, "pkgOutput", path)?,
        bool_property(properties, "pkgQmlIsFullscreen", path)?,
    ))
}

fn string_property(
    properties: &HashMap<String, PropertyValue>,
    key: &str,
    path: &Path,
) -> Result<PkgName, DomainError> {
    match properties.get(key) {
        Some(PropertyValue::String(value)) => Ok(PkgName::new(value.clone())),
        Some(_) => Err(invalid_type(key, path, "string")),
        None => Err(missing(key, path)),
    }
}

fn string_path_property(
    properties: &HashMap<String, PropertyValue>,
    key: &str,
    path: &Path,
) -> Result<RelativeAssetPath, DomainError> {
    match properties.get(key) {
        Some(PropertyValue::String(value)) => Ok(RelativeAssetPath::new(value.clone())),
        Some(_) => Err(invalid_type(key, path, "string")),
        None => Err(missing(key, path)),
    }
}

fn string_output_property(
    properties: &HashMap<String, PropertyValue>,
    key: &str,
    path: &Path,
) -> Result<OutputFileName, DomainError> {
    match properties.get(key) {
        Some(PropertyValue::String(value)) => Ok(OutputFileName::new(value.clone())),
        Some(_) => Err(invalid_type(key, path, "string")),
        None => Err(missing(key, path)),
    }
}

fn bool_property(
    properties: &HashMap<String, PropertyValue>,
    key: &str,
    path: &Path,
) -> Result<bool, DomainError> {
    match properties.get(key) {
        Some(PropertyValue::Bool(value)) => Ok(*value),
        Some(_) => Err(invalid_type(key, path, "bool")),
        None => Err(missing(key, path)),
    }
}

fn missing(property: &str, path: &Path) -> DomainError {
    DomainError::MissingProperty {
        property: property.to_string(),
        path: path.to_path_buf(),
    }
}

fn invalid_type(property: &str, path: &Path, expected: &str) -> DomainError {
    DomainError::InvalidProperty {
        property: property.to_string(),
        path: path.to_path_buf(),
        message: format!("expected {expected} property"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkgdesc::dialect::PkgDescDialect;

    const REFLOAT_PKGDESC: &str = r#"import QtQuick 2.15

Item {
    property string pkgName: "Refloat"
    property string pkgDescriptionMd: "package_README-gen.md"
    property string pkgLisp: "lisp/package.lisp"
    property string pkgQml: "ui.qml"
    property bool pkgQmlIsFullscreen: false
    property string pkgOutput: "refloat.vescpkg"

    function isCompatible (fwRxParams) {
        return true;
    }
}
"#;

    const LEGACY_POC_PKGDESC: &str = r#"import QtQuick 2.15

Item {
    property string packageName: "Rust BLE loopback test package"
    property string packageVersion: "0.1.0"
    property string nativeLibraryPath: "src/package_lib.bin"
    property string loaderScriptPath: "code.lisp"
}
"#;

    #[test]
    fn parse_refloat_pkgdesc() {
        let parsed = parse_pkgdesc_qml(REFLOAT_PKGDESC, "pkgdesc.qml").expect("parse refloat");
        assert_eq!(parsed.dialect(), PkgDescDialect::VescTool);
        let ParsedPkgDesc::VescTool(desc) = parsed;
        assert_eq!(desc.pkg_name.as_str(), "Refloat");
        assert_eq!(desc.output_name.as_str(), "refloat.vescpkg");
        assert!(!desc.qml_is_fullscreen);
    }

    #[test]
    fn reject_legacy_poc_pkgdesc() {
        let err = parse_pkgdesc_qml(LEGACY_POC_PKGDESC, "pkgdesc.qml").unwrap_err();
        assert!(matches!(err, DomainError::LegacyPocDialect { .. }));
    }

    #[test]
    fn reject_unknown_dialect() {
        let content = r#"Item { property string foo: "bar" }"#;
        let err = parse_pkgdesc_qml(content, "pkgdesc.qml").unwrap_err();
        assert!(matches!(err, DomainError::UnknownDialect { .. }));
    }

    #[test]
    fn reject_mixed_vesc_tool_and_legacy_poc_fields() {
        let content = r#"Item {
            property string pkgName: "x"
            property string packageName: "y"
        }"#;
        let err = parse_pkgdesc_qml(content, "pkgdesc.qml").unwrap_err();
        assert!(matches!(err, DomainError::LegacyPocDialect { .. }));
    }
}
