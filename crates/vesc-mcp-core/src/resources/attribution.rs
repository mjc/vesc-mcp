//! Source attribution footer template for MCP resource bodies.
//!
//! Footer format:
//! ```text
//!
//! ---
//! Source: {repo}/{path}#L{line}
//! ```

use std::fmt::Write as _;

/// One `Source:` line in a resource attribution footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRef {
    pub repo: String,
    pub path: String,
    pub line: Option<u64>,
}

impl SourceRef {
    #[must_use]
    pub fn new(repo: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            path: path.into(),
            line: None,
        }
    }

    #[must_use]
    pub const fn with_line(mut self, line: u64) -> Self {
        self.line = Some(line);
        self
    }

    #[must_use]
    pub fn literal(location: impl Into<String>) -> Self {
        let location = location.into();
        Self {
            repo: String::new(),
            path: location,
            line: None,
        }
    }
}

/// Format a single `Source:` line (no leading newline).
#[must_use]
pub fn format_source_line(repo: &str, path: &str, line: Option<u64>) -> String {
    match line {
        Some(line_no) => format!("Source: {repo}/{path}#L{line_no}"),
        None if repo.is_empty() => format!("Source: {path}"),
        None => format!("Source: {repo}/{path}"),
    }
}

/// Append the markdown attribution footer (`\n\n---\n` + source lines) to `out`.
pub fn append_source_footer(out: &mut String, sources: &[SourceRef]) {
    if sources.is_empty() {
        return;
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    let _ = writeln!(out, "---");
    for source in sources {
        let line = format_source_line(&source.repo, &source.path, source.line);
        let _ = writeln!(out, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_source_line_with_line_anchor() {
        assert_eq!(
            format_source_line("refloat", "Makefile", Some(42)),
            "Source: refloat/Makefile#L42"
        );
    }

    #[test]
    fn format_source_line_without_line_anchor() {
        assert_eq!(
            format_source_line("refloat", "Makefile", None),
            "Source: refloat/Makefile"
        );
    }

    #[test]
    fn format_source_line_literal_catalog_path() {
        assert_eq!(
            format_source_line("", "catalog/refloat/build-flow.yaml", None),
            "Source: catalog/refloat/build-flow.yaml"
        );
    }

    #[test]
    fn append_source_footer_renders_separator_and_lines() {
        let mut out = "# Title\n".to_owned();
        append_source_footer(
            &mut out,
            &[
                SourceRef::new("refloat", "Makefile"),
                SourceRef::literal("catalog/refloat/build-flow.yaml"),
            ],
        );
        assert!(out.contains("\n---\n"));
        assert!(out.contains("Source: refloat/Makefile"));
        assert!(out.contains("Source: catalog/refloat/build-flow.yaml"));
    }
}
