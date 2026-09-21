#![allow(dead_code)] // each test binary uses a subset of these helpers

//! Shared helpers for the integration tests.

use std::sync::Mutex;

use review_gate::classifier::{BatchResult, Classifier, NoulAnswer, NoulQuestion};
use review_gate::config::Config;
use review_gate::Result;
use serde_json::Value;

pub const SAMPLE_DIFF: &str = include_str!("../fixtures/sample.diff");

pub const BASIC_CONFIG: &str = r#"
version: 1
defaults:
  threshold: 0.6
  min_confidence: 0.55
reviewers:
  devops:
    description: Infra
    team: acme/platform
  qa: {}
fallback_reviewers: [devops]
rules:
  - id: lambda-event-contract
    question: A parameter that a Lambda handler reads from its input event is removed or renamed.
    reviewers: [devops]
    paths: ["**/*.py"]
  - id: test-removed
    question: An existing test is deleted or skipped without a replacement.
    reviewers: [qa]
"#;

pub fn basic_config() -> Config {
    Config::parse(BASIC_CONFIG).expect("the basic test config is valid")
}

/// What the fake classifier should do when asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behaviour {
    /// Answer every request.
    Answer,
    /// Fail every request.
    FailAlways,
    /// Fail only the first request.
    FailFirst,
}

/// Returns canned answers and records what it was asked.
#[derive(Debug)]
pub struct FakeClassifier {
    answers: Vec<(String, NoulAnswer)>,
    behaviour: Behaviour,
    calls: Mutex<Vec<(Value, Vec<String>)>>,
}

impl FakeClassifier {
    pub fn new(answers: &[(&str, f64, f64)]) -> Self {
        Self {
            answers: answers
                .iter()
                .map(|(id, probability, confidence)| {
                    (id.to_string(), NoulAnswer::new(*probability, *confidence))
                })
                .collect(),
            behaviour: Behaviour::Answer,
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn with_behaviour(mut self, behaviour: Behaviour) -> Self {
        self.behaviour = behaviour;
        self
    }

    /// Every (state, question ids) pair the engine asked about, in call order.
    pub fn calls(&self) -> Vec<(Value, Vec<String>)> {
        self.calls.lock().expect("calls mutex").clone()
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().expect("calls mutex").len()
    }
}

impl Classifier for FakeClassifier {
    fn ask(&self, state: &Value, questions: &[NoulQuestion]) -> Result<BatchResult> {
        let ids: Vec<String> = questions.iter().map(|q| q.id.clone()).collect();
        let call_index = {
            let mut calls = self.calls.lock().expect("calls mutex");
            calls.push((state.clone(), ids));
            calls.len() - 1
        };

        let fail = match self.behaviour {
            Behaviour::Answer => false,
            Behaviour::FailAlways => true,
            Behaviour::FailFirst => call_index == 0,
        };
        if fail {
            return Err(review_gate::Error::Model("boom".to_string()));
        }

        Ok(BatchResult {
            answers: questions
                .iter()
                .map(|question| {
                    let answer = self
                        .answers
                        .iter()
                        .find(|(id, _)| *id == question.id)
                        .map(|(_, answer)| *answer)
                        .unwrap_or(NoulAnswer::new(0.0, 1.0));
                    (question.id.clone(), answer)
                })
                .collect(),
            model: "jev-1.13.0".to_string(),
            input_tokens: 100,
            output_tokens: 5,
        })
    }
}

/// A diff with `count` small files, each `padding` characters wide.
pub fn synthetic_diff(count: usize, padding: usize) -> String {
    (0..count)
        .map(|index| {
            format!(
                "diff --git a/s{index}.py b/s{index}.py\n@@ -1 +1 @@\n-{}\n+{}\n",
                "x".repeat(padding),
                "y".repeat(padding)
            )
        })
        .collect()
}
