//! `run_package_checks` — run fmt/clippy/test in a sandboxed package root.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::config::{allowed_package_roots, validate_sandbox_path};

/// Default cargo checks when no Makefile targets are present.
const CARGO_PACKAGE_CHECKS: &[(&str, &str, &[&str])] = &[
    ("fmt", "cargo", &["fmt", "--all", "--check"]),
    (
        "clippy",
        "cargo",
        &[
            "clippy",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
    ),
    ("test", "cargo", &["test"]),
];

/// Make targets used when a root Makefile defines fmt/clippy/test.
const MAKE_PACKAGE_CHECKS: &[(&str, &str, &[&str])] = &[
    ("fmt", "make", &["fmt"]),
    ("clippy", "make", &["clippy"]),
    ("test", "make", &["test"]),
];

const MAKE_CHECK_TARGETS: &[&str] = &["fmt", "clippy", "test"];

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RunPackageChecksParams {
    /// Package root directory (must lie under `VESC_PACKAGE_ROOTS`).
    pub root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct CheckResult {
    pub name: String,
    pub passed: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct RunPackageChecksResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Subprocess backend for cargo checks (mockable in unit tests).
pub trait PackageCheckRunner {
    /// Run `program` with `args` using `root` as the working directory.
    fn run_check(&self, root: &Path, name: &str, program: &str, args: &[&str]) -> CheckResult;
}

/// Production runner that spawns real cargo subprocesses.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealPackageCheckRunner;

impl PackageCheckRunner for RealPackageCheckRunner {
    fn run_check(&self, root: &Path, name: &str, program: &str, args: &[&str]) -> CheckResult {
        let output = Command::new(program)
            .current_dir(root)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        match output {
            Ok(output) => CheckResult {
                name: name.into(),
                passed: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            Err(err) => CheckResult {
                name: name.into(),
                passed: false,
                stdout: String::new(),
                stderr: format!("spawn {program}: {err}"),
            },
        }
    }
}

#[must_use]
pub fn run_package_checks_tool(root: &str) -> RunPackageChecksResponse {
    run_package_checks_tool_with_runner(root, &RealPackageCheckRunner, None)
}

#[must_use]
pub fn run_package_checks_tool_with_runner(
    root: &str,
    runner: &dyn PackageCheckRunner,
    allowed_roots_override: Option<&[PathBuf]>,
) -> RunPackageChecksResponse {
    let root_path = PathBuf::from(root);
    let allowed_roots = allowed_package_roots(allowed_roots_override);

    let canonical_root = match validate_sandbox_path(&root_path, &allowed_roots) {
        Ok(path) => path,
        Err(err) => {
            return RunPackageChecksResponse {
                ok: false,
                checks: Vec::new(),
                error: Some(err),
            };
        }
    };

    let mut checks = Vec::with_capacity(CARGO_PACKAGE_CHECKS.len());
    for (name, program, args) in package_checks_for_root(&canonical_root) {
        checks.push(runner.run_check(&canonical_root, name, program, args));
    }

    let ok = checks.iter().all(|check| check.passed);
    RunPackageChecksResponse {
        ok,
        checks,
        error: None,
    }
}

fn package_checks_for_root(
    root: &Path,
) -> &'static [(&'static str, &'static str, &'static [&'static str])] {
    let makefile = root.join("Makefile");
    if !makefile.is_file() {
        return CARGO_PACKAGE_CHECKS;
    }
    let Ok(content) = std::fs::read_to_string(&makefile) else {
        return CARGO_PACKAGE_CHECKS;
    };
    if MAKE_CHECK_TARGETS
        .iter()
        .all(|target| makefile_has_target(&content, target))
    {
        MAKE_PACKAGE_CHECKS
    } else {
        CARGO_PACKAGE_CHECKS
    }
}

fn makefile_has_target(content: &str, target: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.split('#').next().unwrap_or("").trim();
        trimmed.starts_with(&format!("{target}:"))
            || (trimmed.starts_with(".PHONY:")
                && trimmed.split_whitespace().any(|token| token == target))
    })
}

/// Serialize a tool response as JSON text for rmcp handlers.
#[must_use]
pub fn run_package_checks_json(params: &RunPackageChecksParams) -> String {
    let response = run_package_checks_tool(&params.root);
    serde_json::to_string(&response)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempWorkspace, fixture_path};

    struct MockPackageCheckRunner {
        outputs: Vec<CheckResult>,
    }

    impl PackageCheckRunner for MockPackageCheckRunner {
        fn run_check(
            &self,
            _root: &Path,
            name: &str,
            _program: &str,
            _args: &[&str],
        ) -> CheckResult {
            self.outputs
                .iter()
                .find(|check| check.name == name)
                .cloned()
                .unwrap_or_else(|| CheckResult {
                    name: name.into(),
                    passed: true,
                    stdout: format!("mock {name} ok"),
                    stderr: String::new(),
                })
        }
    }

    #[test]
    fn tool_run_checks_on_fixture() {
        let root = fixture_path("refloat-minimal");
        let allowed = vec![fixture_path("")];
        let runner = MockPackageCheckRunner {
            outputs: vec![
                CheckResult {
                    name: "fmt".into(),
                    passed: true,
                    stdout: "fmt ok".into(),
                    stderr: String::new(),
                },
                CheckResult {
                    name: "clippy".into(),
                    passed: true,
                    stdout: "clippy ok".into(),
                    stderr: String::new(),
                },
                CheckResult {
                    name: "test".into(),
                    passed: true,
                    stdout: "test ok".into(),
                    stderr: String::new(),
                },
            ],
        };

        let response = run_package_checks_tool_with_runner(
            &root.display().to_string(),
            &runner,
            Some(&allowed),
        );

        assert!(response.ok, "error: {:?}", response.error);
        assert_eq!(response.checks.len(), 3);
        assert!(response.checks.iter().all(|check| check.passed));
        assert_eq!(response.checks[0].name, "fmt");
        assert_eq!(response.checks[1].name, "clippy");
        assert_eq!(response.checks[2].name, "test");
        assert!(response.error.is_none());
    }

    #[test]
    fn tool_run_checks_reports_failed_check() {
        let root = fixture_path("refloat-minimal");
        let allowed = vec![fixture_path("")];
        let runner = MockPackageCheckRunner {
            outputs: vec![CheckResult {
                name: "clippy".into(),
                passed: false,
                stdout: String::new(),
                stderr: "warning: unused".into(),
            }],
        };

        let response = run_package_checks_tool_with_runner(
            &root.display().to_string(),
            &runner,
            Some(&allowed),
        );

        assert!(!response.ok);
        assert_eq!(response.checks.len(), 3);
        let clippy = response
            .checks
            .iter()
            .find(|check| check.name == "clippy")
            .expect("clippy check");
        assert!(!clippy.passed);
        assert!(clippy.stderr.contains("unused"));
    }

    #[test]
    fn tool_run_checks_rejects_path_outside_roots() {
        let workspace = TempWorkspace::new();
        let allowed = vec![fixture_path("refloat-minimal")];

        let response = run_package_checks_tool_with_runner(
            &workspace.root.display().to_string(),
            &MockPackageCheckRunner { outputs: vec![] },
            Some(&allowed),
        );

        assert!(!response.ok);
        assert!(response.checks.is_empty());
        assert!(
            response
                .error
                .as_ref()
                .is_some_and(|err| err.contains("outside configured VESC_PACKAGE_ROOTS")),
            "error: {:?}",
            response.error
        );
    }

    #[test]
    fn tool_run_checks_rejects_when_no_roots_configured() {
        let root = fixture_path("refloat-minimal");
        let response = run_package_checks_tool_with_runner(
            &root.display().to_string(),
            &MockPackageCheckRunner { outputs: vec![] },
            Some(&[]),
        );

        assert!(!response.ok);
        assert!(
            response
                .error
                .as_ref()
                .is_some_and(|err| err.contains("VESC_PACKAGE_ROOTS")),
            "error: {:?}",
            response.error
        );
    }

    #[test]
    fn tool_run_checks_rejects_missing_directory() {
        let allowed = vec![fixture_path("")];
        let response = run_package_checks_tool_with_runner(
            "/nonexistent/vesc-mcp-package-root",
            &MockPackageCheckRunner { outputs: vec![] },
            Some(&allowed),
        );

        assert!(!response.ok);
        assert!(
            response
                .error
                .as_ref()
                .is_some_and(|err| err.contains("not a directory")),
            "error: {:?}",
            response.error
        );
    }

    #[test]
    fn path_within_root_rejects_prefix_collision() {
        use crate::config::path_within_root;

        let root = PathBuf::from("/tmp/vesc");
        let sibling = PathBuf::from("/tmp/vesc-other");
        assert!(!path_within_root(&sibling, &root));
        assert!(path_within_root(&root.join("pkg"), &root));
    }

    #[test]
    fn package_checks_prefers_makefile_when_targets_present() {
        let workspace = TempWorkspace::new();
        std::fs::write(
            workspace.root.join("Makefile"),
            ".PHONY: fmt clippy test\nfmt:\nclippy:\ntest:\n",
        )
        .expect("write makefile");

        let checks = package_checks_for_root(&workspace.root);
        assert_eq!(checks[0], ("fmt", "make", &["fmt"][..]));
        assert_eq!(checks[1], ("clippy", "make", &["clippy"][..]));
        assert_eq!(checks[2], ("test", "make", &["test"][..]));
    }

    #[test]
    fn package_checks_falls_back_to_cargo_without_makefile() {
        let workspace = TempWorkspace::new();
        let checks = package_checks_for_root(&workspace.root);
        assert_eq!(checks, CARGO_PACKAGE_CHECKS);
    }

    #[test]
    fn package_checks_falls_back_when_makefile_missing_targets() {
        let workspace = TempWorkspace::new();
        std::fs::write(workspace.root.join("Makefile"), "all:\n").expect("write makefile");
        let checks = package_checks_for_root(&workspace.root);
        assert_eq!(checks, CARGO_PACKAGE_CHECKS);
    }
}
