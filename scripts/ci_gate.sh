#!/usr/bin/env bash
# Turn the latest GitHub Actions run for HEAD into a pass/fail command.
set -u

BRANCH=$(git rev-parse --abbrev-ref HEAD)
SHA=$(git rev-parse HEAD)
WORKFLOW=${WORKFLOW:-ci.yml}
ATTEMPTS=${ATTEMPTS:-30}
SLEEP_SECS=${SLEEP_SECS:-10}

echo "ci_gate: branch=$BRANCH sha=$SHA workflow=$WORKFLOW"

run_id=""
attempt=1
while [ "$attempt" -le "$ATTEMPTS" ]; do
  run_id=$(gh run list --workflow "$WORKFLOW" --branch "$BRANCH" --commit "$SHA" \
    --limit 1 --json databaseId --jq '.[0].databaseId // empty' 2>/dev/null || true)
  if [ -n "$run_id" ]; then
    break
  fi
  echo "ci_gate: no run yet for $SHA (attempt $attempt/$ATTEMPTS), sleeping ${SLEEP_SECS}s"
  sleep "$SLEEP_SECS"
  attempt=$((attempt + 1))
done

if [ -z "$run_id" ]; then
  echo "ci_gate: FAIL no workflow run found for branch=$BRANCH sha=$SHA workflow=$WORKFLOW after $ATTEMPTS attempts (check gh auth status, WORKFLOW name, and that the push created a run for this commit)" >&2
  exit 1
fi

echo "ci_gate: run_id=$run_id"

if ! gh run watch "$run_id" --exit-status --compact --interval 15; then
  echo "ci_gate: FAIL run_id=$run_id" >&2
  gh run view "$run_id" --json url,headSha,conclusion
  exit 1
fi

summary=$(gh run view "$run_id" --json url,headSha,conclusion \
  --jq '"\(.url) \(.headSha) \(.conclusion)"')
read -r url head_sha conclusion <<< "$summary"

echo "ci_gate: PASS url=$url sha=$head_sha conclusion=$conclusion"

gh run view "$run_id" --json jobs \
  --jq '.jobs[] | "ci_gate: job=\(.name) conclusion=\(.conclusion)"'
