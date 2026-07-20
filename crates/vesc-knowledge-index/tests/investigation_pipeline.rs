use std::collections::VecDeque;

use vesc_knowledge_index::{
    investigation::{
        Evidence, EvidenceRelationship, HistoricalTraceRevisions, InvestigationContract,
        RelationshipKind, Repository, Stage,
    },
    pipeline::{
        AcquiredEvidence, AcquisitionRequest, AcquisitionRound, AnswerDraft, EvidenceAcquirer,
        EvidenceProvenance, FrontierAnswerer, InvestigationPipeline, OperatingPoint, PipelineError,
        PipelineOutcome,
    },
};

fn contract() -> InvestigationContract {
    InvestigationContract::historical_package_loader(HistoricalTraceRevisions::new(
        "pkg", "fw", "rtos", "consumer",
    ))
}

fn evidence(id: &str, repository: Repository, stage: Stage, revision: &str) -> AcquiredEvidence {
    AcquiredEvidence {
        evidence: Evidence::decisive(id, repository, stage, revision, format!("{id}.c")),
        context_bytes: 100,
        provenance: EvidenceProvenance {
            lexical_rank: Some(1),
            semantic_rank: Some(2),
            graph_path: Vec::new(),
            graph_hops: 0,
            rerank_rank: 1,
            retrieval_score_micros: 1_000_000,
            rerank_score_micros: None,
        },
    }
}

fn complete_round() -> AcquisitionRound {
    let retained = vec![
        evidence(
            "package",
            Repository::VescPackageLib,
            Stage::PackageFormat,
            "pkg",
        ),
        evidence(
            "entry",
            Repository::VescPackageLib,
            Stage::GeneratedEntry,
            "pkg",
        ),
        evidence(
            "loader",
            Repository::VescFirmware,
            Stage::FirmwareLoader,
            "fw",
        ),
        evidence("abi", Repository::VescFirmware, Stage::AbiDispatch, "fw"),
        evidence(
            "rtos",
            Repository::ChibiOs,
            Stage::RuntimeModuleLoading,
            "rtos",
        ),
        evidence(
            "consumer",
            Repository::Refloat,
            Stage::ConsumerInvocation,
            "consumer",
        ),
    ];
    let relationships = vec![
        EvidenceRelationship::new("package", "entry", RelationshipKind::Produces),
        EvidenceRelationship::new("entry", "loader", RelationshipKind::LoadedBy),
        EvidenceRelationship::new("loader", "abi", RelationshipKind::DispatchesTo),
        EvidenceRelationship::new("abi", "rtos", RelationshipKind::RunsIn),
        EvidenceRelationship::new("rtos", "consumer", RelationshipKind::InvokedBy),
    ];
    AcquisitionRound {
        retained,
        relationships,
        model_calls: 0,
    }
}

struct ScriptedAcquirer(VecDeque<AcquisitionRound>);

impl EvidenceAcquirer for ScriptedAcquirer {
    fn acquire(&mut self, request: &AcquisitionRequest) -> AcquisitionRound {
        assert!(request.round > 0);
        assert!(request.max_candidates > 0);
        self.0.pop_front().unwrap_or_default()
    }
}

#[derive(Default)]
struct Answerer {
    calls: usize,
    unsupported_claim: bool,
}

impl FrontierAnswerer for Answerer {
    fn answer(
        &mut self,
        evidence: &[AcquiredEvidence],
        _audit: &vesc_knowledge_index::investigation::CoverageAudit,
    ) -> AnswerDraft {
        self.calls += 1;
        let claimed_relationships = if self.unsupported_claim {
            vec![EvidenceRelationship::new(
                "package",
                "consumer",
                RelationshipKind::LoadedBy,
            )]
        } else {
            Vec::new()
        };
        AnswerDraft {
            text: "complete cited trace".into(),
            cited_evidence: evidence
                .iter()
                .map(|item| item.evidence.id.clone())
                .collect(),
            claimed_relationships,
        }
    }
}

#[test]
fn complete_loader_path_is_the_only_route_to_the_answerer() {
    let acquirer = ScriptedAcquirer(VecDeque::from([complete_round()]));
    let mut pipeline = InvestigationPipeline::new(acquirer, Answerer::default());

    let outcome = pipeline
        .run(contract(), OperatingPoint::HardRulesOnly, None, None)
        .expect("complete pipeline");

    let PipelineOutcome::Answered(answered) = outcome else {
        panic!("complete evidence must answer")
    };
    assert_eq!(answered.audit.qualifying_evidence.len(), 6);
    assert_eq!(answered.relationships.len(), 5);
    assert_eq!(answered.usage.rounds, 1);
    assert_eq!(answered.usage.model_calls, 1);
}

