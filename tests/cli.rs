//! End-to-end runs of the built binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    manifest_dir().join("tests/fixtures").join(name)
}

/// A scratch directory of its own for each test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("review-gate-cli-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn review_gate(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_review-gate"))
        .args(args)
        .current_dir(manifest_dir())
        .env_remove("REVIEW_GATE_MOCK_ANSWERS")
        .env_remove("TYPESAFE_API_KEY")
        .env_remove("GITHUB_OUTPUT")
        .output()
        .expect("the binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn config_path() -> String {
    fixture("e2e-config.yml").display().to_string()
}

fn sample_diff() -> String {
    fixture("sample.diff").display().to_string()
}

fn mock_answers() -> String {
    fixture("mock-answers.json").display().to_string()
}

#[test]
fn evaluate_writes_json_markdown_and_outputs() {
    let dir = scratch("full");
    let result = dir.join("result.json");
    let markdown = dir.join("summary.md");
    let gh_output = dir.join("gh-output");

    let output = review_gate(&[
        "evaluate",
        "--config",
        &config_path(),
        "--diff-file",
        &sample_diff(),
        "--mock-answers",
        &mock_answers(),
        "--output",
        &result.display().to_string(),
        "--markdown",
        &markdown.display().to_string(),
        "--github-output",
        &gh_output.display().to_string(),
        "--repo",
        "acme/api",
        "--pr-number",
        "42",
        "--pr-title",
        "Rename order id",
    ]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));

    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&result).unwrap()).unwrap();
    assert_eq!(payload["required_reviewers"], serde_json::json!(["devops"]));
    assert_eq!(payload["pull_request"]["number"], 42);
    assert_eq!(payload["model"], "mock");

    assert!(std::fs::read_to_string(&markdown)
        .unwrap()
        .contains("Review Gate"));
    assert!(std::fs::read_to_string(&gh_output)
        .unwrap()
        .contains("required_reviewers_csv=devops"));
    assert!(stderr(&output).contains("required reviewers: devops"));
}

