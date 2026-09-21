#!/usr/bin/env bash
# Container action entrypoint: turns action inputs into a review-gate call.
set -euo pipefail

workspace="${GITHUB_WORKSPACE:-$PWD}"
cd "$workspace"
git config --global --add safe.directory "$workspace" 2>/dev/null || true

event="${GITHUB_EVENT_PATH:-}"
# Read a field out of the pull_request payload, if this run has one.
pr_field() {
  if [[ -n "$event" && -f "$event" ]]; then
    jq -r ".pull_request.$1 // empty" "$event"
  fi
}

if [[ -n "${INPUT_TYPESAFE_API_KEY:-}" ]]; then
  export TYPESAFE_API_KEY="$INPUT_TYPESAFE_API_KEY"
fi

args=(evaluate --config "${INPUT_CONFIG:-.github/review-gate.yml}")

if [[ -n "${INPUT_DIFF_FILE:-}" ]]; then
  args+=(--diff-file "$INPUT_DIFF_FILE")
else
  base="${INPUT_BASE:-}"
  head="${INPUT_HEAD:-}"
  [[ -z "$base" ]] && base="$(pr_field 'base.sha')"
  [[ -z "$head" ]] && head="$(pr_field 'head.sha')"
  if [[ -z "$base" ]]; then
    echo "review-gate: no diff source: set 'base' (or 'diff-file'), or run on a pull request" >&2
    exit 2
  fi
  # A shallow checkout may not contain the base commit; fetch it when missing.
  if ! git cat-file -e "${base}^{commit}" 2>/dev/null; then
    git fetch --no-tags --depth=200 origin "$base" 2>/dev/null || true
  fi
  args+=(--base "$base")
  if [[ -n "$head" ]]; then
    args+=(--head "$head")
  fi
fi

output="${INPUT_OUTPUT:-review-gate-result.json}"
markdown="${INPUT_MARKDOWN:-review-gate-summary.md}"
args+=(--output "$output" --markdown "$markdown")

add_if_set() {  # add_if_set <flag> <value>
  if [[ -n "$2" ]]; then
    args+=("$1" "$2")
  fi
}

add_if_set --github-output "${GITHUB_OUTPUT:-}"
add_if_set --model "${INPUT_MODEL:-}"
add_if_set --mock-answers "${INPUT_MOCK_ANSWERS:-}"
add_if_set --repo "${GITHUB_REPOSITORY:-}"
add_if_set --pr-number "$(pr_field 'number')"
add_if_set --pr-title "$(pr_field 'title')"
add_if_set --pr-body "$(pr_field 'body')"

if [[ "${INPUT_FAIL_ON_DEGRADED:-false}" == "true" ]]; then
  args+=(--fail-on-degraded)
fi

status=0
review-gate "${args[@]}" || status=$?

if [[ -f "$markdown" ]]; then
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    echo "markdown_path=$markdown" >> "$GITHUB_OUTPUT"
  fi
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    cat "$markdown" >> "$GITHUB_STEP_SUMMARY"
  fi
fi

# Post (or update) a single summary comment on the pull request.
number="$(pr_field 'number')"
if [[ "${INPUT_COMMENT:-false}" == "true" && -n "$number" && -f "$markdown" ]]; then
  if [[ -z "${INPUT_GITHUB_TOKEN:-}" ]]; then
    echo "review-gate: 'comment: true' needs 'github-token'" >&2
    exit 2
  fi
  api="${GITHUB_API_URL:-https://api.github.com}"
  marker="<!-- review-gate -->"
  body="$(printf '%s\n%s' "$marker" "$(cat "$markdown")")"
  payload="$(jq -n --arg body "$body" '{body: $body}')"
  existing="$(curl -fsS -H "Authorization: Bearer $INPUT_GITHUB_TOKEN" \
    -H "Accept: application/vnd.github+json" \
    "$api/repos/$GITHUB_REPOSITORY/issues/$number/comments?per_page=100" \
    | jq -r --arg marker "$marker" \
      '[.[] | select(.body | startswith($marker))][0].id // empty')"

  if [[ -n "$existing" ]]; then
    curl -fsS -X PATCH -H "Authorization: Bearer $INPUT_GITHUB_TOKEN" \
      -H "Accept: application/vnd.github+json" \
      "$api/repos/$GITHUB_REPOSITORY/issues/comments/$existing" -d "$payload" >/dev/null
  else
    curl -fsS -X POST -H "Authorization: Bearer $INPUT_GITHUB_TOKEN" \
      -H "Accept: application/vnd.github+json" \
      "$api/repos/$GITHUB_REPOSITORY/issues/$number/comments" -d "$payload" >/dev/null
  fi
fi

exit "$status"
