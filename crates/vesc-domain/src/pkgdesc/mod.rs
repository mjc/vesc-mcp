//! pkgdesc.qml parsing and dialect-specific descriptor types.

mod dialect;
mod native_lib;
mod newtypes;
mod parse;
mod vesc_tool;

pub use dialect::{ParsedPkgDesc, PkgDescDialect};
pub use native_lib::PkgDescNativeLib;
pub use newtypes::{OutputFileName, PackageVersion, PkgName, RelativeAssetPath};
pub use parse::parse_pkgdesc_qml;
pub use vesc_tool::PkgDescVescTool;
