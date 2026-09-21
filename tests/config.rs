//! Configuration loading and validation.

mod common;

use std::path::Path;

use review_gate::config::{find_config, Config};

fn invalid(text: &str) -> String {
    Config::parse(text)
        .expect_err("expected a validation error")
        .to_string()
}

#[test]
fn loads_and_normalises() {
    let config = common::basic_config();
    let ids: Vec<&str> = config.rules.iter().map(|rule| rule.id.as_str()).collect();
    assert_eq!(ids, ["lambda-event-contract", "test-removed"]);
    assert_eq!(
        config.reviewers["devops"].team.as_deref(),
        Some("acme/platform")
    );
    assert_eq!(config.effective_fallback(), ["devops"]);

    let names = ["qa".to_string(), "devops".to_string()]
        .into_iter()
        .collect();
    assert_eq!(config.order_reviewers(&names), ["devops", "qa"]);
    assert_eq!(
        config.teams_for(&["devops".to_string(), "qa".to_string()]),
        ["acme/platform"]
    );
}

#[test]
fn collapses_whitespace_in_questions() {
    let config = Config::parse(
        r#"
version: 1
reviewers: {qa: {}}
rules:
  - id: a
    question: >-
      Something meaningful
      happened in the diff.
    reviewers: [qa]
"#,
    )
    .unwrap();
    assert_eq!(
        config.rules[0].question,
        "Something meaningful happened in the diff."
    );
}

#[test]
fn fallback_defaults_to_all_reviewers() {
    let config = Config::parse(
        r#"
version: 1
reviewers: {devops: {}, qa: {}}
rules:
  - id: a
    question: Something meaningful happened in the diff.
    reviewers: [qa]
"#,
    )
    .unwrap();
    assert_eq!(config.effective_fallback(), ["devops", "qa"]);
}

#[test]
fn reviewer_with_no_body_is_accepted() {
    let config = Config::parse(
        r#"
version: 1
reviewers:
  qa:
rules:
  - id: a
    question: Something meaningful happened in the diff.
    reviewers: [qa]
"#,
    )
    .unwrap();
    assert_eq!(config.reviewers.len(), 1);
    assert!(config.reviewers["qa"].team.is_none());
}

#[test]
fn unknown_reviewer_is_rejected() {
    let message = invalid(
        r#"
version: 1
reviewers: {qa: {}}
rules:
  - id: a
    question: Something meaningful happened in the diff.
    reviewers: [devops]
"#,
    );
    assert!(message.contains("undeclared reviewer"), "{message}");
}

#[test]
fn duplicate_rule_id_is_rejected() {
    let message = invalid(
        r#"
version: 1
reviewers: {qa: {}}
rules:
  - id: a
    question: Something meaningful happened in the diff.
    reviewers: [qa]
  - id: a
    question: Something else meaningful happened in the diff.
    reviewers: [qa]
"#,
    );
    assert!(message.contains("duplicate rule id"), "{message}");
}

#[test]
fn rejects_bad_fields() {
    let cases = [
        ("version: 2\nreviewers: {qa: {}}\nrules: []\n", "version"),
        (
            "version: 1\nreviewers: {qa: {}}\nrules:\n  - id: a\n    question: short\n    reviewers: [qa]\n",
            "at least 10 characters",
        ),
        (
            "version: 1\nreviewers: {qa: {}}\nrules:\n  - id: a\n    question: Something meaningful happened in the diff.\n    reviewers: [qa]\n    threshold: 1.5\n",
            "threshold",
        ),
        (
            "version: 1\nreviewers: {qa: {}}\nrules:\n  - id: a\n    question: Something meaningful happened in the diff.\n    reviewers: [qa]\n    criteria: {maybe: x}\n",
            "criteria",
        ),
        (
            "version: 1\nreviewers: {qa: {}}\nrules:\n  - id: a\n    question: Something meaningful happened in the diff.\n    reviewers: []\n",
            "at least one reviewer",
        ),
        (
            "version: 1\nreviewers: {qa: {}}\nfallback_reviewers: [ops]\nrules:\n  - id: a\n    question: Something meaningful happened in the diff.\n    reviewers: [qa]\n",
            "fallback_reviewers",
        ),
        ("- just\n- a list\n", "invalid"),
        (
            "version: 1\nreviewers: {qa: {}}\nsurprise: 1\nrules:\n  - id: a\n    question: Something meaningful happened in the diff.\n    reviewers: [qa]\n",
            "surprise",
        ),
    ];
    for (text, expected) in cases {
        let message = invalid(text);
        assert!(
            message.contains(expected),
            "expected {expected:?} in {message}"
        );
    }
}

#[test]
fn reports_every_problem_at_once() {
    let message = invalid(
        r#"
version: 1
reviewers: {qa: {}}
rules:
  - id: a
    question: short
    reviewers: [nobody]
"#,
    );
    assert!(message.contains("at least 10 characters"), "{message}");
    assert!(message.contains("undeclared reviewer"), "{message}");
}

#[test]
fn path_prefilter_matches_at_the_root_too() {
    let config = common::basic_config();
    let rule = &config.rules[0];
    assert!(rule.matches("services/orders/handler.py"));
    assert!(rule.matches("handler.py"));
    assert!(!rule.matches("infra/stack.ts"));
    // A rule without paths applies everywhere.
    assert!(config.rules[1].matches("anything.ts"));
}

#[test]
fn ignore_paths_are_compiled() {
    let config = Config::parse(
        r#"
version: 1
reviewers: {qa: {}}
ignore_paths: ["**/*.lock", "docs/**"]
rules:
  - id: a
    question: Something meaningful happened in the diff.
    reviewers: [qa]
"#,
    )
    .unwrap();
    assert!(config.is_ignored("Cargo.lock"));
    assert!(config.is_ignored("docs/PRD.md"));
    assert!(!config.is_ignored("src/main.rs"));
}

#[test]
fn find_config_searches_the_default_locations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let found = find_config(None, root).unwrap();
    assert!(found.ends_with(".github/review-gate.yml"), "{found:?}");

    let message = find_config(None, &root.join("src"))
        .unwrap_err()
        .to_string();
    assert!(message.contains("no config file found"), "{message}");

    let message = find_config(Some(Path::new("missing.yml")), root)
        .unwrap_err()
        .to_string();
    assert!(message.contains("not found"), "{message}");
}

#[test]
fn shipped_configs_are_valid() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        ".github/review-gate.yml",
        "examples/review-gate.example.yml",
        "tests/fixtures/e2e-config.yml",
    ] {
        let config = Config::load(&root.join(relative))
            .unwrap_or_else(|err| panic!("{relative} should be valid: {err}"));
        assert!(!config.active_rules().is_empty(), "{relative} has no rules");
    }
}
