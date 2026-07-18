//! Integration tests for the proposed knowledge feedback and correction
//! design document, and for the README link that surfaces it.
//!
//! This document (docs/knowledge-feedback-design.md) describes tools that do
//! not ship yet (VESCM-183 / VESCM-PLAN-5). These tests only verify the
//! document itself and its linkage from README.md; they do not exercise any
//! runtime behavior since none has been implemented.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_readme() -> String {
    let path = repo_root().join("README.md");
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read README.md: {err}"))
}

fn read_knowledge_feedback_design_doc() -> String {
    let path = repo_root().join("docs/knowledge-feedback-design.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read docs/knowledge-feedback-design.md: {err}"))
}

#[test]
fn knowledge_feedback_design_doc_file_exists() {
    let path = repo_root().join("docs/knowledge-feedback-design.md");
    assert!(
        path.is_file(),
        "expected doc at {}, linked from README.md",
        path.display()
    );
}

#[test]
fn readme_links_to_knowledge_feedback_design_doc() {
    let readme = read_readme();
    assert!(
        readme.contains(
            "[Proposed knowledge feedback and correction design](docs/knowledge-feedback-design.md)"
        ),
        "README.md should link to the new design doc under contributor documentation:\n{readme}"
    );
}

#[test]
fn readme_knowledge_feedback_link_appears_in_contributor_documentation_section() {
    let readme = read_readme();
    let contributor_idx = readme
        .find("Contributor documentation:")
        .expect("README.md should have a 'Contributor documentation:' section");
    let link_idx = readme
        .find("docs/knowledge-feedback-design.md")
        .expect("README.md should reference docs/knowledge-feedback-design.md");
    assert!(
        link_idx > contributor_idx,
        "expected the knowledge feedback design link to appear after the \
         'Contributor documentation:' heading, but link_idx={link_idx} <= contributor_idx={contributor_idx}"
    );

    let nix_idx = readme
        .find("## Nix")
        .expect("README.md should still have a '## Nix' section after contributor docs");
    assert!(
        link_idx < nix_idx,
        "expected the knowledge feedback design link to appear before the '## Nix' \
         section, but link_idx={link_idx} >= nix_idx={nix_idx}"
    );
}

/// General regression guard: every `docs/...` markdown link in README.md
/// must resolve to a file that actually exists on disk. This specifically
/// covers the newly added link, but also protects the rest of the README's
/// doc index from silently drifting out of sync with the docs/ directory.
#[test]
fn readme_doc_links_all_resolve_to_existing_files() {
    let readme = read_readme();
    let root = repo_root();

    let mut checked = 0usize;
    let mut rest = readme.as_str();
    while let Some(start) = rest.find("](docs/") {
        // Move past "](" so `target` begins at the path itself.
        let after_open = &rest[start + 2..];
        let end = after_open
            .find(')')
            .expect("unterminated markdown link target in README.md");
        let target = &after_open[..end];

        // Strip optional in-page anchors (e.g. docs/configuration.md#configuration-file).
        let path_part = target.split('#').next().unwrap_or(target);
        let full_path = root.join(path_part);
        assert!(
            full_path.is_file(),
            "README.md links to '{target}' which does not exist at {}",
            full_path.display()
        );
        checked += 1;

        rest = &after_open[end..];
    }

    assert!(
        checked >= 17,
        "expected to check at least 17 docs/ links in README.md (16 pre-existing \
         plus the new knowledge feedback design link), but only found {checked}"
    );
}

#[test]
fn knowledge_feedback_design_doc_declares_proposed_status_with_tracking_ids() {
    let body = read_knowledge_feedback_design_doc();
    assert!(
        body.starts_with("# Knowledge feedback and correction design"),
        "missing expected top-level heading:\n{body}"
    );
    assert!(
        body.contains("Status: proposed in VESCM-183 and VESCM-PLAN-5."),
        "missing status line with tracking IDs:\n{body}"
    );
    assert!(
        body.contains("The tools described here do\nnot ship yet."),
        "missing explicit not-yet-shipped disclaimer:\n{body}"
    );
}

