#!/usr/bin/env bash
# Checks how entrypoint.sh turns action inputs into a review-gate command line.
#
# GitHub hands a Docker action its inputs as INPUT_<NAME> with dashes kept
# (INPUT_DIFF-FILE, not INPUT_DIFF_FILE), which is the mistake this guards.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# A stub that records the arguments it was called with.
mkdir -p "$work/bin"
cat > "$work/bin/review-gate" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$RECORDED_ARGS"
STUB
chmod +x "$work/bin/review-gate"
export PATH="$work/bin:$PATH"

mkdir -p "$work/workspace"
cat > "$work/event.json" <<'JSON'
{"pull_request": {"number": 42, "title": "Rename order id", "body": "why",
                  "base": {"sha": "basesha"}, "head": {"sha": "headsha"}}}
JSON

failures=0

# run_entrypoint <name> [VAR=VALUE ...] — returns the recorded arguments.
run_entrypoint() {
  local name="$1"
  shift
  export RECORDED_ARGS="$work/$name.args"
  : > "$RECORDED_ARGS"
  env -i \
    PATH="$PATH" HOME="$HOME" \
    RECORDED_ARGS="$RECORDED_ARGS" \
    GITHUB_WORKSPACE="$work/workspace" \
    GITHUB_EVENT_PATH="$work/event.json" \
    GITHUB_REPOSITORY="acme/api" \
    "$@" \
    bash "$root/entrypoint.sh"
  cat "$RECORDED_ARGS"
}

expect_contains() {  # expect_contains <label> <args> <flag> <value>
  local label="$1" args="$2" flag="$3" value="$4"
  if ! grep -qxF -- "$flag" <<<"$args"; then
    echo "FAIL [$label]: missing $flag in:"$'\n'"$args" >&2
    failures=$((failures + 1))
    return
  fi
  if [[ -n "$value" ]]; then
    local following
    following="$(grep -A1 -xF -- "$flag" <<<"$args" | tail -n1)"
    if [[ "$following" != "$value" ]]; then
      echo "FAIL [$label]: $flag was '$following', expected '$value'" >&2
      failures=$((failures + 1))
    fi
  fi
  echo "ok [$label] $flag ${value:+= $value}"
}

expect_absent() {  # expect_absent <label> <args> <flag>
  if grep -qxF -- "$3" <<<"$2"; then
    echo "FAIL [$1]: unexpected $3" >&2
    failures=$((failures + 1))
  else
    echo "ok [$1] no $3"
  fi
}

# 1. The dashed spelling GitHub actually sets.
args="$(run_entrypoint dashed \
  'INPUT_CONFIG=rules.yml' \
  'INPUT_DIFF-FILE=sample.diff' \
  'INPUT_MOCK-ANSWERS=answers.json' \
  'INPUT_FAIL-ON-DEGRADED=true' \
  'INPUT_OUTPUT=out.json' \
  'INPUT_MARKDOWN=out.md')"
expect_contains dashed "$args" --config rules.yml
expect_contains dashed "$args" --diff-file sample.diff
expect_contains dashed "$args" --mock-answers answers.json
expect_contains dashed "$args" --output out.json
expect_contains dashed "$args" --markdown out.md
expect_contains dashed "$args" --fail-on-degraded ""
expect_contains dashed "$args" --repo acme/api
expect_contains dashed "$args" --pr-number 42
expect_contains dashed "$args" --pr-title "Rename order id"
expect_absent dashed "$args" --base

# 2. The underscored spelling, for running the image by hand.
args="$(run_entrypoint underscored 'INPUT_DIFF_FILE=sample.diff')"
expect_contains underscored "$args" --diff-file sample.diff
expect_contains underscored "$args" --config .github/review-gate.yml
expect_absent underscored "$args" --fail-on-degraded

# 3. With no diff-file, the refs come from the pull request event.
args="$(run_entrypoint event)"
expect_contains event "$args" --base basesha
expect_contains event "$args" --head headsha

# 4. Explicit base/head win over the event.
args="$(run_entrypoint explicit 'INPUT_BASE=main' 'INPUT_HEAD=topic')"
expect_contains explicit "$args" --base main
expect_contains explicit "$args" --head topic

# 5. No diff source at all is a usage error.
rm -f "$work/empty-event.json"
echo '{}' > "$work/empty-event.json"
status=0
env -i PATH="$PATH" HOME="$HOME" RECORDED_ARGS="$work/none.args" \
  GITHUB_WORKSPACE="$work/workspace" GITHUB_EVENT_PATH="$work/empty-event.json" \
  bash "$root/entrypoint.sh" >/dev/null 2>&1 || status=$?
if [[ "$status" != "2" ]]; then
  echo "FAIL [no-source]: expected exit 2, got $status" >&2
  failures=$((failures + 1))
else
  echo "ok [no-source] exits 2"
fi

if [[ "$failures" -gt 0 ]]; then
  echo "$failures check(s) failed" >&2
  exit 1
fi
echo "entrypoint.sh: all checks passed"
