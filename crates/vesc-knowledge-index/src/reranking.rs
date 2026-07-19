//! Deterministic per-facet candidate retention for bounded investigations.

use std::{collections::BTreeMap, error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::investigation::{Era, Evidence, InvestigationContract};

pub const MAX_FACET_QUOTA: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FacetQuota(usize);

impl FacetQuota {
    /// Creates a bounded per-facet quota.
    ///
    /// # Errors
    ///
    /// Returns an error unless `value` is in `1..=4`.
    pub const fn new(value: usize) -> Result<Self, FacetQuotaError> {
        if value == 0 || value > MAX_FACET_QUOTA {
            Err(FacetQuotaError(value))
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FacetQuotaError(usize);

impl fmt::Display for FacetQuotaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "facet quota {} is outside 1..={MAX_FACET_QUOTA}",
            self.0
        )
    }
}

impl Error for FacetQuotaError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FacetCandidate {
    pub evidence: Evidence,
    pub retrieval_score: f32,
    pub rerank_score: Option<f32>,
}

impl FacetCandidate {
    #[must_use]
    pub const fn new(evidence: Evidence, retrieval_score: f32) -> Self {
        Self {
            evidence,
            retrieval_score,
            rerank_score: None,
        }
    }

    #[must_use]
    pub const fn with_rerank_score(mut self, score: f32) -> Self {
        self.rerank_score = Some(score);
        self
    }

    fn score(&self) -> f32 {
        self.rerank_score.unwrap_or(self.retrieval_score)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedFacetCandidate {
    pub facet_id: String,
    pub facet_rank: usize,
    pub evidence: Evidence,
    pub retrieval_score: f32,
    pub rerank_score: Option<f32>,
}

/// Retains an independent deterministic quota inside every matching facet.
#[must_use]
pub fn retain_per_facet(
    contract: &InvestigationContract,
    candidates: Vec<FacetCandidate>,
    quota: FacetQuota,
) -> Vec<RetainedFacetCandidate> {
    let mut by_facet = BTreeMap::<String, Vec<FacetCandidate>>::new();
    for candidate in candidates {
        if !candidate.score().is_finite() || !candidate.retrieval_score.is_finite() {
            continue;
        }
        for facet in contract.facets() {
            let era_matches = match &facet.era {
                Era::Exact(revision) => revision == &candidate.evidence.revision,
                Era::Any => true,
            };
            if facet.repository == candidate.evidence.repository
                && facet.stage == candidate.evidence.stage
                && era_matches
            {
                by_facet
                    .entry(facet.id.clone())
                    .or_default()
                    .push(candidate.clone());
            }
        }
    }

    let mut retained = Vec::new();
    for (facet_id, mut rows) in by_facet {
        rows.sort_by(|left, right| {
            right
                .score()
                .total_cmp(&left.score())
                .then_with(|| right.retrieval_score.total_cmp(&left.retrieval_score))
                .then_with(|| left.evidence.id.cmp(&right.evidence.id))
        });
        rows.dedup_by(|left, right| left.evidence.id == right.evidence.id);
        retained.extend(
            rows.into_iter()
                .take(quota.0)
                .enumerate()
                .map(|(index, candidate)| RetainedFacetCandidate {
                    facet_id: facet_id.clone(),
                    facet_rank: index + 1,
                    evidence: candidate.evidence,
                    retrieval_score: candidate.retrieval_score,
                    rerank_score: candidate.rerank_score,
                }),
        );
    }
    retained
}
