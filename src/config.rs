//! Loading and validation of the Review Gate rule configuration.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer};

use crate::{Error, Result};

/// Locations searched when `--config` is not given.
pub const DEFAULT_CONFIG_PATHS: [&str; 4] = [
    ".github/review-gate.yml",
    ".github/review-gate.yaml",
    "review-gate.yml",
    "review-gate.yaml",
];

/// A reviewer type that rules can require.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reviewer {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub team: Option<String>,
}

/// Evaluation defaults, overridable per rule where noted.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    #[serde(default = "default_threshold")]
    pub threshold: f64,
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
    #[serde(default)]
    pub on_low_confidence: LowConfidence,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_files")]
    pub max_files: usize,
    #[serde(default = "default_max_chunk_chars")]
    pub max_chunk_chars: usize,
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
}

/// What to do with an over-threshold answer the model is not confident about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LowConfidence {
    /// Fire the rule anyway, flagged as low confidence.
    #[default]
    Require,
    /// Treat the answer as a non-match.
    Ignore,
}

fn default_threshold() -> f64 {
    0.6
}
fn default_min_confidence() -> f64 {
    0.55
}
fn default_model() -> String {
    "jev-latest".to_string()
}
fn default_max_files() -> usize {
    200
}
fn default_max_chunk_chars() -> usize {
    60_000
}
fn default_max_concurrency() -> usize {
    4
}
fn default_true() -> bool {
    true
}
fn default_version() -> u8 {
    1
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            threshold: default_threshold(),
            min_confidence: default_min_confidence(),
            on_low_confidence: LowConfidence::default(),
            model: default_model(),
            max_files: default_max_files(),
            max_chunk_chars: default_max_chunk_chars(),
            max_concurrency: default_max_concurrency(),
        }
    }
}

/// One natural-language condition that, when true of the diff, requires reviewers.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub question: String,
    pub reviewers: Vec<String>,
    #[serde(default)]
    pub criteria: Option<IndexMap<String, String>>,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    #[serde(default)]
    pub threshold: Option<f64>,
    #[serde(default)]
    pub min_confidence: Option<f64>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Compiled form of `paths`, built by [`Config::load`].
    #[serde(skip)]
    globs: Option<GlobSet>,
}

impl Rule {
    /// Whether this rule's cheap path pre-filter accepts `path`.
    pub fn matches(&self, path: &str) -> bool {
        match &self.globs {
            None => true,
            Some(set) => set.is_match(path),
        }
    }

    /// The threshold in force for this rule.
    pub fn threshold(&self, defaults: &Defaults) -> f64 {
        self.threshold.unwrap_or(defaults.threshold)
    }

    /// The confidence floor in force for this rule.
    pub fn min_confidence(&self, defaults: &Defaults) -> f64 {
        self.min_confidence.unwrap_or(defaults.min_confidence)
    }
}

/// The whole `review-gate.yml` document.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u8,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default, deserialize_with = "nullable_map")]
    pub reviewers: IndexMap<String, Reviewer>,
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub always_reviewers: Vec<String>,
    #[serde(default)]
    pub fallback_reviewers: Vec<String>,
    #[serde(default)]
    pub ignore_paths: Vec<String>,
    #[serde(skip)]
    ignore_globs: Option<GlobSet>,
}

/// Accepts `reviewer_name:` with an empty value as well as a mapping.
fn nullable_map<'de, D>(
    deserializer: D,
) -> std::result::Result<IndexMap<String, Reviewer>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: IndexMap<String, Option<Reviewer>> = IndexMap::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .map(|(name, reviewer)| (name, reviewer.unwrap_or_default()))
        .collect())
}

