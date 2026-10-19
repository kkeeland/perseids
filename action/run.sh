#!/bin/bash
# The Perseids GitHub Action: runs this repo's workflow on Perseids from one small runner, writes a job summary,
# and exits with Perseids' exit code.
set -uo pipefail
[ -n "${BOAT_API_KEY:-}" ] || { echo "::error::boat-api-key is empty: add your Boat key as a repo secret and pass it in"; exit 99; }
echo "::add-mask::$BOAT_API_KEY"
command -v jq >/dev/null && command -v ruby >/dev/null || { sudo apt-get update -qq && sudo apt-get install -y -qq jq ruby >/dev/null; }

wf=$INPUT_WORKFLOW
if [ -z "$wf" ]; then for f in ci.yml ci.yaml; do [ -f ".github/workflows/$f" ] && wf=$f && break; done; fi
[ -n "$wf" ] || { echo "::error::set the workflow input, for example workflow: ci.yml"; exit 99; }
case $wf in */*) ;; *) wf=.github/workflows/$wf ;; esac
# Running the workflow that is running this action would start itself again.
self=${GITHUB_WORKFLOW_REF#*/*/}; self=${self%@*}
[ "$wf" != "$self" ] || { echo "::error::$wf runs this action; point workflow at your CI workflow instead (for example ci.yml)"; exit 99; }

# Perseids bundles the commit with its history, so a shallow checkout gets the rest first.
[ "$(git rev-parse --is-shallow-repository)" = true ] && git fetch -q --unshallow

args=(run -w "$wf")
[ -n "$INPUT_JOBS" ] && read -ra jobs <<<"$INPUT_JOBS" && args+=("${jobs[@]}")
[ "$INPUT_KEEP_GOING" = true ] && args+=(--keep-going)
export PERSEIDS_EVENT=${GITHUB_EVENT_NAME:-push} PERSEIDS_CLOUD=0
log=$(mktemp)
"$GITHUB_ACTION_PATH/bin/perseids" "${args[@]}" 2>&1 | tee "$log"
rc=${PIPESTATUS[0]}
echo "exit-code=$rc" >>"$GITHUB_OUTPUT"

{ # the summary block Perseids prints, as a table
  echo "### Perseids · exit $rc"
  echo
  echo "| Result | Job | Note |"
  echo "|---|---|---|"
  sed -n '/^--- perseids:/,/^--- exit/p' "$log" | sed '1d;$d' | grep -v '^  ↳' | grep -v '^    |' |
    awk '{ r=$1; $1=""; sub(/^ +/, ""); printf "| %s | %s | |\n", (r=="pass" ? "✅ pass" : r=="fail" ? "❌ fail" : r), $0 }'
  echo
  echo "$(grep '^--- exit' "$log" | tail -1 | sed 's/^--- //')"
} >>"${GITHUB_STEP_SUMMARY:-/dev/null}"
exit "$rc"