#[test]
fn knowledge_feedback_design_doc_has_expected_top_level_sections_in_order() {
    let body = read_knowledge_feedback_design_doc();
    let expected_headings = [
        "## Goal",
        "## Model discovery contract",
        "## Tool selection",
        "## Retrieval behavior",
        "## Loader example",
        "## Safety and non-goals",
        "## Delivery",
    ];

    let mut last_idx: Option<usize> = None;
    for heading in expected_headings {
        let idx = body
            .find(heading)
            .unwrap_or_else(|| panic!("missing expected heading '{heading}':\n{body}"));
        if let Some(prev) = last_idx {
            assert!(
                idx > prev,
                "heading '{heading}' appears out of order relative to the previous heading"
            );
        }
        last_idx = Some(idx);
    }
}

#[test]
fn knowledge_feedback_design_doc_names_both_new_tools() {
    let body = read_knowledge_feedback_design_doc();
    assert!(
        body.contains("submit_vesc_knowledge_feedback"),
        "missing reference to submit_vesc_knowledge_feedback tool:\n{body}"
    );
    assert!(
        body.contains("correct_vesc_knowledge"),
        "missing reference to correct_vesc_knowledge tool:\n{body}"
    );
    assert!(
        body.contains("search_vesc_knowledge"),
        "missing reference to existing search_vesc_knowledge tool for context:\n{body}"
    );
}

#[test]
fn knowledge_feedback_design_doc_describes_correction_loop_steps() {
    let body = read_knowledge_feedback_design_doc();
    assert!(
        body.contains("The intended correction loop is:"),
        "missing correction loop intro:\n{body}"
    );
    for step_fragment in [
        "leads the model to conclusion X.",
        "The user challenges X.",
        "reads the returned\n   VESC resources.",
        "Those resources support a corrected or more qualified conclusion Y.",
        "with X, Y, affected result IDs,",
        "show the correction before ordinary results",
    ] {
        assert!(
            body.contains(step_fragment),
            "missing correction loop step fragment '{step_fragment}':\n{body}"
        );
    }
    assert!(
        body.contains("It does not rewrite upstream files or\nthe immutable curated knowledge artifact."),
        "missing durability/non-destructive clarification:\n{body}"
    );
}

#[test]
fn knowledge_feedback_design_doc_tool_selection_table_covers_key_situations() {
    let body = read_knowledge_feedback_design_doc();
    assert!(
        body.contains("| Situation | Action |"),
        "missing tool selection table header:\n{body}"
    );

    let expected_rows = [
        "Search already answers the question",
        "The model learned a reusable detail, but cannot cite authoritative VESC resources",
        "The user disagrees, but follow-up evidence is not conclusive",
        "Follow-up VESC resources show the original conclusion was wrong or incomplete",
        "The user states a preference or general instruction",
        "Submitted evidence is an arbitrary URL or filesystem path",
    ];
    for row in expected_rows {
        assert!(
            body.contains(row),
            "missing tool selection table row '{row}':\n{body}"
        );
    }
    assert!(
        body.contains("Reject it"),
        "missing rejection outcome for arbitrary URL/path evidence:\n{body}"
    );
}

#[test]
fn knowledge_feedback_design_doc_describes_input_resolution_safety() {
    let body = read_knowledge_feedback_design_doc();
    assert!(
        body.contains(
            "The server resolves every evidence URI through its resource registry and"
        ),
        "missing evidence resolution description:\n{body}"
    );
    assert!(
        body.contains("It never fetches a submitted\nURL or reads a submitted path."),
        "missing explicit safety guarantee about not fetching submitted URLs/paths:\n{body}"
    );
}

