#!/usr/bin/env bash
# fork: the upstream sync's merge (see FORK.md), run by .github/workflows/upstream-sync.yml (job "merge")
# and by its test, .fork/tests/sync-dry-run.sh: the original's latest commit merged into SYNC_BRANCH, made
# again from the branch checked out; the base moved on (.fork/base-commit) and the guard run. Says what
# happened in GITHUB_OUTPUT: status=none | merged | conflict | guard | workflows (and files=…), and
# settings=yes when the original changes its timing defaults or moves saved settings.
# Environment: UPSTREAM, UPSTREAM_BRANCH, SYNC_BRANCH, HAS_SYNC_TOKEN (true/false), GITHUB_OUTPUT.
set -euo pipefail
git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git fetch --no-tags "$UPSTREAM" "$UPSTREAM_BRANCH"
up="$(git rev-parse FETCH_HEAD)"
echo "upstream=$up" >> "$GITHUB_OUTPUT"
if git merge-base --is-ancestor "$up" HEAD; then
  echo "Nothing new from the original (${up})"
  echo "status=none" >> "$GITHUB_OUTPUT"
  exit 0
fi
before="$(git rev-parse HEAD)"
git checkout -B "$SYNC_BRANCH"
if ! git merge --no-ff --no-edit -m "Merge the original's updates (${up:0:7})" "$up"; then
  files="$(git diff --name-only --diff-filter=U | tr '\n' ' ')"
  git merge --abort
  echo "status=conflict" >> "$GITHUB_OUTPUT"
  echo "files=${files}" >> "$GITHUB_OUTPUT"
  exit 0
fi
# (the original changing its timing defaults or moving saved settings to new ones — as v1.4.0 did
# for everyone's timing — can undo the calibrated timing: said in the pull request)
if git diff "$before" HEAD -- core/src/config.rs | grep -qE '^[+-].*(DEFAULTS_VERSION|DEFAULT_TIMING|defaults_version|capture_fps|preview_fps|interval_ms|stable_ms|clear_after_ms|timing_custom)'; then
  echo "settings=yes" >> "$GITHUB_OUTPUT"
fi
printf '%s\n' "$up" > .fork/base-commit
git add .fork/base-commit
# (and the original's latest release in the new base's history: its sources keep the version before a
# release — its release workflow sets it from the tag — so this copy's build sets it from
# .fork/base-version; no release tag found: the version stays)
tags="$(git ls-remote --tags "$UPSTREAM" 'v*' 2>/dev/null || true)"
version="$(printf '%s\n' "$tags" | while read -r sha ref; do
  tag="${ref#refs/tags/}"
  tag="${tag%"^{}"}"
  [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || continue
  if git merge-base --is-ancestor "$sha" "$up" 2>/dev/null; then echo "${tag#v}"; fi
done | sort -V | tail -1)"
if [ -n "$version" ]; then
  printf '%s\n' "$version" > .fork/base-version
  git add .fork/base-version
  echo "version=${version}" >> "$GITHUB_OUTPUT"
fi
git commit -m "fork: based on the original's ${up:0:7}"
if ! bash .fork/guard.sh; then
  echo "status=guard" >> "$GITHUB_OUTPUT"
  exit 0
fi
workflows="$(git diff --name-only "$before" HEAD -- .github/workflows | tr '\n' ' ')"
if [ -n "$workflows" ] && [ "$HAS_SYNC_TOKEN" != "true" ]; then
  echo "status=workflows" >> "$GITHUB_OUTPUT"
  echo "files=${workflows}" >> "$GITHUB_OUTPUT"
  exit 0
fi
git push --force origin "$SYNC_BRANCH"
echo "status=merged" >> "$GITHUB_OUTPUT"
