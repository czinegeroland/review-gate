"""Clients that answer Noul questions about a diff chunk.

The production implementation talks to TypeSafe AI's System One HTTP API
(https://docs.typesafe.ai/api). It is deliberately a thin client behind the
``Classifier`` protocol so the official SDK — or a fixture — can take its place.
"""

from __future__ import annotations

import json
import os
import random
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Protocol

import httpx

from .errors import ModelError

DEFAULT_BASE_URL = "https://api.typesafe.ai"
SYSTEM_ONE_PATH = "/v1/systemone"
RETRYABLE_STATUS = frozenset({408, 409, 425, 429, 500, 502, 503, 504, 529})


@dataclass(frozen=True)
class NoulQuestion:
    """A yes/no question about the state, in TypeSafe's Noul shape."""

    id: str
    question: str
    focus: str | None = None
    criteria: dict[str, str] | None = None

    def payload(self) -> dict[str, Any]:
        instructions: dict[str, str] = {"question": self.question}
        if self.focus:
            instructions["focus"] = self.focus
        body: dict[str, Any] = {"type": "noul", "instructions": instructions}
        if self.criteria:
            body["criteria"] = dict(self.criteria)
        return body


@dataclass(frozen=True)
class NoulAnswer:
    """The model's answer: probability that the statement is true, plus confidence."""

    probability: float
    confidence: float


@dataclass
class BatchResult:
    """Answers for one request, with usage accounting."""

    answers: dict[str, NoulAnswer]
    model: str = ""
    input_tokens: int = 0
    output_tokens: int = 0


class Classifier(Protocol):
    """Answers a batch of Noul questions about one state."""

    def ask(self, state: dict[str, Any], questions: list[NoulQuestion]) -> BatchResult: ...

    def close(self) -> None: ...


@dataclass
class JevClient:
    """TypeSafe System One client."""

    api_key: str
    model: str = "jev-latest"
    base_url: str = DEFAULT_BASE_URL
    timeout: float = 60.0
    max_retries: int = 3
    _client: httpx.Client = field(init=False)

    def __post_init__(self) -> None:
        self._client = httpx.Client(
            base_url=self.base_url.rstrip("/"),
            timeout=self.timeout,
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
                "User-Agent": "review-gate",
            },
        )

    @classmethod
    def from_env(cls, model: str, timeout: float = 60.0) -> JevClient:
        api_key = os.environ.get("TYPESAFE_API_KEY", "").strip()
        if not api_key:
            raise ModelError("TYPESAFE_API_KEY is not set")
        return cls(
            api_key=api_key,
            model=model,
            base_url=os.environ.get("TYPESAFE_BASE_URL", DEFAULT_BASE_URL),
            timeout=timeout,
        )

    def ask(self, state: dict[str, Any], questions: list[NoulQuestion]) -> BatchResult:
        body = {
            "model": self.model,
            "state": state,
            "questions": {question.id: question.payload() for question in questions},
        }
        data = self._post(body)
        return _parse_response(data, questions)

    def _post(self, body: dict[str, Any]) -> dict[str, Any]:
        last_error: str = "unknown error"
        for attempt in range(self.max_retries + 1):
            try:
                response = self._client.post(SYSTEM_ONE_PATH, json=body)
            except httpx.HTTPError as exc:
                last_error = f"request failed: {exc}"
            else:
                if response.status_code < 400:
                    try:
                        parsed: dict[str, Any] = response.json()
                    except ValueError as exc:
                        raise ModelError(f"malformed JSON from TypeSafe: {exc}") from exc
                    return parsed
                last_error = _describe(response)
                if response.status_code not in RETRYABLE_STATUS:
                    raise ModelError(last_error)
                if response.status_code == 401:  # pragma: no cover - not retryable anyway
                    raise ModelError(last_error)
            if attempt == self.max_retries:
                break
            time.sleep(_backoff(attempt))
        raise ModelError(f"TypeSafe request failed after {self.max_retries + 1} attempts: "
                         f"{last_error}")

    def close(self) -> None:
        self._client.close()


def _backoff(attempt: int) -> float:
    jitter: float = 1.0 + random.random() * 0.2
    delay = min(8.0, 0.5 * float(2**attempt))
    return delay * jitter


def _describe(response: httpx.Response) -> str:
    detail = response.text.strip()
    if len(detail) > 300:
        detail = detail[:300] + "..."
    return f"HTTP {response.status_code} from TypeSafe: {detail}"


def _parse_response(data: dict[str, Any], questions: list[NoulQuestion]) -> BatchResult:
    raw_answers = data.get("answers")
    if not isinstance(raw_answers, dict):
        raise ModelError("TypeSafe response has no 'answers' object")

    answers: dict[str, NoulAnswer] = {}
    for question in questions:
        answer = raw_answers.get(question.id)
        if not isinstance(answer, dict) or "noul" not in answer:
            raise ModelError(f"no noul answer for question '{question.id}'")
        answers[question.id] = NoulAnswer(
            probability=_as_probability(answer["noul"], question.id, "noul"),
            confidence=_as_probability(answer.get("confidence", 1.0), question.id, "confidence"),
        )

    raw_usage = data.get("usage")
    usage: dict[str, Any] = raw_usage if isinstance(raw_usage, dict) else {}
    return BatchResult(
        answers=answers,
        model=str(data.get("model", "")),
        input_tokens=int(usage.get("input_tokens", 0) or 0),
        output_tokens=int(usage.get("output_tokens", 0) or 0),
    )


def _as_probability(value: Any, question_id: str, field_name: str) -> float:
    try:
        number = float(value)
    except (TypeError, ValueError) as exc:
        raise ModelError(f"'{field_name}' for '{question_id}' is not a number: {value!r}") from exc
    return min(1.0, max(0.0, number))


@dataclass
class MockClassifier:
    """Answers from a fixture file; used by tests and the CI end-to-end smoke run.

    The fixture maps rule id to either a probability or
    ``{"probability": 0.9, "confidence": 0.8}``. Rules absent from the fixture
    answer 0.0 with full confidence.
    """

    answers: dict[str, NoulAnswer]
    model: str = "mock"
    requests: int = 0

    @classmethod
    def from_file(cls, path: str | Path) -> MockClassifier:
        try:
            raw = json.loads(Path(path).read_text(encoding="utf-8"))
        except (OSError, ValueError) as exc:
            raise ModelError(f"cannot read mock answers from {path}: {exc}") from exc
        if not isinstance(raw, dict):
            raise ModelError(f"{path}: expected a JSON object of rule id -> answer")
        parsed: dict[str, NoulAnswer] = {}
        for rule_id, value in raw.items():
            if isinstance(value, dict):
                parsed[rule_id] = NoulAnswer(
                    probability=float(value.get("probability", 0.0)),
                    confidence=float(value.get("confidence", 1.0)),
                )
            else:
                parsed[rule_id] = NoulAnswer(probability=float(value), confidence=1.0)
        return cls(answers=parsed)

    def ask(self, state: dict[str, Any], questions: list[NoulQuestion]) -> BatchResult:
        self.requests += 1
        return BatchResult(
            answers={
                question.id: self.answers.get(question.id, NoulAnswer(0.0, 1.0))
                for question in questions
            },
            model=self.model,
        )

    def close(self) -> None:
        return None
