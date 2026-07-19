#![cfg(feature = "semantic-fastembed")]

use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vesc_knowledge_index::path_evaluation::{
    Ablation, EvidenceIdentity, PathEvaluationCase, PathEvaluationReport, PathEvaluationRun,
    PathEvaluationSuite, evaluate_path_run,
};
use vesc_knowledge_index::reranking::{
    FacetCandidate, FacetQuota, FastEmbedReranker, RetainedFacetCandidate, retain_per_facet,
};

#[derive(Deserialize)]
struct ModelManifest {
    schema: u16,
    candidates: Vec<ModelSpec>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ModelSpec {
    name: String,
    model_id: String,
    model_revision: String,
    directory: String,
    license: String,
    onnx_sha256: String,
    onnx_bytes: u64,
    head_artifacts: Vec<ArtifactSpec>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ArtifactSpec {
    path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Serialize)]
struct Timing {
    initialization_seconds: f64,
    warm_p50_seconds: f64,
    warm_p95_seconds: f64,
    candidate_pairs_per_second: f64,
    repetitions: usize,
}

#[derive(Serialize)]
struct Decision {
    evidence_id: String,
    retrieval_score: f32,
    rerank_score: f32,
}

#[derive(Serialize)]
struct Comparison {
    no_reranker_global: PathEvaluationReport,
    no_reranker_per_facet: PathEvaluationReport,
    reranker_global: PathEvaluationReport,
    reranker_per_facet: PathEvaluationReport,
}

#[derive(Serialize)]
struct BenchmarkReport {
    schema: u16,
    suite_id: String,
    case_id: String,
    model: ModelSpec,
    provider: &'static str,
    os: &'static str,
    arch: &'static str,
    cpu: Option<String>,
    ort_dylib: Option<String>,
    ort_dylib_sha256: Option<String>,
    max_length: usize,
    batch_size: usize,
    intra_threads: usize,
    facet_quota: usize,
    candidate_count: usize,
    candidate_set_sha256: String,
    peak_rss_bytes: Option<u64>,
    timing: Timing,
    decisions: Vec<Decision>,
    retained: Vec<RetainedFacetCandidate>,
    comparison: Comparison,
    warning: &'static str,
}

#[allow(clippy::too_many_lines)] // Straight-line benchmark orchestration is clearer kept together.
fn main() -> Result<(), Box<dyn Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 11 {
        return Err("usage: reranker_bench <suite.json> <models.json> <model-root> <candidate> <output.json> <repetitions> <max-length> <batch-size> <threads> <facet-quota> <peak-rss-bytes-or-0>".into());
    }
    let suite: PathEvaluationSuite = read_json(&args[0])?;
    suite.validate()?;
    let manifest: ModelManifest = read_json(&args[1])?;
    if manifest.schema != 1 {
        return Err("unsupported reranker model manifest schema".into());
    }
    let spec = manifest
        .candidates
        .iter()
        .find(|candidate| candidate.name == args[3])
        .cloned()
        .ok_or("unknown reranker candidate")?;
    let repetitions = positive(&args[5], "repetitions")?;
    let max_length = positive(&args[6], "max length")?;
    let batch_size = positive(&args[7], "batch size")?;
    let intra_threads = positive(&args[8], "threads")?;
    let quota_value = positive(&args[9], "facet quota")?;
    let quota = FacetQuota::new(quota_value)?;
    let peak_rss = args[10].parse::<u64>()?;

    let model_dir = Path::new(&args[2]).join(&spec.directory);
    verify_model(&model_dir, &spec)?;
    let case = suite.cases.first().ok_or("suite has no cases")?;
    let identities = case
        .judgments
        .iter()
        .chain(&case.distractors)
        .collect::<Vec<_>>();
    let documents = identities
        .iter()
        .map(|identity| identity.reranker_text())
        .collect::<Vec<_>>();
    let retrieval_scores = (0..documents.len())
        .map(|index| {
            u16::try_from(documents.len() - index)
                .map(f32::from)
                .expect("bounded evaluation candidate count fits u16")
        })
        .collect::<Vec<_>>();

