//! pkgdesc.qml parsing and dialect-specific descriptor types.

mod native_lib;
mod newtypes;
mod vesc_tool;

pub use native_lib::PkgDescNativeLib;
pub use newtypes::{OutputFileName, PackageVersion, PkgName, RelativeAssetPath};
pub use vesc_tool::PkgDescVescTool;
