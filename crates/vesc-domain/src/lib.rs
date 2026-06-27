//! VESC / vescpkg domain types.
//!
//! Parse, validate, and inspect package metadata and artifacts.

pub mod error;
pub mod layout;
pub mod paths;
pub mod pkgdesc;
pub mod validate;
pub mod wire;

pub use error::DomainError;
pub use pkgdesc::{
    OutputFileName, PackageVersion, ParsedPkgDesc, PkgDescDialect, PkgDescNativeLib,
    PkgDescVescTool, PkgName, RelativeAssetPath, parse_pkgdesc_qml,
};
