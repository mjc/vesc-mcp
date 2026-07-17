//! Small deterministic retrieval-quality evaluator.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Query intent used to report quality by user job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Intent {
    Identifier,
    ErrorText,
    HowTo,
    Concept,
    Comparison,
    Safety,
}

/// Retrieval backend represented in an evaluation report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationMode {
    Legacy,
    Lexical,
    Semantic,
    Hybrid,
}

/// One judged query. Relevance is 0, 1, or 2; zero-valued entries are retained
/// so the fixture can distinguish judged non-relevant results from unjudged ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationQuery {
    pub id: String,
    pub text: String,
    pub intent: Intent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_category: Option<String>,
    pub relevant: BTreeMap<String, u8>,
}

/// Versioned judged-suite metadata for full-corpus model comparisons.
///
/// The metadata is part of the fixture identity: a model result is not
/// comparable when it was produced from another corpus or another set of
/// judged queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationSuite {
    pub schema: u16,
    pub suite_id: String,
    pub corpus_digest: String,
    pub corpus_documents: usize,
    pub corpus_chunks: usize,
    pub queries: Vec<EvaluationQuery>,
}

/// Required VESCM-165 model-discrimination categories.
pub const V2_FAILURE_CATEGORIES: [&str; 5] = [
    "conceptual_to_implementation",
    "identifier_to_declaration",
    "description_to_obscure_c_function",
    "vesc_tool_to_firmware",
    "refloat_to_control_code",
];

impl EvaluationSuite {
    /// Returns the five model-discrimination categories declared by VESCM-165.
    #[must_use]
    pub fn failure_categories(&self) -> BTreeSet<String> {
        self.queries
            .iter()
            .filter_map(|query| query.failure_category.clone())
            .collect()
    }

    /// Checks that this suite is comparable with one exact corpus artifact.
    ///
    /// Relevance IDs are intentionally checked against the artifact's actual
    /// chunk IDs at runtime. This prevents a plausible-looking quality report
    /// from silently evaluating a different corpus or stale judged IDs.
    ///
    /// # Errors
    ///
    /// Returns a description of the first suite/artifact identity or judgment
    /// mismatch.
    pub fn validate_for_corpus(
        &self,
        corpus_digest: &str,
        corpus_documents: usize,
        corpus_chunks: usize,
        chunk_ids: &BTreeSet<String>,
    ) -> Result<(), String> {
        if self.schema != 2 {
            return Err(format!(
                "expected evaluation suite schema 2, got {}",
                self.schema
            ));
        }
        if self.suite_id.trim().is_empty() {
            return Err("evaluation suite_id must not be empty".into());
        }
        if self.corpus_digest != corpus_digest {
            return Err(format!(
                "evaluation suite corpus digest {} does not match artifact {}",
                self.corpus_digest, corpus_digest
            ));
        }
        if self.corpus_documents != corpus_documents {
            return Err(format!(
                "evaluation suite document count {} does not match artifact {}",
                self.corpus_documents, corpus_documents
            ));
        }
        if self.corpus_chunks != corpus_chunks {
            return Err(format!(
                "evaluation suite chunk count {} does not match artifact {}",
                self.corpus_chunks, corpus_chunks
            ));
        }
        if self.queries.is_empty() {
            return Err("evaluation suite must contain at least one query".into());
        }

        let required_categories: BTreeSet<_> = V2_FAILURE_CATEGORIES
            .iter()
            .map(|category| (*category).to_owned())
            .collect();
        let categories = self.failure_categories();
        if categories != required_categories {
            return Err(format!(
                "evaluation suite categories must be exactly {required_categories:?}, got {categories:?}"
            ));
        }

        let mut query_ids = BTreeSet::new();
        for query in &self.queries {
            if query.id.trim().is_empty() || !query_ids.insert(query.id.clone()) {
                return Err(format!(
                    "evaluation query ID is empty or duplicated: {:?}",
                    query.id
                ));
            }
            if query.text.trim().is_empty() {
                return Err(format!("evaluation query {} has empty text", query.id));
            }
            let category = query
                .failure_category
                .as_deref()
                .ok_or_else(|| format!("evaluation query {} has no failure category", query.id))?;
            if !required_categories.contains(category) {
                return Err(format!(
                    "evaluation query {} has unknown failure category {}",
                    query.id, category
                ));
            }
            if query.relevant.is_empty() {
                return Err(format!(
                    "evaluation query {} has no relevance judgments",
                    query.id
                ));
            }
            for (chunk_id, grade) in &query.relevant {
                if !chunk_id.starts_with("chunk-") {
                    return Err(format!(
                        "evaluation query {} references non-corpus ID {}",
                        query.id, chunk_id
                    ));
                }
                if !chunk_ids.contains(chunk_id) {
                    return Err(format!(
                        "evaluation query {} references unknown corpus chunk {}",
                        query.id, chunk_id
                    ));
                }
                if *grade > 2 {
                    return Err(format!(
                        "evaluation query {} has invalid relevance grade {} for {}",
                        query.id, grade, chunk_id
                    ));
                }
            }
        }
        Ok(())
    }
}

