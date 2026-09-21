# Review Gate

Decide **which kind of reviewer a pull request actually needs** from the diff
itself, not from a folder name.

Most repos approximate this with path rules in CI — *"someone touched `infra/`,
so require a DevOps approval"*. That cannot tell a renamed local variable in a
CDK stack apart from a deleted field that a Lambda handler reads out of its
event. So every trivial PR waits on three or four approvals, and the approvals
stop meaning anything.

Review Gate lets you write the conditions that *actually* require a specialist as
plain English sentences:

```yaml
- id: lambda-event-contract
  question: >-
    A parameter that an AWS Lambda handler reads from its input event is removed
    or renamed.
  reviewers: [devops, backend]
```

and answers them against the diff with [TypeSafe AI's](https://docs.typesafe.ai/introduction)
System One model **Jev**, which returns a typed yes/no probability plus a
confidence instead of prose. Output is a structured JSON document listing the
required reviewer types — fast and cheap enough to run on every push, and
impossible to get an unparseable or invented answer out of.

```json
{
  "required_reviewers": ["devops"],
  "required_reviewer_teams": ["acme/platform"],
  "decisions": [
    {
      "rule_id": "lambda-event-contract",
      "fired": true,
      "probability": 0.94,
      "confidence": 0.88,
      "reviewers": ["devops"],
      "files": ["services/orders/handler.py"]
    }
  ]
}
```

See [`docs/PRD.md`](docs/PRD.md) for the full product spec and
[`examples/review-gate.example.yml`](examples/review-gate.example.yml) for a
ready-made rule set.

## Use as a GitHub Action

```yaml
name: review-gate
on:
  pull_request:

permissions:
  contents: read
  pull-requests: write

jobs:
  reviewers:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - id: gate
        uses: czinegeroland/review-gate@v0
        with:
          config: .github/review-gate.yml
          base: ${{ github.event.pull_request.base.sha }}
          head: ${{ github.event.pull_request.head.sha }}
          typesafe-api-key: ${{ secrets.TYPESAFE_API_KEY }}
          comment: 'true'          # post/update a summary comment on the PR
          github-token: ${{ secrets.GITHUB_TOKEN }}

      - run: echo "Required reviewers: ${{ steps.gate.outputs.required_reviewers_csv }}"
```

### Action inputs

| Input | Default | Description |
| --- | --- | --- |
| `config` | `.github/review-gate.yml` | Rule configuration. |
| `base` / `head` | PR base/head | Refs to diff. Ignored when `diff-file` is set. |
| `diff-file` | – | Use a pre-computed unified diff instead of git. |
| `typesafe-api-key` | – | **Required** unless `mock-answers` is set. |
| `model` | from config | Override the model id (e.g. `jev-1.13.0`). |
| `fail-on-degraded` | `false` | Fail the step when classification was incomplete. |
| `comment` | `false` | Post/update a summary comment on the pull request. |
| `github-token` | – | Needed when `comment` is `true`. |
| `mock-answers` | – | JSON fixture of answers; for testing without a key. |
| `output` | `review-gate-result.json` | Where the JSON result is written. |

### Action outputs

| Output | Example |
| --- | --- |
| `required_reviewers` | `["devops","security"]` |
| `required_reviewers_csv` | `devops,security` |
| `required_reviewer_teams` | `["acme/platform"]` |
| `reviewer_count` | `2` |
| `degraded` | `false` |
| `result_path` | `review-gate-result.json` |
| `markdown_path` | `review-gate-summary.md` |

Gate a job on the result:

```yaml
  devops-approval:
    needs: reviewers
    if: contains(fromJSON(needs.reviewers.outputs.required_reviewers), 'devops')
    runs-on: ubuntu-latest
    steps:
      - run: echo "This PR needs a platform review."
```

## Use locally

```bash
pip install review-gate           # or: uv tool install review-gate
export TYPESAFE_API_KEY=...       # https://console.typesafe.ai/

review-gate validate --config .github/review-gate.yml
review-gate evaluate --base origin/main --head HEAD
git diff origin/main | review-gate evaluate --diff-file -
```

Or with the container image:

```bash
docker run --rm -e TYPESAFE_API_KEY -v "$PWD:/src" -w /src \
  ghcr.io/czinegeroland/review-gate:latest \
  evaluate --base origin/main --head HEAD
```

## Configuration

```yaml
version: 1

defaults:
  threshold: 0.6          # Noul probability at/above which a rule fires
  min_confidence: 0.55    # answers below this are treated as unsure
  on_low_confidence: require   # require | ignore
  model: jev-latest
  max_files: 200
  max_chunk_chars: 60000  # diff characters per model request
  max_concurrency: 4

reviewers:
  devops:
    description: Infrastructure, deployment and runtime configuration.
    team: acme/platform   # optional, echoed as required_reviewer_teams
  qa: {}

always_reviewers: []            # always required, regardless of the diff
fallback_reviewers: [devops]    # required when the model cannot answer
ignore_paths: ["**/*.lock"]

rules:
  - id: iam-permission-widened
    question: >-
      An IAM policy, role or resource grant becomes broader — more actions, more
      resources, or a wildcard where there was none.
    reviewers: [devops, security]
    criteria:                    # optional, sharpens the yes/no boundary
      "true": A statement gains actions or resources, or a wildcard appears.
      "false": Only formatting, comments or local names change.
    paths: ["infra/**"]          # optional cheap pre-filter (tokens, not policy)
    threshold: 0.5               # optional per-rule override
    enabled: true
```

**Writing good rules**

* One condition per rule, phrased as a statement that is either true or false of
  the diff — the model answers *how likely is this statement true*.
* Say what *false* looks like in `criteria`; borderline cases are where the
  probability (and your reviewers' time) is won or lost.
* Use `paths` only to save tokens, never as the actual policy — the whole point is
  that the decision is semantic.
* Start with `threshold: 0.5` for high-stakes rules (security, IAM, data loss) and
  `0.7` for noisy ones, then tune from the probabilities in the JSON output.

## How it works

1. The diff is parsed per file; binary and `ignore_paths` files are dropped.
2. Files are packed into chunks of `max_chunk_chars`; a huge file is split by hunk.
3. Each chunk goes to `POST https://api.typesafe.ai/v1/systemone` as the `state`,
   with **every applicable rule as a Noul question in the same request** — Jev
   evaluates them in parallel, so 20 rules cost about as much as one.
4. A rule's probability is the maximum across chunks; it fires at
   `probability >= threshold` when `confidence >= min_confidence`.
   Over threshold but under-confident is handled by `on_low_confidence`.
5. The union of the fired rules' reviewers (plus `always_reviewers`) is the result.

**Failing safe.** If the API is unreachable, a chunk fails, or the diff is bigger
than `max_files`, the run is marked `degraded`, `fallback_reviewers` are added to
the required set, and the errors are listed in the JSON. Review Gate never
returns an empty reviewer set because something went wrong.

Exit codes: `0` completed · `1` unexpected error · `2` invalid config or usage ·
`3` degraded with `--fail-on-degraded`.

## Development

```bash
uv venv && . .venv/bin/activate
uv pip install -e ".[dev]"
ruff check . && mypy && pytest
```

Tests never touch the network: the HTTP client is exercised through
`httpx.MockTransport`, and the CLI can answer from a fixture with
`--mock-answers` / `REVIEW_GATE_MOCK_ANSWERS`.

## Licence

MIT.
