//! VESC / vescpkg domain types.
//!
//! Parse, validate, and inspect package metadata and artifacts.

pub mod error;
pub mod layout;
pub mod legacy;
pub mod paths;
pub mod pkgdesc;
pub mod validate;
pub mod wire;

pub use error::DomainError;
pub use layout::{expected_artifact_name, sanitize_pkg_name};
pub use legacy::{LegacyBuildPkgDescriptor, parse_legacy_buildpkg_colon};
pub use pkgdesc::{
    OutputFileName, PackageVersion, ParsedPkgDesc, PkgDescDialect, PkgDescVescTool, PkgName,
    RelativeAssetPath, parse_pkgdesc_qml,
};
pub use validate::{LayoutIssue, LayoutValidationReport, validate_package_layout};
pub use wire::{
    LispImport, PackageField, VescPackageBuildInput, VescPackageFields, build_vescpkg_bytes,
    parse_lisp_imports, parse_vescpkg_fields, payload_matches_native_with_only_nul_tail,
    read_vescpkg_fields, write_vescpkg_file,
};