#[test]
fn missing_facet_returns_actionable_insufficiency_without_answering() {
    let mut round = complete_round();
    round.retained.retain(|item| item.evidence.id != "rtos");
    round
        .relationships
        .retain(|edge| edge.from_evidence != "rtos" && edge.to_evidence != "rtos");
    let acquirer = ScriptedAcquirer(VecDeque::from([round]));
    let mut pipeline = InvestigationPipeline::new(acquirer, Answerer::default());

    let outcome = pipeline
        .run(contract(), OperatingPoint::HardRulesOnly, None, None)
        .expect("insufficient report");

    let PipelineOutcome::Insufficient(report) = outcome else {
        panic!("incomplete evidence must not answer")
    };
    assert!(
        report
            .missing_facets
            .contains(&"runtime-module-loading".into())
    );
    assert!(
        report
            .suggested_queries
            .iter()
            .any(|query| query.facet_id == "runtime-module-loading")
    );
    assert_eq!(report.usage.model_calls, 0);
}

#[test]
fn unsupported_answer_path_is_rejected_after_coverage() {
    let acquirer = ScriptedAcquirer(VecDeque::from([complete_round()]));
    let answerer = Answerer {
        unsupported_claim: true,
        ..Answerer::default()
    };
    let mut pipeline = InvestigationPipeline::new(acquirer, answerer);

    assert_eq!(
        pipeline.run(contract(), OperatingPoint::HardRulesOnly, None, None),
        Err(PipelineError::UnsupportedRelationship)
    );
}

#[test]
fn candidate_context_graph_and_model_budgets_fail_closed() {
    let mut candidate_overflow = complete_round();
    candidate_overflow.retained.resize_with(129, || {
        evidence(
            "duplicate",
            Repository::Refloat,
            Stage::ConsumerInvocation,
            "consumer",
        )
    });
    let mut pipeline = InvestigationPipeline::new(
        ScriptedAcquirer(VecDeque::from([candidate_overflow])),
        Answerer::default(),
    );
    assert_eq!(
        pipeline.run(contract(), OperatingPoint::HardRulesOnly, None, None),
        Err(PipelineError::Budget("candidate budget"))
    );

    let mut graph_overflow = complete_round();
    graph_overflow.retained[0].provenance.graph_hops = 5;
    graph_overflow.retained[0].provenance.graph_path =
        vec![EvidenceRelationship::new("package", "entry", RelationshipKind::Produces); 5];
    let mut pipeline = InvestigationPipeline::new(
        ScriptedAcquirer(VecDeque::from([graph_overflow])),
        Answerer::default(),
    );
    assert_eq!(
        pipeline.run(contract(), OperatingPoint::HardRulesOnly, None, None),
        Err(PipelineError::Budget("graph-hop budget"))
    );

    let mut context_overflow = complete_round();
    context_overflow.retained[0].context_bytes = 65_537;
    let mut pipeline = InvestigationPipeline::new(
        ScriptedAcquirer(VecDeque::from([context_overflow])),
        Answerer::default(),
    );
    assert_eq!(
        pipeline.run(contract(), OperatingPoint::HardRulesOnly, None, None),
        Err(PipelineError::Budget("context-byte budget"))
    );

    let mut model_overflow = complete_round();
    model_overflow.model_calls = 5;
    let mut pipeline = InvestigationPipeline::new(
        ScriptedAcquirer(VecDeque::from([model_overflow])),
        Answerer::default(),
    );
    assert_eq!(
        pipeline.run(contract(), OperatingPoint::HardRulesOnly, None, None),
        Err(PipelineError::Budget("model-call budget"))
    );

    let mut relationship_overflow = complete_round();
    relationship_overflow.relationships.resize(
        129,
        EvidenceRelationship::new("package", "entry", RelationshipKind::Produces),
    );
    let mut pipeline = InvestigationPipeline::new(
        ScriptedAcquirer(VecDeque::from([relationship_overflow])),
        Answerer::default(),
    );
    assert_eq!(
        pipeline.run(contract(), OperatingPoint::HardRulesOnly, None, None),
        Err(PipelineError::Budget("relationship budget"))
    );
}

#[test]
fn wrong_era_adversary_never_opens_the_answer_gate() {
    let mut round = complete_round();
    round.retained[2].evidence.revision = "wrong-era".into();
    let mut pipeline = InvestigationPipeline::new(
        ScriptedAcquirer(VecDeque::from([round])),
        Answerer::default(),
    );
    let outcome = pipeline
        .run(contract(), OperatingPoint::HardRulesOnly, None, None)
        .expect("insufficiency");
    assert!(matches!(outcome, PipelineOutcome::Insufficient(_)));
}
