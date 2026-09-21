//! Clients that answer Noul questions about a diff chunk.
//!
//! The production implementation talks to TypeSafe AI's System One HTTP API
//! (<https://docs.typesafe.ai/api>). It is deliberately a thin client behind the
//! [`Classifier`] trait so the official SDK — or a fixture — can take its place.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::{Error, Result};

/// Default TypeSafe API root.
pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";
const SYSTEM_ONE_PATH: &str = "/v1/systemone";
const RETRYABLE: [u16; 8] = [408, 409, 425, 429, 500, 502, 503, 529];

/// A yes/no question about the state, in TypeSafe's Noul shape.
#[derive(Debug, Clone)]
pub struct NoulQuestion {
    pub id: String,
    pub question: String,
    pub focus: Option<String>,
    /// Ordered `true`/`false` descriptions that sharpen the yes/no boundary.
    pub criteria: Option<Vec<(String, String)>>,
}

impl NoulQuestion {
    /// A question with no criteria or focus.
    pub fn new(id: &str, question: &str) -> Self {
        Self {
            id: id.to_string(),
            question: question.to_string(),
            focus: None,
            criteria: None,
        }
    }

    /// The JSON body for this question.
    pub fn payload(&self) -> Value {
        let mut instructions = Map::new();
        instructions.insert("question".into(), json!(self.question));
        if let Some(focus) = &self.focus {
            instructions.insert("focus".into(), json!(focus));
        }
        let mut body = Map::new();
        body.insert("type".into(), json!("noul"));
        body.insert("instructions".into(), Value::Object(instructions));
        if let Some(criteria) = &self.criteria {
            let mut map = Map::new();
            for (key, value) in criteria {
                map.insert(key.clone(), json!(value));
            }
            body.insert("criteria".into(), Value::Object(map));
        }
        Value::Object(body)
    }
}

/// The model's answer: probability that the statement is true, plus confidence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoulAnswer {
    pub probability: f64,
    pub confidence: f64,
}

impl NoulAnswer {
    pub fn new(probability: f64, confidence: f64) -> Self {
        Self {
            probability,
            confidence,
        }
    }
}

/// Answers for one request, with usage accounting.
#[derive(Debug, Clone, Default)]
pub struct BatchResult {
    pub answers: HashMap<String, NoulAnswer>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Answers a batch of Noul questions about one state.
pub trait Classifier: Sync {
    /// Ask every question about `state` in a single call.
    fn ask(&self, state: &Value, questions: &[NoulQuestion]) -> Result<BatchResult>;
}

/// TypeSafe System One client.
#[derive(Debug)]
pub struct JevClient {
    api_key: String,
    model: String,
    base_url: String,
    agent: ureq::Agent,
    max_retries: u32,
    backoff_ms: u64,
}

impl JevClient {
    /// Build a client from an explicit key.
    pub fn new(api_key: String, model: String, base_url: String, timeout: Duration) -> Self {
        Self {
            api_key,
            model,
            base_url: base_url.trim_end_matches('/').to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout(timeout)
                .user_agent("review-gate")
                .build(),
            max_retries: 3,
            backoff_ms: 500,
        }
    }

