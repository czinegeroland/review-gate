from __future__ import annotations

import textwrap
from pathlib import Path

import pytest

from review_gate.config import find_config, load_config
from review_gate.errors import ConfigError


def write(tmp_path: Path, body: str) -> Path:
    path = tmp_path / "review-gate.yml"
    path.write_text(textwrap.dedent(body))
    return path


def test_loads_and_normalises(config_file: Path) -> None:
    config = load_config(config_file)
    assert [rule.id for rule in config.rules] == ["lambda-event-contract", "test-removed"]
    assert config.reviewers["devops"].team == "acme/platform"
    assert config.effective_fallback == ["devops"]
    assert config.order_reviewers({"qa", "devops"}) == ["devops", "qa"]
    assert config.teams_for(["devops", "qa"]) == ["acme/platform"]


def test_fallback_defaults_to_all_reviewers(tmp_path: Path) -> None:
    path = write(
        tmp_path,
        """
        version: 1
        reviewers: {devops: {}, qa: {}}
        rules:
          - id: a
            question: Something meaningful happened in the diff.
            reviewers: [qa]
        """,
    )
    assert load_config(path).effective_fallback == ["devops", "qa"]


def test_unknown_reviewer_is_rejected(tmp_path: Path) -> None:
    path = write(
        tmp_path,
        """
        version: 1
        reviewers: {qa: {}}
        rules:
          - id: a
            question: Something meaningful happened in the diff.
            reviewers: [devops]
        """,
    )
    with pytest.raises(ConfigError, match="undeclared reviewer"):
        load_config(path)


def test_duplicate_rule_id_is_rejected(tmp_path: Path) -> None:
    path = write(
        tmp_path,
        """
        version: 1
        reviewers: {qa: {}}
        rules:
          - id: a
            question: Something meaningful happened in the diff.
            reviewers: [qa]
          - id: a
            question: Something else meaningful happened in the diff.
            reviewers: [qa]
        """,
    )
    with pytest.raises(ConfigError, match="duplicate rule id"):
        load_config(path)


@pytest.mark.parametrize(
    ("body", "message"),
    [
        ("version: 2\nreviewers: {qa: {}}\nrules: []\n", "version"),
        (
            "version: 1\nreviewers: {qa: {}}\nrules:\n  - id: a\n    question: short\n"
            "    reviewers: [qa]\n",
            "at least 10 characters",
        ),
        (
            "version: 1\nreviewers: {qa: {}}\nrules:\n  - id: a\n"
            "    question: Something meaningful happened in the diff.\n"
            "    reviewers: [qa]\n    threshold: 1.5\n",
            "threshold",
        ),
        (
            "version: 1\nreviewers: {qa: {}}\nrules:\n  - id: a\n"
            "    question: Something meaningful happened in the diff.\n"
            "    reviewers: [qa]\n    criteria: {maybe: x}\n",
            "criteria",
        ),
    ],
)
def test_invalid_configs(tmp_path: Path, body: str, message: str) -> None:
    path = tmp_path / "review-gate.yml"
    path.write_text(body)
    with pytest.raises(ConfigError, match=message):
        load_config(path)


def test_not_a_mapping(tmp_path: Path) -> None:
    path = tmp_path / "review-gate.yml"
    path.write_text("- just\n- a list\n")
    with pytest.raises(ConfigError, match="mapping"):
        load_config(path)


def test_rule_path_prefilter(config_file: Path) -> None:
    config = load_config(config_file)
    rule = config.rules[0]
    assert rule.matches("services/orders/handler.py")
    assert rule.matches("handler.py")  # '**/' also matches at the root
    assert not rule.matches("infra/stack.ts")
    assert config.rules[1].matches("anything.ts")  # no paths -> always applicable


def test_find_config_searches_defaults(tmp_path: Path) -> None:
    (tmp_path / ".github").mkdir()
    target = tmp_path / ".github" / "review-gate.yml"
    target.write_text("version: 1\n")
    assert find_config(None, tmp_path) == target
    with pytest.raises(ConfigError, match="no config file found"):
        find_config(None, tmp_path / "empty")
    with pytest.raises(ConfigError, match="not found"):
        find_config(tmp_path / "missing.yml")


def test_shipped_example_configs_are_valid() -> None:
    root = Path(__file__).resolve().parents[1]
    shipped = [root / ".github" / "review-gate.yml", root / "examples" / "review-gate.example.yml"]
    for path in shipped:
        assert load_config(path).active_rules()
