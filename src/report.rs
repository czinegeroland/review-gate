//! Rendering an evaluation as JSON, Markdown and GitHub Action outputs.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::engine::{Evaluation, PullRequestContext};
use crate::{Error, Result, SCHEMA_VERSION};

/// The stable JSON document described in the PRD.
pub fn to_value(evaluation: &Evaluation, context: &PullRequestContext) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "generated_at": now_rfc3339(),
        "model": evaluation.model,
        "degraded": evaluation.degraded,
        "errors": evaluation.errors,
        "pull_request": {
            "repo": context.repo,
            "number": context.number,
            "title": context.title,
        },
        "required_reviewers": evaluation.required_reviewers,
        "required_reviewer_teams": evaluation.required_reviewer_teams,
        "decisions": evaluation.decisions.iter().map(|decision| json!({
            "rule_id": decision.rule_id,
            "question": decision.question,
            "fired": decision.fired,
            "probability": decision.probability,
            "confidence": decision.confidence,
            "threshold": decision.threshold,
            "min_confidence": decision.min_confidence,
            "low_confidence": decision.low_confidence,
            "reviewers": decision.reviewers,
            "files": decision.files,
        })).collect::<Vec<_>>(),
        "skipped_rules": evaluation.skipped.iter().map(|skipped| json!({
            "rule_id": skipped.rule_id,
            "reason": skipped.reason,
        })).collect::<Vec<_>>(),
        "stats": {
            "files": evaluation.stats.files,
            "chunks": evaluation.stats.chunks,
            "requests": evaluation.stats.requests,
            "duration_ms": evaluation.stats.duration_ms,
            "input_tokens": evaluation.stats.input_tokens,
            "output_tokens": evaluation.stats.output_tokens,
        },
    })
}

/// The JSON document, pretty printed.
pub fn to_json(evaluation: &Evaluation, context: &PullRequestContext) -> String {
    serde_json::to_string_pretty(&to_value(evaluation, context))
        .expect("the result document is always serialisable")
}

/// A short summary for the job summary or a pull request comment.
pub fn to_markdown(evaluation: &Evaluation) -> String {
    let mut out = String::from("## Review Gate\n\n");

    if evaluation.required_reviewers.is_empty() {
        out.push_str("**Required reviewers:** none — no rule matched this diff.\n");
    } else {
        let listed = evaluation
            .required_reviewers
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "**Required reviewers:** {listed}");
    }
    if !evaluation.required_reviewer_teams.is_empty() {
        let teams = evaluation
            .required_reviewer_teams
            .iter()
            .map(|team| format!("`{team}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "**Teams:** {teams}");
    }
    out.push('\n');

    if evaluation.degraded {
        out.push_str(
            "> [!WARNING]\n> Classification was incomplete, so the fallback reviewer set is \
             required.\n",
        );
        for error in &evaluation.errors {
            let _ = writeln!(out, "> - {error}");
        }
        out.push('\n');
    }

    let fired: Vec<_> = evaluation.decisions.iter().filter(|d| d.fired).collect();
    if !fired.is_empty() {
        out.push_str("| Rule | Reviewers | Probability | Confidence | Files |\n");
        out.push_str("| --- | --- | --: | --: | --- |\n");
        for decision in fired {
            let mut files = decision
                .files
                .iter()
                .take(3)
                .map(|path| format!("`{path}`"))
                .collect::<Vec<_>>()
                .join(", ");
            if decision.files.len() > 3 {
                let _ = write!(files, " +{} more", decision.files.len() - 3);
            }
            let flag = if decision.low_confidence {
                " ⚠️ low confidence"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "| `{}`{flag} | {} | {:.2} | {:.2} | {files} |",
                decision.rule_id,
                decision.reviewers.join(", "),
                decision.probability,
                decision.confidence,
            );
        }
        out.push('\n');
    }

    let quiet: Vec<_> = evaluation.decisions.iter().filter(|d| !d.fired).collect();
    if !quiet.is_empty() {
        let _ = writeln!(
            out,
            "<details><summary>{} rule(s) did not fire</summary>\n",
            quiet.len()
        );
        for decision in quiet {
            let _ = writeln!(
                out,
                "- `{}` — p={:.2} (threshold {:.2}), confidence {:.2}",
                decision.rule_id, decision.probability, decision.threshold, decision.confidence
            );
        }
        out.push_str("\n</details>\n\n");
    }

    let stats = &evaluation.stats;
    let model = if evaluation.model.is_empty() {
        "n/a"
    } else {
        &evaluation.model
    };
    let _ = writeln!(
        out,
        "<sub>{} file(s), {} chunk(s), {} request(s), {} ms, model `{model}`</sub>",
        stats.files, stats.chunks, stats.requests, stats.duration_ms
    );
    out
}

/// Append `name=value` pairs to `$GITHUB_OUTPUT`.
pub fn write_github_outputs(
    path: &Path,
    evaluation: &Evaluation,
    result_path: Option<&str>,
) -> Result<()> {
    let outputs = [
        (
            "required_reviewers",
            json!(evaluation.required_reviewers).to_string(),
        ),
        (
            "required_reviewers_csv",
            evaluation.required_reviewers.join(","),
        ),
        (
            "required_reviewer_teams",
            json!(evaluation.required_reviewer_teams).to_string(),
        ),
        (
            "reviewer_count",
            evaluation.required_reviewers.len().to_string(),
        ),
        ("degraded", evaluation.degraded.to_string()),
        ("result_path", result_path.unwrap_or_default().to_string()),
    ];

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| Error::Io(format!("cannot write {}: {err}", path.display())))?;
    for (key, value) in outputs {
        writeln!(file, "{key}={value}")
            .map_err(|err| Error::Io(format!("cannot write {}: {err}", path.display())))?;
    }
    Ok(())
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
fn now_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    format_epoch(seconds)
}

/// Format seconds since the Unix epoch as an RFC 3339 UTC timestamp.
fn format_epoch(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let time = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3_600,
        (time % 3_600) / 60,
        time % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since the epoch -> (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_timestamps() {
        assert_eq!(format_epoch(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_epoch(1_758_412_800), "2025-09-21T00:00:00Z");
        assert_eq!(format_epoch(1_772_323_199), "2026-02-28T23:59:59Z");
        assert_eq!(format_epoch(1_772_323_200), "2026-03-01T00:00:00Z");
        // A leap day.
        assert_eq!(format_epoch(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn generated_at_is_well_formed() {
        let stamp = now_rfc3339();
        assert_eq!(stamp.len(), 20, "{stamp}");
        assert!(stamp.ends_with('Z'));
        assert!(stamp.starts_with("20"));
    }
}
