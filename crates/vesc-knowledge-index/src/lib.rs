//! Searchable firmware and package knowledge index types and builders.

mod builder;
mod entry;
pub mod parsers;
mod search;

pub use builder::IndexBuilder;
pub use entry::{Category, IndexEntry, SourceRef};
pub use parsers::poc_abi::PocAbiParseError;
pub use parsers::refloat_commands::RefloatCommandsParseError;
pub use parsers::vesc_c_if::VescCIfParseError;
pub use search::{ScoredEntry, rank_entries};
