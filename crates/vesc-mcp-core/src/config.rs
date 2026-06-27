//! MCP server configuration from `~/.config/vesc-mcp/config.toml` and environment.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use crate::catalog::CatalogRepo;
use crate::workspace;

/// Environment variable for comma- or colon-separated package sandbox roots.
pub const VESC_PACKAGE_ROOTS_ENV: &str = "VESC_PACKAGE_ROOTS";
/// Environment variable overriding the `vesc_tool` binary path.
pub const VESC_TOOL_PATH_ENV: &str = "VESC_TOOL_PATH";
/// Environment variable gating flash/upload tools (default off).
pub const VESC_MCP_ENABLE_FLASH_ENV: &str = "VESC_MCP_ENABLE_FLASH";

static CONFIG: OnceLock<McpConfig> = OnceLock::new();

/// Resolved MCP server configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpConfig {
    pub package_roots: Vec<PathBuf>,
    pub refloat_root: PathBuf,
    pub bldc_root: PathBuf,
    pub poc_root: PathBuf,
    pub vesc_tool_root: PathBuf,
    pub vesc_tool_path: PathBuf,
    pub enable_flash: bool,
}

impl McpConfig {
    /// Load configuration once per process (env overrides file overrides defaults).
    #[must_use]
    pub fn load() -> &'static Self {
        CONFIG.get_or_init(Self::from_sources)
    }

    fn from_sources() -> Self {
        let file = read_config_file(&default_config_path());
        let config = merge_config(&file, &read_env_overrides());
        if config.package_roots.is_empty() {
            #[cfg(any(test, feature = "test-fixtures"))]
            {
                return Self {
                    package_roots: vec![
                        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures"),
                    ],
                    ..config
                };
            }
        }
        config
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ConfigFile {
    paths: Option<PathsSection>,
    features: Option<FeaturesSection>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PathsSection {
    package_roots: Option<Vec<String>>,
    refloat_root: Option<String>,
    bldc_root: Option<String>,
    poc_root: Option<String>,
    vesc_tool_root: Option<String>,
    vesc_tool: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FeaturesSection {
    enable_flash: Option<bool>,
}

#[derive(Debug, Clone, Default)]
struct EnvOverrides {
    package_roots: Option<Vec<PathBuf>>,
    refloat_root: Option<PathBuf>,
    bldc_root: Option<PathBuf>,
    poc_root: Option<PathBuf>,
    vesc_tool_root: Option<PathBuf>,
    vesc_tool_path: Option<PathBuf>,
    enable_flash: Option<bool>,
}

/// Default config file location: `~/.config/vesc-mcp/config.toml`.
#[must_use]
pub fn default_config_path() -> PathBuf {
    env::var("HOME").map_or_else(
        |_| PathBuf::from(".config/vesc-mcp/config.toml"),
        |home| PathBuf::from(home).join(".config/vesc-mcp/config.toml"),
    )
}

fn read_config_file(path: &Path) -> ConfigFile {
    let Ok(content) = std::fs::read_to_string(path) else {
        return ConfigFile::default();
    };
    toml::from_str(&content).unwrap_or_default()
}

fn read_env_overrides() -> EnvOverrides {
    EnvOverrides {
        package_roots: env::var(VESC_PACKAGE_ROOTS_ENV)
            .ok()
            .map(|value| split_path_list(&value)),
        refloat_root: env::var(CatalogRepo::Refloat.env_var())
            .ok()
            .map(PathBuf::from),
        bldc_root: env::var(CatalogRepo::Bldc.env_var())
            .ok()
            .map(PathBuf::from),
        poc_root: env::var(CatalogRepo::Poc.env_var()).ok().map(PathBuf::from),
        vesc_tool_root: env::var(CatalogRepo::VescTool.env_var())
            .ok()
            .map(PathBuf::from),
        vesc_tool_path: env::var(VESC_TOOL_PATH_ENV).ok().map(PathBuf::from),
        enable_flash: env::var(VESC_MCP_ENABLE_FLASH_ENV)
            .ok()
            .as_deref()
            .map(parse_enable_flash_env),
    }
}

fn merge_config(file: &ConfigFile, env: &EnvOverrides) -> McpConfig {
    let paths = file.paths.as_ref();
    let features = file.features.as_ref();

    let package_roots = env
        .package_roots
        .clone()
        .or_else(|| {
            paths.and_then(|section| {
                section.package_roots.as_ref().map(|roots| {
                    roots
                        .iter()
                        .map(|entry| workspace::expand_path(entry))
                        .collect()
                })
            })
        })
        .unwrap_or_default();

    McpConfig {
        package_roots,
        refloat_root: env.refloat_root.clone().unwrap_or_else(|| {
            paths
                .and_then(|section| section.refloat_root.as_deref())
                .map_or_else(
                    || CatalogRepo::Refloat.resolve_root(),
                    workspace::expand_path,
                )
        }),
        bldc_root: env.bldc_root.clone().unwrap_or_else(|| {
            paths
                .and_then(|section| section.bldc_root.as_deref())
                .map_or_else(|| CatalogRepo::Bldc.resolve_root(), workspace::expand_path)
        }),
        poc_root: env.poc_root.clone().unwrap_or_else(|| {
            paths
                .and_then(|section| section.poc_root.as_deref())
                .map_or_else(|| CatalogRepo::Poc.resolve_root(), workspace::expand_path)
        }),
        vesc_tool_root: env.vesc_tool_root.clone().unwrap_or_else(|| {
            paths
                .and_then(|section| section.vesc_tool_root.as_deref())
                .map_or_else(
                    || CatalogRepo::VescTool.resolve_root(),
                    workspace::expand_path,
                )
        }),
        vesc_tool_path: env.vesc_tool_path.clone().unwrap_or_else(|| {
            paths
                .and_then(|section| section.vesc_tool.as_deref())
                .map_or_else(|| PathBuf::from("vesc_tool"), workspace::expand_path)
        }),
        enable_flash: env
            .enable_flash
            .or_else(|| features.and_then(|section| section.enable_flash))
            .unwrap_or(false),
    }
}

/// Resolve package roots from explicit tool params or loaded config.
#[must_use]
pub fn resolve_package_roots(explicit: &[String], config: &McpConfig) -> Vec<PathBuf> {
    if !explicit.is_empty() {
        return explicit.iter().map(PathBuf::from).collect();
    }
    config.package_roots.clone()
}

/// Split a comma- or colon-separated path list (env `VESC_PACKAGE_ROOTS` format).
#[must_use]
pub fn split_path_list(value: &str) -> Vec<PathBuf> {
    value
        .split([',', ':'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(workspace::expand_path)
        .collect()
}

#[must_use]
pub fn expand_tilde(path: &str) -> PathBuf {
    workspace::expand_path(path)
}

fn parse_enable_flash_env(value: &str) -> bool {
    matches!(
        value.trim(),
        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
    )
}

/// Resolve allowed roots from an override or loaded config.
#[must_use]
pub fn allowed_package_roots(override_roots: Option<&[PathBuf]>) -> Vec<PathBuf> {
    override_roots.map_or_else(
        || McpConfig::load().package_roots.clone(),
        <[PathBuf]>::to_vec,
    )
}

/// Validate that `path` is a directory under one of the configured package roots.
///
/// # Errors
///
/// Returns an error when no roots are configured, the path is not a directory,
/// canonicalization fails, or the path lies outside all allowed roots.
pub fn validate_sandbox_path(path: &Path, allowed_roots: &[PathBuf]) -> Result<PathBuf, String> {
    if allowed_roots.is_empty() {
        return Err(format!(
            "path sandbox: set {VESC_PACKAGE_ROOTS_ENV} to allow package roots (comma-separated paths)"
        ));
    }

    if !path.is_dir() {
        return Err(format!("path is not a directory: {}", path.display()));
    }

    let canonical = path
        .canonicalize()
        .map_err(|err| format!("resolve path {}: {err}", path.display()))?;

    for allowed in allowed_roots {
        let Ok(canonical_allowed) = allowed.canonicalize() else {
            continue;
        };
        if path_within_root(&canonical, &canonical_allowed) {
            return Ok(canonical);
        }
    }

    Err(format!(
        "path {} is outside configured {VESC_PACKAGE_ROOTS_ENV}",
        path.display()
    ))
}

/// Validate any filesystem path (file or directory) lies under configured roots.
///
/// # Errors
///
/// Returns an error when no roots are configured, the path does not exist,
/// canonicalization fails, or the path lies outside all allowed roots.
pub fn validate_sandbox_file(path: &Path, allowed_roots: &[PathBuf]) -> Result<PathBuf, String> {
    if allowed_roots.is_empty() {
        return Err(format!(
            "path sandbox: set {VESC_PACKAGE_ROOTS_ENV} to allow package roots (comma-separated paths)"
        ));
    }

    if !path.exists() {
        return Err(format!("path does not exist: {}", path.display()));
    }

    let canonical = path
        .canonicalize()
        .map_err(|err| format!("resolve path {}: {err}", path.display()))?;

    for allowed in allowed_roots {
        let Ok(canonical_allowed) = allowed.canonicalize() else {
            continue;
        };
        if path_within_root(&canonical, &canonical_allowed) {
            return Ok(canonical);
        }
    }

    Err(format!(
        "path {} is outside configured {VESC_PACKAGE_ROOTS_ENV}",
        path.display()
    ))
}

/// True when `path` is equal to or nested under `root` (prefix-safe).
#[must_use]
pub fn path_within_root(path: &Path, root: &Path) -> bool {
    let mut root_components = root.components();
    for component in path.components() {
        match root_components.next() {
            Some(expected) if expected == component => {}
            Some(_) => return false,
            None => return true,
        }
    }
    root_components.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempWorkspace, fixture_path};

    #[test]
    fn config_resolves_roots_from_env() {
        let merged = merge_config(
            &ConfigFile::default(),
            &EnvOverrides {
                package_roots: Some(split_path_list("/tmp/pkg-a,/tmp/pkg-b")),
                ..EnvOverrides::default()
            },
        );
        assert_eq!(
            merged.package_roots,
            vec![PathBuf::from("/tmp/pkg-a"), PathBuf::from("/tmp/pkg-b")]
        );
    }

    #[test]
    fn config_resolves_roots_from_toml() {
        let file: ConfigFile = toml::from_str(
            r#"
[paths]
package_roots = ["/data/refloat", "/data/poc"]
vesc_tool = "/usr/local/bin/vesc_tool"

[features]
enable_flash = true
"#,
        )
        .expect("parse example toml");

        let merged = merge_config(&file, &EnvOverrides::default());
        assert_eq!(
            merged.package_roots,
            vec![PathBuf::from("/data/refloat"), PathBuf::from("/data/poc")]
        );
        assert_eq!(
            merged.vesc_tool_path,
            PathBuf::from("/usr/local/bin/vesc_tool")
        );
        assert!(merged.enable_flash);
    }

    #[test]
    fn config_env_overrides_toml() {
        let file: ConfigFile = toml::from_str(
            r#"
[paths]
package_roots = ["/from/file"]
"#,
        )
        .expect("parse toml");

        let merged = merge_config(
            &file,
            &EnvOverrides {
                package_roots: Some(vec![PathBuf::from("/from/env")]),
                ..EnvOverrides::default()
            },
        );
        assert_eq!(merged.package_roots, vec![PathBuf::from("/from/env")]);
    }

    #[test]
    fn config_rejects_paths_outside_sandbox() {
        let allowed = vec![fixture_path("refloat-minimal")];
        let workspace = TempWorkspace::new();

        let err = validate_sandbox_path(&workspace.root, &allowed).expect_err("outside roots");
        assert!(err.contains("outside configured VESC_PACKAGE_ROOTS"));
    }

    #[test]
    fn config_accepts_path_within_sandbox() {
        let allowed = vec![fixture_path("")];
        let root = fixture_path("refloat-minimal");

        let canonical =
            validate_sandbox_path(&root, &allowed).expect("fixture root should be allowed");
        assert!(canonical.is_dir());
    }

    #[test]
    fn split_path_list_accepts_colon_separator() {
        assert_eq!(
            split_path_list("/tmp/a:/tmp/b"),
            vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]
        );
    }

    #[test]
    fn path_within_root_rejects_prefix_collision() {
        let root = PathBuf::from("/tmp/vesc");
        let sibling = PathBuf::from("/tmp/vesc-other");
        assert!(!path_within_root(&sibling, &root));
        assert!(path_within_root(&root.join("pkg"), &root));
    }

    #[test]
    fn enable_flash_defaults_off() {
        let merged = merge_config(&ConfigFile::default(), &EnvOverrides::default());
        assert!(!merged.enable_flash);
    }
}
