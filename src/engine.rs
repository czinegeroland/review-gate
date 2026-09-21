//! Evaluation: diff + rules -> required reviewer types.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde_json::{json, Map, Value};

use crate::classifier::{BatchResult, Classifier, NoulAnswer, NoulQuestion};
use crate::config::{Config, LowConfidence, Rule};
use crate::diff::{chunk_files, Chunk, FileDiff};

/// Optional metadata about the pull request, added to the model state.
#[derive(Debug, Clone, Default)]
pub struct PullRequestContext {
    pub repo: Option<String>,
    pub number: Option<u64>,
    pub title: Option<String>,
    pub body: Option<String>,
}

impl PullRequestContext {
    fn as_state(&self) -> Option<Value> {
        let mut state = Map::new();
        if let Some(title) = &self.title {
            state.insert("title".into(), json!(title));
        }
        if let Some(body) = &self.body {
            state.insert("description".into(), json!(clip(body, 4_000)));
        }
        if state.is_empty() {
            None
        } else {
            Some(Value::Object(state))
        }
    }
}

/// The outcome for one rule.
#[derive(Debug, Clone)]
pub struct Decision {
    pub rule_id: String,
    pub question: String,
    pub reviewers: Vec<String>,
    pub fired: bool,
    pub probability: f64,
    pub confidence: f64,
    pub threshold: f64,
    pub min_confidence: f64,
    pub low_confidence: bool,
    pub files: Vec<String>,
}

/// A rule that was not asked about, and why.
#[derive(Debug, Clone)]
pub struct Skipped {
    pub rule_id: String,
    pub reason: String,
}

/// Counters for the result document.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub files: usize,
    pub chunks: usize,
    pub requests: usize,
    pub duration_ms: u128,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Everything the reporter needs to render a result.
#[derive(Debug, Clone, Default)]
pub struct Evaluation {
    pub required_reviewers: Vec<String>,
    pub required_reviewer_teams: Vec<String>,
    pub decisions: Vec<Decision>,
    pub skipped: Vec<Skipped>,
    pub stats: Stats,
    pub model: String,
    pub degraded: bool,
    pub errors: Vec<String>,
}

/// Turn a configured rule into the Noul question sent to the model.
pub fn build_question(rule: &Rule) -> NoulQuestion {
    NoulQuestion {
        id: rule.id.clone(),
        question: rule.question.clone(),
        focus: rule.focus.clone(),
        criteria: rule.criteria.as_ref().map(|criteria| {
            criteria
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        }),
    }
}

