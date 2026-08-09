use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing_subscriber::EnvFilter;
use vesc_mcp_core::config::{McpConfig, SemanticIngestionProvider};
use vesc_mcp_core::managed_git::ManagedGitStore;
use vesc_mcp_core::managed_repositories::{DataRoot, KnowledgeDataLayout, RepositoryPolicy};
use vesc_mcp_core::managed_snapshots::{
    KnowledgeSnapshotStore, SnapshotBuildPhase, SnapshotDisposition, SnapshotState,
};
use vesc_mcp_core::preparation_status::{
    KnowledgePreparationStatus, KnowledgeRepositoryPublicationStatus, PreparationPhase,
    PreparationState, ValidatedVectorArtifact, read_preparation_status, write_preparation_status,
};
use vesc_mcp_core::server::KnowledgePreparation;
use vesc_mcp_core::tools::prepare_knowledge::{
    CACHED_ONLY_ARG, PREPARE_KNOWLEDGE_CHILD_ARG, PrepareKnowledgeChildRequest,
    knowledge_config_fingerprint, prepare_cached_vesc_knowledge_json,
    prepare_vesc_knowledge_failure_json, prepare_vesc_knowledge_json,
};

const PROFILE_INITIAL_TRAINING_ARG: &str = "--profile-initial-training";
const INSTALL_DISTRIBUTED_CACHE_ARG: &str = "--install-distributed-cache";
const REPOSITORY_PREPARATION_TIMEOUT_ARG: &str = "--repository-preparation-timeout-secs";
const REFRESH_ON_STARTUP_ARG: &str = "--refresh-on-startup";
const EAGER_INDEX_ARG: &str = "--eager-index";
const REFRESH_INTERVAL_ARG: &str = "--refresh-interval-secs";
const WATCH_DISTRIBUTED_CACHE_ARG: &str = "--watch-distributed-cache";
const PUBLISH_DISTRIBUTED_CACHE_ARG: &str = "--publish-distributed-cache";
const DISTRIBUTED_PUBLICATION_MARKER: &str = ".vesc-mcp-publication-complete";
const DEFAULT_REPOSITORY_PREPARATION_TIMEOUT_SECS: u64 = 900;

struct PreparationReporter {
    data_root: PathBuf,
    repositories_total: usize,
    repositories_completed: usize,
    freshness_required: bool,
    validated_vector: Option<ValidatedVectorArtifact>,
    finished: bool,
}

impl PreparationReporter {
    fn new(data_root: PathBuf, repositories_total: usize, freshness_required: bool) -> Self {
        let validated_vector =
            read_preparation_status(&data_root).and_then(|status| status.validated_vector);
        let reporter = Self {
            data_root,
            repositories_total,
            repositories_completed: 0,
            freshness_required,
            validated_vector,
            finished: false,
        };
        reporter.publish(&KnowledgePreparationStatus::preparing(
            PreparationPhase::Starting,
            0,
            repositories_total,
        ));
        reporter
    }

    fn repositories_synchronized(&mut self, completed: usize) {
        self.repositories_completed = completed;
        self.publish(&KnowledgePreparationStatus::preparing(
            PreparationPhase::SynchronizingRepositories,
            completed,
            self.repositories_total,
        ));
    }

    fn planning_history(&mut self) {
        self.repositories_completed = self.repositories_total;
        self.publish(&KnowledgePreparationStatus::preparing(
            PreparationPhase::PlanningHistory,
            self.repositories_completed,
            self.repositories_total,
        ));
    }

    fn snapshot_progress_reporter(&self) -> impl Fn(SnapshotBuildPhase) + Send + Sync + 'static {
        let data_root = self.data_root.clone();
        let repositories_completed = self.repositories_completed;
        let repositories_total = self.repositories_total;
        let freshness_required = self.freshness_required;
        move |phase| {
            let status = KnowledgePreparationStatus::preparing(
                preparation_phase_for_build(phase),
                repositories_completed,
                repositories_total,
            )
            .with_freshness_required(freshness_required);
            if let Err(error) = write_preparation_status(&data_root, &status) {
                tracing::warn!(%error, "could not publish knowledge preparation status");
            }
        }
    }

    fn finish(&mut self, state: PreparationState) {
        self.publish(&KnowledgePreparationStatus::finished(
            state,
            self.repositories_completed,
            self.repositories_total,
        ));
        self.finished = true;
    }

    fn validated_vector(&mut self, artifact_root: &Path) {
        self.validated_vector = vesc_mcp_core::preparation_status::validated_vector(artifact_root);
    }

    fn snapshot_available(&self, snapshot: &str) {
        let status = KnowledgePreparationStatus::finished(
            PreparationState::Ready,
            self.repositories_completed,
            self.repositories_total,
        )
        .with_publication(
            Some(snapshot.to_owned()),
            Some(snapshot.to_owned()),
            Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_secs()),
            ),
            None,
        );
        self.publish(&status);
    }

    fn publish(&self, status: &KnowledgePreparationStatus) {
        let prior = read_preparation_status(&self.data_root);
        let status = status
            .clone()
            .with_freshness_required(self.freshness_required)
            .with_validated_vector(self.validated_vector.clone())
            .with_publication(
                prior
                    .as_ref()
                    .and_then(|value| value.active_snapshot.clone()),
                prior
                    .as_ref()
                    .and_then(|value| value.available_snapshot.clone()),
                prior
                    .as_ref()
                    .and_then(|value| value.last_refresh_unix_secs),
                prior.as_ref().and_then(|value| value.last_error.clone()),
            )
            .with_repositories(prior.map_or_else(BTreeMap::new, |value| value.repositories));
        if let Err(error) = write_preparation_status(&self.data_root, &status) {
            tracing::warn!(%error, "could not publish knowledge preparation status");
        }
    }
}

const fn preparation_phase_for_build(phase: SnapshotBuildPhase) -> PreparationPhase {
    match phase {
        SnapshotBuildPhase::PlanningHistory => PreparationPhase::PlanningHistory,
        SnapshotBuildPhase::BuildingLexicalIndex => PreparationPhase::BuildingLexicalIndex,
        SnapshotBuildPhase::BuildingSemanticIndex => PreparationPhase::BuildingSemanticIndex,
        SnapshotBuildPhase::Publishing => PreparationPhase::Publishing,
    }
}

impl Drop for PreparationReporter {
    fn drop(&mut self) {
        if !self.finished {
            self.publish(&KnowledgePreparationStatus::finished(
                PreparationState::Failed,
                self.repositories_completed,
                self.repositories_total,
            ));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeProfile {
    Preparation,
    Serving { worker_threads: usize },
}

impl RuntimeProfile {
    fn from_args(args: &[String]) -> Self {
        if args.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "--refresh-repositories"
                    | PREPARE_KNOWLEDGE_CHILD_ARG
                    | INSTALL_DISTRIBUTED_CACHE_ARG
            )
        }) || args.iter().any(|arg| arg == PROFILE_INITIAL_TRAINING_ARG)
        {
            Self::Preparation
        } else {
            Self::Serving { worker_threads: 2 }
        }
    }

