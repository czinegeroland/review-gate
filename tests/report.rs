//! The JSON document, the Markdown summary and the action outputs.

mod common;

use common::{Behaviour, FakeClassifier, SAMPLE_DIFF};
use review_gate::diff;
use review_gate::engine::{evaluate, Evaluation, PullRequestContext};
use review_gate::report;

fn evaluation(answers: &[(&str, f64, f64)], behaviour: Behaviour) -> Evaluation {
    let config = common::basic_config();
    let files = diff::parse(SAMPLE_DIFF);
    let client = FakeClassifier::new(answers).with_behaviour(behaviour);
    evaluate(&config, &files, &client, &PullRequestContext::default())
}

fn fired() -> Evaluation {
    evaluation(&[("lambda-event-contract", 0.94, 0.88)], Behaviour::Answer)
}

#[test]
fn json_document_shape() {
    let context = PullRequestContext {
        repo: Some("acme/api".to_string()),
        number: Some(42),
        title: Some("Rename".to_string()),
        body: None,
    };
    let payload = report::to_value(&fired(), &context);

    assert_eq!(payload["schema_version"], "1.0");
    assert_eq!(payload["required_reviewers"], serde_json::json!(["devops"]));
    assert_eq!(
        payload["required_reviewer_teams"],
        serde_json::json!(["acme/platform"])
    );
    assert_eq!(
        payload["pull_request"],
        serde_json::json!({"repo": "acme/api", "number": 42, "title": "Rename"})
    );
    assert_eq!(payload["degraded"], false);

    let decision = &payload["decisions"][0];
    assert_eq!(decision["rule_id"], "lambda-event-contract");
    assert_eq!(decision["fired"], true);
    assert_eq!(decision["probability"], 0.94);
    assert_eq!(decision["reviewers"], serde_json::json!(["devops"]));
    assert_eq!(
        decision["files"],
        serde_json::json!(["services/orders/handler.py"])
    );
    assert_eq!(payload["stats"]["files"], 2);
    assert!(payload["generated_at"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn json_keys_are_in_document_order() {
    let text = report::to_json(&fired(), &PullRequestContext::default());
    let schema = text.find("schema_version").unwrap();
    let reviewers = text.find("required_reviewers").unwrap();
    let stats = text.find("\"stats\"").unwrap();
    assert!(schema < reviewers && reviewers < stats, "{text}");
}

#[test]
fn markdown_lists_fired_and_quiet_rules() {
    let text = report::to_markdown(&fired());
    assert!(text.contains("**Required reviewers:** `devops`"), "{text}");
    assert!(text.contains("**Teams:** `acme/platform`"), "{text}");
    assert!(
        text.contains("| `lambda-event-contract` | devops | 0.94 | 0.88 |"),
        "{text}"
    );
    assert!(text.contains("1 rule(s) did not fire"), "{text}");
}

#[test]
fn markdown_when_nothing_fires() {
    let text = report::to_markdown(&evaluation(&[], Behaviour::Answer));
    assert!(text.contains("none — no rule matched"), "{text}");
}

#[test]
fn markdown_warns_when_degraded() {
    let text = report::to_markdown(&evaluation(&[], Behaviour::FailAlways));
    assert!(text.contains("[!WARNING]"), "{text}");
    assert!(text.contains("fallback reviewer set"), "{text}");
    assert!(text.contains("boom"), "{text}");
}

#[test]
fn markdown_flags_low_confidence() {
    let text = report::to_markdown(&evaluation(
        &[("lambda-event-contract", 0.9, 0.1)],
        Behaviour::Answer,
    ));
    assert!(text.contains("low confidence"), "{text}");
}

#[test]
fn github_outputs_are_appended() {
    let dir = std::env::temp_dir().join("review-gate-outputs-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gh-output");
    std::fs::write(&path, "existing=1\n").unwrap();

    report::write_github_outputs(&path, &fired(), Some("result.json")).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    let lines: std::collections::HashMap<&str, &str> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    assert_eq!(lines["existing"], "1");
    assert_eq!(lines["required_reviewers"], r#"["devops"]"#);
    assert_eq!(lines["required_reviewers_csv"], "devops");
    assert_eq!(lines["required_reviewer_teams"], r#"["acme/platform"]"#);
    assert_eq!(lines["reviewer_count"], "1");
    assert_eq!(lines["degraded"], "false");
    assert_eq!(lines["result_path"], "result.json");
    std::fs::remove_file(&path).unwrap();
}