#[test]
fn knowledge_feedback_design_doc_distinguishes_provenance_from_entailment() {
    let body = read_knowledge_feedback_design_doc();
    assert!(
        body.contains("Resource grounding proves provenance, not semantic entailment."),
        "missing provenance vs. entailment distinction:\n{body}"
    );
    assert!(
        body.contains("resource-backed correction")
            && body.contains("verified fact"),
        "missing wording guardrail distinguishing 'resource-backed correction' from 'verified fact':\n{body}"
    );
}

#[test]
fn knowledge_feedback_design_doc_loader_example_references_expected_resources() {
    let body = read_knowledge_feedback_design_doc();
    for resource in [
        "vesc://catalog/doc/topic/lisp_imports",
        "vesc://catalog/doc/topic/vescpackage_reference",
        "vesc://knowledge/chunk/{id}",
    ] {
        assert!(
            body.contains(resource),
            "missing loader example resource reference '{resource}':\n{body}"
        );
    }
    for term in ["lispData", "(import ...)", "(load-native-lib tag)"] {
        assert!(
            body.contains(term),
            "missing loader example term '{term}':\n{body}"
        );
    }
}

#[test]
fn knowledge_feedback_design_doc_lists_safety_and_non_goals() {
    let body = read_knowledge_feedback_design_doc();
    for bullet in [
        "Writes require an explicit configured store and write policy.",
        "Remote writes additionally require the existing authenticated HTTP boundary.",
        "All records and responses are bounded.",
        "Full conversations and raw user pushback are not retained.",
        "Arbitrary URLs and paths cannot ground a correction.",
        "Feedback cannot acquire first-party or curated trust.",
    ] {
        assert!(
            body.contains(bullet),
            "missing safety/non-goal bullet '{bullet}':\n{body}"
        );
    }
}

#[test]
fn knowledge_feedback_design_doc_lists_delivery_tickets_in_order() {
    let body = read_knowledge_feedback_design_doc();
    assert!(
        body.contains("The durable implementation plan is VESCM-PLAN-5:"),
        "missing delivery plan reference:\n{body}"
    );

    let tickets = [
        "VESCM-184",
        "VESCM-185",
        "VESCM-186",
        "VESCM-187",
        "VESCM-188",
        "VESCM-189",
    ];
    let mut last_idx: Option<usize> = None;
    for ticket in tickets {
        let idx = body
            .find(ticket)
            .unwrap_or_else(|| panic!("missing delivery ticket '{ticket}':\n{body}"));
        if let Some(prev) = last_idx {
            assert!(
                idx > prev,
                "delivery ticket '{ticket}' appears out of order"
            );
        }
        last_idx = Some(idx);
    }
}

#[test]
fn knowledge_feedback_design_doc_final_test_requirement_covers_both_transports() {
    let body = read_knowledge_feedback_design_doc();
    assert!(
        body.contains("Tests must cover enabled and disabled initialization instructions on both\ntransports"),
        "missing final testing requirement paragraph:\n{body}"
    );
    assert!(
        body.contains("Stdio and Streamable HTTP should\ngenerate their instructions from one shared implementation"),
        "missing stdio/HTTP shared-implementation requirement:\n{body}"
    );
}

/// Sanity check on markdown formatting: every inline code span opened with a
/// single backtick must be closed, so the total backtick count must be even.
/// This document intentionally contains no fenced (triple-backtick) code
/// blocks.
#[test]
fn knowledge_feedback_design_doc_has_balanced_inline_code_spans() {
    let body = read_knowledge_feedback_design_doc();
    assert!(
        !body.contains("```"),
        "expected no fenced code blocks in this design doc, found triple backticks:\n{body}"
    );
    let backtick_count = body.matches('`').count();
    assert_eq!(
        backtick_count % 2,
        0,
        "expected an even number of backticks (balanced inline code spans), found {backtick_count}"
    );
    assert!(
        backtick_count > 0,
        "expected at least some inline code spans referencing tool/field names"
    );
}