/// A ranked result with optional identity metadata for diversity reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievedHit {
    pub id: String,
    pub document_id: Option<String>,
}

impl RetrievedHit {
    /// Creates a result from an identifier-only backend.
    #[must_use]
    pub fn from_id(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            document_id: None,
        }
    }
}

/// Per-query metrics and the returned identifiers used to explain regressions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryEvaluation {
    pub id: String,
    pub intent: Intent,
    pub recall_at_5: f64,
    pub recall_at_10: f64,
    pub reciprocal_rank_at_10: f64,
    pub ndcg_at_10: f64,
    pub zero_result: bool,
    pub duplicate_rate_at_5: f64,
    pub diversity_at_5: f64,
    pub identifier_query: bool,
    pub top_one_exact: bool,
    pub returned: Vec<String>,
}

/// Aggregate metrics for one declared intent class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentEvaluation {
    pub query_count: usize,
    pub recall_at_5: f64,
    pub recall_at_10: f64,
    pub mrr_at_10: f64,
    pub ndcg_at_10: f64,
    pub zero_result_rate: f64,
    pub duplicate_rate_at_5: f64,
    pub diversity_at_5: f64,
    pub exact_identifier_top_one: f64,
}

/// Deterministic aggregate report for a judged suite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationReport {
    pub mode: EvaluationMode,
    pub warnings: Vec<String>,
    pub query_count: usize,
    pub recall_at_5: f64,
    pub recall_at_10: f64,
    pub mrr_at_10: f64,
    pub ndcg_at_10: f64,
    pub zero_result_rate: f64,
    pub duplicate_rate_at_5: f64,
    pub diversity_at_5: f64,
    pub exact_identifier_top_one: f64,
    pub by_intent: BTreeMap<Intent, IntentEvaluation>,
    pub by_category: BTreeMap<String, IntentEvaluation>,
    pub by_source: BTreeMap<String, IntentEvaluation>,
    pub by_failure_category: BTreeMap<String, IntentEvaluation>,
    pub queries: Vec<QueryEvaluation>,
}

/// Release thresholds for the locked judged set.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityThresholds {
    pub recall_at_5: f64,
    pub mrr_at_10: f64,
    pub ndcg_at_10: f64,
    pub exact_identifier_top_one: f64,
}

impl Default for QualityThresholds {
    fn default() -> Self {
        Self {
            recall_at_5: 0.90,
            mrr_at_10: 0.80,
            ndcg_at_10: 0.80,
            exact_identifier_top_one: 1.0,
        }
    }
}

/// One aggregate metric that prevented a quality gate from passing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityFailure {
    pub metric: String,
    pub actual: f64,
    pub required: f64,
}

/// A deterministic quality-gate result with query-level regression evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityGateReport {
    pub passed: bool,
    pub thresholds: QualityThresholds,
    pub failures: Vec<QualityFailure>,
    pub regression_queries: Vec<QueryEvaluation>,
}

