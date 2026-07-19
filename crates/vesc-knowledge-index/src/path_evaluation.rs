//! Deterministic path-completeness evaluation for investigation pipelines.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::investigation::{
    CoverageOutcome, Evidence, EvidenceRelationship, HistoricalTraceRevisions,
    InvestigationContract, RejectionReason, Repository, Stage,
};

const PATH_EVALUATION_SCHEMA: u16 = 1;
const MAX_RANKED_EVIDENCE: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathEvaluationSuite {
    pub schema: u16,
    pub suite_id: String,
    pub cases: Vec<PathEvaluationCase>,
}

impl PathEvaluationSuite {
    /// # Errors
    ///
    /// Returns an error when identities are unstable, duplicated, unknown, or
    /// when a locked adversarial bundle disagrees with the contract audit.
    pub fn validate(&self) -> Result<(), PathEvaluationError> {
        if self.schema != PATH_EVALUATION_SCHEMA || self.cases.is_empty() {
            return Err(PathEvaluationError::InvalidSuite("schema or cases"));
        }
        let mut case_ids = BTreeSet::new();
        for case in &self.cases {
            if !case_ids.insert(&case.id) || case.question.trim().is_empty() {
                return Err(PathEvaluationError::InvalidSuite("case identity"));
            }
            case.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathEvaluationCase {
    pub id: String,
    pub question: String,
    pub revisions: RevisionSet,
    pub judgments: Vec<EvidenceIdentity>,
    pub distractors: Vec<EvidenceIdentity>,
    pub relationships: Vec<EvidenceRelationship>,
    pub adversarial_bundles: Vec<AdversarialBundle>,
}

impl PathEvaluationCase {
    fn contract(&self) -> InvestigationContract {
        InvestigationContract::historical_package_loader(HistoricalTraceRevisions::new(
            &self.revisions.package,
            &self.revisions.firmware,
            &self.revisions.rtos,
            &self.revisions.consumer,
        ))
    }

    fn evidence_by_id(&self) -> BTreeMap<&str, &EvidenceIdentity> {
        self.judgments
            .iter()
            .chain(&self.distractors)
            .map(|identity| (identity.id.as_str(), identity))
            .collect()
    }

    fn validate(&self) -> Result<(), PathEvaluationError> {
        if self.judgments.is_empty()
            || ![
                &self.revisions.package,
                &self.revisions.firmware,
                &self.revisions.rtos,
                &self.revisions.consumer,
            ]
            .into_iter()
            .all(|revision| is_git_oid(revision))
        {
            return Err(PathEvaluationError::InvalidSuite("revision identity"));
        }
        let evidence = self.evidence_by_id();
        if evidence.len() != self.judgments.len() + self.distractors.len()
            || evidence.values().any(|identity| !identity.is_stable())
        {
            return Err(PathEvaluationError::InvalidSuite("evidence identity"));
        }
        let gold = self
            .judgments
            .iter()
            .map(EvidenceIdentity::as_evidence)
            .collect::<Vec<_>>();
        if self.contract().audit(&gold, &self.relationships).outcome != CoverageOutcome::Complete {
            return Err(PathEvaluationError::InvalidSuite("gold path is incomplete"));
        }
        for bundle in &self.adversarial_bundles {
            let selected = bundle
                .evidence_ids
                .iter()
                .map(|id| evidence.get(id.as_str()).copied())
                .collect::<Option<Vec<_>>>()
                .ok_or(PathEvaluationError::UnknownEvidence)?;
            let audit = self.contract().audit(
                &selected
                    .into_iter()
                    .map(EvidenceIdentity::as_evidence)
                    .collect::<Vec<_>>(),
                &self.relationships,
            );
            if audit
                .missing_facets
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                != bundle.expected_missing_facets
            {
                return Err(PathEvaluationError::InvalidSuite("adversarial expectation"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionSet {
    pub package: String,
    pub firmware: String,
    pub rtos: String,
    pub consumer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceIdentity {
    pub id: String,
    pub repository: Repository,
    pub stage: Stage,
    pub revision: String,
    pub path: String,
    pub symbol: String,
    pub content_key: String,
    #[serde(default)]
    pub adjacent: bool,
}

impl EvidenceIdentity {
    fn is_stable(&self) -> bool {
        !self.id.trim().is_empty()
            && is_git_oid(&self.revision)
            && !self.path.trim().is_empty()
            && !self.symbol.trim().is_empty()
            && !self.content_key.trim().is_empty()
    }

    fn as_evidence(&self) -> Evidence {
        if self.adjacent {
            Evidence::adjacent(
                &self.id,
                self.repository.clone(),
                self.stage.clone(),
                &self.revision,
                &self.path,
            )
        } else {
            Evidence::decisive(
                &self.id,
                self.repository.clone(),
                self.stage.clone(),
                &self.revision,
                &self.path,
            )
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdversarialBundle {
    pub id: String,
    pub evidence_ids: Vec<String>,
    pub expected_missing_facets: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ablation {
    Lexical,
    Embeddings,
    GraphExpansion,
    Reranking,
    Planner,
    Critic,
    HardGate,
}

impl Ablation {
    pub const ALL: [Self; 7] = [
        Self::Lexical,
        Self::Embeddings,
        Self::GraphExpansion,
        Self::Reranking,
        Self::Planner,
        Self::Critic,
        Self::HardGate,
    ];
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalControls {
    pub recall_at_5: f64,
    pub recall_at_10: f64,
    pub mrr_at_10: f64,
    pub ndcg_at_10: f64,
    pub exact_identifier_top_one: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathEvaluationRun {
    pub schema: u16,
    pub case_id: String,
    pub ablation: Ablation,
    pub budget_n: usize,
    pub ranked_evidence_ids: Vec<String>,
    pub relationships: Vec<EvidenceRelationship>,
    pub reported_missing_facets: BTreeSet<String>,
    pub answered: bool,
    pub answer_citations: BTreeSet<String>,
    pub controls: Option<RetrievalControls>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathEvaluationReport {
    pub schema: u16,
    pub suite_id: String,
    pub case_id: String,
    pub ablation: Ablation,
    pub budget_n: usize,
    pub facet_coverage_at_budget: f64,
    pub path_complete_at_n: f64,
    pub wrong_era_rate: f64,
    pub missing_facet_detection: f64,
    pub frontier_shortcut_rate: f64,
    pub frontier_shortcut_target: f64,
    pub evidence_utilization: f64,
    pub duplicate_history_waste: f64,
    pub missing_facets: Vec<String>,
    pub missing_relationships: Vec<String>,
    pub controls: Option<RetrievalControls>,
    pub release_gate_passed: bool,
}

impl PathEvaluationReport {
    /// # Panics
    ///
    /// Panics only if this fixed report schema cannot be represented as JSON.
    #[must_use]
    pub fn canonical_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("path report serialization cannot fail") + "\n"
    }

    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut output = format!(
            "# Path evaluation: {}\n\nAblation: `{:?}` · budget: {}\n\n| Metric | Value |\n|---|---:|\n",
            self.case_id, self.ablation, self.budget_n
        );
        for (name, value) in [
            ("FacetCoverage@Budget", self.facet_coverage_at_budget),
            ("PathComplete@N", self.path_complete_at_n),
            ("WrongEraRate", self.wrong_era_rate),
            ("MissingFacetDetection", self.missing_facet_detection),
            ("FrontierShortcutRate", self.frontier_shortcut_rate),
            ("EvidenceUtilization", self.evidence_utilization),
            ("DuplicateHistoryWaste", self.duplicate_history_waste),
        ] {
            writeln!(output, "| {name} | {value:.3} |").expect("write to string");
        }
        writeln!(
            output,
            "\nRelease gate: **{}** (FrontierShortcutRate target = 0)",
            if self.release_gate_passed {
                "PASS"
            } else {
                "FAIL"
            }
        )
        .expect("write to string");
        if let Some(controls) = &self.controls {
            writeln!(
                output,
                "\nRetrieval controls: recall@5 {:.3}, recall@10 {:.3}, MRR@10 {:.3}, nDCG@10 {:.3}, exact-ID top-1 {:.3}.",
                controls.recall_at_5,
                controls.recall_at_10,
                controls.mrr_at_10,
                controls.ndcg_at_10,
                controls.exact_identifier_top_one
            )
            .expect("write to string");
        }
        output
    }
}

/// # Errors
///
/// Returns an error for an invalid suite/run, unknown case, or unknown
/// evidence identity.
pub fn evaluate_path_run(
    suite: &PathEvaluationSuite,
    run: &PathEvaluationRun,
) -> Result<PathEvaluationReport, PathEvaluationError> {
    suite.validate()?;
    if run.schema != PATH_EVALUATION_SCHEMA
        || run.budget_n == 0
        || run.ranked_evidence_ids.len() > MAX_RANKED_EVIDENCE
    {
        return Err(PathEvaluationError::InvalidRun);
    }
    let case = suite
        .cases
        .iter()
        .find(|case| case.id == run.case_id)
        .ok_or(PathEvaluationError::UnknownCase)?;
    let by_id = case.evidence_by_id();
    let selected = run
        .ranked_evidence_ids
        .iter()
        .take(run.budget_n)
        .map(|id| by_id.get(id.as_str()).copied())
        .collect::<Option<Vec<_>>>()
        .ok_or(PathEvaluationError::UnknownEvidence)?;
    let evidence = selected
        .iter()
        .map(|identity| identity.as_evidence())
        .collect::<Vec<_>>();
    let contract = case.contract();
    let audit = contract.audit(&evidence, &run.relationships);
    let qualifying = audit
        .qualifying_evidence
        .values()
        .flatten()
        .collect::<BTreeSet<_>>();
    let wrong_era = audit
        .rejected_evidence
        .iter()
        .filter(|rejected| rejected.reason == RejectionReason::WrongEra)
        .count();
    let duplicate_count = duplicate_history_count(&selected);
    let missing = audit
        .missing_facets
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let frontier_shortcut_rate =
        f64::from(run.answered && audit.outcome != CoverageOutcome::Complete);
    Ok(PathEvaluationReport {
        schema: PATH_EVALUATION_SCHEMA,
        suite_id: suite.suite_id.clone(),
        case_id: case.id.clone(),
        ablation: run.ablation,
        budget_n: run.budget_n,
        facet_coverage_at_budget: ratio(audit.qualifying_evidence.len(), contract.facets().len()),
        path_complete_at_n: f64::from(audit.outcome == CoverageOutcome::Complete),
        wrong_era_rate: ratio(wrong_era, selected.len()),
        missing_facet_detection: f64::from(missing == run.reported_missing_facets),
        frontier_shortcut_rate,
        frontier_shortcut_target: 0.0,
        evidence_utilization: ratio(
            qualifying
                .iter()
                .filter(|id| run.answer_citations.contains(id.as_str()))
                .count(),
            selected.len(),
        ),
        duplicate_history_waste: ratio(duplicate_count, selected.len()),
        missing_facets: audit.missing_facets,
        missing_relationships: audit.missing_relationships,
        controls: run.controls.clone(),
        release_gate_passed: frontier_shortcut_rate == 0.0,
    })
}

fn duplicate_history_count(selected: &[&EvidenceIdentity]) -> usize {
    let mut seen = BTreeSet::new();
    selected
        .iter()
        .filter(|identity| !seen.insert(identity.content_key.as_str()))
        .count()
}

fn is_git_oid(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        f64::from(u32::try_from(numerator).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(denominator).unwrap_or(u32::MAX))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathEvaluationError {
    #[error("invalid path evaluation suite: {0}")]
    InvalidSuite(&'static str),
    #[error("invalid path evaluation run")]
    InvalidRun,
    #[error("unknown path evaluation case")]
    UnknownCase,
    #[error("unknown evidence identity")]
    UnknownEvidence,
}
