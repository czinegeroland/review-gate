"""Evaluation: diff + rules -> required reviewer types."""

from __future__ import annotations

import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from typing import Any

from .classifier import BatchResult, Classifier, NoulAnswer, NoulQuestion
from .config import Config, Rule
from .diff import Chunk, FileDiff, chunk_files
from .errors import ModelError


@dataclass
class PullRequestContext:
    """Optional metadata about the pull request, added to the model state."""

    repo: str | None = None
    number: int | None = None
    title: str | None = None
    body: str | None = None

    def as_state(self) -> dict[str, str]:
        state: dict[str, str] = {}
        if self.title:
            state["title"] = self.title
        if self.body:
            state["description"] = _clip(self.body, 4000)
        return state

    def as_output(self) -> dict[str, Any]:
        return {
            "repo": self.repo,
            "number": self.number,
            "title": self.title,
        }


@dataclass
class Decision:
    """The outcome for one rule."""

    rule: Rule
    fired: bool
    probability: float
    confidence: float
    threshold: float
    min_confidence: float
    low_confidence: bool
    files: list[str] = field(default_factory=list)


@dataclass
class Skipped:
    rule_id: str
    reason: str


@dataclass
class Stats:
    files: int = 0
    chunks: int = 0
    requests: int = 0
    duration_ms: int = 0
    input_tokens: int = 0
    output_tokens: int = 0


@dataclass
class Evaluation:
    """Everything the reporter needs to render a result."""

    required_reviewers: list[str]
    required_reviewer_teams: list[str]
    decisions: list[Decision]
    skipped: list[Skipped]
    stats: Stats
    model: str
    degraded: bool = False
    errors: list[str] = field(default_factory=list)


def build_question(rule: Rule) -> NoulQuestion:
    """Turn a configured rule into the Noul question sent to the model."""
    return NoulQuestion(
        id=rule.id,
        question=rule.question,
        focus=rule.focus,
        criteria=dict(rule.criteria) if rule.criteria else None,
    )


def evaluate(
    config: Config,
    files: list[FileDiff],
    classifier: Classifier,
    context: PullRequestContext | None = None,
) -> Evaluation:
    """Classify the diff and resolve the required reviewer set."""
    started = time.monotonic()
    context = context or PullRequestContext()

    considered = [f for f in files if not config.is_ignored(f.path) and not f.binary]
    truncated_files = False
    if len(considered) > config.defaults.max_files:
        considered = considered[: config.defaults.max_files]
        truncated_files = True

    rules = config.active_rules()
    skipped: list[Skipped] = [
        Skipped(rule.id, "disabled") for rule in config.rules if not rule.enabled
    ]

    applicable: list[Rule] = []
    for rule in rules:
        if any(rule.matches(file.path) for file in considered):
            applicable.append(rule)
        else:
            skipped.append(Skipped(rule.id, "no matching files"))

    stats = Stats(files=len(considered))

    if not considered or not applicable:
        stats.duration_ms = _elapsed_ms(started)
        reviewers = config.order_reviewers(set(config.always_reviewers))
        return Evaluation(
            required_reviewers=reviewers,
            required_reviewer_teams=config.teams_for(reviewers),
            decisions=[],
            skipped=skipped,
            stats=stats,
            model="",
        )

    chunks = chunk_files(considered, config.defaults.max_chunk_chars)
    stats.chunks = len(chunks)

    results, errors, model_name, usage = _run_chunks(
        chunks, applicable, classifier, config, context
    )
    stats.requests = len(results)
    stats.input_tokens, stats.output_tokens = usage

    degraded = bool(errors)
    if degraded and not results:
        stats.duration_ms = _elapsed_ms(started)
        reviewers = config.order_reviewers(
            set(config.effective_fallback) | set(config.always_reviewers)
        )
        return Evaluation(
            required_reviewers=reviewers,
            required_reviewer_teams=config.teams_for(reviewers),
            decisions=[],
            skipped=skipped,
            stats=stats,
            model=model_name,
            degraded=True,
            errors=errors,
        )

    decisions = _decide(applicable, results, config)
    required: set[str] = set(config.always_reviewers)
    for decision in decisions:
        if decision.fired:
            required.update(decision.rule.reviewers)
    if truncated_files:
        errors.append(
            f"diff has more than {config.defaults.max_files} files; only the first "
            f"{config.defaults.max_files} were classified"
        )
        degraded = True
    if degraded:
        required.update(config.effective_fallback)

    ordered = config.order_reviewers(required)
    stats.duration_ms = _elapsed_ms(started)
    return Evaluation(
        required_reviewers=ordered,
        required_reviewer_teams=config.teams_for(ordered),
        decisions=decisions,
        skipped=skipped,
        stats=stats,
        model=model_name,
        degraded=degraded,
        errors=errors,
    )


