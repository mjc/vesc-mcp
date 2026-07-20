use std::{
    collections::{BTreeSet, VecDeque},
    env,
    error::Error,
    fmt::Write as _,
    fs,
    path::Path,
};

use serde::Serialize;
use vesc_knowledge_index::{
    path_evaluation::{EvidenceIdentity, PathEvaluationCase, PathEvaluationSuite},
    pipeline::{
        AcquiredEvidence, AcquisitionRequest, AcquisitionRound, AnswerDraft, BudgetUsage,
        EvidenceAcquirer, EvidenceProvenance, FrontierAnswerer, InvestigationPipeline,
        OperatingPoint, PipelineOutcome,
    },
    planning::{CriticProposal, PlannerProposal},
};

#[derive(Serialize)]
struct Report {
    schema: u16,
    suite_id: String,
    case_id: String,
    operating_points: Vec<OperatingPointReport>,
}

#[derive(Serialize)]
struct OperatingPointReport {
    operating_point: OperatingPoint,
    complete_path_answered: bool,
    complete_path_facets: usize,
    complete_path_relationships: usize,
    missing_facet_answered: bool,
    missing_facets: Vec<String>,
    frontier_shortcut_rate: f64,
    complete_usage: BudgetUsage,
    incomplete_usage: BudgetUsage,
}

struct Acquirer(VecDeque<AcquisitionRound>);

impl EvidenceAcquirer for Acquirer {
    fn acquire(&mut self, _request: &AcquisitionRequest) -> AcquisitionRound {
        self.0.pop_front().unwrap_or_default()
    }
}

struct Answerer(Vec<vesc_knowledge_index::investigation::EvidenceRelationship>);

impl FrontierAnswerer for Answerer {
    fn answer(
        &mut self,
        evidence: &[AcquiredEvidence],
        _audit: &vesc_knowledge_index::investigation::CoverageAudit,
    ) -> AnswerDraft {
        AnswerDraft {
            text: "The locked loader path is complete and every mandatory stage is cited.".into(),
            cited_evidence: evidence
                .iter()
                .map(|item| item.evidence.id.clone())
                .collect(),
            claimed_relationships: self.0.clone(),
        }
    }
}

fn retained(
    rows: impl IntoIterator<Item = EvidenceIdentity>,
    relationships: &[vesc_knowledge_index::investigation::EvidenceRelationship],
) -> Vec<AcquiredEvidence> {
    let rows = rows.into_iter().collect::<Vec<_>>();
    let ids = rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<BTreeSet<_>>();
    rows.into_iter()
        .enumerate()
        .map(|(index, identity)| {
            let graph_path = relationships
                .iter()
                .find(|edge| {
                    edge.to_evidence == identity.id
                        && ids.contains(&edge.from_evidence)
                        && ids.contains(&edge.to_evidence)
                })
                .cloned()
                .into_iter()
                .collect::<Vec<_>>();
            AcquiredEvidence {
                evidence: identity.as_evidence(),
                context_bytes: u32::try_from(identity.reranker_text().len()).unwrap_or(u32::MAX),
                provenance: EvidenceProvenance {
                    lexical_rank: Some(u16::try_from(index + 1).unwrap_or(u16::MAX)),
                    semantic_rank: Some(u16::try_from(index + 1).unwrap_or(u16::MAX)),
                    graph_hops: u8::try_from(graph_path.len()).unwrap_or(u8::MAX),
                    graph_path,
                    rerank_rank: u16::try_from(index + 1).unwrap_or(u16::MAX),
                    retrieval_score_micros: i64::try_from(1_000_000_usize.saturating_sub(index))
                        .unwrap_or(i64::MAX),
                    rerank_score_micros: None,
                },
            }
        })
        .collect()
}