    let started = Instant::now();
    let mut reranker =
        FastEmbedReranker::from_model_dir(&model_dir, max_length, batch_size, intra_threads)?;
    let initialization_seconds = started.elapsed().as_secs_f64();
    let _ = reranker.score(&case.question, &documents)?;
    let mut samples = Vec::with_capacity(repetitions);
    let mut scores = Vec::new();
    for _ in 0..repetitions {
        let started = Instant::now();
        scores = reranker.score(&case.question, &documents)?;
        samples.push(started.elapsed().as_secs_f64());
    }
    let total_seconds = samples.iter().sum::<f64>();
    samples.sort_by(f64::total_cmp);

    let candidates = identities
        .iter()
        .zip(retrieval_scores.iter().zip(&scores))
        .map(|(identity, (&retrieval_score, &rerank_score))| {
            FacetCandidate::new(identity.as_evidence(), retrieval_score)
                .with_rerank_score(rerank_score)
        })
        .collect::<Vec<_>>();
    let retained = retain_per_facet(&case.contract(), candidates, quota);
    let baseline_ids = identities
        .iter()
        .map(|identity| identity.id.clone())
        .collect::<Vec<_>>();
    let reranked_ids = ranked_ids(&identities, &scores);
    let retained_ids = retained
        .iter()
        .map(|row| row.evidence.id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let baseline_retained = retain_per_facet(
        &case.contract(),
        identities
            .iter()
            .zip(&retrieval_scores)
            .map(|(identity, &score)| FacetCandidate::new(identity.as_evidence(), score))
            .collect(),
        quota,
    )
    .into_iter()
    .map(|row| row.evidence.id)
    .collect::<BTreeSet<_>>()
    .into_iter()
    .collect::<Vec<_>>();
    let budget = case.contract().facets().len() * quota_value;
    let comparison = Comparison {
        no_reranker_global: evaluate(&suite, case, baseline_ids, budget)?,
        no_reranker_per_facet: evaluate(&suite, case, baseline_retained, budget)?,
        reranker_global: evaluate(&suite, case, reranked_ids, budget)?,
        reranker_per_facet: evaluate(&suite, case, retained_ids, budget)?,
    };
    let decisions = identities
        .iter()
        .zip(retrieval_scores.into_iter().zip(scores))
        .map(|(identity, (retrieval_score, rerank_score))| Decision {
            evidence_id: identity.id.clone(),
            retrieval_score,
            rerank_score,
        })
        .collect();
    let ort_dylib = env::var("ORT_DYLIB_PATH").ok();
    let report = BenchmarkReport {
        schema: 1,
        suite_id: suite.suite_id.clone(),
        case_id: case.id.clone(),
        model: spec,
        provider: "CPUExecutionProvider",
        os: env::consts::OS,
        arch: env::consts::ARCH,
        cpu: cpu_name(),
        ort_dylib_sha256: ort_dylib
            .as_deref()
            .map(Path::new)
            .map(sha256_file)
            .transpose()?,
        ort_dylib,
        max_length,
        batch_size,
        intra_threads,
        facet_quota: quota_value,
        candidate_count: documents.len(),
        candidate_set_sha256: sha256_bytes(documents.join("\n\0\n").as_bytes()),
        peak_rss_bytes: (peak_rss != 0).then_some(peak_rss),
        timing: Timing {
            initialization_seconds,
            warm_p50_seconds: percentile(&samples, 50),
            warm_p95_seconds: percentile(&samples, 95),
            candidate_pairs_per_second: f64::from(u32::try_from(documents.len())?)
                * f64::from(u32::try_from(repetitions)?)
                / total_seconds,
            repetitions,
        },
        decisions,
        retained,
        comparison,
        warning: "The locked suite stores evidence metadata rather than complete source passages; these measurements prove runtime and path-retention behavior, not standalone semantic reranker quality.",
    };
    write_json(&args[4], &report)?;
    Ok(())
}

fn evaluate(
    suite: &PathEvaluationSuite,
    case: &PathEvaluationCase,
    ids: Vec<String>,
    budget: usize,
) -> Result<PathEvaluationReport, Box<dyn Error>> {
    let by_id = case
        .judgments
        .iter()
        .chain(&case.distractors)
        .map(|identity| (identity.id.as_str(), identity))
        .collect::<std::collections::BTreeMap<_, _>>();
    let selected = ids
        .iter()
        .take(budget)
        .filter_map(|id| by_id.get(id.as_str()))
        .map(|identity| identity.as_evidence())
        .collect::<Vec<_>>();
    let audit = case.contract().audit(&selected, &case.relationships);
    let run = PathEvaluationRun {
        schema: 1,
        case_id: case.id.clone(),
        ablation: Ablation::Reranking,
        budget_n: budget,
        ranked_evidence_ids: ids,
        relationships: case.relationships.clone(),
        reported_missing_facets: audit.missing_facets.into_iter().collect(),
        answered: false,
        answer_citations: selected.into_iter().map(|evidence| evidence.id).collect(),
        controls: None,
    };
    Ok(evaluate_path_run(suite, &run)?)
}

fn ranked_ids(identities: &[&EvidenceIdentity], scores: &[f32]) -> Vec<String> {
    let mut indices = (0..identities.len()).collect::<Vec<_>>();
    indices.sort_by(|&left, &right| {
        scores[right]
            .total_cmp(&scores[left])
            .then_with(|| identities[left].id.cmp(&identities[right].id))
    });
    indices
        .into_iter()
        .map(|index| identities[index].id.clone())
        .collect()
}

fn verify_model(root: &Path, spec: &ModelSpec) -> Result<(), Box<dyn Error>> {
    verify_artifact(root, "model.onnx", spec.onnx_bytes, &spec.onnx_sha256)?;
    for artifact in &spec.head_artifacts {
        verify_artifact(root, &artifact.path, artifact.bytes, &artifact.sha256)?;
    }
    Ok(())
}

fn verify_artifact(
    root: &Path,
    relative_path: &str,
    bytes: u64,
    sha256: &str,
) -> Result<(), Box<dyn Error>> {
    let path = root.join(relative_path);
    if fs::metadata(&path)?.len() != bytes || sha256_file(&path)? != sha256 {
        return Err(format!(
            "model artifact does not match pinned bytes/hash: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn percentile(sorted: &[f64], percentile: usize) -> f64 {
    let index = ((sorted.len() - 1) * percentile).div_ceil(100);
    sorted[index]
}

fn positive(value: &str, name: &str) -> Result<usize, Box<dyn Error>> {
    let parsed = value.parse::<usize>()?;
    if parsed == 0 {
        return Err(format!("{name} must be positive").into());
    }
    Ok(parsed)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &str) -> Result<T, Box<dyn Error>> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_json<T: Serialize>(path: &str, value: &T) -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn Error>> {
    Ok(sha256_bytes(&fs::read(path)?))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("write to string");
            output
        })
}

fn cpu_name() -> Option<String> {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|content| {
            content.lines().find_map(|line| {
                line.strip_prefix("model name\t: ")
                    .or_else(|| line.strip_prefix("Hardware\t: "))
                    .map(str::to_owned)
            })
        })
        .or_else(|| {
            std::process::Command::new("sysctl")
                .args(["-n", "machdep.cpu.brand_string"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranked_ids_break_equal_scores_by_stable_evidence_id() {
        let suite: PathEvaluationSuite = serde_json::from_str(include_str!(
            "../../../../tests/evaluation/v3/loader_path.json"
        ))
        .unwrap();
        let identities = suite.cases[0].judgments.iter().take(2).collect::<Vec<_>>();
        let ranked = ranked_ids(&identities, &[0.5, 0.5]);
        let mut expected = ranked.clone();
        expected.sort();
        assert_eq!(ranked, expected);
    }
}
