//! Pkgdesc dialect identification.

/// Canonical pkgdesc.qml property dialect (`vesc_tool` / refloat).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PkgDescDialect {
    /// `vesc_tool` schema (`pkgName`, `pkgLisp`, …).
    VescTool,
}

/// Parsed pkgdesc content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedPkgDesc {
    VescTool(super::vesc_tool::PkgDescVescTool),
}

impl ParsedPkgDesc {
    #[must_use]
    pub const fn dialect(&self) -> PkgDescDialect {
        match self {
            Self::VescTool(_) => PkgDescDialect::VescTool,
        }
    }
}
