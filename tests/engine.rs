//! Threshold, aggregation and fail-safe behaviour.

mod common;

use common::{synthetic_diff, Behaviour, FakeClassifier, SAMPLE_DIFF};
use review_gate::config::{Config, LowConfidence};
use review_gate::diff;
use review_gate::engine::{evaluate, Evaluation, PullRequestContext};

fn run(config: &Config, diff_text: &str, client: &FakeClassifier) -> Evaluation {
    let files = diff::parse(diff_text);
    evaluate(config, &files, client, &PullRequestContext::default())
}

fn fired_ids(evaluation: &Evaluation) -> Vec<&str> {
    evaluation
        .decisions
        .iter()
        .filter(|decision| decision.fired)
        .map(|decision| decision.rule_id.as_str())
        .collect()
}

#[test]
fn fires_only_the_matching_rule() {
    let client = FakeClassifier::new(&[
        ("lambda-event-contract", 0.94, 0.88),
        ("test-removed", 0.04, 0.99),
    ]);
    let result = run(&common::basic_config(), SAMPLE_DIFF, &client);

    assert_eq!(result.required_reviewers, ["devops"]);
    assert_eq!(result.required_reviewer_teams, ["acme/platform"]);
    assert!(!result.degraded);
    assert_eq!(fired_ids(&result), ["lambda-event-contract"]);
    let decision = &result.decisions[0];
    assert_eq!(decision.files, ["services/orders/handler.py"]);
    assert_eq!(result.stats.input_tokens, 100);
    assert_eq!(result.model, "jev-1.13.0");
}

#[test]
fn binary_and_ignored_files_are_dropped() {
    let config = Config::parse(&format!(
        "{}\nignore_paths: [\"infra/**\"]\n",
        common::BASIC_CONFIG
    ))
    .unwrap();

    let client = FakeClassifier::new(&[]);
    let result = run(&config, SAMPLE_DIFF, &client);

    // The binary logo and the ignored infra file are both gone.
    assert_eq!(result.stats.files, 1);
    let (state, _) = &client.calls()[0];
    assert_eq!(
        state["changed_files"],
        serde_json::json!(["services/orders/handler.py"])
    );
}

#[test]
fn rule_without_matching_files_is_skipped() {
    let client = FakeClassifier::new(&[("test-removed", 0.9, 0.9)]);
    let diff_text = "diff --git a/infra/stack.ts b/infra/stack.ts\n@@ -1 +1 @@\n-a\n+b\n";
    let result = run(&common::basic_config(), diff_text, &client);

    let skipped: Vec<&str> = result.skipped.iter().map(|s| s.rule_id.as_str()).collect();
    assert_eq!(skipped, ["lambda-event-contract"]);
    assert_eq!(result.required_reviewers, ["qa"]);
    assert_eq!(client.calls()[0].1, ["test-removed"]);
}

#[test]
fn probability_below_threshold_does_not_fire() {
    let client = FakeClassifier::new(&[("lambda-event-contract", 0.59, 0.99)]);
    let result = run(&common::basic_config(), SAMPLE_DIFF, &client);
    assert!(result.required_reviewers.is_empty());
}

#[test]
fn low_confidence_requires_by_default() {
    let client = FakeClassifier::new(&[("lambda-event-contract", 0.8, 0.2)]);
    let result = run(&common::basic_config(), SAMPLE_DIFF, &client);

    let decision = result
        .decisions
        .iter()
        .find(|d| d.rule_id == "lambda-event-contract")
        .unwrap();
    assert!(decision.fired);
    assert!(decision.low_confidence);
    assert_eq!(result.required_reviewers, ["devops"]);
}

#[test]
fn low_confidence_can_be_ignored() {
    let mut config = common::basic_config();
    config.defaults.on_low_confidence = LowConfidence::Ignore;
    let client = FakeClassifier::new(&[("lambda-event-contract", 0.8, 0.2)]);
    assert!(run(&config, SAMPLE_DIFF, &client)
        .required_reviewers
        .is_empty());
}

#[test]
fn per_rule_threshold_overrides_the_default() {
    let mut config = common::basic_config();
    config.rules[0].threshold = Some(0.95);
    let client = FakeClassifier::new(&[("lambda-event-contract", 0.94, 0.99)]);
    assert!(run(&config, SAMPLE_DIFF, &client)
        .required_reviewers
        .is_empty());
}

