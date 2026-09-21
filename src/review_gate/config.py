"""Loading and validation of the Review Gate rule configuration."""

from __future__ import annotations

import fnmatch
from pathlib import Path
from typing import Annotated, Any, Literal

import yaml
from pydantic import BaseModel, ConfigDict, Field, ValidationError, field_validator, model_validator

from .errors import ConfigError

Probability = Annotated[float, Field(ge=0.0, le=1.0)]

DEFAULT_CONFIG_PATHS = (
    ".github/review-gate.yml",
    ".github/review-gate.yaml",
    "review-gate.yml",
    "review-gate.yaml",
)


class Reviewer(BaseModel):
    """A reviewer type that rules can require."""

    model_config = ConfigDict(extra="forbid")

    description: str = ""
    team: str | None = None


class Defaults(BaseModel):
    """Evaluation defaults, overridable per rule where noted."""

    model_config = ConfigDict(extra="forbid")

    threshold: Probability = 0.6
    min_confidence: Probability = 0.55
    on_low_confidence: Literal["require", "ignore"] = "require"
    model: str = "jev-latest"
    max_files: int = Field(default=200, ge=1)
    max_chunk_chars: int = Field(default=60_000, ge=1_000)
    max_concurrency: int = Field(default=4, ge=1, le=32)


class Rule(BaseModel):
    """One natural-language condition that, when true of the diff, requires reviewers."""

    model_config = ConfigDict(extra="forbid")

    id: str
    question: str
    reviewers: list[str] = Field(min_length=1)
    criteria: dict[str, str] | None = None
    focus: str | None = None
    paths: list[str] | None = None
    threshold: Probability | None = None
    min_confidence: Probability | None = None
    enabled: bool = True

    @field_validator("id")
    @classmethod
    def _id_is_slug(cls, value: str) -> str:
        cleaned = value.strip()
        if not cleaned:
            raise ValueError("rule id must not be empty")
        if any(ch.isspace() for ch in cleaned):
            raise ValueError("rule id must not contain whitespace")
        return cleaned

    @field_validator("question")
    @classmethod
    def _question_is_meaningful(cls, value: str) -> str:
        cleaned = " ".join(value.split())
        if len(cleaned) < 10:
            raise ValueError("question must be a sentence of at least 10 characters")
        return cleaned

    @field_validator("criteria")
    @classmethod
    def _criteria_keys(cls, value: dict[str, str] | None) -> dict[str, str] | None:
        if value is None:
            return None
        allowed = {"true", "false"}
        unknown = set(value) - allowed
        if unknown:
            raise ValueError(f"criteria keys must be 'true'/'false', got {sorted(unknown)}")
        return value

    def matches(self, path: str) -> bool:
        """Whether this rule's cheap path pre-filter accepts ``path``."""
        if not self.paths:
            return True
        return any(_glob_match(path, pattern) for pattern in self.paths)


class Config(BaseModel):
    """The whole ``review-gate.yml`` document."""

    model_config = ConfigDict(extra="forbid")

    version: Literal[1] = 1
    defaults: Defaults = Field(default_factory=Defaults)
    reviewers: dict[str, Reviewer] = Field(default_factory=dict)
    rules: list[Rule] = Field(min_length=1)
    always_reviewers: list[str] = Field(default_factory=list)
    fallback_reviewers: list[str] = Field(default_factory=list)
    ignore_paths: list[str] = Field(default_factory=list)

    @model_validator(mode="after")
    def _cross_checks(self) -> Config:
        seen: set[str] = set()
        for rule in self.rules:
            if rule.id in seen:
                raise ValueError(f"duplicate rule id: {rule.id}")
            seen.add(rule.id)

        known = set(self.reviewers)
        for rule in self.rules:
            unknown = [r for r in rule.reviewers if r not in known]
            if unknown:
                raise ValueError(
                    f"rule '{rule.id}' references undeclared reviewer(s) {unknown}; "
                    f"declare them under 'reviewers'"
                )
        for field in ("always_reviewers", "fallback_reviewers"):
            unknown = [r for r in getattr(self, field) if r not in known]
            if unknown:
                raise ValueError(f"{field} references undeclared reviewer(s) {unknown}")
        return self

    @property
    def effective_fallback(self) -> list[str]:
        """Reviewers required when the model cannot answer: explicit list, else everyone."""
        return self.fallback_reviewers or list(self.reviewers)

    def active_rules(self) -> list[Rule]:
        return [rule for rule in self.rules if rule.enabled]

    def is_ignored(self, path: str) -> bool:
        return any(_glob_match(path, pattern) for pattern in self.ignore_paths)

    def order_reviewers(self, names: set[str]) -> list[str]:
        """Return ``names`` in the order the reviewers are declared in the config."""
        return [name for name in self.reviewers if name in names]

    def teams_for(self, names: list[str]) -> list[str]:
        teams = [self.reviewers[name].team for name in names if self.reviewers[name].team]
        return [team for team in teams if team is not None]


def _glob_match(path: str, pattern: str) -> bool:
    """Glob match where ``**/`` also matches at the repository root."""
    if fnmatch.fnmatch(path, pattern):
        return True
    return pattern.startswith("**/") and fnmatch.fnmatch(path, pattern[3:])


def find_config(explicit: str | Path | None, root: Path | None = None) -> Path:
    """Resolve the config path, searching the default locations when not given."""
    if explicit is not None:
        path = Path(explicit)
        if not path.is_file():
            raise ConfigError(f"config file not found: {path}")
        return path
    base = root or Path.cwd()
    for candidate in DEFAULT_CONFIG_PATHS:
        path = base / candidate
        if path.is_file():
            return path
    raise ConfigError(
        "no config file found; looked for " + ", ".join(DEFAULT_CONFIG_PATHS) + " (use --config)"
    )


def load_config(path: str | Path) -> Config:
    """Parse and validate a configuration file."""
    path = Path(path)
    try:
        raw: Any = yaml.safe_load(path.read_text(encoding="utf-8"))
    except OSError as exc:  # pragma: no cover - surfaced verbatim
        raise ConfigError(f"cannot read {path}: {exc}") from exc
    except yaml.YAMLError as exc:
        raise ConfigError(f"{path} is not valid YAML: {exc}") from exc

    if not isinstance(raw, dict):
        raise ConfigError(f"{path}: expected a YAML mapping at the top level")

    try:
        return Config.model_validate(raw)
    except ValidationError as exc:
        raise ConfigError(f"{path} is invalid:\n{_format_errors(exc)}") from exc


def _format_errors(exc: ValidationError) -> str:
    lines = []
    for error in exc.errors():
        location = ".".join(str(part) for part in error["loc"]) or "<root>"
        lines.append(f"  - {location}: {error['msg']}")
    return "\n".join(lines)
