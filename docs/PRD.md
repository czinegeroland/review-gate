# PRD — Review Gate

**Status:** accepted · **Version:** 1.0 · **Owner:** @czinegeroland

## 1. Problem

Most repositories decide "who must review this pull request" from file paths in CI:

```yaml
if: contains(changed_files, 'infra/')  ->  require devops approval
```

Path rules cannot tell the difference between *renaming a local variable in a CDK
stack* and *deleting a field that a Lambda handler reads out of its input event*.
Both touch `infra/`, only one is risky. The result is that every trivial PR waits
on three or four approvals, reviewers get numb, and the signal in "devops must
approve" disappears.

What actually determines the required reviewer is a small set of semantic
conditions on the diff that people can write down in one sentence each:

> "A parameter that an AWS Lambda handler reads from its input event is removed or renamed."

Those conditions are cheap to state and expensive to detect — a reasoning LLM can
do it, but running one on every PR (and every file of every PR) is slow and
costly, and its free-text output has to be parsed and trusted.

## 2. Solution

**Review Gate** is a CLI, container image and GitHub Action that classifies a pull
request against a list of natural-language rules and emits a structured JSON
document naming the reviewer types the PR actually requires.

Classification runs on [TypeSafe AI's](https://docs.typesafe.ai/introduction)
System One model *Jev*. Jev answers typed questions — here **Noul** questions
(yes/no with a calibrated probability plus a confidence value) — instead of
generating prose. It is the right tool for this job because:

* the answer space is fixed, so the output is always a number we can threshold —
  no parsing, no hallucinated reviewer type;
* many questions are evaluated in parallel inside one request, so N rules cost
  roughly one request per diff chunk;
* it is orders of magnitude faster and cheaper than a reasoning model, which
  makes per-file fan-out affordable on every push.

Each rule maps to one or more reviewer types. A rule fires when its probability
clears a threshold; the reviewers of every fired rule form the required set.

## 3. Goals / Non-goals

**Goals**

1. Rules are written as plain English sentences in a YAML file in the repo,
   reviewed like code, with no path list required.
2. Deterministic, schema-stable JSON output listing required reviewer types.
3. Runs as a single step in a GitHub Actions workflow (container action) and
   identically on a laptop.
4. Fails *safe*: if the model is unreachable or unsure, the gate escalates to the
   configured fallback reviewers rather than silently approving.
5. Explainable: every decision carries the rule that fired, the probability, the
   confidence and the files that triggered it.

**Non-goals**

* Assigning specific people, or replacing CODEOWNERS ownership of files — Review
  Gate emits reviewer *types*; mapping to teams/users and enforcing approvals is
  the consuming workflow's job (we do emit optional team slugs to make that easy).
* Reviewing code quality or finding bugs.
* Auto-approving or merging pull requests.

## 4. Users and use cases

| User | Use case |
| --- | --- |
| Platform / DevOps team | Stop being pinged for cosmetic IaC changes; still be pinged when an interface, permission or runtime contract moves. |
| PR author | Gets one or zero required reviewer types on a trivial change instead of four. |
| Release manager | Machine-readable JSON that a branch-protection bot or a Slack notifier can consume. |

## 5. Functional requirements

### 5.1 Input

* A unified diff: from `--diff-file` (`-` = stdin), or computed locally from
  `--base`/`--head` git refs.
* A config file (default `.github/review-gate.yml`), see §5.2.
* `TYPESAFE_API_KEY` in the environment.
* Optional PR metadata (`--pr-title`, `--pr-body`, `--pr-number`, `--repo`) which
  is added to the model state as context and echoed into the output.

### 5.2 Configuration

```yaml
version: 1

defaults:
  threshold: 0.6           # Noul probability at/above which a rule fires
  min_confidence: 0.55     # below this the answer is treated as "unsure"
  on_low_confidence: require   # require | ignore
  max_files: 200
  max_chunk_chars: 60000

reviewers:
  devops:
    description: Infrastructure, deployment and runtime configuration.
    team: acme/platform     # optional, echoed into output
  qa: {}
  security: {}

fallback_reviewers: [devops, security]   # used when the model cannot answer

rules:
  - id: lambda-event-contract
    question: >-
      A parameter that an AWS Lambda handler reads from its input event is
      removed or renamed.
    reviewers: [devops]
    criteria:                 # optional, sharpens the Noul
      "true": Handler code stops reading a field, or reads it under a new name.
      "false": Only local variables, comments or formatting changed.
    paths: ["**/*.py", "**/*.ts"]   # optional cheap pre-filter
    threshold: 0.7                  # optional per-rule override
```

Validation errors (unknown reviewer key, duplicate rule id, threshold out of
range, empty question) are reported with the offending path and exit code 2.

### 5.3 Evaluation

1. Parse the diff into per-file diffs; drop files matching `ignore_paths`;
   apply each rule's `paths` pre-filter.
2. Group files into chunks under `max_chunk_chars` (a single oversized file is
   split by hunk; every chunk keeps its file header).
3. For each chunk, send **one** request to `POST https://api.typesafe.ai/v1/systemone`
   containing the chunk as `state` and every applicable rule as a Noul question.
   Chunks are sent concurrently with a bounded worker pool and retried with
   exponential backoff on 429/5xx.
4. A rule's result is the *maximum* probability across chunks (any chunk that
   trips the rule trips the rule); confidence is the one from the winning chunk.
