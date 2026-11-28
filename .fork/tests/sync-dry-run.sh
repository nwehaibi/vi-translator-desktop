#!/usr/bin/env bash
# fork: the upstream sync's merge (.fork/sync-merge.sh, as .github/workflows/upstream-sync.yml runs it)
# against made-up updates of the original, on local copies only (it pushes to a local bare copy, never to
# GitHub). Each update is a new commit on the original version this copy is based on (.fork/base-commit):
# - plain (a line added to README.md): merged, .fork/base-commit moved to it, the guard passing;
# - conflict (the line right before a hook of this copy's changed): reported, nothing merged or pushed;
# - workflows (a workflow file of the original's changed), without SYNC_TOKEN: reported, nothing pushed;
# - settings (a timing default changed): merged, and flagged for the calibrated timing.
# Run from anywhere in the repository: bash .fork/tests/sync-dry-run.sh
set -euo pipefail
repo="$(git rev-parse --show-toplevel)"
base="$(tr -d '[:space:]' < "$repo/.fork/base-commit")"
head="$(git -C "$repo" rev-parse HEAD)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# this copy as it is now, the default branch of a local "origin"
git clone -q --bare --no-local "$repo" "$work/origin.git"
git -C "$work/origin.git" update-ref refs/heads/fork "$head"
git -C "$work/origin.git" symbolic-ref HEAD refs/heads/fork

# a made-up update of the original: $1 its name, $2 the change (run in a copy of the original version)
original() {
  git clone -q --no-local "$repo" "$work/up-$1"
  (
    cd "$work/up-$1"
    git checkout -q -B main "$base"
    eval "$2"
    git -c user.name=original -c user.email=original@example.invalid commit -qam "original: $1"
  )
}
original plain 'echo "A line the original added." >> README.md'
original conflict 'python3 - <<PY
p = "core/src/engine.rs"
s = open(p).read()
a = "        let (interval_ms, stable_ms, clear_after_ms) = c.timing();\n"
assert s.count(a) == 1, "the line before the settings hook"
open(p, "w").write(s.replace(a, a.replace("c.timing();", "c.timing(); // changed by the original")))
PY'
original workflows 'echo "# changed by the original" >> .github/workflows/release.yml'
original settings 'sed -i "s/clear_after_ms: 100 };/clear_after_ms: 120 };/" core/src/config.rs && grep -q "clear_after_ms: 120 };" core/src/config.rs'

failures=0
# $1 the update, $2 the status expected, $3 settings flagged (yes/no)
expect() {
  local out="$work/out-$1" dir="$work/fork-$1"
  : > "$out"
  git clone -q "$work/origin.git" "$dir"
  if ! (cd "$dir" && UPSTREAM="$work/up-$1" UPSTREAM_BRANCH=main SYNC_BRANCH="sync/$1" HAS_SYNC_TOKEN=false GITHUB_OUTPUT="$out" \
    bash "$repo/.fork/sync-merge.sh" > "$work/log-$1" 2>&1); then
    echo "FAIL $1: the merge script failed"; sed 's/^/     /' "$work/log-$1"; failures=$((failures + 1)); return
  fi
  local status settings pushed
  status="$(sed -n 's/^status=//p' "$out")"
  settings="$(sed -n 's/^settings=//p' "$out")"; settings="${settings:-no}"
  pushed="$(git -C "$work/origin.git" rev-parse -q --verify "refs/heads/sync/$1" > /dev/null && echo yes || echo no)"
  local ok=1
  [ "$status" = "$2" ] || ok=0
  [ "$settings" = "$3" ] || ok=0
  if [ "$2" = merged ]; then
    # merged: pushed, its base the original's update, and the guard passed on it
    [ "$pushed" = yes ] || ok=0
    [ "$(git -C "$work/origin.git" show "refs/heads/sync/$1:.fork/base-commit" | tr -d '[:space:]')" = "$(git -C "$work/up-$1" rev-parse HEAD)" ] || ok=0
    grep -q "^guard: " "$work/log-$1" || ok=0
  else
    [ "$pushed" = no ] || ok=0
  fi
  if [ "$ok" = 1 ]; then
    echo "ok   $1: status $status, settings flagged $settings, pushed $pushed"
  else
    echo "FAIL $1: status $status (expected $2), settings flagged $settings (expected $3), pushed $pushed"
    sed 's/^/     /' "$work/log-$1"
    failures=$((failures + 1))
  fi
}
expect plain merged no
expect conflict conflict no
expect workflows workflows no
expect settings merged yes

if [ "$failures" = 0 ]; then echo "sync dry run: all passed"; else echo "sync dry run: $failures failed"; exit 1; fi
