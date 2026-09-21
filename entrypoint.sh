#!/usr/bin/env bash
# Container action entrypoint: turns action inputs into a review-gate call.
set -euo pipefail

workspace="${GITHUB_WORKSPACE:-$PWD}"
cd "$workspace"
git config --global --add safe.directory "$workspace" 2>/dev/null || true

# GitHub passes a Docker action's inputs as INPUT_<NAME> with the name uppercased
# but its dashes kept, e.g. `diff-file` arrives as INPUT_DIFF-FILE. Those are not
# valid shell identifiers, so they have to be read through printenv. The
# underscored spelling is accepted too, for running the image by hand.
input() {
  local dashed="INPUT_${1^^}"
  local underscored="${dashed//-/_}"
  printenv "$dashed" 2>/dev/null || printenv "$underscored" 2>/dev/null || true
}

event="${GITHUB_EVENT_PATH:-}"
# Read a field out of the pull_request payload, if this run has one.
pr_field() {
  if [[ -n "$event" && -f "$event" ]]; then
    jq -r ".pull_request.$1 // empty" "$event"
  fi
}

api_key="$(input typesafe-api-key)"
if [[ -n "$api_key" ]]; then
  export TYPESAFE_API_KEY="$api_key"
fi

config="$(input config)"
args=(evaluate --config "${config:-.github/review-gate.yml}")

diff_file="$(input diff-file)"
if [[ -n "$diff_file" ]]; then
  args+=(--diff-file "$diff_file")
else
  base="$(input base)"
  head="$(input head)"
  [[ -z "$base" ]] && base="$(pr_field 'base.sha')"
  [[ -z "$head" ]] && head="$(pr_field 'head.sha')"
  if [[ -z "$base" ]]; then
    echo "review-gate: no diff source: set 'base' (or 'diff-file'), or run on a pull request" >&2
    exit 2
  fi
  # A shallow checkout may not contain both ends of the range; fetch what is missing.
  for commit in "$base" "$head"; do
    if [[ -n "$commit" ]] && ! git cat-file -e "${commit}^{commit}" 2>/dev/null; then
      git fetch --no-tags --depth=200 origin "$commit" 2>/dev/null \
        || git fetch --no-tags --unshallow origin 2>/dev/null \
        || true
    fi
  done
  args+=(--base "$base")
  if [[ -n "$head" ]]; then
    args+=(--head "$head")
  fi
fi

output="$(input output)"
output="${output:-review-gate-result.json}"
markdown="$(input markdown)"
markdown="${markdown:-review-gate-summary.md}"
args+=(--output "$output" --markdown "$markdown")

add_if_set() {  # add_if_set <flag> <value>
  if [[ -n "$2" ]]; then
    args+=("$1" "$2")
  fi
}

add_if_set --github-output "${GITHUB_OUTPUT:-}"
add_if_set --model "$(input model)"
add_if_set --mock-answers "$(input mock-answers)"
add_if_set --repo "${GITHUB_REPOSITORY:-}"
add_if_set --pr-number "$(pr_field 'number')"
add_if_set --pr-title "$(pr_field 'title')"
add_if_set --pr-body "$(pr_field 'body')"

if [[ "$(input fail-on-degraded)" == "true" ]]; then
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
if [[ "$(input comment)" == "true" && -n "$number" && -f "$markdown" ]]; then
  token="$(input github-token)"
  if [[ -z "$token" ]]; then
    echo "review-gate: 'comment: true' needs 'github-token'" >&2
    exit 2
  fi
  api="${GITHUB_API_URL:-https://api.github.com}"
  marker="<!-- review-gate -->"
  body="$(printf '%s\n%s' "$marker" "$(cat "$markdown")")"
  payload="$(jq -n --arg body "$body" '{body: $body}')"
  existing="$(curl -fsS -H "Authorization: Bearer $token" \
    -H "Accept: application/vnd.github+json" \
    "$api/repos/$GITHUB_REPOSITORY/issues/$number/comments?per_page=100" \
    | jq -r --arg marker "$marker" \
      '[.[] | select(.body | startswith($marker))][0].id // empty')"

  if [[ -n "$existing" ]]; then
    curl -fsS -X PATCH -H "Authorization: Bearer $token" \
      -H "Accept: application/vnd.github+json" \
      "$api/repos/$GITHUB_REPOSITORY/issues/comments/$existing" -d "$payload" >/dev/null
  else
    curl -fsS -X POST -H "Authorization: Bearer $token" \
      -H "Accept: application/vnd.github+json" \
      "$api/repos/$GITHUB_REPOSITORY/issues/$number/comments" -d "$payload" >/dev/null
  fi
fi

exit "$status"