def _run_chunks(
    chunks: list[Chunk],
    rules: list[Rule],
    classifier: Classifier,
    config: Config,
    context: PullRequestContext,
) -> tuple[list[tuple[Chunk, list[Rule], BatchResult]], list[str], str, tuple[int, int]]:
    jobs: list[tuple[Chunk, list[Rule]]] = []
    for chunk in chunks:
        chunk_rules = [rule for rule in rules if any(rule.matches(path) for path in chunk.files)]
        if chunk_rules:
            jobs.append((chunk, chunk_rules))

    def run(job: tuple[Chunk, list[Rule]]) -> tuple[Chunk, list[Rule], BatchResult | str]:
        chunk, chunk_rules = job
        state = _state_for(chunk, context)
        questions = [build_question(rule) for rule in chunk_rules]
        try:
            return chunk, chunk_rules, classifier.ask(state, questions)
        except ModelError as exc:
            return chunk, chunk_rules, str(exc)

    workers = min(config.defaults.max_concurrency, max(1, len(jobs)))
    if workers > 1:
        with ThreadPoolExecutor(max_workers=workers) as pool:
            outcomes = list(pool.map(run, jobs))
    else:
        outcomes = [run(job) for job in jobs]

    results: list[tuple[Chunk, list[Rule], BatchResult]] = []
    errors: list[str] = []
    model_name = ""
    input_tokens = output_tokens = 0
    for chunk, chunk_rules, outcome in outcomes:
        if isinstance(outcome, str):
            errors.append(f"{', '.join(chunk.files)}: {outcome}")
            continue
        results.append((chunk, chunk_rules, outcome))
        model_name = model_name or outcome.model
        input_tokens += outcome.input_tokens
        output_tokens += outcome.output_tokens
    return results, errors, model_name, (input_tokens, output_tokens)


def _state_for(chunk: Chunk, context: PullRequestContext) -> dict[str, Any]:
    state: dict[str, Any] = {}
    pr_state = context.as_state()
    if pr_state:
        state["pull_request"] = pr_state
    state["changed_files"] = chunk.files
    state["diff"] = chunk.text
    return state


def _decide(
    rules: list[Rule],
    results: list[tuple[Chunk, list[Rule], BatchResult]],
    config: Config,
) -> list[Decision]:
    best: dict[str, tuple[NoulAnswer, list[str]]] = {}
    for chunk, chunk_rules, result in results:
        for rule in chunk_rules:
            answer = result.answers.get(rule.id)
            if answer is None:
                continue
            matched = [path for path in chunk.files if rule.matches(path)]
            current = best.get(rule.id)
            if current is None or answer.probability > current[0].probability:
                best[rule.id] = (answer, matched)
            elif answer.probability == current[0].probability:
                current[1].extend(path for path in matched if path not in current[1])

    decisions: list[Decision] = []
    for rule in rules:
        entry = best.get(rule.id)
        if entry is None:
            continue
        answer, files = entry
        threshold = rule.threshold if rule.threshold is not None else config.defaults.threshold
        floor = (
            rule.min_confidence
            if rule.min_confidence is not None
            else config.defaults.min_confidence
        )
        over_threshold = answer.probability >= threshold
        low_confidence = answer.confidence < floor
        if over_threshold and low_confidence:
            fired = config.defaults.on_low_confidence == "require"
        else:
            fired = over_threshold
        decisions.append(
            Decision(
                rule=rule,
                fired=fired,
                probability=round(answer.probability, 4),
                confidence=round(answer.confidence, 4),
                threshold=threshold,
                min_confidence=floor,
                low_confidence=over_threshold and low_confidence,
                files=sorted(files),
            )
        )
    decisions.sort(key=lambda d: (not d.fired, -d.probability, d.rule.id))
    return decisions


def _clip(text: str, limit: int) -> str:
    return text if len(text) <= limit else text[:limit] + "..."


def _elapsed_ms(started: float) -> int:
    return int((time.monotonic() - started) * 1000)
