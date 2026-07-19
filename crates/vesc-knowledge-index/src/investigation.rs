//! Deterministic evidence-completeness contracts.
//!
//! Classification creates hard requirements, retrieval supplies ordinary
//! evidence, coverage auditing qualifies that evidence without model input,
//! and answer validation enforces use of every satisfied mandatory facet.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::corpus::SchemaVersion;

pub const INVESTIGATION_SCHEMA_V1: SchemaVersion = SchemaVersion { major: 1, minor: 0 };
const MAX_FACETS: usize = 64;
const MAX_RELATIONSHIPS: usize = 128;
const MAX_ROUNDS: u8 = 16;
const MAX_CANDIDATES_PER_ROUND: u16 = 4_096;
const MAX_CONTEXT_BYTES: u32 = 4 * 1024 * 1024;
const MAX_GRAPH_HOPS: u8 = 16;
const MAX_MODEL_CALLS: u8 = 32;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionClass {
    HistoricalCrossRepositoryExecutionTrace,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Repository {
    VescPackageLib,
    VescFirmware,
    ChibiOs,
    Refloat,
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    PackageFormat,
    GeneratedEntry,
    FirmwareLoader,
    AbiDispatch,
    RuntimeModuleLoading,
    ConsumerInvocation,
    Configuration,
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "revision")]
pub enum Era {
    Exact(String),
    Any,
}

impl Era {
    pub fn exact(revision: impl Into<String>) -> Self {
        Self::Exact(revision.into())
    }

