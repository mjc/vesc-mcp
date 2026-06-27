//! Knowledge index builder from catalog sources.

use std::path::Path;

use crate::IndexEntry;
use crate::parsers::poc_abi::{self, PocAbiParseError};
use crate::parsers::refloat_commands::{self, RefloatCommandsParseError};
use crate::parsers::vesc_c_if::{self, VescCIfParseError};

/// Builds searchable index entries from catalog artifacts.
#[derive(Debug, Clone, Copy, Default)]
pub struct IndexBuilder;

impl IndexBuilder {
    /// Parse package-relevant `vesc_c_if` function groups from catalog YAML.
    ///
    /// # Errors
    ///
    /// Returns [`VescCIfParseError`] when the catalog file is missing or invalid.
    pub fn parse_vesc_c_if_groups(
        catalog_root: &Path,
    ) -> Result<Vec<IndexEntry>, VescCIfParseError> {
        vesc_c_if::parse_catalog(catalog_root)
    }

    /// Parse `vesc_c_if` groups and optionally validate symbols against the upstream header.
    ///
    /// When `bldc_root` is `Some`, every indexed symbol must appear in
    /// `{bldc_root}/lispBM/c_libs/vesc_c_if.h`.
    ///
    /// # Errors
    ///
    /// Returns [`VescCIfParseError`] on catalog or header validation failure.
    pub fn parse_vesc_c_if_groups_validated(
        catalog_root: &Path,
        bldc_root: Option<&Path>,
    ) -> Result<Vec<IndexEntry>, VescCIfParseError> {
        vesc_c_if::parse_catalog_with_header_validation(catalog_root, bldc_root)
    }

    /// Parse POC `abi_inventory` requirements from catalog YAML.
    ///
    /// # Errors
    ///
    /// Returns [`PocAbiParseError`] when the catalog file is missing or invalid.
    pub fn parse_abi_inventory(catalog_root: &Path) -> Result<Vec<IndexEntry>, PocAbiParseError> {
        poc_abi::parse_catalog(catalog_root)
    }

    /// Parse POC ABI requirements and optionally validate symbols against the upstream source.
    ///
    /// When `poc_root` is `Some`, every indexed symbol must appear in the primary
    /// `abi_inventory.rs` source file under that checkout.
    ///
    /// # Errors
    ///
    /// Returns [`PocAbiParseError`] on catalog or source validation failure.
    pub fn parse_abi_inventory_validated(
        catalog_root: &Path,
        poc_root: Option<&Path>,
    ) -> Result<Vec<IndexEntry>, PocAbiParseError> {
        poc_abi::parse_catalog_with_source_validation(catalog_root, poc_root)
    }

    /// Parse refloat command markdown docs from catalog YAML and upstream checkout.
    ///
    /// Reads doc paths under `{refloat_root}/doc/commands/` and extracts each title
    /// plus first paragraph as the entry summary.
    ///
    /// # Errors
    ///
    /// Returns [`RefloatCommandsParseError`] when the catalog or a referenced doc is missing.
    pub fn parse_refloat_commands(
        catalog_root: &Path,
        refloat_root: &Path,
    ) -> Result<Vec<IndexEntry>, RefloatCommandsParseError> {
        refloat_commands::parse_catalog(catalog_root, refloat_root)
    }
}
