#!/bin/sh
# install.sh against this checkout, in a throwaway HOME: fresh install, update, and a missing tool.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
fail() { echo "install_test: $*"; exit 1; }

run() { HOME=$t/home PERSEIDS_REPO=$root PATH=$1 /bin/sh "$root/install.sh"; }
mkdir -p "$t/home"
run "$PATH" > "$t/out" || fail "install failed: $(cat "$t/out")"
[ -L "$t/home/.local/bin/perseids" ] || fail "no symlink in ~/.local/bin"
grep -q 'perseids key' "$t/out" || fail "doesn't point to perseids key"
grep -q 'export PATH' "$t/out" || fail "doesn't say how to add ~/.local/bin to PATH"
HOME=$t/home "$t/home/.local/bin/perseids" help | grep -q 'perseids run' || fail "perseids help didn't run"

run "$PATH" > "$t/out" || fail "second run (update) failed: $(cat "$t/out")"

mkdir "$t/nojq"
for c in git curl ssh ruby uname sed; do ln -s "$(command -v "$c")" "$t/nojq/$c"; done
if run "$t/nojq" > "$t/out"; then fail "succeeded without jq"; fi
grep -q 'install.*jq' "$t/out" || fail "no install command for jq: $(cat "$t/out")"

echo "install_test: ok"
