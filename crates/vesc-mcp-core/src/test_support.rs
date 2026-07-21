//! Shared helpers for fixture-driven integration tests.

use std::path::{Path, PathBuf};

/// Temporary workspace directory that is removed when dropped.
pub struct TempWorkspace {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
}

impl Default for TempWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

impl TempWorkspace {
    #[must_use]
    pub fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path().to_path_buf();
        Self { _temp: temp, root }
    }
}

/// Workspace-root `tests/fixtures/` directory.
#[must_use]
pub fn fixtures_root() -> PathBuf {
    crate::workspace::fixtures_root()
}

/// Allowed sandbox roots covering all in-repo fixtures (unit tests).
#[must_use]
pub fn fixture_sandbox_roots() -> Vec<PathBuf> {
    vec![fixtures_root()]
}

/// Resolve a named fixture directory under [`fixtures_root`].
#[must_use]
pub fn fixture_path(name: &str) -> PathBuf {
    fixtures_root().join(name)
}

/// Resolve `vesc_tool` for optional golden-stability tests (`VESC_TOOL_PATH` or `vesc_tool` on PATH).
#[must_use]
pub fn resolve_vesc_tool_for_tests() -> Option<PathBuf> {
    use std::process::{Command, Stdio};

    use crate::config::McpConfig;

    let path = McpConfig::load().vesc_tool_path.clone();
    if path.is_file() {
        return Some(path);
    }
    match Command::new(&path)
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => Some(path),
        _ => None,
    }
}

/// Read a fixture file relative to a named fixture directory.
///
/// # Panics
///
/// Panics if the fixture file cannot be read.
#[must_use]
pub fn read_fixture_file(fixture: &str, relative: impl AsRef<Path>) -> String {
    let path = fixture_path(fixture).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("read fixture file {}: {err}", path.display());
    })
}

/// Return true when a referenced asset path is missing under a package root.
#[must_use]
pub fn asset_missing(root: &Path, relative: &Path) -> bool {
    !root.join(relative).is_file()
}

/// Three-repository managed knowledge fixture with release and tag refs.
#[cfg(feature = "managed-git")]
pub struct VersionedKnowledgeFixture {
    _temp: tempfile::TempDir,
    knowledge: crate::config::KnowledgeConfig,
    old_commit: String,
    tagged_commit: String,
}

#[cfg(feature = "managed-git")]
impl VersionedKnowledgeFixture {
    /// Build and synchronize a local three-repository fixture.
    pub async fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (remote, old_commit, tagged_commit) = versioned_fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let toml = format!(
            "[knowledge]\ndata_root = \"{}\"\n{}{}{}",
            data_root.display(),
            versioned_repository_toml("bldc"),
            versioned_repository_toml("refloat"),
            versioned_repository_toml("vesc_tool")
        );
        let knowledge = crate::config::McpConfig::from_toml(
            &toml,
            &crate::managed_repositories::DataRootInputs::default(),
        )
        .expect("knowledge config")
        .knowledge;
        let layout = crate::managed_repositories::KnowledgeDataLayout::new(
            knowledge.data_root.clone().expect("data root"),
        );
        let git = crate::managed_git::ManagedGitStore::new(layout);
        for repository in knowledge.repositories.iter() {
            git.sync_source(
                repository.id(),
                remote.to_str().expect("UTF-8 remote"),
                repository.default_ref(),
            )
            .await
            .expect("managed source sync");
        }
        Self {
            _temp: temp,
            knowledge,
            old_commit,
            tagged_commit,
        }
    }

    #[must_use]
    pub const fn knowledge(&self) -> &crate::config::KnowledgeConfig {
        &self.knowledge
    }

    #[must_use]
    pub fn old_commit(&self) -> &str {
        &self.old_commit
    }

    #[must_use]
    pub fn tagged_commit(&self) -> &str {
        &self.tagged_commit
    }

    #[must_use]
    pub fn data_root(&self) -> &Path {
        self.knowledge
            .data_root
            .as_ref()
            .expect("fixture data root")
            .as_path()
    }

    #[must_use]
    pub fn selection() -> serde_json::Value {
        serde_json::json!({
            "sources": {
                "bldc": "refs/heads/release_6_06",
                "vesc_tool": "refs/heads/release_6_06",
                "refloat": "refs/tags/v1.2.3"
            }
        })
    }
}

