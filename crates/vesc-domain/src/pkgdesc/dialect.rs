//! Pkgdesc dialect identification.

/// Known pkgdesc.qml property dialects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PkgDescDialect {
    /// `vesc_tool` / refloat schema (`pkgName`, `pkgLisp`, …).
    VescTool,
    /// POC native-lib baseline schema (`packageName`, `nativeLibraryPath`, …).
    NativeLibBaseline,
}

/// Parsed pkgdesc content, tagged by dialect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedPkgDesc {
    VescTool(super::vesc_tool::PkgDescVescTool),
    NativeLib(super::native_lib::PkgDescNativeLib),
}

impl ParsedPkgDesc {
    #[must_use]
    pub const fn dialect(&self) -> PkgDescDialect {
        match self {
            Self::VescTool(_) => PkgDescDialect::VescTool,
            Self::NativeLib(_) => PkgDescDialect::NativeLibBaseline,
        }
    }
}
