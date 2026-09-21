from __future__ import annotations

import textwrap
from pathlib import Path

import pytest

from review_gate.config import Config, load_config

FIXTURES = Path(__file__).parent / "fixtures"

BASIC_CONFIG = """
version: 1
defaults:
  threshold: 0.6
  min_confidence: 0.55
reviewers:
  devops:
    description: Infra
    team: acme/platform
  qa: {}
fallback_reviewers: [devops]
rules:
  - id: lambda-event-contract
    question: A parameter that a Lambda handler reads from its input event is removed or renamed.
    reviewers: [devops]
    paths: ["**/*.py"]
  - id: test-removed
    question: An existing test is deleted or skipped without a replacement.
    reviewers: [qa]
"""


@pytest.fixture()
def config_file(tmp_path: Path) -> Path:
    path = tmp_path / "review-gate.yml"
    path.write_text(textwrap.dedent(BASIC_CONFIG))
    return path


@pytest.fixture()
def config(config_file: Path) -> Config:
    return load_config(config_file)


@pytest.fixture()
def sample_diff() -> str:
    return (FIXTURES / "sample.diff").read_text()