    fn build(self) -> std::io::Result<tokio::runtime::Runtime> {
        let mut builder = match self {
            Self::Preparation => tokio::runtime::Builder::new_multi_thread(),
            Self::Serving { worker_threads } => {
                let mut builder = tokio::runtime::Builder::new_multi_thread();
                builder.worker_threads(worker_threads);
                builder
            }
        };
        builder.enable_all().build()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StartupPolicy {
    refresh: bool,
    eager_index: bool,
    allow_offline_restart: bool,
    knowledge_preparation: KnowledgePreparation,
}

impl StartupPolicy {
    fn from_args(args: &[String]) -> Self {
        let cached_only = args.iter().any(|arg| arg == CACHED_ONLY_ARG);
        Self {
            refresh: !cached_only
                && args.iter().any(|arg| {
                    matches!(
                        arg.as_str(),
                        "--refresh-repositories" | REFRESH_ON_STARTUP_ARG
                    )
                })
                && !args.iter().any(|arg| arg == "--skip-repository-refresh"),
            eager_index: !cached_only && args.iter().any(|arg| arg == EAGER_INDEX_ARG),
            allow_offline_restart: !args.iter().any(|arg| arg == "--require-fresh-repositories"),
            knowledge_preparation: if cached_only {
                KnowledgePreparation::CachedOnly
            } else {
                KnowledgePreparation::BuildMissing
            },
        }
    }
}

const fn policy_for_available_data(
    policy: StartupPolicy,
    current_snapshot_ready: bool,
) -> StartupPolicy {
    if current_snapshot_ready
        || matches!(
            policy.knowledge_preparation,
            KnowledgePreparation::CachedOnly
        )
    {
        policy
    } else {
        StartupPolicy {
            refresh: true,
            eager_index: true,
            allow_offline_restart: policy.allow_offline_restart,
            knowledge_preparation: KnowledgePreparation::BuildMissing,
        }
    }
}

fn migraphx_cache_path(
    data_root: Option<&DataRoot>,
    provider: Option<SemanticIngestionProvider>,
) -> Option<PathBuf> {
    match (data_root, provider) {
        (Some(root), Some(SemanticIngestionProvider::Migraphx)) => {
            Some(root.as_path().join("migraphx-cache"))
        }
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn configure_migraphx_cache() -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;

    const CACHE_ENV: &str = "ORT_MIGRAPHX_MODEL_CACHE_PATH";
    if env::var_os(CACHE_ENV).is_some() {
        return Ok(());
    }
    let config = McpConfig::load();
    let provider = config
        .knowledge
        .semantic_ingestion
        .as_ref()
        .map(|ingestion| ingestion.provider);
    let Some(path) = migraphx_cache_path(config.knowledge.data_root.as_ref(), provider) else {
        return Ok(());
    };
    fs::create_dir_all(&path)?;
    let error = std::process::Command::new(env::current_exe()?)
        .args(env::args_os().skip(1))
        .env(CACHE_ENV, path)
        .exec();
    Err(error.into())
}

#[cfg(not(target_os = "linux"))]
fn configure_migraphx_cache() -> anyhow::Result<()> {
    Ok(())
}

fn main() -> anyhow::Result<()> {
    configure_migraphx_cache()?;
    let args = env::args().skip(1).collect::<Vec<_>>();
    RuntimeProfile::from_args(&args)
        .build()?
        .block_on(async_main(args))
}

async fn async_main(args: Vec<String>) -> anyhow::Result<()> {
    if args.iter().any(|arg| arg == "--benchmark-search") {
        run_benchmark(&args)?;
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    if args.iter().any(|arg| arg == INSTALL_DISTRIBUTED_CACHE_ARG) {
        let staged_root = argument_value(&args, INSTALL_DISTRIBUTED_CACHE_ARG)
            .ok_or_else(|| anyhow::anyhow!("{INSTALL_DISTRIBUTED_CACHE_ARG} requires a path"))?;
        println!(
            "installed distributed knowledge snapshot {}",
            install_distributed_cache(Path::new(staged_root.as_str()),)?
        );
        return Ok(());
    }

    let startup_policy = StartupPolicy::from_args(&args);
    let refresh_interval = refresh_interval(&args)?;
    let watch_distributed_cache = distributed_cache_watch_path(&args)?;
    let publish_distributed_cache = publish_distributed_cache(&args)?;
    if args.iter().any(|arg| arg == "--refresh-repositories") {
        synchronize_managed_repositories(startup_policy).await?;
        return Ok(());
    }
    if args.iter().any(|arg| arg == PROFILE_INITIAL_TRAINING_ARG) {
        #[cfg(feature = "coz-profile")]
        {
            synchronize_managed_repositories(StartupPolicy {
                refresh: false,
                eager_index: true,
                allow_offline_restart: true,
                knowledge_preparation: KnowledgePreparation::BuildMissing,
            })
            .await?;
            return Ok(());
        }
        #[cfg(not(feature = "coz-profile"))]
        anyhow::bail!("initial-training profiling is unavailable in this build");
    }
    if args.iter().any(|arg| arg == PREPARE_KNOWLEDGE_CHILD_ARG) {
        let request =
            serde_json::from_reader::<_, PrepareKnowledgeChildRequest>(std::io::stdin().lock())?;
        let config = McpConfig::load();
        if request.config_fingerprint != knowledge_config_fingerprint(&config.knowledge) {
            println!(
                "{}",
                prepare_vesc_knowledge_failure_json(
                    "knowledge configuration changed; restart the MCP service"
                )
            );
            return Ok(());
        }
        let response = match startup_policy.knowledge_preparation {
            KnowledgePreparation::BuildMissing => {
                prepare_vesc_knowledge_json(&request.params, &config.knowledge).await
            }
            KnowledgePreparation::CachedOnly => {
                prepare_cached_vesc_knowledge_json(&request.params, &config.knowledge).await
            }
        };
        println!("{response}");
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--http") {
        let refresh_args = repository_refresh_args(&args);
        initialize_managed_repository_preparation(&refresh_args)?;
        spawn_background_refresh(
            refresh_interval,
            startup_policy,
            publish_distributed_cache.clone(),
        );
        spawn_distributed_cache_watcher(watch_distributed_cache, refresh_interval);
        run_http(
            vesc_mcp_server::http::HttpServerConfig::from_env(),
            synchronize_managed_repositories_in_child(
                refresh_args,
                publish_distributed_cache.clone(),
            ),
            startup_policy.knowledge_preparation,
        )
        .await?;
        return Ok(());
    }

    let refresh_args = repository_refresh_args(&args);
    initialize_managed_repository_preparation(&refresh_args)?;
    spawn_background_refresh(
        refresh_interval,
        startup_policy,
        publish_distributed_cache.clone(),
    );
    spawn_distributed_cache_watcher(watch_distributed_cache, refresh_interval);
    tokio::spawn(async move {
        if let Err(error) =
            synchronize_managed_repositories_in_child(refresh_args, publish_distributed_cache).await
        {
            tracing::error!(%error, "managed repository preparation failed");
        }
    });
    vesc_mcp_core::server::run_stdio_server_with_knowledge_preparation(
        startup_policy.knowledge_preparation,
    )
    .await
}

async fn run_http<F>(
    config: vesc_mcp_server::http::HttpServerConfig,
    preparation: F,
    knowledge_preparation: KnowledgePreparation,
) -> anyhow::Result<()>
where
    F: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let server = vesc_mcp_server::http::bind(config).await?;
    tokio::spawn(async move {
        if let Err(error) = preparation.await {
            tracing::error!(%error, "managed repository preparation failed");
        }
    });
    server
        .serve_with_knowledge_preparation(knowledge_preparation)
        .await
}

fn refresh_interval(args: &[String]) -> anyhow::Result<Option<Duration>> {
    let requested = args.iter().any(|arg| arg == REFRESH_INTERVAL_ARG);
    let Some(value) = argument_value(args, REFRESH_INTERVAL_ARG) else {
        if requested {
            anyhow::bail!("{REFRESH_INTERVAL_ARG} requires a value");
        }
        return Ok(None);
    };
    let seconds = value
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("{REFRESH_INTERVAL_ARG} must be an integer"))?;
    if seconds == 0 {
        anyhow::bail!("{REFRESH_INTERVAL_ARG} must be greater than zero");
    }
    Ok(Some(Duration::from_secs(seconds)))
}

fn distributed_cache_watch_path(args: &[String]) -> anyhow::Result<Option<PathBuf>> {
    let requested = args.iter().any(|arg| arg == WATCH_DISTRIBUTED_CACHE_ARG);
    let Some(value) = argument_value(args, WATCH_DISTRIBUTED_CACHE_ARG) else {
        if requested {
            anyhow::bail!("{WATCH_DISTRIBUTED_CACHE_ARG} requires a path");
        }
        return Ok(None);
    };
    Ok(Some(PathBuf::from(value)))
}

fn publish_distributed_cache(args: &[String]) -> anyhow::Result<Option<PathBuf>> {
    let requested = args.iter().any(|arg| arg == PUBLISH_DISTRIBUTED_CACHE_ARG);
    let Some(value) = argument_value(args, PUBLISH_DISTRIBUTED_CACHE_ARG) else {
        if requested {
            anyhow::bail!("{PUBLISH_DISTRIBUTED_CACHE_ARG} requires a path");
        }
        return Ok(None);
    };
    Ok(Some(PathBuf::from(value)))
}

fn spawn_background_refresh(
    interval: Option<Duration>,
    startup: StartupPolicy,
    publish_target: Option<PathBuf>,
) {
    let Some(interval) = interval else { return };
    if matches!(
        startup.knowledge_preparation,
        KnowledgePreparation::CachedOnly
    ) {
        return;
    }
    tokio::spawn(async move {
        let policy = StartupPolicy {
            refresh: true,
            eager_index: true,
            allow_offline_restart: startup.allow_offline_restart,
            knowledge_preparation: KnowledgePreparation::BuildMissing,
        };
        loop {
            tokio::time::sleep(interval).await;
            if let Err(error) = synchronize_managed_repositories(policy).await {
                tracing::warn!(%error, "background knowledge refresh failed; retaining prior generation");
            } else if let Some(target) = &publish_target
                && let Err(error) = publish_distributed_bundle(target)
            {
                tracing::warn!(%error, "distributed knowledge publication failed; retaining prior target");
            }
        }
    });
}

fn spawn_distributed_cache_watcher(staged_root: Option<PathBuf>, interval: Option<Duration>) {
    let (Some(staged_root), Some(interval)) = (staged_root, interval) else {
        return;
    };
    tokio::spawn(async move {
        let mut last_staged = None;
        loop {
            tokio::time::sleep(interval).await;
            let staged_id = staged_manifest_id(&staged_root);
            if staged_id.is_some() && staged_id == last_staged {
                continue;
            }
            let staged_root = staged_root.clone();
            let result =
                tokio::task::spawn_blocking(move || install_distributed_cache(&staged_root))
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!("distributed cache watcher join failed: {error}")
                    })
                    .and_then(|result| result);
            match result {
                Ok(snapshot) => {
                    last_staged = Some(snapshot.clone());
                    publish_distributed_status(Some(snapshot.clone()), None);
                    tracing::info!(%snapshot, "activated distributed knowledge snapshot");
                }
                Err(error) => {
                    publish_distributed_status(None, Some(error.to_string()));
                    tracing::warn!(%error, "distributed cache refresh failed; retaining prior generation");
                }
            }
        }
    });
}

