#!/bin/bash
# bash test/cli_test.sh — bin/perseids end to end, with no Boat account: fake curl, ssh, scp and ssh-keygen answer
# Boat's API and run each "box" in its own folder here. Checks the exit code for jobs gated on other jobs.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
fail() { echo "cli_test: $*"; exit 1; }
export FAKE=$t/boxes
mkdir -p "$FAKE" "$t/bin"

cat >"$t/bin/curl" <<'EOF'
#!/bin/sh
# Boat's API: every box is ready at once, and its id is a folder under $FAKE.
cat >/dev/null
m=GET u=
while [ $# -gt 0 ]; do case $1 in -X) m=$2; shift ;; http*) u=$1 ;; esac; shift; done
p=${u#*/api/v1}
case "$m $p" in
  "GET /sandboxes?"*) echo '{"sandboxes":[]}' ;;
  "POST /sandboxes") d=$(mktemp -d "$FAKE/box-XXXXXX"); echo "{\"sandbox\":{\"id\":\"${d##*/}\"}}" ;;
  "POST /sandboxes/"*/sshkey) id=${p#/sandboxes/}; echo "{\"ok\":true,\"hostKey\":\"k\",\"sshEndpoint\":\"${id%/sshkey}:22\"}" ;;
  "GET /sandboxes/"*) echo '{"sandbox":{"state":"ready"}}' ;;
  *) echo '{}' ;;
esac
EOF
cat >"$t/bin/ssh" <<'EOF'
#!/bin/sh
# runs the command with the box's folder as its home
for a; do case $a in user@*) id=${a#user@} ;; esac; last=$a; done
[ "$last" = true ] && exit 0
cd "$FAKE/$id" && HOME=$FAKE/$id exec bash -c "$last"
EOF
cat >"$t/bin/scp" <<'EOF'
#!/bin/sh
for a; do last=$a; done
id=${last#user@}; id=${id%%:*}
for a; do if [ "$a" != "$last" ] && [ -f "$a" ]; then cp "$a" "$FAKE/$id/"; fi; done
EOF
cat >"$t/bin/ssh-keygen" <<'EOF'
#!/bin/sh
while [ $# -gt 0 ]; do [ "$1" = -f ] && f=$2; shift; done
: >"$f"; : >"$f.pub"
EOF
chmod +x "$t/bin/"*

mkdir -p "$t/repo/.github/workflows"
cd "$t/repo"
git init -q && git config user.email t@t && git config user.name t
# changes decides with an output whether test runs; test fails; after needs test; gate always runs and reports.
cat >.github/workflows/ci.yml <<'EOF'
on: push
jobs:
  changes:
    runs-on: ubuntu-latest
    outputs:
      code: ${{ steps.f.outputs.code }}
    steps:
      - id: f
        run: echo "code=$CODE" >> "$GITHUB_OUTPUT"
  test:
    needs: changes
    if: needs.changes.outputs.code == 'true'
    runs-on: ubuntu-latest
    steps:
      - run: exit 1
  after:
    needs: test
    runs-on: ubuntu-latest
    steps:
      - run: echo never
  gate:
    needs: [changes, test]
    if: always()
    runs-on: ubuntu-latest
    steps:
      - run: echo "gate saw test=${{ needs.test.result }}"
EOF
git add -A && git commit -qm ci

perseids() { # exit code, then the output in $t/out
  set +e
  PATH=$t/bin:$PATH BOAT_API=http://boat.test/api/v1 BOAT_API_KEY=boat_test XDG_STATE_HOME=$t/state \
    "$root/bin/perseids" "$@" >"$t/out" 2>&1
  rc=$?
  set -e
}

CODE=true perseids run test # the one job: it brings changes, so its if: sees changes' real output
[ "$rc" = 1 ] || fail "perseids run test: exit $rc, want 1 (test fails): $(cat "$t/out")"

CODE=true perseids run
[ "$rc" = 1 ] || fail "perseids run: exit $rc, want 1: $(cat "$t/out")"
grep -q 'skip *after .*needs test' "$t/out" || fail "after should be skipped for its failed need: $(cat "$t/out")"
grep -rq 'gate saw test=failure' "$t/state" || fail "gate didn't run with needs.test.result"

CODE=false perseids run # nothing needs testing: a real skip, so green, as on GitHub
[ "$rc" = 0 ] || fail "CODE=false: exit $rc, want 0: $(cat "$t/out")"
grep -q 'skip *test .*its if: is false' "$t/out" || fail "test should be skipped by its if: $(cat "$t/out")"

echo "cli_test: ok"