/// Applies release thresholds and preserves the affected query evidence.
#[must_use]
pub fn evaluate_quality_gate(
    report: &EvaluationReport,
    thresholds: QualityThresholds,
) -> QualityGateReport {
    let mut failures = Vec::new();
    for (metric, actual, required) in [
        ("recall_at_5", report.recall_at_5, thresholds.recall_at_5),
        ("mrr_at_10", report.mrr_at_10, thresholds.mrr_at_10),
        ("ndcg_at_10", report.ndcg_at_10, thresholds.ndcg_at_10),
        (
            "exact_identifier_top_one",
            report.exact_identifier_top_one,
            thresholds.exact_identifier_top_one,
        ),
    ] {
        if actual < required {
            failures.push(QualityFailure {
                metric: metric.into(),
                actual,
                required,
            });
        }
    }
    let regression_queries = report
        .queries
        .iter()
        .filter(|query| query.zero_result || (query.identifier_query && !query.top_one_exact))
        .cloned()
        .collect();
    QualityGateReport {
        passed: failures.is_empty(),
        thresholds,
        failures,
        regression_queries,
    }
}

/// Evaluates one ranked result list against graded judgments.
#[must_use]
pub fn evaluate_query(query: &EvaluationQuery, ranked_ids: &[String]) -> QueryEvaluation {
    let ranked = ranked_ids
        .iter()
        .cloned()
        .map(RetrievedHit::from_id)
        .collect::<Vec<_>>();
    evaluate_query_with_hits(query, &ranked)
}

/// Evaluates one ranked result list while retaining document identity for
/// duplicate and diversity metrics.
#[must_use]
pub fn evaluate_query_with_hits(
    query: &EvaluationQuery,
    ranked: &[RetrievedHit],
) -> QueryEvaluation {
    let ranked_ids: Vec<String> = ranked.iter().map(|hit| hit.id.clone()).collect();
    let top_five = ranked_ids.iter().take(5);
    let relevant_count = query.relevant.values().filter(|&&value| value > 0).count();
    let found_at_five = top_five
        .clone()
        .filter(|id| {
            query
                .relevant
                .get(id.as_str())
                .is_some_and(|value| *value > 0)
        })
        .count();
    let recall_at_5 = ratio(found_at_five, relevant_count);
    let found_at_ten = ranked_ids
        .iter()
        .take(10)
        .filter(|id| {
            query
                .relevant
                .get(id.as_str())
                .is_some_and(|value| *value > 0)
        })
        .count();
    let recall_at_10 = ratio(found_at_ten, relevant_count);
    let reciprocal_rank_at_10 = ranked_ids
        .iter()
        .take(10)
        .position(|id| query.relevant.get(id).is_some_and(|value| *value > 0))
        .map_or(0.0, |index| 1.0 / to_f64(index + 1));
    let mut ideal: Vec<u8> = query
        .relevant
        .values()
        .copied()
        .filter(|value| *value > 0)
        .collect();
    ideal.sort_unstable_by_key(|grade| Reverse(*grade));
    let dcg = ranked_ids
        .iter()
        .take(10)
        .enumerate()
        .filter_map(|(index, id)| query.relevant.get(id).map(|grade| (*grade, index)))
        .map(|(grade, index)| f64::from(2_u32.pow(u32::from(grade)) - 1) / to_f64(index + 2).log2())
        .sum::<f64>();
    let idcg = ideal
        .iter()
        .take(10)
        .enumerate()
        .map(|(index, grade)| {
            f64::from(2_u32.pow(u32::from(*grade)) - 1) / to_f64(index + 2).log2()
        })
        .sum::<f64>();

    QueryEvaluation {
        id: query.id.clone(),
        intent: query.intent,
        recall_at_5,
        recall_at_10,
        reciprocal_rank_at_10,
        ndcg_at_10: if idcg == 0.0 { 0.0 } else { dcg / idcg },
        zero_result: ranked.is_empty(),
        duplicate_rate_at_5: duplicate_rate(ranked.iter().take(5)),
        diversity_at_5: diversity(ranked.iter().take(5)),
        identifier_query: query.intent == Intent::Identifier,
        top_one_exact: query.intent == Intent::Identifier
            && ranked_ids
                .first()
                .is_some_and(|id| query.relevant.get(id).is_some_and(|value| *value == 2)),
        // Keep enough of the ranking to independently recompute recall@10,
        // MRR@10, and explain rank changes without rerunning the provider.
        returned: ranked_ids.iter().take(50).cloned().collect(),
    }
}

/// Evaluates a suite while preserving the suite's declared order.
#[must_use]
pub fn evaluate_suite<I>(queries: &[EvaluationQuery], search: I) -> EvaluationReport
where
    I: FnMut(&str) -> Vec<String>,
{
    evaluate_suite_with_mode(queries, EvaluationMode::Legacy, Vec::new(), search)
}