fn manifest_id(root: &Path) -> Option<String> {
    let bytes = fs::read(root.join("default-snapshot-corpus-1.1.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    manifest.get("id")?.as_str().map(str::to_owned)
}

fn staged_manifest_id(staged_root: &Path) -> Option<String> {
    let id = manifest_id(staged_root)?;
    fs::read_to_string(staged_root.join(DISTRIBUTED_PUBLICATION_MARKER))
        .ok()
        .filter(|marker| marker.trim() == id)
        .map(|_| id)
}

fn published_manifest_id(root: &Path) -> Option<String> {
    let id = manifest_id(root)?;
    (fs::read_to_string(root.join(DISTRIBUTED_PUBLICATION_MARKER))
        .ok()?
        .trim()
        == id)
        .then_some(id)
}

fn publish_distributed_status(snapshot: Option<String>, error: Option<String>) {
    let config = McpConfig::load();
    let Some(data_root) = config.knowledge.data_root.as_ref() else {
        return;
    };
    let prior = read_preparation_status(data_root.as_path());
    let active = snapshot.clone().or_else(|| {
        prior
            .as_ref()
            .and_then(|status| status.active_snapshot.clone())
    });
    let available = snapshot.or_else(|| {
        prior
            .as_ref()
            .and_then(|status| status.available_snapshot.clone())
    });
    let state = if error.is_some() {
        PreparationState::Stale
    } else {
        PreparationState::Ready
    };
    let repositories = KnowledgeSnapshotStore::new(KnowledgeDataLayout::new(data_root.clone()))
        .default_manifest()
        .ok()
        .map(|manifest| {
            manifest
                .repositories
                .into_iter()
                .map(|repository| {
                    (
                        repository.repository.to_string(),
                        KnowledgeRepositoryPublicationStatus {
                            serving_revision: Some(repository.commit.clone()),
                            available_revision: Some(repository.commit),
                            last_refresh_unix_secs: error.is_none().then(|| {
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map_or(0, |duration| duration.as_secs())
                            }),
                            failure_reason: error.clone(),
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .or_else(|| prior.as_ref().map(|status| status.repositories.clone()))
        .unwrap_or_default();
    let status =
        KnowledgePreparationStatus::finished(state, 0, config.knowledge.repositories.enabled_len())
            .with_publication(
                active,
                available,
                error.is_none().then(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_secs())
                }),
                error.map(|mut reason| {
                    reason.truncate(512);
                    reason
                }),
            )
            .with_repositories(repositories);
    if let Err(write_error) = write_preparation_status(data_root.as_path(), &status) {
        tracing::warn!(%write_error, "could not publish distributed knowledge status");
    }
}

/// Publish a complete immutable data-root bundle with a final marker commit.
/// The watcher ignores the target while that marker is absent, so an
/// interrupted copy leaves the previous live generation untouched.
fn publish_distributed_bundle(target: &Path) -> anyhow::Result<()> {
    let config = McpConfig::load();
    let source = config
        .knowledge
        .data_root
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("distributed publication requires a data root"))?
        .as_path();
    if source == target || source.starts_with(target) || target.starts_with(source) {
        anyhow::bail!("distributed publication target must not overlap the source data root");
    }
    let source_id = manifest_id(source)
        .ok_or_else(|| anyhow::anyhow!("source data root has no valid snapshot manifest"))?;
    publish_bundle(source, target, &source_id)
}

fn publish_bundle(source: &Path, target: &Path, source_id: &str) -> anyhow::Result<()> {
    if let Some(target_id) = published_manifest_id(target)
        && source_id == target_id
    {
        return Ok(());
    }
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("distributed publication target has no parent"))?;
    fs::create_dir_all(parent)?;
    let staged = parent.join(format!(".vesc-mcp-publish-{}", std::process::id()));
    if staged.exists() {
        fs::remove_dir_all(&staged)?;
    }
    let staged_stats = copy_serving_bundle(source, &staged, source_id)?;
    tracing::info!(
        snapshot = source_id,
        files = staged_stats.files,
        bytes = staged_stats.bytes,
        "prepared bounded distributed publication bundle"
    );
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    fs::create_dir_all(target)?;
    let marker = target.join(DISTRIBUTED_PUBLICATION_MARKER);
    let _ = fs::remove_file(&marker);
    let mut target_stats = PublicationCopyStats::default();
    copy_directory_tree(&staged, target, true, &mut target_stats)?;
    let manifest = target.join("default-snapshot-corpus-1.1.json");
    let staged_manifest = staged.join("default-snapshot-corpus-1.1.json");
    let manifest_tmp = target.join(".default-snapshot-corpus-1.1.json.tmp");
    fs::copy(staged_manifest, &manifest_tmp)?;
    fs::rename(manifest_tmp, manifest)?;
    let marker_tmp = target.join(format!("{DISTRIBUTED_PUBLICATION_MARKER}.tmp"));
    fs::write(&marker_tmp, format!("{source_id}\n"))?;
    fs::rename(marker_tmp, marker)?;
    fs::remove_dir_all(staged)?;
    Ok(())
}

#[derive(Debug, Default)]
struct PublicationCopyStats {
    files: u64,
    bytes: u64,
}

fn copy_serving_bundle(
    source: &Path,
    target: &Path,
    source_id: &str,
) -> anyhow::Result<PublicationCopyStats> {
    fs::create_dir_all(target)?;
    let mut stats = PublicationCopyStats::default();
    copy_required_file(
        &source.join("default-snapshot-corpus-1.1.json"),
        &target.join("default-snapshot-corpus-1.1.json"),
        &mut stats,
    )?;
    copy_required_file(
        &source.join("snapshots").join(format!("{source_id}.json")),
        &target.join("snapshots").join(format!("{source_id}.json")),
        &mut stats,
    )?;
    copy_required_directory(
        &source.join("repositories"),
        &target.join("repositories"),
        &mut stats,
    )?;
    copy_required_directory(
        &source.join("artifacts").join(source_id),
        &target.join("artifacts").join(source_id),
        &mut stats,
    )?;
    Ok(stats)
}

fn copy_required_file(
    source: &Path,
    target: &Path,
    stats: &mut PublicationCopyStats,
) -> anyhow::Result<()> {
    if !source.is_file() {
        anyhow::bail!(
            "distributed publication is missing required file {}",
            source.display()
        );
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    stats.bytes += fs::copy(source, target)?;
    stats.files += 1;
    Ok(())
}

fn copy_required_directory(
    source: &Path,
    target: &Path,
    stats: &mut PublicationCopyStats,
) -> anyhow::Result<()> {
    if !source.is_dir() {
        anyhow::bail!(
            "distributed publication is missing required directory {}",
            source.display()
        );
    }
    copy_directory_tree(source, target, false, stats)
}

fn copy_directory_tree(
    source: &Path,
    target: &Path,
    skip_publication_files: bool,
    stats: &mut PublicationCopyStats,
) -> anyhow::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name().to_str() == Some(DISTRIBUTED_PUBLICATION_MARKER)
            || (skip_publication_files
                && entry.file_name().to_str() == Some("default-snapshot-corpus-1.1.json"))
        {
            continue;
        }
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if source_path.is_dir() {
            copy_directory_tree(&source_path, &target_path, skip_publication_files, stats)?;
        } else {
            stats.bytes += fs::copy(source_path, target_path)?;
            stats.files += 1;
        }
    }
    Ok(())
}

fn install_distributed_cache(staged_root: &Path) -> anyhow::Result<String> {
    let config = McpConfig::load();
    let live_root =
        config.knowledge.data_root.clone().ok_or_else(|| {
            anyhow::anyhow!("distributed cache installation requires a data root")
        })?;
    let installed = KnowledgeSnapshotStore::new(KnowledgeDataLayout::new(live_root))
        .with_semantic_config(&config.knowledge)?
        .install_distributed_default(
            DataRoot::new(staged_root.to_owned())?,
            &config.knowledge.repositories,
        )?;
    Ok(installed.manifest.id.to_string())
}

fn repository_refresh_args(args: &[String]) -> Vec<String> {
    let mut refresh_args = vec!["--refresh-repositories".to_owned()];
    if !args.iter().any(|arg| arg == REFRESH_ON_STARTUP_ARG) {
        refresh_args.push("--skip-repository-refresh".to_owned());
    }
    refresh_args.extend(
        args.iter()
            .filter(|arg| {
                matches!(
                    arg.as_str(),
                    REFRESH_ON_STARTUP_ARG
                        | EAGER_INDEX_ARG
                        | CACHED_ONLY_ARG
                        | "--require-fresh-repositories"
                )
            })
            .cloned(),
    );
    if args
        .iter()
        .any(|arg| arg == REPOSITORY_PREPARATION_TIMEOUT_ARG)
    {
        refresh_args.push(REPOSITORY_PREPARATION_TIMEOUT_ARG.to_owned());
        if let Some(value) = argument_value(args, REPOSITORY_PREPARATION_TIMEOUT_ARG) {
            refresh_args.push(value);
        }
    }
    refresh_args
}

fn repository_preparation_timeout(args: &[String]) -> anyhow::Result<Duration> {
    let Some(value) = argument_value(args, REPOSITORY_PREPARATION_TIMEOUT_ARG) else {
        if args
            .iter()
            .any(|arg| arg == REPOSITORY_PREPARATION_TIMEOUT_ARG)
        {
            anyhow::bail!("{REPOSITORY_PREPARATION_TIMEOUT_ARG} requires a value");
        }
        return Ok(Duration::from_secs(
            DEFAULT_REPOSITORY_PREPARATION_TIMEOUT_SECS,
        ));
    };
    let seconds = value
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("{REPOSITORY_PREPARATION_TIMEOUT_ARG} must be an integer"))?;
    if seconds == 0 {
        anyhow::bail!("{REPOSITORY_PREPARATION_TIMEOUT_ARG} must be greater than zero");
    }
    Ok(Duration::from_secs(seconds))
}

fn initialize_managed_repository_preparation(args: &[String]) -> anyhow::Result<()> {
    repository_preparation_timeout(args)?;
    let config = McpConfig::load();
    if !config.knowledge.manages_repositories() {
        return Ok(());
    }
    let data_root = config
        .knowledge
        .data_root
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("managed repositories require a data root"))?;
    let status = KnowledgePreparationStatus::preparing(
        PreparationPhase::Starting,
        0,
        config.knowledge.repositories.enabled_len(),
    )
    .with_freshness_required(!StartupPolicy::from_args(args).allow_offline_restart)
    .with_validated_vector(
        read_preparation_status(data_root.as_path()).and_then(|status| status.validated_vector),
    );
    write_preparation_status(data_root.as_path(), &status)
}

async fn synchronize_managed_repositories_in_child(
    args: Vec<String>,
    publish_target: Option<PathBuf>,
) -> anyhow::Result<()> {
    let config = McpConfig::load();
    if !config.knowledge.manages_repositories() {
        return Ok(());
    }
    let timeout = repository_preparation_timeout(&args)?;
    let freshness_required = !StartupPolicy::from_args(&args).allow_offline_restart;
    let data_root = config.knowledge.data_root.as_ref();
    let repositories_total = config.knowledge.repositories.enabled_len();
    let executable = match env::current_exe() {
        Ok(executable) => executable,
        Err(error) => {
            publish_child_preparation_failure(data_root, repositories_total, freshness_required);
            return Err(error.into());
        }
    };
    let mut command = tokio::process::Command::new(executable);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            publish_child_preparation_failure(data_root, repositories_total, freshness_required);
            return Err(error.into());
        }
    };
    let result = supervise_preparation_child(
        &mut child,
        timeout,
        data_root,
        repositories_total,
        freshness_required,
    )
    .await;
    if result.is_ok()
        && let Some(target) = publish_target
        && let Err(error) = publish_distributed_bundle(&target)
    {
        tracing::warn!(%error, "initial distributed knowledge publication failed; retaining prior target");
    }
    result
}

