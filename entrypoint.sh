#!/usr/bin/env bash
# Container/composite action entrypoint: turns action inputs into a review-gate call.
set -euo pipefail

workspace="${GITHUB_WORKSPACE:-$PWD}"
cd "$workspace"
git config --global --add safe.directory "$workspace" 2>/dev/null || true

args=(evaluate --config "${INPUT_CONFIG:-.github/review-gate.yml}")

if [[ -n "${INPUT_DIFF_FILE:-}" ]]; then
  args+=(--diff-file "$INPUT_DIFF_FILE")
else
  if [[ -z "${INPUT_BASE:-}" ]]; then
    echo "review-gate: no diff source: set 'base' (or 'diff-file')" >&2
    exit 2
  fi
  # A shallow checkout may not contain the base commit; fetch it when missing.
  if ! git cat-file -e "${INPUT_BASE}^{commit}" 2>/dev/null; then
    git fetch --no-tags --depth=200 origin "$INPUT_BASE" 2>/dev/null || true
  fi
  args+=(--base "$INPUT_BASE")
  if [[ -n "${INPUT_HEAD:-}" ]]; then
    args+=(--head "$INPUT_HEAD")
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
add_if_set --repo "${INPUT_REPO:-}"
add_if_set --pr-number "${INPUT_PR_NUMBER:-}"
add_if_set --pr-title "${INPUT_PR_TITLE:-}"
add_if_set --pr-body "${INPUT_PR_BODY:-}"

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

exit "$status"