#[test]
fn always_reviewers_are_always_required() {
    let mut config = common::basic_config();
    config.always_reviewers = vec!["qa".to_string()];
    let client = FakeClassifier::new(&[]);
    assert_eq!(
        run(&config, SAMPLE_DIFF, &client).required_reviewers,
        ["qa"]
    );
}

#[test]
fn model_failure_falls_back_and_never_returns_empty() {
    let client = FakeClassifier::new(&[]).with_behaviour(Behaviour::FailAlways);
    let result = run(&common::basic_config(), SAMPLE_DIFF, &client);

    assert!(result.degraded);
    assert_eq!(result.required_reviewers, ["devops"]);
    assert!(!result.errors.is_empty());
    assert_eq!(result.stats.requests, 0);
}

#[test]
fn partial_failure_keeps_answers_and_adds_the_fallback() {
    let mut config = common::basic_config();
    config.defaults.max_chunk_chars = 1_000;
    config.defaults.max_concurrency = 1;

    let client = FakeClassifier::new(&[("lambda-event-contract", 0.99, 0.99)])
        .with_behaviour(Behaviour::FailFirst);
    let result = run(&config, &synthetic_diff(3, 200), &client);

    assert!(result.degraded);
    assert_eq!(result.required_reviewers, ["devops"]);
    assert!(result.decisions.iter().any(|decision| decision.fired));
}

#[test]
fn too_many_files_degrades() {
    let mut config = common::basic_config();
    config.defaults.max_files = 1;
    let client = FakeClassifier::new(&[]);
    let result = run(&config, &synthetic_diff(3, 4), &client);

    assert!(result.degraded);
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("more than 1 files")),
        "{:?}",
        result.errors
    );
    assert_eq!(result.required_reviewers, ["devops"]);
}

#[test]
fn empty_diff_asks_nothing() {
    let client = FakeClassifier::new(&[]);
    let result = run(&common::basic_config(), "", &client);
    assert!(result.required_reviewers.is_empty());
    assert_eq!(client.call_count(), 0);
}

#[test]
fn disabled_rule_is_skipped() {
    let mut config = common::basic_config();
    config.rules[0].enabled = false;
    let client = FakeClassifier::new(&[("lambda-event-contract", 1.0, 1.0)]);
    let result = run(&config, SAMPLE_DIFF, &client);

    assert!(result
        .skipped
        .iter()
        .any(|s| s.rule_id == "lambda-event-contract" && s.reason == "disabled"));
    assert!(result.required_reviewers.is_empty());
}

#[test]
fn state_includes_pull_request_context() {
    let client = FakeClassifier::new(&[]);
    let files = diff::parse(SAMPLE_DIFF);
    let context = PullRequestContext {
        repo: Some("acme/api".to_string()),
        number: Some(7),
        title: Some("Rename order id".to_string()),
        body: Some("b".repeat(5_000)),
    };
    evaluate(&common::basic_config(), &files, &client, &context);

    let (state, _) = &client.calls()[0];
    assert_eq!(state["pull_request"]["title"], "Rename order id");
    assert!(state["pull_request"]["description"]
        .as_str()
        .unwrap()
        .ends_with("..."));
    assert!(state["diff"].as_str().unwrap().contains("handler.py"));
}

#[test]
fn a_rule_yields_one_decision_across_many_chunks() {
    let mut config = common::basic_config();
    config.defaults.max_chunk_chars = 1_000;
    config.defaults.max_concurrency = 1;

    // Every chunk answers 0.94 for the same rule; one decision comes out, and
    // because the chunks tie, each chunk's files are attributed to it.
    let client = FakeClassifier::new(&[("lambda-event-contract", 0.94, 0.9)]);
    let result = run(&config, &synthetic_diff(4, 400), &client);

    assert!(result.stats.chunks > 1);
    assert_eq!(fired_ids(&result), ["lambda-event-contract"]);
    let decision = &result.decisions[0];
    assert_eq!(decision.probability, 0.94);
    assert_eq!(decision.files, ["s0.py", "s1.py", "s2.py", "s3.py"]);
}

#[test]
fn every_chunk_is_classified_at_any_concurrency() {
    for concurrency in [1usize, 4] {
        let mut config = common::basic_config();
        config.defaults.max_chunk_chars = 1_000;
        config.defaults.max_concurrency = concurrency;

        let client = FakeClassifier::new(&[]);
        let result = run(&config, &synthetic_diff(4, 400), &client);

        assert!(result.stats.chunks > 1, "concurrency {concurrency}");
        assert_eq!(result.stats.chunks, client.call_count());
        assert_eq!(result.stats.requests, client.call_count());
        assert!(!result.degraded);
    }
}