async fn supervise_preparation_child(
    child: &mut tokio::process::Child,
    timeout: Duration,
    data_root: Option<&DataRoot>,
    repositories_total: usize,
    freshness_required: bool,
) -> anyhow::Result<()> {
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => {
            publish_child_preparation_failure(data_root, repositories_total, freshness_required);
            anyhow::bail!("managed repository preparation process exited with {status}");
        }
        Ok(Err(error)) => {
            let termination = kill_and_reap_preparation_child(child).await;
            publish_child_preparation_failure(data_root, repositories_total, freshness_required);
            termination.map_err(|termination_error| {
                anyhow::anyhow!(
                    "managed repository preparation wait failed: {error}; \
                     failed to terminate child: {termination_error}"
                )
            })?;
            Err(error.into())
        }
        Err(_) => {
            let termination = kill_and_reap_preparation_child(child).await;
            publish_child_preparation_failure(data_root, repositories_total, freshness_required);
            termination.map_err(|error| {
                anyhow::anyhow!(
                    "managed repository preparation timed out after {timeout:?}; \
                     failed to terminate child: {error}"
                )
            })?;
            anyhow::bail!("managed repository preparation timed out after {timeout:?}");
        }
    }
}

async fn kill_and_reap_preparation_child(child: &mut tokio::process::Child) -> std::io::Result<()> {
    if child.try_wait()?.is_none() {
        child.kill().await?;
    }
    Ok(())
}