    /// Build a client from `TYPESAFE_API_KEY` (and optionally `TYPESAFE_BASE_URL`).
    pub fn from_env(model: &str, timeout: Duration) -> Result<Self> {
        let api_key = std::env::var("TYPESAFE_API_KEY").unwrap_or_default();
        let api_key = api_key.trim().to_string();
        if api_key.is_empty() {
            return Err(Error::Model("TYPESAFE_API_KEY is not set".to_string()));
        }
        let base_url =
            std::env::var("TYPESAFE_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Ok(Self::new(api_key, model.to_string(), base_url, timeout))
    }

    /// Retry backoff between attempts; zeroed by tests.
    pub fn with_backoff_ms(mut self, backoff_ms: u64) -> Self {
        self.backoff_ms = backoff_ms;
        self
    }

    fn post(&self, body: &Value) -> Result<Value> {
        let url = format!("{}{SYSTEM_ONE_PATH}", self.base_url);
        let mut last_error = "unknown error".to_string();

        for attempt in 0..=self.max_retries {
            let response = self
                .agent
                .post(&url)
                .set("Authorization", &format!("Bearer {}", self.api_key))
                .set("Content-Type", "application/json")
                .send_json(body);

            match response {
                Ok(response) => {
                    return response.into_json::<Value>().map_err(|err| {
                        Error::Model(format!("malformed JSON from TypeSafe: {err}"))
                    })
                }
                Err(ureq::Error::Status(code, response)) => {
                    last_error = describe(code, response);
                    if !RETRYABLE.contains(&code) {
                        return Err(Error::Model(last_error));
                    }
                }
                Err(err) => last_error = format!("request failed: {err}"),
            }

            if attempt < self.max_retries && self.backoff_ms > 0 {
                let delay = self.backoff_ms * 2u64.pow(attempt);
                std::thread::sleep(Duration::from_millis(delay.min(8_000)));
            }
        }

        Err(Error::Model(format!(
            "TypeSafe request failed after {} attempts: {last_error}",
            self.max_retries + 1
        )))
    }
}

impl Classifier for JevClient {
    fn ask(&self, state: &Value, questions: &[NoulQuestion]) -> Result<BatchResult> {
        let mut map = Map::new();
        for question in questions {
            map.insert(question.id.clone(), question.payload());
        }
        let body = json!({
            "model": self.model,
            "state": state,
            "questions": Value::Object(map),
        });
        parse_response(&self.post(&body)?, questions)
    }
}

fn describe(code: u16, response: ureq::Response) -> String {
    let mut detail = response
        .into_string()
        .unwrap_or_default()
        .trim()
        .to_string();
    if detail.len() > 300 {
        detail.truncate(300);
        detail.push_str("...");
    }
    format!("HTTP {code} from TypeSafe: {detail}")
}

fn parse_response(data: &Value, questions: &[NoulQuestion]) -> Result<BatchResult> {
    let raw = data
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Model("TypeSafe response has no 'answers' object".to_string()))?;

    let mut answers = HashMap::new();
    for question in questions {
        let answer = raw
            .get(&question.id)
            .and_then(Value::as_object)
            .filter(|answer| answer.contains_key("noul"))
            .ok_or_else(|| {
                Error::Model(format!("no noul answer for question '{}'", question.id))
            })?;
        let probability = as_probability(answer.get("noul"), &question.id, "noul")?;
        let confidence = match answer.get("confidence") {
            None | Some(Value::Null) => 1.0,
            value => as_probability(value, &question.id, "confidence")?,
        };
        answers.insert(
            question.id.clone(),
            NoulAnswer::new(probability, confidence),
        );
    }

