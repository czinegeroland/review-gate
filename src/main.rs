//! Command line interface.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

use review_gate::classifier::{Classifier, JevClient, MockClassifier};
use review_gate::config::{find_config, Config};
use review_gate::engine::{evaluate, PullRequestContext};
use review_gate::{diff, report, Error, Result};

const EXIT_DEGRADED: i32 = 3;

#[derive(Debug, Parser)]
#[command(
    name = "review-gate",
    version,
    about = "Classify a pull request diff into the reviewer types it actually needs."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Classify a diff and emit the required reviewers.
    Evaluate(Box<EvaluateArgs>),
    /// Check a rule configuration.
    Validate {
        /// Path to review-gate.yml.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
struct EvaluateArgs {
    /// Unified diff file, or '-' for stdin.
    #[arg(long)]
    diff_file: Option<String>,
    /// Base git ref (used when --diff-file is not given).
    #[arg(long)]
    base: Option<String>,
    /// Head git ref, defaults to the working tree.
    #[arg(long)]
    head: Option<String>,
    /// Repository directory for git operations.
    #[arg(long, default_value = ".")]
    repo_dir: PathBuf,

    /// Path to review-gate.yml.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Write the JSON result here (default: stdout).
    #[arg(long)]
    output: Option<PathBuf>,
    /// Also write a Markdown summary here.
    #[arg(long)]
    markdown: Option<PathBuf>,
    /// Append action outputs to this file (defaults to $GITHUB_OUTPUT).
    #[arg(long, num_args = 0..=1)]
    github_output: Option<Option<PathBuf>>,

    /// owner/name, recorded in the result.
    #[arg(long)]
    repo: Option<String>,
    /// Pull request number.
    #[arg(long)]
    pr_number: Option<u64>,
    /// Pull request title, given to the model.
    #[arg(long)]
    pr_title: Option<String>,
    /// Pull request description, given to the model.
    #[arg(long)]
    pr_body: Option<String>,

    /// Override the model id from the config.
    #[arg(long)]
    model: Option<String>,
    /// Per-request timeout in seconds.
    #[arg(long, default_value_t = 60.0)]
    timeout: f64,
    /// Answer from this JSON fixture instead of calling the API (testing).
    #[arg(long)]
    mock_answers: Option<PathBuf>,
    /// Exit 3 when classification was incomplete.
    #[arg(long)]
    fail_on_degraded: bool,
    /// Suppress progress on stderr.
    #[arg(long)]
    quiet: bool,
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("review-gate: {error}");
            error.exit_code()
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Validate { config } => run_validate(config.as_deref()),
        Command::Evaluate(args) => run_evaluate(*args),
    }
}

fn run_validate(config_path: Option<&Path>) -> Result<i32> {
    let path = find_config(config_path, Path::new("."))?;
    let config = Config::load(&path)?;
    let fallback = config.effective_fallback();
    println!(
        "{}: OK — {} active rule(s), {} reviewer type(s), fallback: {}",
        path.display(),
        config.active_rules().len(),
        config.reviewers.len(),
        if fallback.is_empty() {
            "none".to_string()
        } else {
            fallback.join(", ")
        }
    );
    Ok(0)
}

fn run_evaluate(args: EvaluateArgs) -> Result<i32> {
    let config_path = find_config(args.config.as_deref(), Path::new("."))?;
    let mut config = Config::load(&config_path)?;
    if let Some(model) = &args.model {
        config.defaults.model = model.clone();
    }

    let diff_text = diff::read(
        args.diff_file.as_deref(),
        args.base.as_deref(),
        args.head.as_deref(),
        &args.repo_dir,
    )?;
    let files = diff::parse(&diff_text);
    if files.is_empty() && !diff_text.trim().is_empty() {
        return Err(Error::Diff(
            "the input does not look like a unified diff".to_string(),
        ));
    }

    let context = PullRequestContext {
        repo: args.repo.clone(),
        number: args.pr_number,
        title: args.pr_title.clone(),
        body: args.pr_body.clone(),
    };

    let classifier = build_classifier(&args, &config)?;
    let evaluation = evaluate(&config, &files, classifier.as_ref(), &context);

    let payload = report::to_json(&evaluation, &context);
    match &args.output {
        Some(path) => write_file(path, &format!("{payload}\n"))?,
        None => println!("{payload}"),
    }
    if let Some(path) = &args.markdown {
        write_file(path, &report::to_markdown(&evaluation))?;
    }
    if let Some(path) = github_output_path(&args) {
        let result_path = args.output.as_ref().map(|p| p.display().to_string());
        report::write_github_outputs(&path, &evaluation, result_path.as_deref())?;
    }

    if !args.quiet {
        let summary = if evaluation.required_reviewers.is_empty() {
            "none".to_string()
        } else {
            evaluation.required_reviewers.join(", ")
        };
        eprintln!("review-gate: required reviewers: {summary}");
        for error in &evaluation.errors {
            eprintln!("review-gate: warning: {error}");
        }
    }

    if evaluation.degraded && args.fail_on_degraded {
        return Ok(EXIT_DEGRADED);
    }
    Ok(0)
}

/// `--github-output` with no value falls back to `$GITHUB_OUTPUT`.
fn github_output_path(args: &EvaluateArgs) -> Option<PathBuf> {
    match args.github_output.as_ref()? {
        Some(path) if !path.as_os_str().is_empty() => Some(path.clone()),
        _ => std::env::var_os("GITHUB_OUTPUT")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    }
}

fn build_classifier(args: &EvaluateArgs, config: &Config) -> Result<Box<dyn Classifier>> {
    let mock = args
        .mock_answers
        .clone()
        .or_else(|| std::env::var_os("REVIEW_GATE_MOCK_ANSWERS").map(PathBuf::from));
    if let Some(path) = mock {
        return Ok(Box::new(MockClassifier::from_file(&path)?));
    }
    let timeout = Duration::from_secs_f64(args.timeout.max(1.0));
    JevClient::from_env(&config.defaults.model, timeout)
        .map(|client| Box::new(client) as Box<dyn Classifier>)
        .map_err(|error| {
            Error::Model(format!(
                "{error}; set it to a key from https://console.typesafe.ai/ \
                 or pass --mock-answers for a dry run"
            ))
        })
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents)
        .map_err(|err| Error::Io(format!("cannot write {}: {err}", path.display())))
}
