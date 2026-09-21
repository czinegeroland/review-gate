from __future__ import annotations

import json
from pathlib import Path

import pytest

from review_gate.cli import main

FIXTURES = Path(__file__).parent / "fixtures"


def run(args: list[str]) -> int:
    return main(args)


def test_evaluate_end_to_end(
    tmp_path: Path, config_file: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    out = tmp_path / "result.json"
    md = tmp_path / "summary.md"
    gh = tmp_path / "gh-output"
    code = run(
        [
            "evaluate",
            "--config", str(config_file),
            "--diff-file", str(FIXTURES / "sample.diff"),
            "--mock-answers", str(FIXTURES / "mock-answers.json"),
            "--output", str(out),
            "--markdown", str(md),
            "--github-output", str(gh),
            "--repo", "acme/api",
            "--pr-number", "42",
            "--pr-title", "Rename order id",
        ]
    )
    assert code == 0
    payload = json.loads(out.read_text())
    assert payload["required_reviewers"] == ["devops"]
    assert payload["pull_request"]["number"] == 42
    assert "Review Gate" in md.read_text()
    assert "required_reviewers_csv=devops" in gh.read_text()
    assert "required reviewers: devops" in capsys.readouterr().err


def test_evaluate_prints_json_to_stdout(
    config_file: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    code = run(
        [
            "evaluate",
            "--config", str(config_file),
            "--diff-file", str(FIXTURES / "sample.diff"),
            "--mock-answers", str(FIXTURES / "mock-answers.json"),
            "--quiet",
        ]
    )
    assert code == 0
    assert json.loads(capsys.readouterr().out)["schema_version"] == "1.0"


def test_mock_answers_via_env(
    monkeypatch: pytest.MonkeyPatch, config_file: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setenv("REVIEW_GATE_MOCK_ANSWERS", str(FIXTURES / "mock-answers.json"))
    assert run(
        ["evaluate", "--config", str(config_file), "--diff-file", str(FIXTURES / "sample.diff")]
    ) == 0
    assert json.loads(capsys.readouterr().out)["required_reviewers"] == ["devops"]


def test_fail_on_degraded(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, config_file: Path
) -> None:
    empty = tmp_path / "answers.json"
    empty.write_text("{}")
    diff = tmp_path / "big.diff"
    diff.write_text(
        "".join(f"diff --git a/s{i}.py b/s{i}.py\n@@ -1 +1 @@\n-a\n+b\n" for i in range(3))
    )
    base = [
        "evaluate",
        "--config", str(config_file),
        "--diff-file", str(diff),
        "--mock-answers", str(empty),
        "--quiet",
        "--fail-on-degraded",
        "--output", str(tmp_path / "result.json"),
    ]
    assert run(base) == 0

    # Cap the file count so the run is forced to degrade.
    capped = config_file.read_text().replace("defaults:", "defaults:\n  max_files: 1")
    config_file.write_text(capped)
    assert run(base) == 3
    payload = json.loads((tmp_path / "result.json").read_text())
    assert payload["degraded"] is True
    assert payload["required_reviewers"] == ["devops"]


def test_missing_api_key_is_a_clear_error(
    monkeypatch: pytest.MonkeyPatch, config_file: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.delenv("TYPESAFE_API_KEY", raising=False)
    monkeypatch.delenv("REVIEW_GATE_MOCK_ANSWERS", raising=False)
    code = run(
        ["evaluate", "--config", str(config_file), "--diff-file", str(FIXTURES / "sample.diff")]
    )
    assert code == 1
    assert "TYPESAFE_API_KEY" in capsys.readouterr().err


def test_invalid_config_exits_two(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    bad = tmp_path / "review-gate.yml"
    bad.write_text("version: 1\nrules: []\n")
    code = run(["validate", "--config", str(bad)])
    assert code == 2
    assert "invalid" in capsys.readouterr().err


def test_validate_ok(config_file: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert run(["validate", "--config", str(config_file)]) == 0
    assert "2 active rule(s)" in capsys.readouterr().out


def test_not_a_diff(tmp_path: Path, config_file: Path, capsys: pytest.CaptureFixture[str]) -> None:
    junk = tmp_path / "junk.txt"
    junk.write_text("hello world")
    code = run(["evaluate", "--config", str(config_file), "--diff-file", str(junk)])
    assert code == 1
    assert "unified diff" in capsys.readouterr().err