    let usage = data.get("usage");
    Ok(BatchResult {
        answers,
        model: data
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        input_tokens: usage
            .and_then(|u| u.get("input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: usage
            .and_then(|u| u.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

fn as_probability(value: Option<&Value>, question_id: &str, field: &str) -> Result<f64> {
    let number = value.and_then(Value::as_f64).ok_or_else(|| {
        Error::Model(format!(
            "'{field}' for '{question_id}' is not a number: {}",
            value.unwrap_or(&Value::Null)
        ))
    })?;
    Ok(number.clamp(0.0, 1.0))
}

/// Answers from a fixture file; used by tests and the CI end-to-end smoke run.
///
/// The fixture maps rule id to either a probability or
/// `{"probability": 0.9, "confidence": 0.8}`. Rules absent from the fixture
/// answer 0.0 with full confidence.
#[derive(Debug)]
pub struct MockClassifier {
    answers: HashMap<String, NoulAnswer>,
}

impl MockClassifier {
    /// Load answers from a JSON fixture.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|err| {
            Error::Model(format!(
                "cannot read mock answers from {}: {err}",
                path.display()
            ))
        })?;
        let raw: Map<String, Value> = serde_json::from_str(&text).map_err(|err| {
            Error::Model(format!(
                "cannot read mock answers from {}: {err}",
                path.display()
            ))
        })?;

        let mut answers = HashMap::new();
        for (rule_id, value) in raw {
            let answer = match &value {
                Value::Object(map) => NoulAnswer::new(
                    map.get("probability")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0),
                    map.get("confidence").and_then(Value::as_f64).unwrap_or(1.0),
                ),
                other => NoulAnswer::new(other.as_f64().unwrap_or(0.0), 1.0),
            };
            answers.insert(rule_id, answer);
        }
        Ok(Self { answers })
    }
}

impl Classifier for MockClassifier {
    fn ask(&self, _state: &Value, questions: &[NoulQuestion]) -> Result<BatchResult> {
        Ok(BatchResult {
            answers: questions
                .iter()
                .map(|question| {
                    let answer = self
                        .answers
                        .get(&question.id)
                        .copied()
                        .unwrap_or(NoulAnswer::new(0.0, 1.0));
                    (question.id.clone(), answer)
                })
                .collect(),
            model: "mock".to_string(),
            ..BatchResult::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_payload_shape() {
        let mut question = NoulQuestion::new("r1", "Is this risky?");
        question.criteria = Some(vec![
            ("true".to_string(), "yes".to_string()),
            ("false".to_string(), "no".to_string()),
        ]);
        assert_eq!(
            question.payload(),
            json!({
                "type": "noul",
                "instructions": {"question": "Is this risky?"},
                "criteria": {"true": "yes", "false": "no"},
            })
        );

        let mut plain = NoulQuestion::new("r2", "Is this cosmetic?");
        plain.focus = Some("the diff body".to_string());
        let payload = plain.payload();
        assert_eq!(payload["instructions"]["focus"], json!("the diff body"));
        assert!(payload.get("criteria").is_none());
    }

    #[test]
    fn parses_answers_and_usage() {
        let data = json!({
            "model": "jev-1.13.0",
            "answers": {
                "r1": {"type": "noul", "noul": 0.91, "confidence": 0.8},
                "r2": {"type": "noul", "noul": 0.02},
            },
            "usage": {"input_tokens": 300, "output_tokens": 20},
        });
        let questions = [NoulQuestion::new("r1", "?"), NoulQuestion::new("r2", "?")];
        let result = parse_response(&data, &questions).unwrap();
        assert_eq!(result.answers["r1"], NoulAnswer::new(0.91, 0.8));
        assert_eq!(result.answers["r2"].confidence, 1.0);
        assert_eq!(result.model, "jev-1.13.0");
        assert_eq!((result.input_tokens, result.output_tokens), (300, 20));
    }

    #[test]
    fn rejects_malformed_payloads() {
        let questions = [NoulQuestion::new("r1", "?")];
        let err = parse_response(&json!({"oops": 1}), &questions).unwrap_err();
        assert!(err.to_string().contains("no 'answers'"));

        let err = parse_response(&json!({"answers": {}}), &questions).unwrap_err();
        assert!(err.to_string().contains("no noul answer"));

        let err =
            parse_response(&json!({"answers": {"r1": {"noul": "x"}}}), &questions).unwrap_err();
        assert!(err.to_string().contains("not a number"));
    }

    #[test]
    fn clamps_probabilities() {
        let data = json!({"answers": {"r1": {"noul": 1.4, "confidence": -1}}});
        let result = parse_response(&data, &[NoulQuestion::new("r1", "?")]).unwrap();
        assert_eq!(result.answers["r1"], NoulAnswer::new(1.0, 0.0));
    }

    #[test]
    fn from_env_requires_a_key() {
        temp_env(|| {
            std::env::remove_var("TYPESAFE_API_KEY");
            let err = JevClient::from_env("jev-latest", Duration::from_secs(1)).unwrap_err();
            assert!(err.to_string().contains("TYPESAFE_API_KEY"));
        });
    }

    /// Environment mutation is process-wide; keep it in one place.
    fn temp_env(body: impl FnOnce()) {
        let previous = std::env::var("TYPESAFE_API_KEY").ok();
        body();
        if let Some(value) = previous {
            std::env::set_var("TYPESAFE_API_KEY", value);
        }
    }

    #[test]
    fn mock_classifier_defaults_to_zero() {
        let dir = std::env::temp_dir().join("review-gate-mock-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("answers.json");
        std::fs::write(
            &path,
            r#"{"r1": 0.8, "r2": {"probability": 0.1, "confidence": 0.4}}"#,
        )
        .unwrap();

        let mock = MockClassifier::from_file(&path).unwrap();
        let questions = [
            NoulQuestion::new("r1", "?"),
            NoulQuestion::new("r2", "?"),
            NoulQuestion::new("r3", "?"),
        ];
        let result = mock.ask(&json!({}), &questions).unwrap();
        assert_eq!(result.answers["r1"].probability, 0.8);
        assert_eq!(result.answers["r2"].confidence, 0.4);
        assert_eq!(result.answers["r3"].probability, 0.0);

        let err = MockClassifier::from_file(&dir.join("missing.json")).unwrap_err();
        assert!(err.to_string().contains("cannot read mock answers"));
    }
}
