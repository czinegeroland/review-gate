from __future__ import annotations

import json
from pathlib import Path

from review_gate.classifier import NoulAnswer
from review_gate.config import Config
from review_gate.diff import parse_diff
from review_gate.engine import Evaluation, PullRequestContext, evaluate
from review_gate.report import to_dict, to_json, to_markdown, write_github_outputs
from tests.test_engine import FakeClassifier


def build(config: Config, sample_diff: str, **answers: NoulAnswer) -> Evaluation:
    return evaluate(config, parse_diff(sample_diff), FakeClassifier(dict(answers)))


def test_json_document_shape(config: Config, sample_diff: str) -> None:
    context = PullRequestContext(repo="acme/api", number=42, title="Rename")
    result = build(config, sample_diff, **{"lambda-event-contract": NoulAnswer(0.94, 0.88)})
    payload = json.loads(to_json(result, context))

    assert payload["schema_version"] == "1.0"
    assert payload["required_reviewers"] == ["devops"]
    assert payload["required_reviewer_teams"] == ["acme/platform"]
    assert payload["pull_request"] == {"repo": "acme/api", "number": 42, "title": "Rename"}
    assert payload["degraded"] is False
    decision = payload["decisions"][0]
    assert decision["rule_id"] == "lambda-event-contract"
    assert decision["fired"] is True
    assert decision["reviewers"] == ["devops"]
    assert decision["files"] == ["services/orders/handler.py"]
    assert payload["stats"]["files"] == 2
    assert payload["generated_at"].endswith("Z")


def test_markdown_lists_fired_and_quiet_rules(config: Config, sample_diff: str) -> None:
    result = build(config, sample_diff, **{"lambda-event-contract": NoulAnswer(0.94, 0.88)})
    text = to_markdown(result, PullRequestContext())
    assert "**Required reviewers:** `devops`" in text
    assert "`lambda-event-contract`" in text
    assert "did not fire" in text


def test_markdown_when_nothing_fires(config: Config, sample_diff: str) -> None:
    text = to_markdown(build(config, sample_diff), PullRequestContext())
    assert "none — no rule matched" in text


def test_markdown_warns_when_degraded(config: Config, sample_diff: str) -> None:
    result = evaluate(config, parse_diff(sample_diff), FakeClassifier({}, fail=True))
    text = to_markdown(result, PullRequestContext())
    assert "WARNING" in text
    assert "fallback reviewer set" in text


def test_markdown_flags_low_confidence(config: Config, sample_diff: str) -> None:
    result = build(config, sample_diff, **{"lambda-event-contract": NoulAnswer(0.9, 0.1)})
    assert "low confidence" in to_markdown(result, PullRequestContext())


def test_github_outputs(tmp_path: Path, config: Config, sample_diff: str) -> None:
    result = build(config, sample_diff, **{"lambda-event-contract": NoulAnswer(0.94, 0.88)})
    path = tmp_path / "gh-output"
    path.write_text("existing=1\n")
    write_github_outputs(path, result, "result.json")

    lines = dict(line.split("=", 1) for line in path.read_text().splitlines())
    assert lines["existing"] == "1"
    assert json.loads(lines["required_reviewers"]) == ["devops"]
    assert lines["required_reviewers_csv"] == "devops"
    assert lines["reviewer_count"] == "1"
    assert lines["degraded"] == "false"
    assert lines["result_path"] == "result.json"


def test_to_dict_is_json_serialisable(config: Config, sample_diff: str) -> None:
    payload = to_dict(build(config, sample_diff), PullRequestContext())
    json.dumps(payload)
