//! Knowledge index builder from catalog sources.

use std::path::Path;

use crate::IndexEntry;
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
}
