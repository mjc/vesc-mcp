//! pkgdesc.qml parsing and dialect-specific descriptor types.

mod newtypes;
mod vesc_tool;

pub use newtypes::{OutputFileName, PackageVersion, PkgName, RelativeAssetPath};
pub use vesc_tool::PkgDescVescTool;
