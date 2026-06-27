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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkgdesc::vesc_tool::PkgDescVescTool;
    use crate::pkgdesc::{OutputFileName, PkgName, RelativeAssetPath};

    #[test]
    fn parsed_pkgdesc_reports_vesc_tool_dialect() {
        let parsed = ParsedPkgDesc::VescTool(PkgDescVescTool::new(
            PkgName::new("x"),
            RelativeAssetPath::new("README.md"),
            RelativeAssetPath::new("a.lisp"),
            RelativeAssetPath::new(""),
            OutputFileName::new("out.vescpkg"),
            false,
        ));
        assert_eq!(parsed.dialect(), PkgDescDialect::VescTool);
    }
}
