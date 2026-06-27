//! `inspect_pkgdesc` — parse a pkgdesc.qml file and return structured fields.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vesc_domain::{ParsedPkgDesc, parse_pkgdesc_qml};

use super::list_packages::dialect_label;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InspectPkgdescParams {
    /// Absolute or relative path to a `pkgdesc.qml` file.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct ParsedPkgdescJson {
    pub pkg_name: String,
    pub description_md_path: String,
    pub lisp_path: String,
    pub qml_path: String,
    pub output_name: String,
    pub qml_is_fullscreen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct InspectPkgdescResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dialect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed: Option<ParsedPkgdescJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[must_use]
pub fn inspect_pkgdesc(path: &str) -> InspectPkgdescResponse {
    let path_buf = PathBuf::from(path);

    let content = match fs::read_to_string(&path_buf) {
        Ok(content) => content,
        Err(err) => {
            return InspectPkgdescResponse {
                ok: false,
                dialect: None,
                parsed: None,
                error: Some(format!("read {}: {err}", path_buf.display())),
            };
        }
    };

    match parse_pkgdesc_qml(&content, &path_buf) {
        Ok(parsed) => parsed_to_response(parsed),
        Err(err) => InspectPkgdescResponse {
            ok: false,
            dialect: None,
            parsed: None,
            error: Some(err.to_string()),
        },
    }
}

fn parsed_to_response(parsed: ParsedPkgDesc) -> InspectPkgdescResponse {
    let dialect = dialect_label(parsed.dialect()).into();
    let parsed_json = match parsed {
        ParsedPkgDesc::VescTool(desc) => ParsedPkgdescJson {
            pkg_name: desc.pkg_name.as_str().into(),
            description_md_path: desc.description_md_path.as_path().display().to_string(),
            lisp_path: desc.lisp_path.as_path().display().to_string(),
            qml_path: desc.qml_path.as_path().display().to_string(),
            output_name: desc.output_name.as_str().into(),
            qml_is_fullscreen: desc.qml_is_fullscreen,
        },
    };

    InspectPkgdescResponse {
        ok: true,
        dialect: Some(dialect),
        parsed: Some(parsed_json),
        error: None,
    }
}

/// Serialize a tool response as JSON text for rmcp handlers.
#[must_use]
pub fn inspect_pkgdesc_json(params: &InspectPkgdescParams) -> String {
    let response = inspect_pkgdesc(&params.path);
    serde_json::to_string(&response)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_path;

    #[test]
    fn inspect_refloat_minimal_fixture() {
        let path = fixture_path("refloat-minimal").join("pkgdesc.qml");
        let response = inspect_pkgdesc(&path.display().to_string());

        assert!(response.ok);
        assert_eq!(response.dialect.as_deref(), Some("vesc_tool"));
        let parsed = response.parsed.expect("parsed fields");
        assert_eq!(parsed.pkg_name, "Refloat Minimal");
        assert_eq!(parsed.output_name, "refloat-minimal.vescpkg");
    }

    #[test]
    fn inspect_poc_native_fixture() {
        let path = fixture_path("poc-native-lib-minimal/package/pkgdesc.qml");
        let response = inspect_pkgdesc(&path.display().to_string());

        assert!(response.ok);
        assert_eq!(response.dialect.as_deref(), Some("vesc_tool"));
        let parsed = response.parsed.expect("parsed fields");
        assert_eq!(parsed.pkg_name, "POC native-lib minimal fixture");
        assert_eq!(parsed.output_name, "poc-native-lib-minimal.vescpkg");
        assert_eq!(parsed.qml_path, "");
    }

    #[test]
    fn inspect_missing_file_returns_error() {
        let response = inspect_pkgdesc("/nonexistent/pkgdesc.qml");
        assert!(!response.ok);
        assert!(response.error.is_some());
    }
}
