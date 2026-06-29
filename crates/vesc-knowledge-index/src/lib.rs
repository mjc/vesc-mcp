//! Searchable firmware and package knowledge index types and builders.

mod builder;
mod embedded;
mod entry;
pub mod parsers;
mod search;

pub use builder::IndexBuilder;
pub use embedded::{KnowledgeSearchHit, embedded_entries, search_knowledge};
pub use entry::{Category, IndexEntry, SourceRef};
pub use parsers::native_lib_abi::NativeLibAbiParseError;
pub use parsers::priorities::PrioritiesParseError;
pub use parsers::refloat_commands::RefloatCommandsParseError;
pub use parsers::vesc_c_if::VescCIfParseError;
pub use search::{ScoredEntry, rank_entries};