fn run_point(
    case: &PathEvaluationCase,
    operating_point: OperatingPoint,
) -> Result<OperatingPointReport, Box<dyn Error>> {
    let planner = PlannerProposal::new(Vec::new(), Vec::new(), Vec::new(), true);
    let critic = CriticProposal::new(Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let proposals = match operating_point {
        OperatingPoint::HardRulesOnly => (None, None),
        OperatingPoint::FastPlanner => (Some(&planner), None),
        OperatingPoint::PlannerAndCritic => (Some(&planner), Some(&critic)),
    };

    let complete_round = AcquisitionRound {
        retained: retained(case.judgments.clone(), &case.relationships),
        relationships: case.relationships.clone(),
        model_calls: 0,
    };
    let mut complete = InvestigationPipeline::new(
        Acquirer(VecDeque::from([complete_round])),
        Answerer(case.relationships.clone()),
    );
    let complete_outcome =
        complete.run(case.contract(), operating_point, proposals.0, proposals.1)?;
    let PipelineOutcome::Answered(complete_result) = complete_outcome else {
        return Err("gold loader path did not answer".into());
    };

    let bundle = case
        .adversarial_bundles
        .iter()
        .find(|bundle| !bundle.expected_missing_facets.is_empty())
        .ok_or("locked case has no missing-facet adversary")?;
    let identities = case
        .judgments
        .iter()
        .chain(&case.distractors)
        .filter(|identity| bundle.evidence_ids.contains(&identity.id))
        .cloned()
        .collect::<Vec<_>>();
    let identity_ids = identities
        .iter()
        .map(|identity| &identity.id)
        .collect::<BTreeSet<_>>();
    let incomplete_relationships = case
        .relationships
        .iter()
        .filter(|edge| {
            identity_ids.contains(&edge.from_evidence) && identity_ids.contains(&edge.to_evidence)
        })
        .cloned()
        .collect::<Vec<_>>();
    let incomplete_round = AcquisitionRound {
        retained: retained(identities, &incomplete_relationships),
        relationships: incomplete_relationships,
        model_calls: 0,
    };
    let mut incomplete = InvestigationPipeline::new(
        Acquirer(VecDeque::from([incomplete_round])),
        Answerer(case.relationships.clone()),
    );
    let incomplete_outcome =
        incomplete.run(case.contract(), operating_point, proposals.0, proposals.1)?;
    let PipelineOutcome::Insufficient(incomplete_result) = incomplete_outcome else {
        return Err("missing-facet adversary reached the answerer".into());
    };

    Ok(OperatingPointReport {
        operating_point,
        complete_path_answered: true,
        complete_path_facets: complete_result.audit.qualifying_evidence.len(),
        complete_path_relationships: complete_result.relationships.len(),
        missing_facet_answered: false,
        missing_facets: incomplete_result.missing_facets,
        frontier_shortcut_rate: 0.0,
        complete_usage: complete_result.usage,
        incomplete_usage: incomplete_result.usage,
    })
}

fn markdown(report: &Report) -> String {
    let mut output = format!(
        "# VESCM-200 bounded pipeline evaluation\n\nSuite: `{}` · case: `{}`\n\n| Operating point | Complete path | Missing-facet answer | FrontierShortcutRate | Model calls (complete/incomplete) |\n|---|---:|---:|---:|---:|\n",
        report.suite_id, report.case_id
    );
    for row in &report.operating_points {
        writeln!(
            output,
            "| {:?} | {} | {} | {:.3} | {}/{} |",
            row.operating_point,
            row.complete_path_answered,
            row.missing_facet_answered,
            row.frontier_shortcut_rate,
            row.complete_usage.model_calls,
            row.incomplete_usage.model_calls,
        )
        .expect("write to string");
    }
    output.push_str("\nAll operating points answer only the six-facet, five-relationship gold path. The missing-facet adversary returns an insufficiency report without calling the answerer.\n");
    output
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("usage: pipeline_eval <suite.json> <report.json> <report.md>".into());
    }
    let suite: PathEvaluationSuite = serde_json::from_slice(&fs::read(&args[0])?)?;
    suite.validate()?;
    let case = suite.cases.first().ok_or("suite has no case")?;
    let report = Report {
        schema: 1,
        suite_id: suite.suite_id.clone(),
        case_id: case.id.clone(),
        operating_points: [
            OperatingPoint::HardRulesOnly,
            OperatingPoint::FastPlanner,
            OperatingPoint::PlannerAndCritic,
        ]
        .into_iter()
        .map(|point| run_point(case, point))
        .collect::<Result<Vec<_>, _>>()?,
    };
    write(
        Path::new(&args[1]),
        &(serde_json::to_string_pretty(&report)? + "\n").into_bytes(),
    )?;
    write(Path::new(&args[2]), markdown(&report).as_bytes())?;
    Ok(())
}
