from __future__ import annotations

from typing import Any

import pytest

from review_gate.classifier import BatchResult, NoulAnswer, NoulQuestion
from review_gate.config import Config, load_config
from review_gate.diff import parse_diff
from review_gate.engine import PullRequestContext, build_question, evaluate
from review_gate.errors import ModelError


class FakeClassifier:
    """Returns canned answers and records what it was asked."""

    def __init__(self, answers: dict[str, NoulAnswer], fail: bool = False) -> None:
        self.answers = answers
        self.fail = fail
        self.calls: list[tuple[dict[str, Any], list[NoulQuestion]]] = []
        self.closed = False

    def ask(self, state: dict[str, Any], questions: list[NoulQuestion]) -> BatchResult:
        self.calls.append((state, questions))
        if self.fail:
            raise ModelError("boom")
        return BatchResult(
            answers={q.id: self.answers.get(q.id, NoulAnswer(0.0, 1.0)) for q in questions},
            model="jev-1.13.0",
            input_tokens=100,
            output_tokens=5,
        )

    def close(self) -> None:
        self.closed = True


def test_fires_only_the_matching_rule(config: Config, sample_diff: str) -> None:
    client = FakeClassifier(
        {"lambda-event-contract": NoulAnswer(0.94, 0.88), "test-removed": NoulAnswer(0.04, 0.99)}
    )
    result = evaluate(config, parse_diff(sample_diff), client)

    assert result.required_reviewers == ["devops"]
    assert result.required_reviewer_teams == ["acme/platform"]
    assert result.degraded is False
    fired = [d for d in result.decisions if d.fired]
    assert [d.rule.id for d in fired] == ["lambda-event-contract"]
    assert fired[0].files == ["services/orders/handler.py"]
    assert result.stats.input_tokens == 100


def test_binary_and_ignored_files_are_dropped(config: Config, sample_diff: str) -> None:
    config.ignore_paths = ["infra/**"]
    client = FakeClassifier({})
    result = evaluate(config, parse_diff(sample_diff), client)
    assert result.stats.files == 1
    state, _ = client.calls[0]
    assert state["changed_files"] == ["services/orders/handler.py"]


def test_rule_without_matching_files_is_skipped(config: Config) -> None:
    files = parse_diff("diff --git a/infra/stack.ts b/infra/stack.ts\n@@ -1 +1 @@\n-a\n+b\n")
    client = FakeClassifier({"test-removed": NoulAnswer(0.9, 0.9)})
    result = evaluate(config, files, client)

    assert [s.rule_id for s in result.skipped] == ["lambda-event-contract"]
    assert result.required_reviewers == ["qa"]
    asked = [q.id for q in client.calls[0][1]]
    assert asked == ["test-removed"]


def test_probability_below_threshold_does_not_fire(config: Config, sample_diff: str) -> None:
    client = FakeClassifier({"lambda-event-contract": NoulAnswer(0.59, 0.99)})
    result = evaluate(config, parse_diff(sample_diff), client)
    assert result.required_reviewers == []


def test_low_confidence_requires_by_default(config: Config, sample_diff: str) -> None:
    client = FakeClassifier({"lambda-event-contract": NoulAnswer(0.8, 0.2)})
    result = evaluate(config, parse_diff(sample_diff), client)
    decision = next(d for d in result.decisions if d.rule.id == "lambda-event-contract")
    assert decision.fired is True
    assert decision.low_confidence is True
    assert result.required_reviewers == ["devops"]


def test_low_confidence_can_be_ignored(config: Config, sample_diff: str) -> None:
    config.defaults.on_low_confidence = "ignore"
    client = FakeClassifier({"lambda-event-contract": NoulAnswer(0.8, 0.2)})
    result = evaluate(config, parse_diff(sample_diff), client)
    assert result.required_reviewers == []


def test_per_rule_threshold_overrides_default(config: Config, sample_diff: str) -> None:
    config.rules[0].threshold = 0.95
    client = FakeClassifier({"lambda-event-contract": NoulAnswer(0.94, 0.99)})
    result = evaluate(config, parse_diff(sample_diff), client)
    assert result.required_reviewers == []


