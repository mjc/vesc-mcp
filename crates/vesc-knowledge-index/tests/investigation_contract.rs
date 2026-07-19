use std::collections::BTreeSet;

use vesc_knowledge_index::investigation::{
    AnswerUse, CoverageOutcome, Era, Evidence, EvidenceRelationship, FacetRequirement,
    HistoricalTraceRevisions, InvestigationContract, PlannerExtension, RelationshipKind,
    Repository, Stage,
};

fn contract() -> InvestigationContract {
    InvestigationContract::historical_package_loader(HistoricalTraceRevisions::new(
        "pkg-rev",
        "firmware-rev",
        "rtos-rev",
        "consumer-rev",
    ))
}

fn complete_evidence() -> Vec<Evidence> {
    vec![
        Evidence::decisive(
            "package-format-evidence",
            Repository::VescPackageLib,
            Stage::PackageFormat,
            "pkg-rev",
            "src/package.rs",
        ),
        Evidence::decisive(
            "generated-entry-evidence",
            Repository::VescPackageLib,
            Stage::GeneratedEntry,
            "pkg-rev",
            "generated/package.c",
        ),
        Evidence::decisive(
            "firmware-loader-evidence",
            Repository::VescFirmware,
            Stage::FirmwareLoader,
            "firmware-rev",
            "codeloader.c",
        ),
        Evidence::decisive(
            "abi-dispatch-evidence",
            Repository::VescFirmware,
            Stage::AbiDispatch,
            "firmware-rev",
            "lispif.c",
        ),
        Evidence::decisive(
            "runtime-loader-evidence",
            Repository::ChibiOs,
            Stage::RuntimeModuleLoading,
            "rtos-rev",
            "os/rt/modules.c",
        ),
        Evidence::decisive(
            "consumer-evidence",
            Repository::Refloat,
            Stage::ConsumerInvocation,
            "consumer-rev",
            "src/main.c",
        ),
    ]
}

fn complete_relationships() -> Vec<EvidenceRelationship> {
    vec![
        EvidenceRelationship::new(
            "package-format-evidence",
            "generated-entry-evidence",
            RelationshipKind::Produces,
        ),
        EvidenceRelationship::new(
            "generated-entry-evidence",
            "firmware-loader-evidence",
            RelationshipKind::LoadedBy,
        ),
        EvidenceRelationship::new(
            "firmware-loader-evidence",
            "abi-dispatch-evidence",
            RelationshipKind::DispatchesTo,
        ),
        EvidenceRelationship::new(
            "abi-dispatch-evidence",
            "runtime-loader-evidence",
            RelationshipKind::RunsIn,
        ),
        EvidenceRelationship::new(
            "runtime-loader-evidence",
            "consumer-evidence",
            RelationshipKind::InvokedBy,
        ),
    ]
}

#[test]
fn planner_extension_is_monotonic_and_deterministic() {
    let baseline = contract();
    let extension = PlannerExtension::new(vec![FacetRequirement::new(
        "board-config",
        Repository::VescFirmware,
        Stage::Configuration,
        Era::exact("firmware-rev"),
    )]);

    let merged = baseline
        .clone()
        .extend(extension)
        .expect("bounded extension");

    assert!(
        baseline
            .facets()
            .iter()
            .all(|required| merged.facets().contains(required))
    );
    assert_eq!(merged.facets().len(), baseline.facets().len() + 1);
    assert_eq!(
        serde_json::to_vec(&merged).expect("serialize"),
        serde_json::to_vec(&merged).expect("serialize again")
    );
}

#[test]
fn planner_cannot_weaken_a_hard_requirement() {
    let baseline = contract();
    let conflicting = PlannerExtension::new(vec![FacetRequirement::new(
        "firmware-loader",
        Repository::VescFirmware,
        Stage::FirmwareLoader,
        Era::exact("different-era"),
    )]);

    assert!(baseline.extend(conflicting).is_err());
}

#[test]
fn audit_rejects_wrong_repository_stage_era_and_adjacent_evidence() {
    let contract = contract();
    let evidence = vec![
        Evidence::decisive(
            "wrong-repository",
            Repository::Refloat,
            Stage::FirmwareLoader,
            "firmware-rev",
            "codeloader.c",
        ),
        Evidence::decisive(
            "wrong-stage",
            Repository::VescFirmware,
            Stage::Configuration,
            "firmware-rev",
            "codeloader.c",
        ),
        Evidence::decisive(
            "wrong-era",
            Repository::VescFirmware,
            Stage::FirmwareLoader,
            "old-firmware-rev",
            "codeloader.c",
        ),
        Evidence::adjacent(
            "adjacent",
            Repository::VescFirmware,
            Stage::FirmwareLoader,
            "firmware-rev",
            "README.md",
        ),
    ];

    let audit = contract.audit(&evidence, &[]);

    assert_eq!(audit.outcome, CoverageOutcome::Insufficient);
    assert!(audit.missing_facets.contains(&"firmware-loader".into()));
    assert_eq!(audit.rejected_evidence.len(), 4);
}

#[test]
fn complete_bundle_requires_every_facet_and_relationship() {
    let contract = contract();
    let evidence = complete_evidence();
    let relationships = complete_relationships();

    let without_relationships = contract.audit(&evidence, &[]);
    let complete = contract.audit(&evidence, &relationships);

    assert_eq!(without_relationships.outcome, CoverageOutcome::Insufficient);
    assert_eq!(without_relationships.missing_relationships.len(), 5);
    assert_eq!(complete.outcome, CoverageOutcome::Complete);
}

#[test]
fn answer_must_use_and_cite_every_mandatory_facet() {
    let contract = contract();
    let audit = contract.audit(&complete_evidence(), &complete_relationships());
    let incomplete = AnswerUse::new(BTreeSet::from([
        "package-format-evidence".to_owned(),
        "generated-entry-evidence".to_owned(),
    ]));
    let complete = AnswerUse::new(
        complete_evidence()
            .into_iter()
            .map(|evidence| evidence.id)
            .collect(),
    );

    assert!(contract.validate_answer(&audit, &incomplete).is_err());
    assert!(contract.validate_answer(&audit, &complete).is_ok());
}

#[test]
fn monotonic_extension_property_holds_for_every_small_subset() {
    let baseline = contract();
    for mask in 0_u8..16 {
        let additions = (0..4)
            .filter(|bit| mask & (1 << bit) != 0)
            .map(|bit| {
                FacetRequirement::new(
                    format!("planner-{bit}"),
                    Repository::Named(format!("repo-{bit}")),
                    Stage::Named(format!("stage-{bit}")),
                    Era::exact(format!("revision-{bit}")),
                )
            })
            .collect();
        let merged = baseline
            .clone()
            .extend(PlannerExtension::new(additions))
            .expect("small extension");

        assert!(
            baseline
                .facets()
                .iter()
                .all(|required| merged.facets().contains(required))
        );
    }
}

#[test]
fn contract_validation_rejects_unbounded_round_budget() {
    let mut contract = contract();
    contract.budget.max_rounds = u8::MAX;

    assert!(contract.validate().is_err());
}
