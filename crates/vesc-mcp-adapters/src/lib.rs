//! Host-side adapters for vesc-domain inspect and pkgdesc discovery.

#![forbid(unsafe_code)]

pub mod build;
pub mod error;
pub mod inspect;

pub use build::locate_pkgdesc;
pub use error::AdapterError;
pub use inspect::{PackageInspection, inspect_vescpkg};
