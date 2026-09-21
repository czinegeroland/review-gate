"""Command line interface."""

from __future__ import annotations

import argparse
import os
import sys
from collections.abc import Sequence
from pathlib import Path

from . import __version__
from .classifier import Classifier, JevClient, MockClassifier
from .config import Config, find_config, load_config
from .diff import parse_diff, read_diff
from .engine import PullRequestContext, evaluate
from .errors import ConfigError, DiffError, ModelError, ReviewGateError
from .report import to_json, to_markdown, write_github_outputs

EXIT_OK = 0
EXIT_ERROR = 1
EXIT_USAGE = 2
EXIT_DEGRADED = 3


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="review-gate",
        description="Classify a pull request diff into the reviewer types it actually needs.",
    )
    parser.add_argument("--version", action="version", version=f"review-gate {__version__}")
    subparsers = parser.add_subparsers(dest="command", required=True)

    evaluate_parser = subparsers.add_parser(
        "evaluate", help="classify a diff and emit the required reviewers"
    )
    source = evaluate_parser.add_argument_group("diff source")
    source.add_argument("--diff-file", help="unified diff file, or '-' for stdin")
    source.add_argument("--base", help="base git ref (used when --diff-file is not given)")
    source.add_argument("--head", help="head git ref, defaults to the working tree")
    source.add_argument(
        "--repo-dir", default=".", help="repository directory for git operations (default: .)"
    )

    evaluate_parser.add_argument("--config", help="path to review-gate.yml")
    evaluate_parser.add_argument("--output", help="write the JSON result here (default: stdout)")
    evaluate_parser.add_argument("--markdown", help="also write a Markdown summary here")
    evaluate_parser.add_argument(
        "--github-output",
        nargs="?",
        const=os.environ.get("GITHUB_OUTPUT", ""),
        help="append action outputs to this file (defaults to $GITHUB_OUTPUT)",
    )
    evaluate_parser.add_argument("--repo", help="owner/name, recorded in the result")
    evaluate_parser.add_argument("--pr-number", type=int, help="pull request number")
    evaluate_parser.add_argument("--pr-title", help="pull request title, given to the model")
    evaluate_parser.add_argument("--pr-body", help="pull request description, given to the model")
    evaluate_parser.add_argument("--model", help="override the model id from the config")
    evaluate_parser.add_argument(
        "--timeout", type=float, default=60.0, help="per-request timeout in seconds"
    )
    evaluate_parser.add_argument(
        "--mock-answers",
        help="answer from this JSON fixture instead of calling the API (testing)",
    )
    evaluate_parser.add_argument(
        "--fail-on-degraded",
        action="store_true",
        help=f"exit {EXIT_DEGRADED} when classification was incomplete",
    )
    evaluate_parser.add_argument("--quiet", action="store_true", help="suppress progress on stderr")

    validate_parser = subparsers.add_parser("validate", help="check a rule configuration")
    validate_parser.add_argument("--config", help="path to review-gate.yml")

    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if args.command == "validate":
            return _run_validate(args)
        return _run_evaluate(args)
    except ConfigError as exc:
        print(f"review-gate: {exc}", file=sys.stderr)
        return EXIT_USAGE
    except ReviewGateError as exc:
        print(f"review-gate: {exc}", file=sys.stderr)
        return EXIT_ERROR


def _run_validate(args: argparse.Namespace) -> int:
    path = find_config(args.config)
    config = load_config(path)
    rules = config.active_rules()
    print(
        f"{path}: OK — {len(rules)} active rule(s), "
        f"{len(config.reviewers)} reviewer type(s), "
        f"fallback: {', '.join(config.effective_fallback) or 'none'}"
    )
    return EXIT_OK


def _run_evaluate(args: argparse.Namespace) -> int:
    config_path = find_config(args.config)
    config = load_config(config_path)
    if args.model:
        config.defaults.model = args.model

    diff_text = read_diff(args.diff_file, args.base, args.head, Path(args.repo_dir))
    files = parse_diff(diff_text)
    if not files and diff_text.strip():
        raise DiffError("the input does not look like a unified diff")

    context = PullRequestContext(
        repo=args.repo,
        number=args.pr_number,
        title=args.pr_title,
        body=args.pr_body,
    )

    classifier = _build_classifier(args, config)
    try:
        evaluation = evaluate(config, files, classifier, context)
    finally:
        classifier.close()

    payload = to_json(evaluation, context)
    if args.output:
        Path(args.output).write_text(payload + "\n", encoding="utf-8")
    else:
        print(payload)

    if args.markdown:
        Path(args.markdown).write_text(to_markdown(evaluation, context), encoding="utf-8")

    if args.github_output:
        write_github_outputs(args.github_output, evaluation, args.output)

    if not args.quiet:
        summary = ", ".join(evaluation.required_reviewers) or "none"
        print(f"review-gate: required reviewers: {summary}", file=sys.stderr)
        for error in evaluation.errors:
            print(f"review-gate: warning: {error}", file=sys.stderr)

    if evaluation.degraded and args.fail_on_degraded:
        return EXIT_DEGRADED
    return EXIT_OK


def _build_classifier(args: argparse.Namespace, config: Config) -> Classifier:
    mock = args.mock_answers or os.environ.get("REVIEW_GATE_MOCK_ANSWERS")
    if mock:
        return MockClassifier.from_file(mock)
    try:
        return JevClient.from_env(model=config.defaults.model, timeout=args.timeout)
    except ModelError as exc:
        raise ModelError(
            f"{exc}; set it to a key from https://console.typesafe.ai/ "
            f"or pass --mock-answers for a dry run"
        ) from exc


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
