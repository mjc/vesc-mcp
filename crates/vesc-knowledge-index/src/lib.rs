//! Searchable firmware and package knowledge index types and builders.

mod builder;
mod entry;
pub mod parsers;

pub use builder::IndexBuilder;
pub use entry::{Category, IndexEntry, SourceRef};
pub use parsers::vesc_c_if::VescCIfParseError;