fn publish_child_preparation_failure(
    data_root: Option<&DataRoot>,
    repositories_total: usize,
    freshness_required: bool,
) {
    let Some(data_root) = data_root else {
        return;
    };
    let completed = read_preparation_status(data_root.as_path())
        .map_or(0, |status| status.repositories_completed)
        .min(repositories_total);
    let status = KnowledgePreparationStatus::finished(
        PreparationState::Failed,
        completed,
        repositories_total,
    )
    .with_freshness_required(freshness_required);
    if let Err(error) = write_preparation_status(data_root.as_path(), &status) {
        tracing::warn!(%error, "could not publish failed child preparation status");
    }
}

#[allow(clippy::too_many_lines)]
async fn synchronize_managed_repositories(policy: StartupPolicy) -> anyhow::Result<()> {
    let config = vesc_mcp_core::config::McpConfig::load();
    if !config.knowledge.manages_repositories() {
        return Ok(());
    }
    let data_root = config
        .knowledge
        .data_root
        .clone()
        .ok_or_else(|| anyhow::anyhow!("managed repositories require a data root"))?;
    let mut reporter = PreparationReporter::new(
        data_root.as_path().to_owned(),
        config.knowledge.repositories.enabled_len(),
        !policy.allow_offline_restart,
    );
    let layout = KnowledgeDataLayout::new(data_root);
    let snapshot_store =
        KnowledgeSnapshotStore::new(layout.clone()).with_semantic_config(&config.knowledge)?;
    let current_snapshot_ready = snapshot_store.default_manifest().is_ok_and(|manifest| {
        snapshot_store
            .default_configuration_is_current(&config.knowledge.repositories)
            .unwrap_or(false)
            && snapshot_store.status(&manifest.id) == SnapshotState::Ready
    });
    let policy = policy_for_available_data(policy, current_snapshot_ready);
    let mut used_stale_sources = !policy.refresh;
    let mut unavailable_optional = BTreeSet::new();
    if policy.refresh {
        let store = ManagedGitStore::new(layout.clone());
        for (completed, (id, result)) in store
            .startup_sync(&config.knowledge.repositories)
            .await
            .into_iter()
            .enumerate()
        {
            match result {
                Ok(outcome) => {
                    if let Some(warning) = outcome.warning {
                        if !policy.allow_offline_restart {
                            return Err(anyhow::anyhow!(
                                "repository {id} refresh failed and offline restart is disabled: {warning}"
                            ));
                        }
                        used_stale_sources = true;
                        tracing::warn!(repository = %id, %warning, "using stale managed repository catalog");
                    } else {
                        tracing::info!(
                            repository = %id,
                            disposition = ?outcome.disposition,
                            refs = outcome.catalog.refs.len(),
                            "synchronized managed repository"
                        );
                    }
                }
                Err(error) => {
                    let required = config
                        .knowledge
                        .repositories
                        .iter()
                        .find(|repository| repository.id() == &id)
                        .is_some_and(|repository| {
                            repository.policy() == RepositoryPolicy::Required
                        });
                    if required {
                        return Err(anyhow::anyhow!("required repository {id} failed: {error}"));
                    }
                    unavailable_optional.insert(id.clone());
                    tracing::warn!(repository = %id, %error, "optional managed repository unavailable");
                }
            }
            reporter.repositories_synchronized(completed.saturating_add(1));
        }
    }
    if !policy.eager_index {
        reporter.finish(lazy_preparation_terminal_state(current_snapshot_ready));
        return Ok(());
    }

    reporter.planning_history();
    let progress = reporter.snapshot_progress_reporter();
    let repositories = &config.knowledge.repositories;
    let prewarm = config
        .knowledge
        .prewarm
        .iter()
        .map(|selection| {
            selection
                .iter()
                .filter(|(id, _)| !unavailable_optional.contains(*id))
                .map(|(id, selector)| (id.clone(), selector.clone()))
                .collect()
        })
        .collect::<Vec<_>>();
    let prepared = snapshot_store
        .with_progress_reporter(progress)
        .prepare_configured(repositories, &prewarm)
        .await?;
    if prepared.default.disposition == SnapshotDisposition::Stale {
        if !policy.allow_offline_restart {
            return Err(anyhow::anyhow!(
                "default knowledge snapshot is stale and offline restart is disabled"
            ));
        }
        tracing::warn!(
            snapshot = %prepared.default.manifest.id.as_str(),
            "using stale default knowledge snapshot"
        );
    } else {
        tracing::info!(
            snapshot = %prepared.default.manifest.id.as_str(),
            disposition = ?prepared.default.disposition,
            "prepared default knowledge snapshot"
        );
    }
    for snapshot in prepared.prewarmed {
        tracing::info!(
            snapshot = %snapshot.manifest.id.as_str(),
            disposition = ?snapshot.disposition,
            "prepared historical knowledge snapshot"
        );
    }
    reporter.validated_vector(&prepared.default.artifact_path);
    reporter.snapshot_available(prepared.default.manifest.id.as_str());
    reporter.finish(terminal_preparation_state(
        used_stale_sources,
        prepared.default.disposition,
    ));
    Ok(())
}