5. A rule **fires** when `probability >= threshold` and `confidence >= min_confidence`.
   When `probability >= threshold` but confidence is below the floor, behaviour is
   `on_low_confidence`: `require` (fire, flagged `low_confidence`) or `ignore`.
6. Required reviewers = union of the reviewers of all fired rules, plus
   `always_reviewers`, in config order.

### 5.4 Output

Stable JSON (`schema_version: "1.0"`) on stdout or `--output FILE`:

```json
{
  "schema_version": "1.0",
  "generated_at": "2026-09-21T10:00:00Z",
  "model": "jev-1.13.0",
  "degraded": false,
  "pull_request": {"repo": "acme/api", "number": 42, "title": "..."},
  "required_reviewers": ["devops"],
  "required_reviewer_teams": ["acme/platform"],
  "decisions": [
    {"rule_id": "lambda-event-contract", "fired": true, "probability": 0.93,
     "confidence": 0.88, "low_confidence": false, "reviewers": ["devops"],
     "files": ["services/orders/handler.py"], "question": "..."}
  ],
  "skipped_rules": [{"rule_id": "db-migration", "reason": "no matching files"}],
  "stats": {"files": 7, "chunks": 2, "requests": 2, "duration_ms": 412,
            "input_tokens": 9130, "output_tokens": 44}
}
```

Also produced: a Markdown summary (`--markdown FILE`, used for the job summary and
optional PR comment) and GitHub Action outputs (`--github-output`):
`required_reviewers` (JSON array), `required_reviewers_csv`, `reviewer_count`,
`degraded`, `result_path`.

Exit codes: `0` evaluation completed · `1` unexpected error · `2` invalid config or
usage · `3` degraded and `--fail-on-degraded` was set.

### 5.5 Failure behaviour

Any API failure that survives retries marks the run `degraded: true`, sets
`required_reviewers` to `fallback_reviewers` (default: all configured reviewers)
and still exits `0` so the workflow decides. The gate never returns an empty
reviewer set on failure.

## 6. Non-functional requirements

* p95 wall clock < 10 s for a 50-file PR with 20 rules (chunks run concurrently).
* No diff content is written to logs at default verbosity.
* Rust, shipped as one static-ish binary (`clap`, `serde`, `serde_yaml`,
  `globset`, `ureq`); no TypeSafe SDK dependency — the documented HTTP contract
  is small and this keeps the tool installable while Jev is in early access. The
  client sits behind the `Classifier` trait so the official SDK can be swapped
  in. Chunks are classified on a bounded pool of OS threads.
* Minimum supported Rust version 1.88; the container image is a multi-stage
  build on `rust:1-slim-bookworm` and `debian:bookworm-slim`, and doubles as the
  GitHub container action.

## 7. Testing & CI

* Tests for config validation, diff parsing/chunking, threshold and aggregation
  logic, report rendering and CLI exit codes, with the model behind a fake. The
  HTTP client itself is tested against a local `TcpListener` — request shape,
  retries, error mapping — so no test reaches the network.
* `REVIEW_GATE_MOCK_ANSWERS=<file.json>` makes the CLI answer from a fixture
  instead of calling the API, so CI can run a full end-to-end evaluation on a
  sample diff without a key.
* CI (GitHub Actions) runs: `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo test`, an MSRV check, the end-to-end mock run, and the container action
  against a sample diff plus a plain-CLI image smoke test.

## 8. Rollout

1. Land CLI + action + CI (this PR).
2. Adopt in shadow mode: workflow posts the suggested reviewer set as a comment
   but branch protection is unchanged; compare against what humans did.
3. Flip the path-based approval rules over to Review Gate's output, keeping
   `fallback_reviewers` conservative.

## 9. Open questions

* Team-slug → GitHub review-request automation is deliberately left to the
  consuming workflow for v1; a `--request-reviewers` mode is a candidate for v2.
* Per-rule self-consistency (asking the same Noul twice and comparing) is a known
  TypeSafe pattern for borderline cases; deferred until we have real data on how
  often confidence lands in the grey band.
