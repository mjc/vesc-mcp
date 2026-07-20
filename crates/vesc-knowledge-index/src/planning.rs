//! Strict bounded output contract for optional local investigation planners.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::investigation::{
    ContractError, CoverageAudit, FacetRequirement, InvestigationContract, PlannerExtension,
    RelationshipRequirement,
};

const PLANNER_SCHEMA: u16 = 1;
const MAX_ADDITIONS: usize = 8;
const MAX_SEARCH_QUERIES: usize = 8;
const MAX_QUERY_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchQuery {
    pub facet_id: String,
    pub query: String,
}

impl SearchQuery {
    #[must_use]
    pub fn new(facet_id: impl Into<String>, query: impl Into<String>) -> Self {
        Self {
            facet_id: facet_id.into(),
            query: query.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannerProposal {
    pub schema: u16,
    pub facet_additions: Vec<FacetRequirement>,
    pub relationship_additions: Vec<RelationshipRequirement>,
    pub search_queries: Vec<SearchQuery>,
    pub request_critic: bool,
}

impl PlannerProposal {
    #[must_use]
    pub const fn new(
        facet_additions: Vec<FacetRequirement>,
        relationship_additions: Vec<RelationshipRequirement>,
        search_queries: Vec<SearchQuery>,
        request_critic: bool,
    ) -> Self {
        Self {
            schema: PLANNER_SCHEMA,
            facet_additions,
            relationship_additions,
            search_queries,
            request_critic,
        }
    }

    /// Applies a validated monotonic extension to an authoritative contract.
    ///
    /// # Errors
    ///
    /// Rejects invalid schemas, unbounded or duplicate queries, unknown facet
    /// references, and any conflicting contract addition.
    pub fn apply(
        &self,
        contract: &InvestigationContract,
    ) -> Result<InvestigationContract, PlannerProposalError> {
        if self.schema != PLANNER_SCHEMA {
            return Err(PlannerProposalError::InvalidSchema);
        }
        if self.facet_additions.len() > MAX_ADDITIONS
            || self.relationship_additions.len() > MAX_ADDITIONS
            || self.search_queries.len() > MAX_SEARCH_QUERIES
        {
            return Err(PlannerProposalError::LimitExceeded);
        }
        let extended = contract.clone().extend(
            PlannerExtension::new(self.facet_additions.clone())
                .with_relationships(self.relationship_additions.clone()),
        )?;
        let known_facets = extended
            .facets()
            .iter()
            .map(|facet| facet.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut unique_queries = BTreeSet::new();
        for query in &self.search_queries {
            if !known_facets.contains(query.facet_id.as_str()) {
                return Err(PlannerProposalError::UnknownQueryFacet);
            }
            if query.query.trim().is_empty()
                || query.query.len() > MAX_QUERY_BYTES
                || query.query.chars().any(char::is_control)
                || !unique_queries.insert((&query.facet_id, &query.query))
            {
                return Err(PlannerProposalError::InvalidQuery);
            }
        }
        Ok(extended)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlannerProposalError {
    #[error("unsupported planner proposal schema")]
    InvalidSchema,
    #[error("planner proposal exceeds a fixed bound")]
    LimitExceeded,
    #[error("planner query references an unknown facet")]
    UnknownQueryFacet,
    #[error("planner query is blank, duplicated, too long, or contains control characters")]
    InvalidQuery,
    #[error(transparent)]
    Contract(#[from] ContractError),
}

/// Produces the legal no-model baseline: one exact query per missing facet.
#[must_use]
pub fn hard_coded_proposal(
    contract: &InvestigationContract,
    audit: &CoverageAudit,
) -> PlannerProposal {
    let missing = audit.missing_facets.iter().collect::<BTreeSet<_>>();
    let queries = contract
        .facets()
        .iter()
        .filter(|facet| missing.contains(&facet.id))
        .map(|facet| {
            SearchQuery::new(
                &facet.id,
                format!("{:?} {:?} {:?}", facet.repository, facet.stage, facet.era),
            )
        })
        .collect();
    PlannerProposal::new(Vec::new(), Vec::new(), queries, false)
}