    fn matches(&self, revision: &str) -> bool {
        match self {
            Self::Exact(expected) => expected == revision,
            Self::Any => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FacetRequirement {
    pub id: String,
    pub repository: Repository,
    pub stage: Stage,
    pub era: Era,
}

impl FacetRequirement {
    pub fn new(id: impl Into<String>, repository: Repository, stage: Stage, era: Era) -> Self {
        Self {
            id: id.into(),
            repository,
            stage,
            era,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipKind {
    Produces,
    LoadedBy,
    DispatchesTo,
    RunsIn,
    InvokedBy,
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipRequirement {
    pub id: String,
    pub from_facet: String,
    pub to_facet: String,
    pub kind: RelationshipKind,
}

impl RelationshipRequirement {
    pub fn new(
        id: impl Into<String>,
        from_facet: impl Into<String>,
        to_facet: impl Into<String>,
        kind: RelationshipKind,
    ) -> Self {
        Self {
            id: id.into(),
            from_facet: from_facet.into(),
            to_facet: to_facet.into(),
            kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoundBudget {
    pub max_rounds: u8,
    pub max_candidates_per_round: u16,
    pub max_context_bytes: u32,
    pub max_graph_hops: u8,
    pub max_model_calls: u8,
}

impl Default for RoundBudget {
    fn default() -> Self {
        Self {
            max_rounds: 4,
            max_candidates_per_round: 128,
            max_context_bytes: 64 * 1024,
            max_graph_hops: 4,
            max_model_calls: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoricalTraceRevisions {
    package: String,
    firmware: String,
    rtos: String,
    consumer: String,
}

impl HistoricalTraceRevisions {
    pub fn new(
        package: impl Into<String>,
        firmware: impl Into<String>,
        rtos: impl Into<String>,
        consumer: impl Into<String>,
    ) -> Self {
        Self {
            package: package.into(),
            firmware: firmware.into(),
            rtos: rtos.into(),
            consumer: consumer.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannerExtension {
    pub facets: Vec<FacetRequirement>,
    #[serde(default)]
    pub relationships: Vec<RelationshipRequirement>,
}

impl PlannerExtension {
    #[must_use]
    pub const fn new(facets: Vec<FacetRequirement>) -> Self {
        Self {
            facets,
            relationships: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_relationships(mut self, relationships: Vec<RelationshipRequirement>) -> Self {
        self.relationships = relationships;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationContract {
    pub schema: SchemaVersion,
    pub question_class: QuestionClass,
    pub budget: RoundBudget,
    facets: BTreeMap<String, FacetRequirement>,
    relationships: BTreeMap<String, RelationshipRequirement>,
}

impl InvestigationContract {
    #[must_use]
    pub fn historical_package_loader(revisions: HistoricalTraceRevisions) -> Self {
        let HistoricalTraceRevisions {
            package,
            firmware,
            rtos,
            consumer,
        } = revisions;
        let facets = [
            FacetRequirement::new(
                "package-format",
                Repository::VescPackageLib,
                Stage::PackageFormat,
                Era::exact(&package),
            ),
            FacetRequirement::new(
                "generated-entry",
                Repository::VescPackageLib,
                Stage::GeneratedEntry,
                Era::exact(package),
            ),
            FacetRequirement::new(
                "firmware-loader",
                Repository::VescFirmware,
                Stage::FirmwareLoader,
                Era::exact(&firmware),
            ),
            FacetRequirement::new(
                "abi-dispatch",
                Repository::VescFirmware,
                Stage::AbiDispatch,
                Era::exact(firmware),
            ),
            FacetRequirement::new(
                "runtime-module-loading",
                Repository::ChibiOs,
                Stage::RuntimeModuleLoading,
                Era::exact(rtos),
            ),
            FacetRequirement::new(
                "consumer-invocation",
                Repository::Refloat,
                Stage::ConsumerInvocation,
                Era::exact(consumer),
            ),
        ]
        .into_iter()
        .map(|facet| (facet.id.clone(), facet))
        .collect();
        let relationships = [
            RelationshipRequirement::new(
                "package-produces-entry",
                "package-format",
                "generated-entry",
                RelationshipKind::Produces,
            ),
            RelationshipRequirement::new(
                "entry-loaded-by-firmware",
                "generated-entry",
                "firmware-loader",
                RelationshipKind::LoadedBy,
            ),
            RelationshipRequirement::new(
                "loader-dispatches-abi",
                "firmware-loader",
                "abi-dispatch",
                RelationshipKind::DispatchesTo,
            ),
            RelationshipRequirement::new(
                "abi-runs-in-runtime",
                "abi-dispatch",
                "runtime-module-loading",
                RelationshipKind::RunsIn,
            ),
            RelationshipRequirement::new(
                "runtime-invoked-by-consumer",
                "runtime-module-loading",
                "consumer-invocation",
                RelationshipKind::InvokedBy,
            ),
        ]
        .into_iter()
        .map(|relationship| (relationship.id.clone(), relationship))
        .collect();
        Self {
            schema: INVESTIGATION_SCHEMA_V1,
            question_class: QuestionClass::HistoricalCrossRepositoryExecutionTrace,
            budget: RoundBudget::default(),
            facets,
            relationships,
        }
    }

    #[must_use]
    pub fn facets(&self) -> Vec<&FacetRequirement> {
        self.facets.values().collect()
    }

    #[must_use]
    pub fn relationships(&self) -> Vec<&RelationshipRequirement> {
        self.relationships.values().collect()
    }

    /// # Errors
    ///
    /// Returns an error when the schema, requirements, references, or budgets
    /// are invalid or outside their fixed bounds.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema.major != INVESTIGATION_SCHEMA_V1.major {
            return Err(ContractError::Invalid("unsupported schema major"));
        }
        if self.facets.is_empty()
            || self.facets.len() > MAX_FACETS
            || self.relationships.len() > MAX_RELATIONSHIPS
        {
            return Err(ContractError::LimitExceeded);
        }
        if self.budget.max_rounds == 0
            || self.budget.max_rounds > MAX_ROUNDS
            || self.budget.max_candidates_per_round == 0
            || self.budget.max_candidates_per_round > MAX_CANDIDATES_PER_ROUND
            || self.budget.max_context_bytes == 0
            || self.budget.max_context_bytes > MAX_CONTEXT_BYTES
            || self.budget.max_graph_hops > MAX_GRAPH_HOPS
            || self.budget.max_model_calls > MAX_MODEL_CALLS
        {
            return Err(ContractError::Invalid("round budget is outside bounds"));
        }
        if self.facets.iter().any(|(id, facet)| {
            id.trim().is_empty()
                || id != &facet.id
                || matches!(&facet.era, Era::Exact(revision) if revision.trim().is_empty())
        }) {
            return Err(ContractError::Invalid("invalid facet requirement"));
        }
        if self.relationships.iter().any(|(id, relationship)| {
            id.trim().is_empty()
                || id != &relationship.id
                || !self.facets.contains_key(&relationship.from_facet)
                || !self.facets.contains_key(&relationship.to_facet)
        }) {
            return Err(ContractError::UnknownFacet);
        }
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when an extension conflicts with an existing
    /// requirement, references an unknown facet, or exceeds fixed bounds.
    pub fn extend(mut self, extension: PlannerExtension) -> Result<Self, ContractError> {
        self.validate()?;
        if self.facets.len() + extension.facets.len() > MAX_FACETS
            || self.relationships.len() + extension.relationships.len() > MAX_RELATIONSHIPS
        {
            return Err(ContractError::LimitExceeded);
        }
        for facet in extension.facets {
            insert_unchanged_or_new(&mut self.facets, facet.id.clone(), facet)?;
        }
        for relationship in extension.relationships {
            if !self.facets.contains_key(&relationship.from_facet)
                || !self.facets.contains_key(&relationship.to_facet)
            {
                return Err(ContractError::UnknownFacet);
            }
            insert_unchanged_or_new(
                &mut self.relationships,
                relationship.id.clone(),
                relationship,
            )?;
        }
        self.validate()?;
        Ok(self)
    }

    #[must_use]
    pub fn audit(
        &self,
        evidence: &[Evidence],
        relationships: &[EvidenceRelationship],
    ) -> CoverageAudit {
        let mut qualifying = BTreeMap::<String, Vec<String>>::new();
        let mut rejected_evidence = Vec::new();
        for item in evidence {
            if !item.decisive || item.path.trim().is_empty() {
                rejected_evidence.push(RejectedEvidence::new(item, RejectionReason::Adjacent));
                continue;
            }
            let repository_matches = self
                .facets
                .values()
                .filter(|facet| facet.repository == item.repository)
                .collect::<Vec<_>>();
            let stage_matches = repository_matches
                .iter()
                .copied()
                .filter(|facet| facet.stage == item.stage)
                .collect::<Vec<_>>();
            let era_matches = stage_matches
                .iter()
                .copied()
                .filter(|facet| facet.era.matches(&item.revision))
                .collect::<Vec<_>>();
            if era_matches.is_empty() {
                let reason = if repository_matches.is_empty() {
                    RejectionReason::WrongRepository
                } else if stage_matches.is_empty() {
                    RejectionReason::WrongStage
                } else {
                    RejectionReason::WrongEra
                };
                rejected_evidence.push(RejectedEvidence::new(item, reason));
                continue;
            }
            for facet in era_matches {
                qualifying
                    .entry(facet.id.clone())
                    .or_default()
                    .push(item.id.clone());
            }
        }
        for ids in qualifying.values_mut() {
            ids.sort();
            ids.dedup();
        }
        rejected_evidence.sort();

        let missing_facets = self
            .facets
            .keys()
            .filter(|id| !qualifying.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        let missing_relationships = self
            .relationships
            .values()
            .filter(|required| !relationship_is_covered(required, &qualifying, relationships))
            .map(|required| required.id.clone())
            .collect::<Vec<_>>();
        let outcome = if missing_facets.is_empty() && missing_relationships.is_empty() {
            CoverageOutcome::Complete
        } else {
            CoverageOutcome::Insufficient
        };
        CoverageAudit {
            schema: self.schema,
            outcome,
            qualifying_evidence: qualifying,
            missing_facets,
            missing_relationships,
            rejected_evidence,
        }
    }

    /// # Errors
    ///
    /// Returns an error unless coverage is complete and the answer cites
    /// qualifying evidence for every mandatory facet.
    pub fn validate_answer(
        &self,
        audit: &CoverageAudit,
        answer: &AnswerUse,
    ) -> Result<(), AnswerValidationError> {
        if audit.outcome != CoverageOutcome::Complete {
            return Err(AnswerValidationError::IncompleteAudit);
        }
        let missing_facets = self
            .facets
            .keys()
            .filter(|facet| {
                !audit
                    .qualifying_evidence
                    .get(*facet)
                    .is_some_and(|ids| ids.iter().any(|id| answer.cited_evidence.contains(id)))
            })
            .cloned()
            .collect::<Vec<_>>();
        if missing_facets.is_empty() {
            Ok(())
        } else {
            Err(AnswerValidationError::MissingFacetCitations(missing_facets))
        }
    }
}

fn insert_unchanged_or_new<T: PartialEq>(
    values: &mut BTreeMap<String, T>,
    id: String,
    value: T,
) -> Result<(), ContractError> {
    if let Some(existing) = values.get(&id) {
        return if existing == &value {
            Ok(())
        } else {
            Err(ContractError::ConflictingRequirement(id))
        };
    }
    values.insert(id, value);
    Ok(())
}

fn relationship_is_covered(
    required: &RelationshipRequirement,
    qualifying: &BTreeMap<String, Vec<String>>,
    relationships: &[EvidenceRelationship],
) -> bool {
    let Some(from) = qualifying.get(&required.from_facet) else {
        return false;
    };
    let Some(to) = qualifying.get(&required.to_facet) else {
        return false;
    };
    relationships.iter().any(|candidate| {
        candidate.kind == required.kind
            && from.contains(&candidate.from_evidence)
            && to.contains(&candidate.to_evidence)
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub id: String,
    pub repository: Repository,
    pub stage: Stage,
    pub revision: String,
    pub path: String,
    pub decisive: bool,
}

impl Evidence {
    fn new(
        id: impl Into<String>,
        repository: Repository,
        stage: Stage,
        revision: impl Into<String>,
        path: impl Into<String>,
        decisive: bool,
    ) -> Self {
        Self {
            id: id.into(),
            repository,
            stage,
            revision: revision.into(),
            path: path.into(),
            decisive,
        }
    }

    pub fn decisive(
        id: impl Into<String>,
        repository: Repository,
        stage: Stage,
        revision: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self::new(id, repository, stage, revision, path, true)
    }

    pub fn adjacent(
        id: impl Into<String>,
        repository: Repository,
        stage: Stage,
        revision: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self::new(id, repository, stage, revision, path, false)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRelationship {
    pub from_evidence: String,
    pub to_evidence: String,
    pub kind: RelationshipKind,
}

impl EvidenceRelationship {
    pub fn new(
        from_evidence: impl Into<String>,
        to_evidence: impl Into<String>,
        kind: RelationshipKind,
    ) -> Self {
        Self {
            from_evidence: from_evidence.into(),
            to_evidence: to_evidence.into(),
            kind,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageOutcome {
    Complete,
    Insufficient,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    WrongRepository,
    WrongStage,
    WrongEra,
    Adjacent,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RejectedEvidence {
    pub evidence_id: String,
    pub reason: RejectionReason,
}

impl RejectedEvidence {
    fn new(evidence: &Evidence, reason: RejectionReason) -> Self {
        Self {
            evidence_id: evidence.id.clone(),
            reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageAudit {
    pub schema: SchemaVersion,
    pub outcome: CoverageOutcome,
    pub qualifying_evidence: BTreeMap<String, Vec<String>>,
    pub missing_facets: Vec<String>,
    pub missing_relationships: Vec<String>,
    pub rejected_evidence: Vec<RejectedEvidence>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerUse {
    pub cited_evidence: BTreeSet<String>,
}

impl AnswerUse {
    #[must_use]
    pub const fn new(cited_evidence: BTreeSet<String>) -> Self {
        Self { cited_evidence }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContractError {
    #[error("planner extension exceeds the bounded investigation contract")]
    LimitExceeded,
    #[error("planner extension conflicts with existing requirement {0}")]
    ConflictingRequirement(String),
    #[error("relationship refers to an unknown facet")]
    UnknownFacet,
    #[error("invalid investigation contract: {0}")]
    Invalid(&'static str),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AnswerValidationError {
    #[error("answering is unavailable until the coverage audit is complete")]
    IncompleteAudit,
    #[error("answer does not use and cite mandatory facets: {0:?}")]
    MissingFacetCitations(Vec<String>),
}
