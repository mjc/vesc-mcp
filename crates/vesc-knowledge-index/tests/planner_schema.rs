use vesc_knowledge_index::investigation::{
    Era, FacetRequirement, HistoricalTraceRevisions, InvestigationContract, Repository, Stage,
};
use vesc_knowledge_index::planning::{PlannerProposal, SearchQuery, hard_coded_proposal};

fn contract() -> InvestigationContract {
    InvestigationContract::historical_package_loader(HistoricalTraceRevisions::new(
        "1111111111111111111111111111111111111111",
        "2222222222222222222222222222222222222222",
        "3333333333333333333333333333333333333333",
        "4444444444444444444444444444444444444444",
    ))
}

#[test]
fn proposal_applies_only_monotonic_bounded_additions() {
    let proposal = PlannerProposal::new(
        vec![FacetRequirement::new(
            "configuration-source",
            Repository::VescFirmware,
            Stage::Configuration,
            Era::exact("2222222222222222222222222222222222222222"),
        )],
        Vec::new(),
        vec![SearchQuery::new(
            "configuration-source",
            "load-native-lib registration configuration",
        )],
        false,
    );
    let extended = proposal.apply(&contract()).unwrap();
    assert_eq!(extended.facets().len(), contract().facets().len() + 1);
}

#[test]
fn proposal_rejects_queries_for_unknown_facets() {
    let proposal = PlannerProposal::new(
        Vec::new(),
        Vec::new(),
        vec![SearchQuery::new("invented", "anything")],
        false,
    );
    assert!(proposal.apply(&contract()).is_err());
}

#[test]
fn proposal_json_rejects_prose_and_unknown_fields() {
    let json = r#"{
      "schema": 1,
      "facet_additions": [],
      "relationship_additions": [],
      "search_queries": [],
      "request_critic": false,
      "answer": "The loader works like this..."
    }"#;
    assert!(serde_json::from_str::<PlannerProposal>(json).is_err());
}

#[test]
fn proposal_rejects_unbounded_query_lists() {
    let proposal = PlannerProposal::new(
        Vec::new(),
        Vec::new(),
        (0..9)
            .map(|index| SearchQuery::new("package-format", format!("query {index}")))
            .collect(),
        false,
    );
    assert!(proposal.apply(&contract()).is_err());
}

#[test]
fn hard_coded_mode_queries_each_missing_facet_without_a_model() {
    let contract = contract();
    let evidence = Vec::new();
    let audit = contract.audit(&evidence, &[]);
    let proposal = hard_coded_proposal(&contract, &audit);
    assert_eq!(proposal.search_queries.len(), contract.facets().len());
}