/// Classify the diff and resolve the required reviewer set.
pub fn evaluate(
    config: &Config,
    files: &[FileDiff],
    classifier: &dyn Classifier,
    context: &PullRequestContext,
) -> Evaluation {
    let started = Instant::now();

    let mut considered: Vec<FileDiff> = files
        .iter()
        .filter(|file| !config.is_ignored(&file.path) && !file.binary)
        .cloned()
        .collect();
    let truncated_files = considered.len() > config.defaults.max_files;
    if truncated_files {
        considered.truncate(config.defaults.max_files);
    }

    let mut skipped: Vec<Skipped> = config
        .rules
        .iter()
        .filter(|rule| !rule.enabled)
        .map(|rule| Skipped {
            rule_id: rule.id.clone(),
            reason: "disabled".to_string(),
        })
        .collect();

    let mut applicable: Vec<&Rule> = Vec::new();
    for rule in config.active_rules() {
        if considered.iter().any(|file| rule.matches(&file.path)) {
            applicable.push(rule);
        } else {
            skipped.push(Skipped {
                rule_id: rule.id.clone(),
                reason: "no matching files".to_string(),
            });
        }
    }

    let mut stats = Stats {
        files: considered.len(),
        ..Stats::default()
    };

    if considered.is_empty() || applicable.is_empty() {
        stats.duration_ms = started.elapsed().as_millis();
        let required: HashSet<String> = config.always_reviewers.iter().cloned().collect();
        return finish(
            config,
            required,
            Vec::new(),
            skipped,
            stats,
            String::new(),
            false,
            Vec::new(),
        );
    }

    let chunks = chunk_files(&considered, config.defaults.max_chunk_chars);
    stats.chunks = chunks.len();

    let Run {
        results,
        mut errors,
        model,
        input_tokens,
        output_tokens,
    } = run_chunks(&chunks, &applicable, classifier, config, context);

    stats.requests = results.len();
    stats.input_tokens = input_tokens;
    stats.output_tokens = output_tokens;
    let mut degraded = !errors.is_empty();

    if degraded && results.is_empty() {
        stats.duration_ms = started.elapsed().as_millis();
        let required: HashSet<String> = config
            .effective_fallback()
            .into_iter()
            .chain(config.always_reviewers.iter().cloned())
            .collect();
        return finish(
            config,
            required,
            Vec::new(),
            skipped,
            stats,
            model,
            true,
            errors,
        );
    }

    let decisions = decide(&applicable, &results, config);
    let mut required: HashSet<String> = config.always_reviewers.iter().cloned().collect();
    for decision in decisions.iter().filter(|decision| decision.fired) {
        required.extend(decision.reviewers.iter().cloned());
    }

    if truncated_files {
        errors.push(format!(
            "diff has more than {} files; only the first {} were classified",
            config.defaults.max_files, config.defaults.max_files
        ));
        degraded = true;
    }
    if degraded {
        required.extend(config.effective_fallback());
    }

    stats.duration_ms = started.elapsed().as_millis();
    finish(
        config, required, decisions, skipped, stats, model, degraded, errors,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish(
    config: &Config,
    required: HashSet<String>,
    decisions: Vec<Decision>,
    skipped: Vec<Skipped>,
    stats: Stats,
    model: String,
    degraded: bool,
    errors: Vec<String>,
) -> Evaluation {
    let ordered = config.order_reviewers(&required);
    Evaluation {
        required_reviewer_teams: config.teams_for(&ordered),
        required_reviewers: ordered,
        decisions,
        skipped,
        stats,
        model,
        degraded,
        errors,
    }
}

/// One classified chunk: the chunk, the rules asked about it, and the answers.
type ChunkResult<'a> = (&'a Chunk, Vec<&'a Rule>, BatchResult);

struct Run<'a> {
    results: Vec<ChunkResult<'a>>,
    errors: Vec<String>,
    model: String,
    input_tokens: u64,
    output_tokens: u64,
}

fn run_chunks<'a>(
    chunks: &'a [Chunk],
    rules: &[&'a Rule],
    classifier: &dyn Classifier,
    config: &Config,
    context: &PullRequestContext,
) -> Run<'a> {
    let jobs: Vec<(&Chunk, Vec<&Rule>)> = chunks
        .iter()
        .filter_map(|chunk| {
            let chunk_rules: Vec<&Rule> = rules
                .iter()
                .copied()
                .filter(|rule| chunk.files.iter().any(|path| rule.matches(path)))
                .collect();
            (!chunk_rules.is_empty()).then_some((chunk, chunk_rules))
        })
        .collect();

    let run_one = |job: &(&'a Chunk, Vec<&'a Rule>)| -> (&'a Chunk, Vec<&'a Rule>, crate::Result<BatchResult>) {
        let (chunk, chunk_rules) = job;
        let state = state_for(chunk, context);
        let questions: Vec<NoulQuestion> = chunk_rules.iter().map(|rule| build_question(rule)).collect();
        (chunk, chunk_rules.clone(), classifier.ask(&state, &questions))
    };

    let workers = config.defaults.max_concurrency.min(jobs.len().max(1));
    let outcomes: Vec<_> = if workers > 1 && jobs.len() > 1 {
        let per_worker = jobs.len().div_ceil(workers);
        std::thread::scope(|scope| {
            let handles: Vec<_> = jobs
                .chunks(per_worker)
                .map(|slice| scope.spawn(move || slice.iter().map(&run_one).collect::<Vec<_>>()))
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().expect("classification worker panicked"))
                .collect()
        })
    } else {
        jobs.iter().map(&run_one).collect()
    };

    let mut run = Run {
        results: Vec::new(),
        errors: Vec::new(),
        model: String::new(),
        input_tokens: 0,
        output_tokens: 0,
    };
    for (chunk, chunk_rules, outcome) in outcomes {
        match outcome {
            Err(error) => run
                .errors
                .push(format!("{}: {error}", chunk.files.join(", "))),
            Ok(result) => {
                if run.model.is_empty() {
                    run.model = result.model.clone();
                }
                run.input_tokens += result.input_tokens;
                run.output_tokens += result.output_tokens;
                run.results.push((chunk, chunk_rules, result));
            }
        }
    }
    run
}

fn state_for(chunk: &Chunk, context: &PullRequestContext) -> Value {
    let mut state = Map::new();
    if let Some(pull_request) = context.as_state() {
        state.insert("pull_request".into(), pull_request);
    }
    state.insert("changed_files".into(), json!(chunk.files));
    state.insert("diff".into(), json!(chunk.text));
    Value::Object(state)
}

fn decide(rules: &[&Rule], results: &[ChunkResult<'_>], config: &Config) -> Vec<Decision> {
    let mut best: HashMap<&str, (NoulAnswer, Vec<String>)> = HashMap::new();

    for (chunk, chunk_rules, result) in results {
        for rule in chunk_rules {
            let Some(answer) = result.answers.get(&rule.id) else {
                continue;
            };
            let matched: Vec<String> = chunk
                .files
                .iter()
                .filter(|path| rule.matches(path))
                .cloned()
                .collect();
            match best.get_mut(rule.id.as_str()) {
                None => {
                    best.insert(rule.id.as_str(), (*answer, matched));
                }
                Some((current, files)) => {
                    if answer.probability > current.probability {
                        *current = *answer;
                        *files = matched;
                    } else if answer.probability == current.probability {
                        for path in matched {
                            if !files.contains(&path) {
                                files.push(path);
                            }
                        }
                    }
                }
            }
        }
    }

    let mut decisions: Vec<Decision> = rules
        .iter()
        .filter_map(|rule| {
            let (answer, files) = best.get(rule.id.as_str())?;
            let threshold = rule.threshold(&config.defaults);
            let floor = rule.min_confidence(&config.defaults);
            let over_threshold = answer.probability >= threshold;
            let unsure = answer.confidence < floor;
            let fired = if over_threshold && unsure {
                config.defaults.on_low_confidence == LowConfidence::Require
            } else {
                over_threshold
            };
            let mut files = files.clone();
            files.sort();
            Some(Decision {
                rule_id: rule.id.clone(),
                question: rule.question.clone(),
                reviewers: rule.reviewers.clone(),
                fired,
                probability: round4(answer.probability),
                confidence: round4(answer.confidence),
                threshold,
                min_confidence: floor,
                low_confidence: over_threshold && unsure,
                files,
            })
        })
        .collect();

    decisions.sort_by(|a, b| {
        a.fired
            .cmp(&b.fired)
            .reverse()
            .then(
                b.probability
                    .partial_cmp(&a.probability)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.rule_id.cmp(&b.rule_id))
    });
    decisions
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}...")
}
