//! Host-side adapters bridging `vesc-domain` and POC `vesc-pkg-build`.

#![forbid(unsafe_code)]

pub mod build;
pub mod error;
pub mod inspect;

pub use build::{BuiltPackage, build_package_from_root, locate_pkgdesc};
pub use error::AdapterError;
pub use inspect::{PackageInspection, inspect_vescpkg};
