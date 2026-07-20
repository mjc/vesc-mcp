//! Bounded evidence acquisition with a fail-closed answer boundary.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    investigation::{
        AnswerUse, CoverageAudit, CoverageOutcome, Evidence, EvidenceRelationship,
        InvestigationContract,
    },
    planning::{CriticProposal, PlannerProposal, SearchQuery, hard_coded_proposal},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatingPoint {
    HardRulesOnly,
    FastPlanner,
    PlannerAndCritic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceProvenance {
    pub lexical_rank: Option<u16>,
    pub semantic_rank: Option<u16>,
    pub graph_path: Vec<EvidenceRelationship>,
    pub graph_hops: u8,
    pub rerank_rank: u16,
    pub retrieval_score_micros: i64,
    pub rerank_score_micros: Option<i64>,
}

impl EvidenceProvenance {
    fn validate(&self) -> bool {
        (self.lexical_rank.is_some() || self.semantic_rank.is_some() || !self.graph_path.is_empty())
            && self.lexical_rank.is_none_or(|rank| rank > 0)
            && self.semantic_rank.is_none_or(|rank| rank > 0)
            && self.rerank_rank > 0
            && usize::from(self.graph_hops) == self.graph_path.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquiredEvidence {
    pub evidence: Evidence,
    pub context_bytes: u32,
    pub provenance: EvidenceProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquisitionRequest {
    pub round: u8,
    pub missing_facets: Vec<String>,
    pub missing_relationships: Vec<String>,
    pub queries: Vec<SearchQuery>,
    pub max_candidates: u16,
    pub remaining_context_bytes: u32,
    pub max_graph_hops: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcquisitionRound {
    pub retained: Vec<AcquiredEvidence>,
    pub relationships: Vec<EvidenceRelationship>,
    pub model_calls: u8,
}

pub trait EvidenceAcquirer {
    fn acquire(&mut self, request: &AcquisitionRequest) -> AcquisitionRound;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerDraft {
    pub text: String,
    pub cited_evidence: BTreeSet<String>,
    pub claimed_relationships: Vec<EvidenceRelationship>,
}

pub trait FrontierAnswerer {
    fn answer(&mut self, evidence: &[AcquiredEvidence], audit: &CoverageAudit) -> AnswerDraft;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetUsage {
    pub rounds: u8,
    pub candidates: u32,
    pub context_bytes: u32,
    pub graph_hops: u32,
    pub max_graph_hops: u8,
    pub model_calls: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnsweredInvestigation {
    pub answer: AnswerDraft,
    pub audit: CoverageAudit,
    pub evidence: Vec<AcquiredEvidence>,
    pub relationships: Vec<EvidenceRelationship>,
    pub usage: BudgetUsage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsufficiencyReport {
    pub message: String,
    pub missing_facets: Vec<String>,
    pub missing_relationships: Vec<String>,
    pub suggested_queries: Vec<SearchQuery>,
    pub usage: BudgetUsage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum PipelineOutcome {
    Answered(AnsweredInvestigation),
    Insufficient(InsufficiencyReport),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PipelineError {
    #[error("invalid investigation contract: {0}")]
    Contract(String),
    #[error("planner proposal is invalid: {0}")]
    Planner(String),
    #[error("acquisition result exceeds {0}")]
    Budget(&'static str),
    #[error("retained evidence has incomplete retrieval/rerank provenance")]
    InvalidProvenance,
    #[error("the same evidence ID was returned with conflicting content")]
    ConflictingEvidence,
    #[error("frontier answer is blank")]
    BlankAnswer,
    #[error("frontier answer failed mandatory-facet validation: {0}")]
    AnswerValidation(String),
    #[error("frontier answer claims an unsupported relationship")]
    UnsupportedRelationship,
}

pub struct InvestigationPipeline<A, F> {
    acquirer: A,
    answerer: F,
}

impl<A: EvidenceAcquirer, F: FrontierAnswerer> InvestigationPipeline<A, F> {
    #[must_use]
    pub const fn new(acquirer: A, answerer: F) -> Self {
        Self { acquirer, answerer }
    }

    /// Runs bounded acquisition and makes the answerer reachable only after a
    /// deterministic complete-coverage audit.
    ///
    /// # Errors
    ///
    /// Rejects invalid proposals, budgets, provenance, duplicate identities,
    /// unsupported relationship claims, or an invalid final answer.
    #[allow(clippy::too_many_lines)]
    pub fn run(
        &mut self,
        mut contract: InvestigationContract,
        operating_point: OperatingPoint,
        planner: Option<&PlannerProposal>,
        critic: Option<&CriticProposal>,
    ) -> Result<PipelineOutcome, PipelineError> {
        contract
            .validate()
            .map_err(|error| PipelineError::Contract(error.to_string()))?;
        let mut usage = BudgetUsage::default();
        let mut model_queries = Vec::new();
        if operating_point != OperatingPoint::HardRulesOnly {
            let proposal =
                planner.ok_or_else(|| PipelineError::Planner("missing planner".into()))?;
            contract = proposal
                .apply(&contract)
                .map_err(|error| PipelineError::Planner(error.to_string()))?;
            usage.model_calls = 1;
            model_queries.extend(proposal.search_queries.iter().cloned());
            if operating_point == OperatingPoint::PlannerAndCritic {
                let proposal =
                    critic.ok_or_else(|| PipelineError::Planner("missing critic".into()))?;
                contract = proposal
                    .apply(&contract)
                    .map_err(|error| PipelineError::Planner(error.to_string()))?;
                usage.model_calls = 2;
                model_queries.extend(proposal.search_queries.iter().cloned());
            }
        }
        if usage.model_calls > contract.budget.max_model_calls {
            return Err(PipelineError::Budget("model-call budget"));
        }

        let mut retained = BTreeMap::<String, AcquiredEvidence>::new();
        let mut relationships = BTreeSet::<EvidenceRelationship>::new();
        let mut audit = contract.audit(&[], &[]);
        for round in 1..=contract.budget.max_rounds {
            let proposal = hard_coded_proposal(&contract, &audit);
            let missing = audit.missing_facets.iter().collect::<BTreeSet<_>>();
            let mut queries = proposal.search_queries;
            queries.extend(
                model_queries
                    .iter()
                    .filter(|query| missing.contains(&query.facet_id))
                    .cloned(),
            );
            queries.sort_by(|left, right| {
                (&left.facet_id, &left.query).cmp(&(&right.facet_id, &right.query))
            });
            queries.dedup();
            let result = self.acquirer.acquire(&AcquisitionRequest {
                round,
                missing_facets: audit.missing_facets.clone(),
                missing_relationships: audit.missing_relationships.clone(),
                queries,
                max_candidates: contract.budget.max_candidates_per_round,
                remaining_context_bytes: contract
                    .budget
                    .max_context_bytes
                    .saturating_sub(usage.context_bytes),
                max_graph_hops: contract.budget.max_graph_hops,
            });
            if result.retained.len() > usize::from(contract.budget.max_candidates_per_round) {
                return Err(PipelineError::Budget("candidate budget"));
            }
            if result.relationships.len() > usize::from(contract.budget.max_candidates_per_round) {
                return Err(PipelineError::Budget("relationship budget"));
            }
            usage.rounds = round;
            usage.candidates = usage
                .candidates
                .saturating_add(u32::try_from(result.retained.len()).unwrap_or(u32::MAX));
            usage.model_calls = usage.model_calls.saturating_add(result.model_calls);
            if usage.model_calls > contract.budget.max_model_calls {
                return Err(PipelineError::Budget("model-call budget"));
            }
            let available_ids = retained
                .keys()
                .cloned()
                .chain(result.retained.iter().map(|item| item.evidence.id.clone()))
                .collect::<BTreeSet<_>>();
            for item in result.retained {
                if !item.provenance.validate() {
                    return Err(PipelineError::InvalidProvenance);
                }
                if item.provenance.graph_hops > contract.budget.max_graph_hops {
                    return Err(PipelineError::Budget("graph-hop budget"));
                }
                if item.provenance.graph_path.iter().any(|edge| {
                    !result.relationships.contains(edge)
                        || !available_ids.contains(&edge.from_evidence)
                        || !available_ids.contains(&edge.to_evidence)
                }) {
                    return Err(PipelineError::InvalidProvenance);
                }
                usage.context_bytes = usage.context_bytes.saturating_add(item.context_bytes);
                usage.graph_hops = usage
                    .graph_hops
                    .saturating_add(u32::from(item.provenance.graph_hops));
                usage.max_graph_hops = usage.max_graph_hops.max(item.provenance.graph_hops);
                if usage.context_bytes > contract.budget.max_context_bytes {
                    return Err(PipelineError::Budget("context-byte budget"));
                }
                if let Some(existing) = retained.get(&item.evidence.id) {
                    if existing != &item {
                        return Err(PipelineError::ConflictingEvidence);
                    }
                } else {
                    retained.insert(item.evidence.id.clone(), item);
                }
            }
            relationships.extend(result.relationships);
            let evidence = retained
                .values()
                .map(|item| item.evidence.clone())
                .collect::<Vec<_>>();
            let relation_rows = relationships.iter().cloned().collect::<Vec<_>>();
            audit = contract.audit(&evidence, &relation_rows);
            if audit.outcome == CoverageOutcome::Complete {
                if usage.model_calls == contract.budget.max_model_calls {
                    return Err(PipelineError::Budget("answerer model-call budget"));
                }
                usage.model_calls += 1;
                let evidence = retained.into_values().collect::<Vec<_>>();
                let answer = self.answerer.answer(&evidence, &audit);
                if answer.text.trim().is_empty() {
                    return Err(PipelineError::BlankAnswer);
                }
                contract
                    .validate_answer(&audit, &AnswerUse::new(answer.cited_evidence.clone()))
                    .map_err(|error| PipelineError::AnswerValidation(error.to_string()))?;
                let retained_ids = evidence
                    .iter()
                    .map(|item| item.evidence.id.as_str())
                    .collect::<BTreeSet<_>>();
                if answer
                    .cited_evidence
                    .iter()
                    .any(|id| !retained_ids.contains(id.as_str()))
                {
                    return Err(PipelineError::AnswerValidation(
                        "answer cites evidence outside the retained context".into(),
                    ));
                }
                if answer
                    .claimed_relationships
                    .iter()
                    .any(|claim| !relationships.contains(claim))
                {
                    return Err(PipelineError::UnsupportedRelationship);
                }
                return Ok(PipelineOutcome::Answered(AnsweredInvestigation {
                    answer,
                    audit,
                    evidence,
                    relationships: relation_rows,
                    usage,
                }));
            }
        }
        let mut suggested_queries = hard_coded_proposal(&contract, &audit).search_queries;
        for required in contract.relationships() {
            if audit.missing_relationships.contains(&required.id) {
                suggested_queries.push(SearchQuery::new(
                    &required.from_facet,
                    format!(
                        "relationship {:?} from {} to {}",
                        required.kind, required.from_facet, required.to_facet
                    ),
                ));
            }
        }
        suggested_queries.sort_by(|left, right| {
            (&left.facet_id, &left.query).cmp(&(&right.facet_id, &right.query))
        });
        suggested_queries.dedup();
        Ok(PipelineOutcome::Insufficient(InsufficiencyReport {
            message: "Evidence budget exhausted before every mandatory facet and relationship was covered; run the suggested bounded searches or add the missing source revision.".into(),
            missing_facets: audit.missing_facets,
            missing_relationships: audit.missing_relationships,
            suggested_queries,
            usage,
        }))
    }
}