const fn terminal_preparation_state(
    used_stale_sources: bool,
    disposition: SnapshotDisposition,
) -> PreparationState {
    if used_stale_sources || matches!(disposition, SnapshotDisposition::Stale) {
        PreparationState::Stale
    } else {
        PreparationState::Ready
    }
}

/// Report the state of a serving generation when preparation intentionally skips indexing.
#[must_use]
const fn lazy_preparation_terminal_state(current_snapshot_ready: bool) -> PreparationState {
    if current_snapshot_ready {
        PreparationState::Ready
    } else {
        PreparationState::Stale
    }
}

fn run_benchmark(args: &[String]) -> anyhow::Result<()> {
    let suite = argument_value(args, "--suite").map_or_else(
        || PathBuf::from("tests/evaluation/v1/queries.json"),
        PathBuf::from,
    );
    let raw = fs::read_to_string(&suite)?;
    let values: Vec<serde_json::Value> = serde_json::from_str(&raw)?;
    let queries: Vec<String> = values
        .into_iter()
        .filter_map(|value| {
            value
                .get("text")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    let warmup = argument_value(args, "--warmup")
        .as_deref()
        .unwrap_or("3")
        .parse::<usize>()?;
    let repetitions = argument_value(args, "--repetitions")
        .as_deref()
        .unwrap_or("10")
        .parse::<usize>()?;
    let mode = match argument_value(args, "--mode")
        .as_deref()
        .unwrap_or("lexical")
    {
        "lexical" => vesc_mcp_core::tools::search_knowledge::SearchMode::Lexical,
        "hybrid" => vesc_mcp_core::tools::search_knowledge::SearchMode::Hybrid,
        "auto" => vesc_mcp_core::tools::search_knowledge::SearchMode::Auto,
        other => {
            anyhow::bail!("unsupported benchmark mode {other:?}; use lexical, hybrid, or auto")
        }
    };
    let format = argument_value(args, "--format").unwrap_or_else(|| "text".into());
    let mut config = vesc_mcp_core::config::McpConfig::load().knowledge.clone();
    if let Some(artifact) = argument_value(args, "--artifact") {
        config.artifact_path = Some(PathBuf::from(artifact));
    }
    let report = vesc_mcp_core::benchmark::benchmark_search_mode(
        &config,
        &queries,
        warmup,
        repetitions,
        mode,
    )?;
    match format.as_str() {
        "json" => println!("{}", serde_json::to_string_pretty(&report)?),
        "text" => {
            println!("mode: {:?}", report.mode);
            println!(
                "machine: {} {} target={}",
                report.machine.os, report.machine.arch, report.machine.rust_target
            );
            println!(
                "iterations: warmup={} repetitions={} queries={}",
                report.warmup_iterations, report.repetitions, report.query_count
            );
            let timing = &report.handler_and_serialization;
            println!(
                "mcp-handler-json: samples={} min-us={} p50-us={} p95-us={} max-us={}",
                timing.samples, timing.min_us, timing.p50_us, timing.p95_us, timing.max_us
            );
            let bytes = &report.response_bytes;
            println!(
                "response-bytes: samples={} min={} p50={} p95={} max={}",
                bytes.samples, bytes.min_bytes, bytes.p50_bytes, bytes.p95_bytes, bytes.max_bytes
            );
            println!(
                "rss-retained-bytes: before={:?} after={:?} delta={:?}",
                report.rss_before_queries_bytes,
                report.rss_after_queries_bytes,
                report.rss_retained_delta_bytes
            );
            for warning in &report.warnings {
                println!("warning: {warning}");
            }
        }
        other => anyhow::bail!("unsupported benchmark format {other:?}; use text or json"),
    }
    Ok(())
}

fn argument_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use vesc_mcp_core::config::SemanticIngestionProvider;
    use vesc_mcp_core::managed_repositories::DataRoot;
    use vesc_mcp_core::managed_snapshots::{SnapshotBuildPhase, SnapshotDisposition};
    use vesc_mcp_core::preparation_status::PreparationPhase;
    use vesc_mcp_core::server::KnowledgePreparation;

    use super::{
        CACHED_ONLY_ARG, DISTRIBUTED_PUBLICATION_MARKER, EAGER_INDEX_ARG,
        INSTALL_DISTRIBUTED_CACHE_ARG, PROFILE_INITIAL_TRAINING_ARG, PUBLISH_DISTRIBUTED_CACHE_ARG,
        PreparationReporter, REFRESH_INTERVAL_ARG, REFRESH_ON_STARTUP_ARG,
        REPOSITORY_PREPARATION_TIMEOUT_ARG, RuntimeProfile, StartupPolicy,
        WATCH_DISTRIBUTED_CACHE_ARG, distributed_cache_watch_path, lazy_preparation_terminal_state,
        migraphx_cache_path, policy_for_available_data, preparation_phase_for_build,
        publish_bundle, publish_child_preparation_failure, publish_distributed_cache,
        published_manifest_id, refresh_interval, repository_preparation_timeout,
        repository_refresh_args, run_http, staged_manifest_id, supervise_preparation_child,
        terminal_preparation_state,
    };

    #[test]
    fn migraphx_cache_uses_the_configured_data_root() {
        let root = DataRoot::new(PathBuf::from("/var/lib/vesc-mcp")).expect("absolute data root");

        assert_eq!(
            migraphx_cache_path(Some(&root), Some(SemanticIngestionProvider::Migraphx)),
            Some(PathBuf::from("/var/lib/vesc-mcp/migraphx-cache"))
        );
        assert_eq!(
            migraphx_cache_path(Some(&root), Some(SemanticIngestionProvider::Cpu)),
            None
        );
    }

    #[test]
    fn startup_policy_defaults_to_reusing_existing_data() {
        assert_eq!(
            StartupPolicy::from_args(&[]),
            StartupPolicy {
                refresh: false,
                eager_index: false,
                allow_offline_restart: true,
                knowledge_preparation: KnowledgePreparation::BuildMissing,
            }
        );
    }

    #[test]
    fn startup_policy_flags_enable_work_or_require_fresh_sources() {
        let args = [
            REFRESH_ON_STARTUP_ARG.to_owned(),
            EAGER_INDEX_ARG.to_owned(),
            "--require-fresh-repositories".to_owned(),
        ];

        assert_eq!(
            StartupPolicy::from_args(&args),
            StartupPolicy {
                refresh: true,
                eager_index: true,
                allow_offline_restart: false,
                knowledge_preparation: KnowledgePreparation::BuildMissing,
            }
        );
    }

    #[test]
    fn missing_data_forces_initial_refresh_and_index() {
        assert_eq!(
            policy_for_available_data(StartupPolicy::from_args(&[]), false),
            StartupPolicy {
                refresh: true,
                eager_index: true,
                allow_offline_restart: true,
                knowledge_preparation: KnowledgePreparation::BuildMissing,
            }
        );
    }

    #[test]
    fn existing_data_preserves_lazy_startup_policy() {
        let policy = StartupPolicy::from_args(&[]);
        assert_eq!(policy_for_available_data(policy, true), policy);
    }

    #[test]
    fn cached_only_never_bootstraps_missing_data() {
        let policy = StartupPolicy::from_args(&["--cached-only".into()]);
        assert_eq!(policy_for_available_data(policy, false), policy);
    }

    #[test]
    fn cached_only_disables_requested_background_work() {
        assert_eq!(
            StartupPolicy::from_args(&[
                CACHED_ONLY_ARG.into(),
                REFRESH_ON_STARTUP_ARG.into(),
                EAGER_INDEX_ARG.into(),
            ]),
            StartupPolicy {
                refresh: false,
                eager_index: false,
                allow_offline_restart: true,
                knowledge_preparation: KnowledgePreparation::CachedOnly,
            }
        );
    }

    #[test]
    fn build_phases_map_to_precise_live_preparation_status() {
        assert_eq!(
            preparation_phase_for_build(SnapshotBuildPhase::BuildingLexicalIndex),
            PreparationPhase::BuildingLexicalIndex
        );
        assert_eq!(
            preparation_phase_for_build(SnapshotBuildPhase::BuildingSemanticIndex),
            PreparationPhase::BuildingSemanticIndex
        );
        assert_eq!(
            preparation_phase_for_build(SnapshotBuildPhase::Publishing),
            PreparationPhase::Publishing
        );
    }

    #[test]
    fn preparation_is_multithreaded_and_serving_is_bounded() {
        assert_eq!(
            RuntimeProfile::from_args(&["--refresh-repositories".into()]),
            RuntimeProfile::Preparation
        );
        assert_eq!(
            RuntimeProfile::Preparation
                .build()
                .expect("preparation runtime")
                .handle()
                .runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread
        );
        assert_eq!(
            RuntimeProfile::from_args(&["--prepare-knowledge".into()]),
            RuntimeProfile::Preparation
        );
        assert_eq!(
            RuntimeProfile::from_args(&[PROFILE_INITIAL_TRAINING_ARG.into()]),
            RuntimeProfile::Preparation
        );
        assert_eq!(
            RuntimeProfile::from_args(&[INSTALL_DISTRIBUTED_CACHE_ARG.into(), "/stage".into()]),
            RuntimeProfile::Preparation
        );
        assert_eq!(
            RuntimeProfile::from_args(&["--http".into()]),
            RuntimeProfile::Serving { worker_threads: 2 }
        );
        assert_eq!(
            RuntimeProfile::from_args(&[]),
            RuntimeProfile::Serving { worker_threads: 2 }
        );
    }

    #[test]
    fn http_preparation_child_forwards_only_startup_policy() {
        let args = [
            "--http".to_owned(),
            "--benchmark-search".to_owned(),
            REFRESH_ON_STARTUP_ARG.to_owned(),
            EAGER_INDEX_ARG.to_owned(),
            CACHED_ONLY_ARG.to_owned(),
            "--require-fresh-repositories".to_owned(),
            REPOSITORY_PREPARATION_TIMEOUT_ARG.to_owned(),
            "42".to_owned(),
        ];

        assert_eq!(
            repository_refresh_args(&args),
            [
                "--refresh-repositories",
                REFRESH_ON_STARTUP_ARG,
                EAGER_INDEX_ARG,
                CACHED_ONLY_ARG,
                "--require-fresh-repositories",
                REPOSITORY_PREPARATION_TIMEOUT_ARG,
                "42",
            ]
        );
    }

    #[test]
    fn default_preparation_child_skips_refresh_and_eager_indexing() {
        assert_eq!(
            repository_refresh_args(&["--http".to_owned()]),
            ["--refresh-repositories", "--skip-repository-refresh"]
        );
    }

    #[test]
    fn repository_preparation_timeout_is_bounded_and_configurable() {
        assert_eq!(
            repository_preparation_timeout(&[]).expect("default timeout"),
            std::time::Duration::from_mins(15)
        );
        assert_eq!(
            repository_preparation_timeout(&[
                REPOSITORY_PREPARATION_TIMEOUT_ARG.to_owned(),
                "42".to_owned(),
            ])
            .expect("configured timeout"),
            std::time::Duration::from_secs(42)
        );
        assert!(
            repository_preparation_timeout(&[
                REPOSITORY_PREPARATION_TIMEOUT_ARG.to_owned(),
                "0".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn refresh_interval_is_opt_in_and_rejects_invalid_values() {
        assert_eq!(refresh_interval(&[]).expect("no interval"), None);
        assert_eq!(
            refresh_interval(&[REFRESH_INTERVAL_ARG.into(), "42".into()]).expect("interval"),
            Some(std::time::Duration::from_secs(42))
        );
        assert!(refresh_interval(&[REFRESH_INTERVAL_ARG.into(), "0".into()]).is_err());
        assert!(refresh_interval(&[REFRESH_INTERVAL_ARG.into()]).is_err());
    }

    #[test]
    fn distributed_cache_watcher_requires_a_staging_path() {
        assert_eq!(distributed_cache_watch_path(&[]).expect("no watcher"), None);
        assert_eq!(
            distributed_cache_watch_path(&[WATCH_DISTRIBUTED_CACHE_ARG.into(), "/stage".into()])
                .expect("staging path"),
            Some(PathBuf::from("/stage"))
        );
        assert!(distributed_cache_watch_path(&[WATCH_DISTRIBUTED_CACHE_ARG.into()]).is_err());
    }

    #[test]
    fn distributed_cache_publisher_requires_a_target() {
        assert_eq!(publish_distributed_cache(&[]).expect("no publisher"), None);
        assert_eq!(
            publish_distributed_cache(&[
                PUBLISH_DISTRIBUTED_CACHE_ARG.into(),
                "/srv/vesc-cache".into()
            ])
            .expect("publisher target"),
            Some(PathBuf::from("/srv/vesc-cache"))
        );
        assert!(publish_distributed_cache(&[PUBLISH_DISTRIBUTED_CACHE_ARG.into()]).is_err());
    }

    #[test]
    fn distributed_cache_watcher_requires_a_matching_publication_marker() {
        let root = tempfile::tempdir().expect("temporary root");
        std::fs::write(
            root.path().join("default-snapshot-corpus-1.1.json"),
            r#"{"id":"snapshot-1"}"#,
        )
        .expect("manifest");
        assert_eq!(staged_manifest_id(root.path()), None);
        assert_eq!(published_manifest_id(root.path()), None);
        std::fs::write(root.path().join(DISTRIBUTED_PUBLICATION_MARKER), "wrong\n")
            .expect("mismatched publication marker");
        assert_eq!(staged_manifest_id(root.path()), None);
        std::fs::write(
            root.path().join(DISTRIBUTED_PUBLICATION_MARKER),
            "snapshot-1\n",
        )
        .expect("publication marker");
        assert_eq!(
            staged_manifest_id(root.path()).as_deref(),
            Some("snapshot-1")
        );
        assert_eq!(
            published_manifest_id(root.path()).as_deref(),
            Some("snapshot-1")
        );
    }

    #[test]
    fn distributed_bundle_publishes_new_manifest_before_activation_marker() {
        let root = tempfile::tempdir().expect("temporary root");
        let source = root.path().join("source");
        let target = root.path().join("target");
        std::fs::create_dir_all(&source).expect("source");
        std::fs::create_dir_all(&target).expect("target");
        std::fs::write(
            source.join("default-snapshot-corpus-1.1.json"),
            r#"{"id":"new"}"#,
        )
        .expect("source manifest");
        std::fs::create_dir_all(source.join("snapshots")).expect("snapshots");
        std::fs::write(source.join("snapshots/new.json"), "snapshot").expect("snapshot");
        std::fs::create_dir_all(source.join("artifacts/new")).expect("artifact");
        std::fs::write(source.join("artifacts/new/active.json"), "artifact").expect("artifact");
        std::fs::create_dir_all(source.join("repositories")).expect("repositories");
        std::fs::write(source.join("repositories/catalog"), "catalog").expect("catalog");
        std::fs::write(source.join("migraphx-cache"), "producer-only").expect("cache");
        std::fs::create_dir_all(source.join("artifacts/old")).expect("old artifact");
        std::fs::write(source.join("artifacts/old/active.json"), "old").expect("old artifact");
        std::fs::write(
            target.join("default-snapshot-corpus-1.1.json"),
            r#"{"id":"old"}"#,
        )
        .expect("target manifest");
        std::fs::write(target.join(DISTRIBUTED_PUBLICATION_MARKER), "old\n").expect("old marker");

        publish_bundle(&source, &target, "new").expect("publish");

        assert_eq!(published_manifest_id(&target).as_deref(), Some("new"));
        assert_eq!(
            std::fs::read_to_string(target.join("snapshots/new.json")).expect("snapshot"),
            "snapshot"
        );
        assert!(target.join("artifacts/new/active.json").is_file());
        assert!(target.join("repositories/catalog").is_file());
        assert!(!target.join("migraphx-cache").exists());
        assert!(!target.join("artifacts/old").exists());
    }

    #[test]
    fn lazy_preparation_ready_snapshot_reports_ready_state() {
        assert_eq!(
            lazy_preparation_terminal_state(true),
            vesc_mcp_core::preparation_status::PreparationState::Ready
        );
        assert_eq!(
            lazy_preparation_terminal_state(false),
            vesc_mcp_core::preparation_status::PreparationState::Stale
        );
    }

    #[test]
    fn stale_sources_are_retained_in_the_terminal_preparation_state() {
        assert_eq!(
            terminal_preparation_state(false, SnapshotDisposition::Reused),
            vesc_mcp_core::preparation_status::PreparationState::Ready
        );
        assert_eq!(
            terminal_preparation_state(true, SnapshotDisposition::Reused),
            vesc_mcp_core::preparation_status::PreparationState::Stale
        );
        assert_eq!(
            terminal_preparation_state(false, SnapshotDisposition::Stale),
            vesc_mcp_core::preparation_status::PreparationState::Stale
        );
    }

    const HANGING_PREPARATION_CHILD_ENV: &str = "VESC_MCP_TEST_HANG_PREPARATION_CHILD";

    #[test]
    fn hanging_preparation_child_fixture() {
        if std::env::var_os(HANGING_PREPARATION_CHILD_ENV).is_some() {
            loop {
                std::thread::park();
            }
        }
    }

    #[tokio::test]
    async fn preparation_timeout_kills_and_reaps_child_before_persisting_failure() {
        let root = tempfile::tempdir().expect("data root");
        let data_root = DataRoot::new(root.path().to_owned()).expect("absolute data root");
        let mut child =
            tokio::process::Command::new(std::env::current_exe().expect("current test executable"))
                .args(["--exact", "tests::hanging_preparation_child_fixture"])
                .env(HANGING_PREPARATION_CHILD_ENV, "1")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("spawn hanging child");

        let error = supervise_preparation_child(
            &mut child,
            std::time::Duration::from_millis(50),
            Some(&data_root),
            1,
            true,
        )
        .await
        .expect_err("hanging child times out");

        assert!(error.to_string().contains("timed out"));
        assert!(
            child
                .try_wait()
                .expect("inspect child")
                .is_some_and(|status| !status.success()),
            "timed-out child must be dead and reaped"
        );
        assert_eq!(
            vesc_mcp_core::preparation_status::read_preparation_status(root.path()),
            Some(
                vesc_mcp_core::preparation_status::KnowledgePreparationStatus::finished(
                    vesc_mcp_core::preparation_status::PreparationState::Failed,
                    0,
                    1,
                )
                .with_freshness_required(true)
            )
        );
    }

    #[test]
    fn failed_preparation_child_publishes_terminal_status() {
        let root = tempfile::tempdir().expect("data root");
        let data_root = DataRoot::new(root.path().to_owned()).expect("absolute data root");
        vesc_mcp_core::preparation_status::write_preparation_status(
            root.path(),
            &vesc_mcp_core::preparation_status::KnowledgePreparationStatus::preparing(
                vesc_mcp_core::preparation_status::PreparationPhase::BuildingSemanticIndex,
                2,
                4,
            ),
        )
        .expect("preparing status");

        publish_child_preparation_failure(Some(&data_root), 4, true);

        assert_eq!(
            vesc_mcp_core::preparation_status::read_preparation_status(root.path()),
            Some(
                vesc_mcp_core::preparation_status::KnowledgePreparationStatus::finished(
                    vesc_mcp_core::preparation_status::PreparationState::Failed,
                    2,
                    4,
                )
                .with_freshness_required(true)
            ),
        );
    }

    #[tokio::test]
    async fn http_binds_before_repository_preparation() {
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve port");
        let bind = reservation.local_addr().expect("reserved address");
        drop(reservation);
        let started = Arc::new(AtomicBool::new(false));
        let preparing = Arc::clone(&started);
        let config = vesc_mcp_server::http::HttpServerConfig {
            bind,
            path: "/mcp".into(),
            allowed_hosts: vec!["127.0.0.1".into()],
            allowed_origins: Vec::new(),
            auth_token: None,
        };

        let server = tokio::spawn(run_http(
            config,
            async move {
                preparing.store(true, Ordering::Release);
                std::future::pending::<anyhow::Result<()>>().await
            },
            KnowledgePreparation::BuildMissing,
        ));
        while !started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }

        tokio::net::TcpStream::connect(bind)
            .await
            .expect("HTTP listener is available while preparation is pending");
        server.abort();
    }

    #[test]
    fn unfinished_preparation_is_published_as_failed() {
        let root = tempfile::tempdir().expect("data root");
        drop(PreparationReporter::new(root.path().to_owned(), 3, true));

        let status = vesc_mcp_core::preparation_status::read_preparation_status(root.path())
            .expect("published status");
        assert_eq!(
            status.state,
            vesc_mcp_core::preparation_status::PreparationState::Failed
        );
        assert!(status.freshness_required);
    }
}