impl Config {
    /// Parse and validate a configuration file.
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| Error::Config(format!("cannot read {}: {err}", path.display())))?;
        let mut config: Config = serde_yaml::from_str(&text)
            .map_err(|err| Error::Config(format!("{} is invalid:\n  - {err}", path.display())))?;
        config.prepare().map_err(|message| {
            Error::Config(format!("{} is invalid:\n{message}", path.display()))
        })?;
        Ok(config)
    }

    /// Parse and validate a configuration from a string (used by tests).
    pub fn parse(text: &str) -> Result<Config> {
        let mut config: Config =
            serde_yaml::from_str(text).map_err(|err| Error::Config(format!("invalid: {err}")))?;
        config.prepare().map_err(Error::Config)?;
        Ok(config)
    }

    /// Validate cross-field invariants and compile the path filters.
    fn prepare(&mut self) -> std::result::Result<(), String> {
        let mut problems: Vec<String> = Vec::new();

        if self.version != 1 {
            problems.push(format!(
                "  - version: unsupported version {}, expected 1",
                self.version
            ));
        }
        if self.rules.is_empty() {
            problems.push("  - rules: at least one rule is required".to_string());
        }

        let known: HashSet<&String> = self.reviewers.keys().collect();
        let mut seen: HashSet<String> = HashSet::new();

        for (index, rule) in self.rules.iter_mut().enumerate() {
            let at = format!("rules.{index}");
            rule.id = rule.id.trim().to_string();
            if rule.id.is_empty() {
                problems.push(format!("  - {at}.id: must not be empty"));
            } else if rule.id.split_whitespace().count() > 1 {
                problems.push(format!("  - {at}.id: must not contain whitespace"));
            } else if !seen.insert(rule.id.clone()) {
                problems.push(format!("  - {at}.id: duplicate rule id: {}", rule.id));
            }

            rule.question = rule
                .question
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if rule.question.chars().count() < 10 {
                problems.push(format!(
                    "  - {at}.question: must be a sentence of at least 10 characters"
                ));
            }

            if rule.reviewers.is_empty() {
                problems.push(format!(
                    "  - {at}.reviewers: must list at least one reviewer"
                ));
            }
            for reviewer in &rule.reviewers {
                if !known.contains(reviewer) {
                    problems.push(format!(
                        "  - {at}.reviewers: rule '{}' references undeclared reviewer '{reviewer}'; \
                         declare it under 'reviewers'",
                        rule.id
                    ));
                }
            }

            if let Some(criteria) = &rule.criteria {
                let unknown: Vec<&String> = criteria
                    .keys()
                    .filter(|key| key.as_str() != "true" && key.as_str() != "false")
                    .collect();
                if !unknown.is_empty() {
                    problems.push(format!(
                        "  - {at}.criteria: keys must be 'true'/'false', got {unknown:?}"
                    ));
                }
            }

            if let Some(value) = rule.threshold {
                check_probability(&format!("{at}.threshold"), value, &mut problems);
            }
            if let Some(value) = rule.min_confidence {
                check_probability(&format!("{at}.min_confidence"), value, &mut problems);
            }

            match compile(rule.paths.as_deref()) {
                Ok(globs) => rule.globs = globs,
                Err(message) => problems.push(format!("  - {at}.paths: {message}")),
            }
        }

        check_probability("defaults.threshold", self.defaults.threshold, &mut problems);
        check_probability(
            "defaults.min_confidence",
            self.defaults.min_confidence,
            &mut problems,
        );
        if self.defaults.max_files == 0 {
            problems.push("  - defaults.max_files: must be at least 1".to_string());
        }
        if self.defaults.max_chunk_chars < 1_000 {
            problems.push("  - defaults.max_chunk_chars: must be at least 1000".to_string());
        }
        if self.defaults.max_concurrency == 0 || self.defaults.max_concurrency > 32 {
            problems.push("  - defaults.max_concurrency: must be between 1 and 32".to_string());
        }

        for (field, names) in [
            ("always_reviewers", &self.always_reviewers),
            ("fallback_reviewers", &self.fallback_reviewers),
        ] {
            for name in names {
                if !known.contains(name) {
                    problems.push(format!(
                        "  - {field}: references undeclared reviewer '{name}'"
                    ));
                }
            }
        }

        match compile(Some(&self.ignore_paths)) {
            Ok(globs) => self.ignore_globs = globs,
            Err(message) => problems.push(format!("  - ignore_paths: {message}")),
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("\n"))
        }
    }

    /// Reviewers required when the model cannot answer: the explicit list, else everyone.
    pub fn effective_fallback(&self) -> Vec<String> {
        if self.fallback_reviewers.is_empty() {
            self.reviewers.keys().cloned().collect()
        } else {
            self.fallback_reviewers.clone()
        }
    }

    /// Rules that are switched on.
    pub fn active_rules(&self) -> Vec<&Rule> {
        self.rules.iter().filter(|rule| rule.enabled).collect()
    }

    /// Whether `path` is excluded from classification entirely.
    pub fn is_ignored(&self, path: &str) -> bool {
        self.ignore_globs
            .as_ref()
            .is_some_and(|set| set.is_match(path))
    }

    /// Return `names` in the order the reviewers are declared in the config.
    pub fn order_reviewers(&self, names: &HashSet<String>) -> Vec<String> {
        self.reviewers
            .keys()
            .filter(|name| names.contains(*name))
            .cloned()
            .collect()
    }

    /// Team slugs for the given reviewer types, skipping those without one.
    pub fn teams_for(&self, names: &[String]) -> Vec<String> {
        names
            .iter()
            .filter_map(|name| self.reviewers.get(name).and_then(|r| r.team.clone()))
            .collect()
    }
}

fn check_probability(field: &str, value: f64, problems: &mut Vec<String>) {
    if !(0.0..=1.0).contains(&value) || value.is_nan() {
        problems.push(format!("  - {field}: must be between 0 and 1, got {value}"));
    }
}

/// Compile glob patterns; `**/x` also matches `x` at the repository root.
fn compile(patterns: Option<&[String]>) -> std::result::Result<Option<GlobSet>, String> {
    let patterns = match patterns {
        None => return Ok(None),
        Some([]) => return Ok(None),
        Some(patterns) => patterns,
    };
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|err| format!("invalid glob '{pattern}': {err}"))?);
        if let Some(rest) = pattern.strip_prefix("**/") {
            if let Ok(glob) = Glob::new(rest) {
                builder.add(glob);
            }
        }
    }
    builder
        .build()
        .map(Some)
        .map_err(|err| format!("cannot build glob set: {err}"))
}

/// Resolve the config path, searching the default locations when not given.
pub fn find_config(explicit: Option<&Path>, root: &Path) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if !path.is_file() {
            return Err(Error::Config(format!(
                "config file not found: {}",
                path.display()
            )));
        }
        return Ok(path.to_path_buf());
    }
    for candidate in DEFAULT_CONFIG_PATHS {
        let path = root.join(candidate);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(Error::Config(format!(
        "no config file found; looked for {} (use --config)",
        DEFAULT_CONFIG_PATHS.join(", ")
    )))
}