#[cfg(feature = "managed-git")]
fn versioned_git(cwd: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

#[cfg(feature = "managed-git")]
fn versioned_fixture_remote(root: &Path) -> (PathBuf, String, String) {
    let work = root.join("work");
    let remote = root.join("remote.git");
    std::fs::create_dir(&work).expect("work tree");
    versioned_git(&work, &["init", "-b", "main"]);
    std::fs::write(work.join("README.md"), "alphaunique old release\n").expect("old source");
    versioned_git(&work, &["add", "README.md"]);
    versioned_git(
        &work,
        &[
            "-c",
            "user.name=Test Author",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "old release",
        ],
    );
    let old = versioned_git(&work, &["rev-parse", "HEAD"]);
    versioned_git(&work, &["branch", "release_6_06", &old]);
    std::fs::write(work.join("README.md"), "betaunique refloat tag\n").expect("tagged source");
    versioned_git(
        &work,
        &[
            "-c",
            "user.name=Test Author",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-am",
            "refloat tag",
        ],
    );
    let tagged = versioned_git(&work, &["rev-parse", "HEAD"]);
    versioned_git(&work, &["tag", "v1.2.3", &tagged]);
    versioned_git(
        &work,
        &[
            "clone",
            "--bare",
            ".",
            remote.to_str().expect("UTF-8 remote"),
        ],
    );
    (remote, old, tagged)
}

#[cfg(feature = "managed-git")]
fn versioned_repository_toml(id: &str) -> String {
    format!(
        r#"
[[knowledge.repositories]]
id = "{id}"
remote_url = "https://example.invalid/{id}.git"
default_ref = "refs/heads/main"
policy = "required"
include = ["**/*.md"]
exclude = []
trust_tier = "official"
license = "MIT"
attribution = "Test fixture"
max_file_bytes = 1048576
max_files = 100
max_total_bytes = 10485760
"#
    )
}

/// In-process MCP server harness for integration tests.
#[derive(Debug, Clone)]
pub struct McpTestHarness {
    service: crate::VescMcpService,
    knowledge: crate::config::KnowledgeConfig,
    feedback: Option<crate::tools::knowledge_feedback::FeedbackStore>,
    feedback_writes_enabled: bool,
}

impl McpTestHarness {
    #[must_use]
    pub fn new() -> Self {
        // Keep integration tests independent of the user's daemon config and
        // any large managed-repository artifacts in the user data root.
        let knowledge = crate::config::KnowledgeConfig::default();
        Self {
            service: crate::VescMcpService::with_knowledge_config(knowledge.clone()),
            knowledge,
            feedback: None,
            feedback_writes_enabled: false,
        }
    }

    #[must_use]
    pub fn with_feedback_store(root: impl AsRef<Path>, writes_enabled: bool) -> Self {
        let root = root.as_ref();
        let store = crate::tools::knowledge_feedback::FeedbackStore::new(root);
        let knowledge = crate::config::KnowledgeConfig::default();
        Self {
            service: crate::VescMcpService::with_knowledge_config_and_feedback_store(
                knowledge.clone(),
                root,
                writes_enabled,
            ),
            knowledge,
            feedback: Some(store),
            feedback_writes_enabled: writes_enabled,
        }
    }

    #[must_use]
    pub fn with_knowledge_config(knowledge: crate::config::KnowledgeConfig) -> Self {
        Self {
            service: crate::VescMcpService::with_knowledge_config(knowledge.clone()),
            knowledge,
            feedback: None,
            feedback_writes_enabled: false,
        }
    }

    /// Seed one configured managed repository from a test fixture remote.
    #[cfg(feature = "managed-git")]
    pub async fn sync_managed_source(&self, id: &str, remote: &str) {
        let repository = self
            .knowledge
            .repositories
            .iter()
            .find(|repository| repository.id().as_str() == id)
            .unwrap_or_else(|| panic!("configured repository {id}"));
        let root = self.knowledge.data_root.clone().expect("managed data root");
        crate::managed_git::ManagedGitStore::new(
            crate::managed_repositories::KnowledgeDataLayout::new(root),
        )
        .sync_source(repository.id(), remote, repository.default_ref())
        .await
        .expect("managed source sync");
    }

    #[must_use]
    pub fn list_tool_names(&self) -> Vec<String> {
        self.service.list_tool_names()
    }

    /// Read a registered MCP resource through the service registry.
    ///
    /// # Panics
    ///
    /// Panics when the resource cannot be read.
    #[must_use]
    pub fn read_resource(&self, uri: &str) -> String {
        self.service
            .resource_registry()
            .read(uri)
            .unwrap_or_else(|error| panic!("read resource {uri}: {error}"))
    }

    fn call_feedback_tool(&self, name: &str, arguments: serde_json::Value) -> Option<String> {
        use crate::tools::knowledge_feedback::{
            CorrectVescKnowledgeParams, SubmitKnowledgeFeedbackParams,
            correct_vesc_knowledge_tool_with_store, submit_vesc_knowledge_feedback_with_store,
        };
        use crate::tools::search_knowledge::{
            CorrectionReplayReport, ReplayVescKnowledgeCorrectionParams,
            replay_vesc_knowledge_correction,
        };

        let response = match name {
            "submit_vesc_knowledge_feedback" => {
                let params: SubmitKnowledgeFeedbackParams = serde_json::from_value(arguments)
                    .expect("submit_vesc_knowledge_feedback requires its public request schema");
                serde_json::to_string(&submit_vesc_knowledge_feedback_with_store(
                    &params,
                    self.feedback.as_ref().expect("configured feedback store"),
                ))
                .expect("feedback response json")
            }
            "correct_vesc_knowledge" => {
                let params: CorrectVescKnowledgeParams = serde_json::from_value(arguments)
                    .expect("correct_vesc_knowledge requires its public request schema");
                serde_json::to_string(&correct_vesc_knowledge_tool_with_store(
                    &params,
                    self.feedback.as_ref().expect("configured feedback store"),
                    self.service.resource_registry(),
                ))
                .expect("correction response json")
            }
            "replay_vesc_knowledge_correction" => {
                let params: ReplayVescKnowledgeCorrectionParams = serde_json::from_value(arguments)
                    .expect("replay_vesc_knowledge_correction requires its public request schema");
                let report = if params.mark_covered && !self.feedback_writes_enabled {
                    CorrectionReplayReport::failure(
                        &params.correction_id,
                        String::new(),
                        "mark_covered requires enabled feedback writes".into(),
                    )
                } else if let Some(store) = &self.feedback {
                    replay_vesc_knowledge_correction(&params, &self.knowledge, store)
                } else {
                    CorrectionReplayReport::failure(
                        &params.correction_id,
                        String::new(),
                        "knowledge feedback is not configured".into(),
                    )
                };
                serde_json::to_string(&report).expect("replay response json")
            }
            _ => return None,
        };
        Some(response)
    }

    #[cfg(feature = "managed-git")]
    fn call_source_version_tool(&self, name: &str, arguments: serde_json::Value) -> Option<String> {
        use crate::tools::list_source_versions::{
            ListVescSourceVersionsParams, list_vesc_source_versions_json,
        };

        (name == "list_vesc_source_versions").then(|| {
            let params: ListVescSourceVersionsParams =
                serde_json::from_value(arguments).expect("source version filters");
            list_vesc_source_versions_json(&params, &self.knowledge)
        })
    }

    #[cfg(feature = "managed-git")]
    /// Call a tool that may perform asynchronous snapshot preparation.
    pub async fn call_tool_async(&self, name: &str, arguments: serde_json::Value) -> String {
        if name == "prepare_vesc_knowledge" {
            let params = serde_json::from_value(arguments).expect("knowledge source selection");
            return crate::tools::prepare_knowledge::prepare_vesc_knowledge_json(
                &params,
                &self.knowledge,
            )
            .await;
        }
        self.call_tool(name, arguments)
    }

    fn call_ping(&self, arguments: serde_json::Value) -> String {
        use crate::server::{
            PingParams, PingResponse, decide_ping_echo, knowledge_preparation_status,
        };

        let params: PingParams = serde_json::from_value(arguments).unwrap_or_default();
        serde_json::to_string(&PingResponse {
            ok: true,
            echo: decide_ping_echo(params.message),
            server: "vesc-mcp".into(),
            knowledge: knowledge_preparation_status(&self.knowledge),
        })
        .expect("ping response json")
    }

    /// Call a registered MCP tool and return the JSON text payload.
    ///
    /// Dispatches through the same tool handlers registered on [`crate::VescMcpService`].
    ///
    /// # Panics
    ///
    /// Panics when the tool name is unknown or arguments fail to deserialize.
    #[must_use]
    pub fn call_tool(&self, name: &str, arguments: serde_json::Value) -> String {
        use crate::tools::build::{
            BuildVescpkgParams, RealVescToolRunner, build_vescpkg_tool_with_runner,
        };
        use crate::tools::check::{
            RealPackageCheckRunner, RunPackageChecksParams, run_package_checks_tool_with_runner,
        };
        use crate::tools::inspect::{
            InspectPkgdescParams, InspectVescpkgParams, inspect_pkgdesc_with_sandbox,
            inspect_vescpkg_with_sandbox,
        };
        use crate::tools::list_packages::{ListPackagesParams, list_vesc_packages_json};
        use crate::tools::search_knowledge::{
            SearchVescKnowledgeParams, search_vesc_knowledge_json_with_feedback,
        };
        use crate::tools::validate::{
            ValidatePackageLayoutParams, validate_package_layout_tool_with_sandbox,
        };

        let sandbox = fixture_sandbox_roots();

        assert!(
            self.list_tool_names().iter().any(|tool| tool == name),
            "tool {name} is not registered; have {:?}",
            self.list_tool_names()
        );

        if let Some(response) = self.call_feedback_tool(name, arguments.clone()) {
            return response;
        }
        #[cfg(feature = "managed-git")]
        if let Some(response) = self.call_source_version_tool(name, arguments.clone()) {
            return response;
        }

        match name {
            "ping" => self.call_ping(arguments),
            "list_vesc_packages" => {
                let params: ListPackagesParams =
                    serde_json::from_value(arguments).unwrap_or_default();
                list_vesc_packages_json(&params)
            }
            "inspect_pkgdesc" => {
                let params: InspectPkgdescParams = serde_json::from_value(arguments)
                    .expect("inspect_pkgdesc requires { \"path\": \"...\" }");
                let response = inspect_pkgdesc_with_sandbox(&params.path, Some(&sandbox));
                serde_json::to_string(&response).expect("inspect_pkgdesc response json")
            }
            "inspect_vescpkg" => {
                let params: InspectVescpkgParams = serde_json::from_value(arguments)
                    .expect("inspect_vescpkg requires { \"path\": \"...\" }");
                let response = inspect_vescpkg_with_sandbox(&params.path, Some(&sandbox));
                serde_json::to_string(&response).expect("inspect_vescpkg response json")
            }
            "validate_package_layout" => {
                let params: ValidatePackageLayoutParams = serde_json::from_value(arguments)
                    .expect("validate_package_layout requires { \"root\": \"...\" }");
                let response =
                    validate_package_layout_tool_with_sandbox(&params.root, Some(&sandbox));
                serde_json::to_string(&response).expect("validate_package_layout response json")
            }
            "build_vescpkg" => {
                let params: BuildVescpkgParams = serde_json::from_value(arguments)
                    .expect("build_vescpkg requires { \"root\": \"...\" }");
                let response = build_vescpkg_tool_with_runner(
                    &params,
                    &RealVescToolRunner,
                    None,
                    Some(&sandbox),
                );
                serde_json::to_string(&response).expect("build_vescpkg response json")
            }
            "run_package_checks" => {
                let params: RunPackageChecksParams = serde_json::from_value(arguments)
                    .expect("run_package_checks requires { \"root\": \"...\" }");
                let response = run_package_checks_tool_with_runner(
                    &params.root,
                    &RealPackageCheckRunner,
                    Some(&sandbox),
                );
                serde_json::to_string(&response).expect("run_package_checks response json")
            }
            "search_vesc_knowledge" => {
                let params: SearchVescKnowledgeParams = serde_json::from_value(arguments)
                    .expect("search_vesc_knowledge requires { \"query\": \"...\" }");
                search_vesc_knowledge_json_with_feedback(
                    &params,
                    &self.knowledge,
                    self.feedback.as_ref(),
                    self.service.resource_registry(),
                )
            }
            other => panic!("missing harness dispatch for registered tool: {other}"),
        }
    }
}

impl Default for McpTestHarness {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_workspace_creates_empty_directory() {
        let workspace = TempWorkspace::new();
        assert!(workspace.root.is_dir());
        assert!(
            std::fs::read_dir(&workspace.root)
                .expect("read dir")
                .next()
                .is_none()
        );
    }

    #[test]
    fn fixture_path_resolves_refloat_minimal() {
        let path = fixture_path("refloat-minimal");
        assert!(path.join("pkgdesc.qml").is_file(), "{}", path.display());
    }

    #[test]
    fn read_fixture_file_loads_pkgdesc() {
        let content = read_fixture_file("refloat-minimal", "pkgdesc.qml");
        assert!(content.contains("Refloat Minimal"));
    }
}