/// Evaluates a suite while recording the selected backend and degradation notes.
#[must_use]
pub fn evaluate_suite_with_mode<I>(
    queries: &[EvaluationQuery],
    mode: EvaluationMode,
    warnings: Vec<String>,
    mut search: I,
) -> EvaluationReport
where
    I: FnMut(&str) -> Vec<String>,
{
    evaluate_suite_with_hits_mode(queries, mode, warnings, |query| {
        search(query)
            .into_iter()
            .map(RetrievedHit::from_id)
            .collect()
    })
}

/// Evaluates a suite with optional result metadata for diversity reporting.
#[must_use]
pub fn evaluate_suite_with_hits_mode<I>(
    queries: &[EvaluationQuery],
    mode: EvaluationMode,
    warnings: Vec<String>,
    mut search: I,
) -> EvaluationReport
where
    I: FnMut(&str) -> Vec<RetrievedHit>,
{
    let results: Vec<_> = queries
        .iter()
        .map(|query| evaluate_query_with_hits(query, &search(&query.text)))
        .collect();
    let query_count = results.len();
    let query_count_f = to_f64(query_count);
    let identifier_results = results.iter().filter(|result| result.identifier_query);
    let identifier_count = identifier_results.clone().count();
    let mut grouped: BTreeMap<Intent, Vec<&QueryEvaluation>> = BTreeMap::new();
    for result in &results {
        grouped.entry(result.intent).or_default().push(result);
    }
    let by_intent = grouped
        .into_iter()
        .map(|(intent, values)| (intent, summarize_intent(&values)))
        .collect();
    let by_category = grouped_metrics(queries, &results, category_group);
    let by_source = grouped_metrics(queries, &results, source_group);
    let by_failure_category = grouped_metrics(queries, &results, |query| {
        query
            .failure_category
            .clone()
            .unwrap_or_else(|| "unclassified".into())
    });
    EvaluationReport {
        mode,
        warnings,
        query_count,
        recall_at_5: mean(
            results.iter().map(|result| result.recall_at_5),
            query_count_f,
        ),
        recall_at_10: mean(
            results.iter().map(|result| result.recall_at_10),
            query_count_f,
        ),
        mrr_at_10: mean(
            results.iter().map(|result| result.reciprocal_rank_at_10),
            query_count_f,
        ),
        ndcg_at_10: mean(
            results.iter().map(|result| result.ndcg_at_10),
            query_count_f,
        ),
        zero_result_rate: mean(
            results.iter().map(|result| f64::from(result.zero_result)),
            query_count_f,
        ),
        duplicate_rate_at_5: mean(
            results.iter().map(|result| result.duplicate_rate_at_5),
            query_count_f,
        ),
        diversity_at_5: mean(
            results.iter().map(|result| result.diversity_at_5),
            query_count_f,
        ),
        exact_identifier_top_one: mean(
            identifier_results.map(|result| f64::from(result.top_one_exact)),
            to_f64(identifier_count),
        ),
        by_intent,
        by_category,
        by_source,
        by_failure_category,
        queries: results,
    }
}

fn summarize_intent(results: &[&QueryEvaluation]) -> IntentEvaluation {
    let query_count = results.len();
    let query_count_f = to_f64(query_count);
    let identifier_results = results.iter().filter(|result| result.identifier_query);
    let identifier_count = identifier_results.clone().count();
    IntentEvaluation {
        query_count,
        recall_at_5: mean(
            results.iter().map(|result| result.recall_at_5),
            query_count_f,
        ),
        recall_at_10: mean(
            results.iter().map(|result| result.recall_at_10),
            query_count_f,
        ),
        mrr_at_10: mean(
            results.iter().map(|result| result.reciprocal_rank_at_10),
            query_count_f,
        ),
        ndcg_at_10: mean(
            results.iter().map(|result| result.ndcg_at_10),
            query_count_f,
        ),
        zero_result_rate: mean(
            results.iter().map(|result| f64::from(result.zero_result)),
            query_count_f,
        ),
        duplicate_rate_at_5: mean(
            results.iter().map(|result| result.duplicate_rate_at_5),
            query_count_f,
        ),
        diversity_at_5: mean(
            results.iter().map(|result| result.diversity_at_5),
            query_count_f,
        ),
        exact_identifier_top_one: mean(
            identifier_results.map(|result| f64::from(result.top_one_exact)),
            to_f64(identifier_count),
        ),
    }
}

