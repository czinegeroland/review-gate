//! The TypeSafe client, exercised over a real socket.
//!
//! A tiny HTTP server stands in for `api.typesafe.ai`, so the request shape,
//! the retry policy and the error mapping are all checked end to end without
//! reaching the network.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use review_gate::classifier::{Classifier, JevClient, NoulQuestion};
use serde_json::{json, Value};

/// A canned reply: status code and body.
type Reply = (u16, String);

struct MockServer {
    base_url: String,
    requests: Receiver<Value>,
    served: Arc<Mutex<usize>>,
}

impl MockServer {
    /// Serve `replies` in order; the last reply repeats once exhausted.
    fn start(replies: Vec<Reply>) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
        let port = listener.local_addr().unwrap().port();
        let (sender, requests) = channel();
        let served = Arc::new(Mutex::new(0usize));
        let counter = Arc::clone(&served);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let Some(body) = read_request(&mut stream) else {
                    break;
                };
                let index = {
                    let mut count = counter.lock().unwrap();
                    let index = *count;
                    *count += 1;
                    index
                };
                if sender.send(body).is_err() {
                    break;
                }
                let (status, payload) = replies
                    .get(index)
                    .or_else(|| replies.last())
                    .cloned()
                    .unwrap_or((200, "{}".to_string()));
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        MockServer {
            base_url: format!("http://127.0.0.1:{port}"),
            requests,
            served,
        }
    }

    fn client(&self) -> JevClient {
        JevClient::new(
            "test-key".to_string(),
            "jev-latest".to_string(),
            self.base_url.clone(),
            Duration::from_secs(5),
        )
        .with_backoff_ms(0)
    }

    fn last_request(&self) -> Value {
        self.requests
            .recv_timeout(Duration::from_secs(5))
            .expect("a request reached the server")
    }

    fn served(&self) -> usize {
        *self.served.lock().unwrap()
    }
}

/// Read one HTTP request and return its JSON body.
fn read_request(stream: &mut TcpStream) -> Option<Value> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut length = 0usize;
    let mut authorized = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().ok()?;
        }
        if lower.starts_with("authorization: bearer test-key") {
            authorized = true;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
    }
    assert!(authorized, "the client must send a bearer token");

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn questions() -> Vec<NoulQuestion> {
    let mut first = NoulQuestion::new("r1", "Is this risky?");
    first.criteria = Some(vec![
        ("true".to_string(), "yes".to_string()),
        ("false".to_string(), "no".to_string()),
    ]);
    let mut second = NoulQuestion::new("r2", "Is this cosmetic?");
    second.focus = Some("the diff body".to_string());
    vec![first, second]
}

fn ok_body() -> String {
    json!({
        "model": "jev-1.13.0",
        "answers": {
            "r1": {"type": "noul", "noul": 0.91, "confidence": 0.8},
            "r2": {"type": "noul", "noul": 0.02, "confidence": 0.97},
        },
        "usage": {"input_tokens": 300, "output_tokens": 20},
    })
    .to_string()
}

#[test]
fn sends_the_documented_request_shape() {
    let server = MockServer::start(vec![(200, ok_body())]);
    let result = server
        .client()
        .ask(&json!({"diff": "..."}), &questions())
        .expect("the call succeeds");

    let body = server.last_request();
    assert_eq!(body["model"], "jev-latest");
    assert_eq!(body["state"], json!({"diff": "..."}));
    assert_eq!(
        body["questions"]["r1"],
        json!({
            "type": "noul",
            "instructions": {"question": "Is this risky?"},
            "criteria": {"true": "yes", "false": "no"},
        })
    );
    assert_eq!(
        body["questions"]["r2"]["instructions"]["focus"],
        "the diff body"
    );
    assert!(body["questions"]["r2"].get("criteria").is_none());

    assert_eq!(result.answers["r1"].probability, 0.91);
    assert_eq!(result.answers["r1"].confidence, 0.8);
    assert_eq!(result.model, "jev-1.13.0");
    assert_eq!((result.input_tokens, result.output_tokens), (300, 20));
}

#[test]
fn retries_a_429_then_succeeds() {
    let server = MockServer::start(vec![
        (429, r#"{"error":"slow down"}"#.to_string()),
        (503, r#"{"error":"overloaded"}"#.to_string()),
        (200, ok_body()),
    ]);

    let result = server.client().ask(&json!({}), &questions()).unwrap();
    assert_eq!(result.answers["r1"].probability, 0.91);
    assert_eq!(server.served(), 3);
}

#[test]
fn gives_up_after_the_retry_budget() {
    let server = MockServer::start(vec![(503, r#"{"error":"down"}"#.to_string())]);
    let error = server
        .client()
        .ask(&json!({}), &questions())
        .expect_err("a permanent 503 fails");

    assert!(error.to_string().contains("after 4 attempts"), "{error}");
    assert!(error.to_string().contains("HTTP 503"), "{error}");
    assert_eq!(server.served(), 4);
}

#[test]
fn client_errors_are_not_retried() {
    let server = MockServer::start(vec![(401, r#"{"error":"bad key"}"#.to_string())]);
    let error = server.client().ask(&json!({}), &questions()).unwrap_err();

    assert!(error.to_string().contains("HTTP 401"), "{error}");
    assert_eq!(server.served(), 1, "401 must not be retried");
}

#[test]
fn a_missing_answer_is_an_error() {
    let server = MockServer::start(vec![(200, json!({"answers": {}}).to_string())]);
    let error = server.client().ask(&json!({}), &questions()).unwrap_err();
    assert!(error.to_string().contains("no noul answer"), "{error}");
}

#[test]
fn malformed_json_is_reported() {
    let server = MockServer::start(vec![(200, "not json at all".to_string())]);
    let error = server.client().ask(&json!({}), &questions()).unwrap_err();
    assert!(error.to_string().contains("malformed JSON"), "{error}");
}
