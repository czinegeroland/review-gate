"""Rendering an evaluation as JSON, Markdown and GitHub Action outputs."""

from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from . import SCHEMA_VERSION
from .engine import Evaluation, PullRequestContext


def to_dict(evaluation: Evaluation, context: PullRequestContext) -> dict[str, Any]:
    """The stable JSON document described in the PRD."""
    return {
        "schema_version": SCHEMA_VERSION,
        "generated_at": datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "model": evaluation.model,
        "degraded": evaluation.degraded,
        "errors": evaluation.errors,
        "pull_request": context.as_output(),
        "required_reviewers": evaluation.required_reviewers,
        "required_reviewer_teams": evaluation.required_reviewer_teams,
        "decisions": [
            {
                "rule_id": decision.rule.id,
                "question": decision.rule.question,
                "fired": decision.fired,
                "probability": decision.probability,
                "confidence": decision.confidence,
                "threshold": decision.threshold,
                "min_confidence": decision.min_confidence,
                "low_confidence": decision.low_confidence,
                "reviewers": list(decision.rule.reviewers),
                "files": decision.files,
            }
            for decision in evaluation.decisions
        ],
        "skipped_rules": [
            {"rule_id": item.rule_id, "reason": item.reason} for item in evaluation.skipped
        ],
        "stats": {
            "files": evaluation.stats.files,
            "chunks": evaluation.stats.chunks,
            "requests": evaluation.stats.requests,
            "duration_ms": evaluation.stats.duration_ms,
            "input_tokens": evaluation.stats.input_tokens,
            "output_tokens": evaluation.stats.output_tokens,
        },
    }


def to_json(evaluation: Evaluation, context: PullRequestContext) -> str:
    return json.dumps(to_dict(evaluation, context), indent=2, sort_keys=False)


def to_markdown(evaluation: Evaluation, context: PullRequestContext) -> str:
    """A short summary for the job summary or a PR comment."""
    lines: list[str] = ["## Review Gate", ""]

    if evaluation.required_reviewers:
        listed = ", ".join(f"`{name}`" for name in evaluation.required_reviewers)
        lines.append(f"**Required reviewers:** {listed}")
    else:
        lines.append("**Required reviewers:** none — no rule matched this diff.")
    if evaluation.required_reviewer_teams:
        teams = ", ".join(f"`{team}`" for team in evaluation.required_reviewer_teams)
        lines.append(f"**Teams:** {teams}")
    lines.append("")

    if evaluation.degraded:
        lines.append(
            "> [!WARNING]\n"
            "> Classification was incomplete, so the fallback reviewer set is required."
        )
        for error in evaluation.errors:
            lines.append(f"> - {error}")
        lines.append("")

    fired = [d for d in evaluation.decisions if d.fired]
    if fired:
        lines += [
            "| Rule | Reviewers | Probability | Confidence | Files |",
            "| --- | --- | --: | --: | --- |",
        ]
        for decision in fired:
            files = ", ".join(f"`{path}`" for path in decision.files[:3])
            if len(decision.files) > 3:
                files += f" +{len(decision.files) - 3} more"
            flag = " ⚠️ low confidence" if decision.low_confidence else ""
            lines.append(
                f"| `{decision.rule.id}`{flag} | {', '.join(decision.rule.reviewers)} "
                f"| {decision.probability:.2f} | {decision.confidence:.2f} | {files} |"
            )
        lines.append("")

    quiet = [d for d in evaluation.decisions if not d.fired]
    if quiet:
        lines.append(f"<details><summary>{len(quiet)} rule(s) did not fire</summary>\n")
        for decision in quiet:
            lines.append(
                f"- `{decision.rule.id}` — p={decision.probability:.2f} "
                f"(threshold {decision.threshold:.2f}), confidence {decision.confidence:.2f}"
            )
        lines.append("\n</details>\n")

    stats = evaluation.stats
    lines.append(
        f"<sub>{stats.files} file(s), {stats.chunks} chunk(s), {stats.requests} request(s), "
        f"{stats.duration_ms} ms, model `{evaluation.model or 'n/a'}`</sub>"
    )
    return "\n".join(lines) + "\n"


def write_github_outputs(
    path: str | Path, evaluation: Evaluation, result_path: str | None
) -> None:
    """Append ``name=value`` pairs to ``$GITHUB_OUTPUT``."""
    outputs = {
        "required_reviewers": json.dumps(evaluation.required_reviewers),
        "required_reviewers_csv": ",".join(evaluation.required_reviewers),
        "required_reviewer_teams": json.dumps(evaluation.required_reviewer_teams),
        "reviewer_count": str(len(evaluation.required_reviewers)),
        "degraded": "true" if evaluation.degraded else "false",
        "result_path": result_path or "",
    }
    with Path(path).open("a", encoding="utf-8") as handle:
        for key, value in outputs.items():
            handle.write(f"{key}={value}\n")
