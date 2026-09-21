from __future__ import annotations

import json
from pathlib import Path

import httpx
import pytest

from review_gate.classifier import JevClient, MockClassifier, NoulQuestion
from review_gate.errors import ModelError

QUESTIONS = [
    NoulQuestion(id="r1", question="Is this risky?", criteria={"true": "yes", "false": "no"}),
    NoulQuestion(id="r2", question="Is this cosmetic?", focus="the diff body"),
]


def client_with(handler: httpx.MockTransport, **kwargs: object) -> JevClient:
    client = JevClient(api_key="k", **kwargs)  # type: ignore[arg-type]
    client._client = httpx.Client(transport=handler, base_url="https://api.typesafe.ai")
    return client


def test_request_shape_and_parsing() -> None:
    seen: dict[str, object] = {}

    def handle(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        seen["body"] = json.loads(request.content)
        return httpx.Response(
            200,
            json={
                "model": "jev-1.13.0",
                "answers": {
                    "r1": {"type": "noul", "noul": 0.91, "confidence": 0.8},
                    "r2": {"type": "noul", "noul": 0.02, "confidence": 0.97},
                },
                "usage": {"input_tokens": 300, "output_tokens": 20},
            },
        )

    client = client_with(httpx.MockTransport(handle), model="jev-latest")
    result = client.ask({"diff": "..."}, QUESTIONS)

    assert seen["url"] == "https://api.typesafe.ai/v1/systemone"
    body = seen["body"]
    assert isinstance(body, dict)
    assert body["model"] == "jev-latest"
    assert body["state"] == {"diff": "..."}
    assert body["questions"]["r1"] == {
        "type": "noul",
        "instructions": {"question": "Is this risky?"},
        "criteria": {"true": "yes", "false": "no"},
    }
    assert body["questions"]["r2"]["instructions"]["focus"] == "the diff body"
    assert "criteria" not in body["questions"]["r2"]

    assert result.answers["r1"].probability == 0.91
    assert result.answers["r1"].confidence == 0.8
    assert result.model == "jev-1.13.0"
    assert (result.input_tokens, result.output_tokens) == (300, 20)
    client.close()


def test_retries_then_succeeds(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("review_gate.classifier.time.sleep", lambda _: None)
    attempts = {"n": 0}

    def handle(request: httpx.Request) -> httpx.Response:
        attempts["n"] += 1
        if attempts["n"] < 3:
            return httpx.Response(429, text="slow down")
        return httpx.Response(200, json={"answers": {"r1": {"noul": 0.5}}})

    client = client_with(httpx.MockTransport(handle))
    result = client.ask({}, [QUESTIONS[0]])
    assert attempts["n"] == 3
    assert result.answers["r1"].confidence == 1.0


def test_gives_up_after_retries(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("review_gate.classifier.time.sleep", lambda _: None)
    client = client_with(httpx.MockTransport(lambda _: httpx.Response(503, text="down")))
    with pytest.raises(ModelError, match="after 4 attempts"):
        client.ask({}, [QUESTIONS[0]])


def test_client_errors_are_not_retried() -> None:
    calls = {"n": 0}

    def handle(request: httpx.Request) -> httpx.Response:
        calls["n"] += 1
        return httpx.Response(401, text="bad key")

    client = client_with(httpx.MockTransport(handle))
    with pytest.raises(ModelError, match="HTTP 401"):
        client.ask({}, [QUESTIONS[0]])
    assert calls["n"] == 1


def test_missing_answer_is_an_error() -> None:
    client = client_with(httpx.MockTransport(lambda _: httpx.Response(200, json={"answers": {}})))
    with pytest.raises(ModelError, match="no noul answer"):
        client.ask({}, [QUESTIONS[0]])


def test_malformed_payloads() -> None:
    client = client_with(httpx.MockTransport(lambda _: httpx.Response(200, json={"oops": 1})))
    with pytest.raises(ModelError, match="no 'answers'"):
        client.ask({}, [QUESTIONS[0]])

    client = client_with(
        httpx.MockTransport(lambda _: httpx.Response(200, json={"answers": {"r1": {"noul": "x"}}}))
    )
    with pytest.raises(ModelError, match="not a number"):
        client.ask({}, [QUESTIONS[0]])


def test_probabilities_are_clamped() -> None:
    client = client_with(
        httpx.MockTransport(
            lambda _: httpx.Response(200, json={"answers": {"r1": {"noul": 1.4, "confidence": -1}}})
        )
    )
    answer = client.ask({}, [QUESTIONS[0]]).answers["r1"]
    assert (answer.probability, answer.confidence) == (1.0, 0.0)


def test_from_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("TYPESAFE_API_KEY", raising=False)
    with pytest.raises(ModelError, match="TYPESAFE_API_KEY"):
        JevClient.from_env(model="jev-latest")
    monkeypatch.setenv("TYPESAFE_API_KEY", "secret")
    monkeypatch.setenv("TYPESAFE_BASE_URL", "https://example.test")
    client = JevClient.from_env(model="jev-latest")
    assert str(client._client.base_url) == "https://example.test"
    client.close()


def test_mock_classifier(tmp_path: Path) -> None:
    path = tmp_path / "answers.json"
    path.write_text(json.dumps({"r1": 0.8, "r2": {"probability": 0.1, "confidence": 0.4}}))
    mock = MockClassifier.from_file(path)
    result = mock.ask({}, [*QUESTIONS, NoulQuestion(id="r3", question="Unknown question?")])
    assert result.answers["r1"].probability == 0.8
    assert result.answers["r2"].confidence == 0.4
    assert result.answers["r3"].probability == 0.0
    mock.close()

    with pytest.raises(ModelError, match="cannot read mock answers"):
        MockClassifier.from_file(tmp_path / "missing.json")