fn grouped_metrics(
    queries: &[EvaluationQuery],
    results: &[QueryEvaluation],
    mut key: impl FnMut(&EvaluationQuery) -> String,
) -> BTreeMap<String, IntentEvaluation> {
    let mut grouped: BTreeMap<String, Vec<&QueryEvaluation>> = BTreeMap::new();
    for (query, result) in queries.iter().zip(results) {
        grouped.entry(key(query)).or_default().push(result);
    }
    grouped
        .into_iter()
        .map(|(group, values)| (group, summarize_intent(&values)))
        .collect()
}

fn category_group(query: &EvaluationQuery) -> String {
    let groups = query
        .relevant
        .keys()
        .map(|id| {
            if id.starts_with("vesc_c_if.") {
                "firmware_api"
            } else if id.starts_with("refloat_command.") {
                "refloat_command"
            } else if id.contains("native-lib") {
                "native_lib_abi"
            } else if id.contains("nvm") || id.contains("vesc-c-if") {
                "lispbm"
            } else {
                "package_build"
            }
        })
        .collect::<BTreeSet<_>>();
    one_or_mixed(groups)
}

fn source_group(query: &EvaluationQuery) -> String {
    let groups = query
        .relevant
        .keys()
        .filter_map(|id| id.split_once('.').map(|(source, _)| source.to_owned()))
        .collect::<BTreeSet<_>>();
    one_or_mixed(groups)
}

fn one_or_mixed<T>(groups: BTreeSet<T>) -> String
where
    T: Ord + ToString,
{
    if groups.len() == 1 {
        groups
            .into_iter()
            .next()
            .map_or_else(|| "unknown".into(), |group| group.to_string())
    } else if groups.is_empty() {
        "unknown".into()
    } else {
        "mixed".into()
    }
}

fn duplicate_rate<'a>(hits: impl Iterator<Item = &'a RetrievedHit>) -> f64 {
    let hits: Vec<_> = hits.collect();
    if hits.is_empty() {
        return 0.0;
    }
    1.0 - diversity(hits.iter().copied())
}

fn diversity<'a>(hits: impl Iterator<Item = &'a RetrievedHit>) -> f64 {
    let mut identities = BTreeSet::new();
    let mut count = 0_usize;
    for hit in hits {
        count += 1;
        identities.insert(hit.document_id.as_deref().unwrap_or(hit.id.as_str()));
    }
    ratio(identities.len(), count)
}

fn mean(values: impl Iterator<Item = f64>, count: f64) -> f64 {
    if count == 0.0 {
        0.0
    } else {
        values.sum::<f64>() / count
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        to_f64(numerator) / to_f64(denominator)
    }
}

