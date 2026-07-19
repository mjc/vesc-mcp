use vesc_knowledge_index::{
    investigation::{Evidence, HistoricalTraceRevisions, InvestigationContract, Repository, Stage},
    reranking::{FacetCandidate, FacetQuota, retain_per_facet},
};

fn evidence(id: &str, repository: Repository, stage: Stage, revision: &str) -> Evidence {
    Evidence::decisive(id, repository, stage, revision, format!("{id}.c"))
}

#[test]
fn per_facet_quota_prevents_abundant_repository_from_consuming_budget() {
    let revisions = HistoricalTraceRevisions::new("a", "b", "c", "d");
    let contract = InvestigationContract::historical_package_loader(revisions);
    let mut candidates = vec![
        FacetCandidate::new(
            evidence(
                "rtos",
                Repository::ChibiOs,
                Stage::RuntimeModuleLoading,
                "c",
            ),
            0.1,
        ),
        FacetCandidate::new(
            evidence(
                "consumer",
                Repository::Refloat,
                Stage::ConsumerInvocation,
                "d",
            ),
            0.1,
        ),
        FacetCandidate::new(
            evidence(
                "package",
                Repository::VescPackageLib,
                Stage::PackageFormat,
                "a",
            ),
            0.1,
        ),
    ];
    for rank in 0_u8..20 {
        candidates.push(FacetCandidate::new(
            evidence(
                &format!("firmware-{rank}"),
                Repository::VescFirmware,
                Stage::FirmwareLoader,
                "b",
            ),
            1.0 - f32::from(rank) / 100.0,
        ));
    }

    let retained = retain_per_facet(&contract, candidates, FacetQuota::new(1).unwrap());

    assert_eq!(retained.len(), 4);
    assert_eq!(
        retained
            .iter()
            .map(|row| row.facet_id.as_str())
            .collect::<Vec<_>>(),
        [
            "consumer-invocation",
            "firmware-loader",
            "package-format",
            "runtime-module-loading"
        ]
    );
}

#[test]
fn facet_retention_is_stable_and_rejects_wrong_era() {
    let revisions = HistoricalTraceRevisions::new("a", "b", "c", "d");
    let contract = InvestigationContract::historical_package_loader(revisions);
    let candidates = vec![
        FacetCandidate::new(
            evidence("z", Repository::VescFirmware, Stage::FirmwareLoader, "b"),
            0.5,
        ),
        FacetCandidate::new(
            evidence("a", Repository::VescFirmware, Stage::FirmwareLoader, "b"),
            0.5,
        ),
        FacetCandidate::new(
            evidence(
                "wrong-era",
                Repository::VescFirmware,
                Stage::FirmwareLoader,
                "old",
            ),
            1.0,
        ),
    ];

    let retained = retain_per_facet(&contract, candidates, FacetQuota::new(2).unwrap());

    assert_eq!(
        retained
            .iter()
            .map(|row| row.evidence.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    assert_eq!(retained[0].facet_rank, 1);
    assert_eq!(retained[1].facet_rank, 2);
}