#[test]
fn evaluate_prints_json_to_stdout() {
    let output = review_gate(&[
        "evaluate",
        "--config",
        &config_path(),
        "--diff-file",
        &sample_diff(),
        "--mock-answers",
        &mock_answers(),
        "--quiet",
    ]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let payload: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(payload["schema_version"], "1.0");
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
}

#[test]
fn diff_can_come_from_stdin() {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new(env!("CARGO_BIN_EXE_review-gate"))
        .args([
            "evaluate",
            "--config",
            &config_path(),
            "--diff-file",
            "-",
            "--mock-answers",
            &mock_answers(),
            "--quiet",
        ])
        .current_dir(manifest_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(std::fs::read_to_string(sample_diff()).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert_eq!(output.status.code(), Some(0));
    let payload: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(payload["required_reviewers"], serde_json::json!(["devops"]));
}

#[test]
fn mock_answers_can_come_from_the_environment() {
    let output = Command::new(env!("CARGO_BIN_EXE_review-gate"))
        .args([
            "evaluate",
            "--config",
            &config_path(),
            "--diff-file",
            &sample_diff(),
            "--quiet",
        ])
        .current_dir(manifest_dir())
        .env("REVIEW_GATE_MOCK_ANSWERS", mock_answers())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let payload: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(payload["required_reviewers"], serde_json::json!(["devops"]));
}

#[test]
fn github_output_falls_back_to_the_environment_variable() {
    let dir = scratch("gh-env");
    let gh_output = dir.join("gh-output");

    let output = Command::new(env!("CARGO_BIN_EXE_review-gate"))
        .args([
            "evaluate",
            "--config",
            &config_path(),
            "--diff-file",
            &sample_diff(),
            "--mock-answers",
            &mock_answers(),
            "--github-output",
            "--quiet",
        ])
        .current_dir(manifest_dir())
        .env("GITHUB_OUTPUT", &gh_output)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(std::fs::read_to_string(&gh_output)
        .unwrap()
        .contains("reviewer_count=1"));
}

#[test]
fn fail_on_degraded_exits_three() {
    let dir = scratch("degraded");
    let empty = dir.join("answers.json");
    std::fs::write(&empty, "{}").unwrap();
    let diff_file = dir.join("many.diff");
    std::fs::write(
        &diff_file,
        (0..3)
            .map(|i| format!("diff --git a/s{i}.py b/s{i}.py\n@@ -1 +1 @@\n-a\n+b\n"))
            .collect::<String>(),
    )
    .unwrap();

    let capped = dir.join("capped.yml");
    let base = std::fs::read_to_string(fixture("e2e-config.yml")).unwrap();
    std::fs::write(
        &capped,
        base.replace("defaults:", "defaults:\n  max_files: 1"),
    )
    .unwrap();

    let ok = review_gate(&[
        "evaluate",
        "--config",
        &config_path(),
        "--diff-file",
        &diff_file.display().to_string(),
        "--mock-answers",
        &empty.display().to_string(),
        "--fail-on-degraded",
        "--quiet",
    ]);
    assert_eq!(ok.status.code(), Some(0), "{}", stderr(&ok));

    let degraded = review_gate(&[
        "evaluate",
        "--config",
        &capped.display().to_string(),
        "--diff-file",
        &diff_file.display().to_string(),
        "--mock-answers",
        &empty.display().to_string(),
        "--fail-on-degraded",
        "--quiet",
    ]);
    assert_eq!(degraded.status.code(), Some(3), "{}", stderr(&degraded));
    let payload: serde_json::Value = serde_json::from_str(&stdout(&degraded)).unwrap();
    assert_eq!(payload["degraded"], true);
    assert_eq!(payload["required_reviewers"], serde_json::json!(["devops"]));
}

#[test]
fn missing_api_key_is_a_clear_error() {
    let output = review_gate(&[
        "evaluate",
        "--config",
        &config_path(),
        "--diff-file",
        &sample_diff(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let message = stderr(&output);
    assert!(message.contains("TYPESAFE_API_KEY"), "{message}");
    assert!(message.contains("--mock-answers"), "{message}");
}

#[test]
fn invalid_config_exits_two() {
    let dir = scratch("invalid");
    let bad = dir.join("review-gate.yml");
    std::fs::write(&bad, "version: 1\nrules: []\n").unwrap();

    let output = review_gate(&["validate", "--config", &bad.display().to_string()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("at least one rule"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn validate_reports_the_rule_count() {
    let output = review_gate(&["validate", "--config", &config_path()]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let message = stdout(&output);
    assert!(message.contains("2 active rule(s)"), "{message}");
    assert!(message.contains("fallback: devops"), "{message}");
}

#[test]
fn validate_finds_the_repository_config_by_default() {
    let output = review_gate(&["validate"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains(".github/review-gate.yml"));
}

#[test]
fn input_that_is_not_a_diff_is_rejected() {
    let dir = scratch("junk");
    let junk = dir.join("junk.txt");
    std::fs::write(&junk, "hello world").unwrap();

    let output = review_gate(&[
        "evaluate",
        "--config",
        &config_path(),
        "--diff-file",
        &junk.display().to_string(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("unified diff"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn diff_can_be_computed_from_git_refs() {
    let dir = scratch("git");
    let run_git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(status.status.success(), "git {args:?} failed");
    };
    run_git(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("handler.py"), "event['customerId']\n").unwrap();
    run_git(&["add", "-A"]);
    run_git(&["commit", "-qm", "first"]);
    std::fs::write(dir.join("handler.py"), "event['customer_id']\n").unwrap();
    run_git(&["add", "-A"]);
    run_git(&["commit", "-qm", "second"]);

    let output = review_gate(&[
        "evaluate",
        "--config",
        &config_path(),
        "--repo-dir",
        &dir.display().to_string(),
        "--base",
        "HEAD~1",
        "--head",
        "HEAD",
        "--mock-answers",
        &mock_answers(),
        "--quiet",
    ]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let payload: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(payload["required_reviewers"], serde_json::json!(["devops"]));
    assert_eq!(payload["stats"]["files"], 1);
}

#[test]
fn a_bad_git_ref_is_reported() {
    let output = review_gate(&[
        "evaluate",
        "--config",
        &config_path(),
        "--base",
        "definitely-not-a-ref",
        "--repo-dir",
        &manifest_dir().display().to_string(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("git diff"), "{}", stderr(&output));
}

#[test]
fn version_and_help_are_available() {
    let version = review_gate(&["--version"]);
    assert_eq!(version.status.code(), Some(0));
    assert!(stdout(&version).contains(env!("CARGO_PKG_VERSION")));

    let help = review_gate(&["--help"]);
    assert!(stdout(&help).contains("reviewer types"));
}

#[test]
fn unknown_flags_are_rejected() {
    let output = review_gate(&["evaluate", "--nonsense"]);
    assert_ne!(output.status.code(), Some(0));
}

#[test]
fn every_path_in_the_repository_config_is_still_reachable() {
    // A guard against renaming a source file without updating the rule prefilters.
    let root = manifest_dir();
    let text = std::fs::read_to_string(root.join(".github/review-gate.yml")).unwrap();
    for line in text.lines() {
        let Some(paths) = line.trim().strip_prefix("paths: [") else {
            continue;
        };
        for raw in paths.trim_end_matches(']').split(',') {
            let pattern = raw.trim().trim_matches('"');
            if pattern.contains('*') || pattern.is_empty() {
                continue;
            }
            assert!(
                Path::new(&root).join(pattern).exists(),
                "rule path '{pattern}' does not exist"
            );
        }
    }
}