def test_always_reviewers_are_always_required(config: Config, sample_diff: str) -> None:
    config.always_reviewers = ["qa"]
    client = FakeClassifier({})
    result = evaluate(config, parse_diff(sample_diff), client)
    assert result.required_reviewers == ["qa"]


def test_model_failure_falls_back_and_never_returns_empty(config: Config, sample_diff: str) -> None:
    result = evaluate(config, parse_diff(sample_diff), FakeClassifier({}, fail=True))
    assert result.degraded is True
    assert result.required_reviewers == ["devops"]
    assert result.errors


def test_partial_failure_keeps_answers_and_adds_fallback(config: Config) -> None:
    diff = "".join(
        f"diff --git a/s{i}.py b/s{i}.py\n@@ -1 +1 @@\n-{'x' * 200}\n+{'y' * 200}\n"
        for i in range(3)
    )
    config.defaults.max_chunk_chars = 1_000
    config.defaults.max_concurrency = 1

    class Flaky(FakeClassifier):
        def ask(self, state: dict[str, Any], questions: list[NoulQuestion]) -> BatchResult:
            if len(self.calls) == 1:
                self.calls.append((state, questions))
                raise ModelError("transient")
            return super().ask(state, questions)

    client = Flaky({"lambda-event-contract": NoulAnswer(0.99, 0.99)})
    result = evaluate(config, parse_diff(diff), client)
    assert result.degraded is True
    assert result.required_reviewers == ["devops"]
    assert any(d.fired for d in result.decisions)


def test_too_many_files_degrades(config: Config) -> None:
    config.defaults.max_files = 1
    diff = "".join(
        f"diff --git a/s{i}.py b/s{i}.py\n@@ -1 +1 @@\n-a\n+b\n" for i in range(3)
    )
    result = evaluate(config, parse_diff(diff), FakeClassifier({}))
    assert result.degraded is True
    assert "more than 1 files" in " ".join(result.errors)


def test_empty_diff_asks_nothing(config: Config) -> None:
    client = FakeClassifier({})
    result = evaluate(config, [], client)
    assert result.required_reviewers == []
    assert client.calls == []


def test_disabled_rule_is_skipped(config: Config, sample_diff: str) -> None:
    config.rules[0].enabled = False
    client = FakeClassifier({"lambda-event-contract": NoulAnswer(1.0, 1.0)})
    result = evaluate(config, parse_diff(sample_diff), client)
    assert ("lambda-event-contract", "disabled") in [(s.rule_id, s.reason) for s in result.skipped]
    assert result.required_reviewers == []


def test_state_includes_pr_context(config: Config, sample_diff: str) -> None:
    client = FakeClassifier({})
    evaluate(
        config,
        parse_diff(sample_diff),
        client,
        PullRequestContext(repo="acme/api", number=7, title="Rename order id", body="b" * 5000),
    )
    state, _ = client.calls[0]
    assert state["pull_request"]["title"] == "Rename order id"
    assert state["pull_request"]["description"].endswith("...")
    assert "diff" in state


def test_build_question_payload() -> None:
    config = load_config("examples/review-gate.example.yml")
    rule = next(r for r in config.rules if r.id == "lambda-event-contract")
    payload = build_question(rule).payload()
    assert payload["type"] == "noul"
    assert payload["instructions"]["question"].startswith("A parameter")
    assert set(payload["criteria"]) == {"true", "false"}


@pytest.mark.parametrize("concurrency", [1, 4])
def test_chunks_are_all_classified(config: Config, concurrency: int) -> None:
    config.defaults.max_chunk_chars = 1_000
    config.defaults.max_concurrency = concurrency
    diff = "".join(
        f"diff --git a/s{i}.py b/s{i}.py\n@@ -1 +1 @@\n-{'x' * 400}\n+{'y' * 400}\n"
        for i in range(4)
    )
    client = FakeClassifier({})
    result = evaluate(config, parse_diff(diff), client)
    assert result.stats.chunks == len(client.calls) == result.stats.requests
    assert result.stats.chunks > 1
