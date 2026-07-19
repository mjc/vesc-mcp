use std::collections::BTreeSet;

use vesc_knowledge_index::path_evaluation::{
    Ablation, PathEvaluationRun, PathEvaluationSuite, evaluate_path_run,
};

fn suite() -> PathEvaluationSuite {
    serde_json::from_str(include_str!(
        "../../../tests/evaluation/v3/loader_path.json"
    ))
    .expect("locked path suite")
}

fn run(evidence_ids: Vec<String>, answered: bool) -> PathEvaluationRun {
    let case = &suite().cases[0];
    PathEvaluationRun {
        schema: 1,
        case_id: case.id.clone(),
        ablation: Ablation::HardGate,
        budget_n: 20,
        ranked_evidence_ids: evidence_ids.clone(),
        relationships: case.relationships.clone(),
        reported_missing_facets: BTreeSet::new(),
        answered,
        answer_citations: evidence_ids.into_iter().collect(),
        controls: None,
    }
}

#[test]
fn locked_suite_has_stable_source_identities_and_all_ablations() {
    let suite = suite();

    suite.validate().expect("valid locked judgments");
    assert_eq!(suite.cases[0].judgments.len(), 6);
    assert_eq!(Ablation::ALL.len(), 7);
}

#[test]
fn removing_chibios_reports_the_exact_missing_facet() {
    let suite = suite();
    let bundle = &suite.cases[0].adversarial_bundles[1];
    let mut run = run(bundle.evidence_ids.clone(), false);
    run.reported_missing_facets = bundle.expected_missing_facets.clone();

    let report = evaluate_path_run(&suite, &run).expect("evaluate");

    assert_eq!(report.missing_facets, vec!["runtime-module-loading"]);
    assert!((report.missing_facet_detection - 1.0).abs() < f64::EPSILON);
    assert!(report.path_complete_at_n.abs() < f64::EPSILON);
}

#[test]
fn wrong_era_and_duplicate_history_do_not_count_as_coverage() {
    let suite = suite();
    let bundle = &suite.cases[0].adversarial_bundles[2];
    let mut ids = bundle.evidence_ids.clone();
    ids.push("vesc:c835e9f:lispBM/lispif_c_lib.c:ext_load_native_lib".into());
    let report = evaluate_path_run(&suite, &run(ids, false)).expect("evaluate");

    assert!(report.wrong_era_rate > 0.0);
    assert!(report.duplicate_history_waste > 0.0);
}

#[test]
fn hard_gate_release_target_rejects_frontier_shortcuts() {
    let suite = suite();
    let bundle = &suite.cases[0].adversarial_bundles[1];
    let report = evaluate_path_run(&suite, &run(bundle.evidence_ids.clone(), true))
        .expect("evaluate shortcut");

    assert!((report.frontier_shortcut_rate - 1.0).abs() < f64::EPSILON);
    assert!(report.frontier_shortcut_target.abs() < f64::EPSILON);
    assert!(!report.release_gate_passed);
}

#[test]
fn complete_report_is_deterministic_json_and_generated_markdown() {
    let suite = suite();
    let bundle = &suite.cases[0].adversarial_bundles[0];
    let report = evaluate_path_run(&suite, &run(bundle.evidence_ids.clone(), true))
        .expect("evaluate complete path");

    assert!((report.path_complete_at_n - 1.0).abs() < f64::EPSILON);
    assert!(report.frontier_shortcut_rate.abs() < f64::EPSILON);
    assert!(report.release_gate_passed);
    assert_eq!(report.canonical_json(), report.canonical_json());
    assert!(
        report
            .to_markdown()
            .contains("FrontierShortcutRate | 0.000")
    );
}

#[test]
fn committed_reports_are_generated_from_the_locked_run() {
    let suite = suite();
    let run: PathEvaluationRun = serde_json::from_str(include_str!(
        "../../../tests/evaluation/v3/runs/hard-gate-complete.json"
    ))
    .expect("locked run");
    let report = evaluate_path_run(&suite, &run).expect("evaluate locked run");

    assert_eq!(
        report.canonical_json(),
        include_str!("../../../tests/evaluation/v3/reports/hard-gate-complete.json")
    );
    assert_eq!(
        report.to_markdown(),
        include_str!("../../../tests/evaluation/v3/reports/hard-gate-complete.md")
    );
}