fn to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graded_metrics_match_hand_computed_fixture() {
        let query = EvaluationQuery {
            id: "q1".into(),
            text: "find it".into(),
            intent: Intent::Concept,
            failure_category: None,
            relevant: BTreeMap::from([(String::from("a"), 2), (String::from("b"), 1)]),
        };
        let result = evaluate_query(&query, &[String::from("b"), String::from("x")]);

        assert!((result.recall_at_5 - 0.5).abs() < f64::EPSILON);
        assert!((result.reciprocal_rank_at_10 - 1.0).abs() < f64::EPSILON);
        assert!(result.ndcg_at_10 > 0.0);
        assert!(!result.zero_result);
    }

    #[test]
    fn empty_suite_report_is_zeroed() {
        let report = evaluate_suite(&[], |_| Vec::new());
        assert_eq!(report.query_count, 0);
        assert!(report.mrr_at_10.abs() < f64::EPSILON);
    }

    #[test]
    fn exact_identifier_metric_uses_only_identifier_queries() {
        let queries = vec![
            EvaluationQuery {
                id: "identifier-one".into(),
                text: "one".into(),
                intent: Intent::Identifier,
                failure_category: None,
                relevant: BTreeMap::from([(String::from("one"), 2)]),
            },
            EvaluationQuery {
                id: "concept-one".into(),
                text: "concept".into(),
                intent: Intent::Concept,
                failure_category: None,
                relevant: BTreeMap::from([(String::from("concept"), 2)]),
            },
        ];
        let report = evaluate_suite(&queries, |query| vec![query.into()]);
        assert!((report.exact_identifier_top_one - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn quality_gate_lists_failed_metrics_and_identifier_queries() {
        let queries = vec![EvaluationQuery {
            id: "identifier-miss".into(),
            text: "one".into(),
            intent: Intent::Identifier,
            failure_category: None,
            relevant: BTreeMap::from([(String::from("one"), 2)]),
        }];
        let report = evaluate_suite(&queries, |_| vec!["wrong".into()]);
        let gate = evaluate_quality_gate(&report, QualityThresholds::default());

        assert!(!gate.passed);
        assert!(
            gate.failures
                .iter()
                .any(|failure| failure.metric == "exact_identifier_top_one")
        );
        assert_eq!(gate.regression_queries[0].id, "identifier-miss");
        assert_eq!(gate.regression_queries[0].returned, vec!["wrong"]);
    }

    #[test]
    fn report_includes_judged_category_source_and_diversity_groups() {
        let queries = vec![EvaluationQuery {
            id: "native-query".into(),
            text: "native".into(),
            intent: Intent::Concept,
            failure_category: Some("conceptual_to_implementation".into()),
            relevant: BTreeMap::from([(String::from("priority.native-lib-minimal-abi"), 2)]),
        }];
        let report =
            evaluate_suite_with_hits_mode(&queries, EvaluationMode::Lexical, Vec::new(), |_| {
                vec![
                    RetrievedHit {
                        id: "wrong-one".into(),
                        document_id: Some("document-a".into()),
                    },
                    RetrievedHit {
                        id: "wrong-two".into(),
                        document_id: Some("document-a".into()),
                    },
                ]
            });

        assert_eq!(report.by_category["native_lib_abi"].query_count, 1);
        assert_eq!(report.by_source["priority"].query_count, 1);
        assert!((report.duplicate_rate_at_5 - 0.5).abs() < f64::EPSILON);
        assert!((report.diversity_at_5 - 0.5).abs() < f64::EPSILON);
    }

    fn valid_v2_suite() -> EvaluationSuite {
        EvaluationSuite {
            schema: 2,
            suite_id: "v2-test".into(),
            corpus_digest: "sha256:test".into(),
            corpus_documents: 1,
            corpus_chunks: 5,
            queries: V2_FAILURE_CATEGORIES
                .iter()
                .enumerate()
                .map(|(index, category)| EvaluationQuery {
                    id: format!("q{index}"),
                    text: format!("query {index}"),
                    intent: Intent::Concept,
                    failure_category: Some((*category).into()),
                    relevant: BTreeMap::from([(format!("chunk-{index}"), 2)]),
                })
                .collect(),
        }
    }

    #[test]
    fn v2_suite_validation_accepts_exact_corpus_identity() {
        let suite = valid_v2_suite();
        let ids = (0..5).map(|index| format!("chunk-{index}")).collect();
        assert!(suite.validate_for_corpus("sha256:test", 1, 5, &ids).is_ok());
    }

    #[test]
    fn v2_suite_validation_rejects_stale_or_unknown_judgments() {
        let mut suite = valid_v2_suite();
        suite.corpus_digest = "sha256:stale".into();
        let ids = (0..5).map(|index| format!("chunk-{index}")).collect();
        let error = suite
            .validate_for_corpus("sha256:test", 1, 5, &ids)
            .expect_err("stale corpus must fail");
        assert!(error.contains("does not match artifact"));

        let mut suite = valid_v2_suite();
        suite.queries[0].relevant = BTreeMap::from([(String::from("chunk-missing"), 2)]);
        let error = suite
            .validate_for_corpus("sha256:test", 1, 5, &ids)
            .expect_err("unknown chunk must fail");
        assert!(error.contains("unknown corpus chunk"));
    }

    #[test]
    fn v2_suite_validation_rejects_missing_category() {
        let mut suite = valid_v2_suite();
        suite.queries.pop();
        let ids = (0..5).map(|index| format!("chunk-{index}")).collect();
        let error = suite
            .validate_for_corpus("sha256:test", 1, 5, &ids)
            .expect_err("missing category must fail");
        assert!(error.contains("categories must be exactly"));
    }
}
