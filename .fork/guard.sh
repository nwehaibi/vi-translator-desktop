#!/usr/bin/env bash
# fork: what this copy never changes in the original app (see FORK.md), compared with the original
# version it is based on (.fork/base-commit, moved on with each merge of the original's updates):
# - the translation memory and its downloads (core/src/memory.rs, core/src/sync.rs): the server, the
#   download keys, the sources' ranks, how a download replaces the player's own lines;
# - the key that checks updates and downloaded scripts (core/src/sign.rs), and where it is used;
# - the app's identifier (its data folder: settings, memory, downloads).
# Run from anywhere in the repository: bash .fork/guard.sh
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
base="$(tr -d '[:space:]' < .fork/base-commit)"
if ! git cat-file -e "${base}^{commit}" 2>/dev/null; then
  echo "::error::the original version ${base} is not in this clone (a shallow clone? fetch with --unshallow)"
  exit 1
fi
fail=0
for f in core/src/memory.rs core/src/sync.rs core/src/sign.rs; do
  # (the working tree: committed or not)
  if ! git diff --quiet "$base" -- "$f"; then
    echo "::error file=${f}::${f} differs from the original (${base}): this copy must not change it"
    git --no-pager diff --stat "$base" -- "$f"
    fail=1
  fi
done
# the key's uses: the updates' check and the downloaded scripts' check
for f in core/src/pagescript.rs src-tauri/src/updater.rs; do
  was="$(git show "${base}:${f}" | grep -c 'sign::PUBLIC_KEY' || true)"
  now="$(grep -c 'sign::PUBLIC_KEY' "$f" || true)"
  if [ "$now" -lt "$was" ]; then
    echo "::error file=${f}::${f} checks with the original key in fewer places (${now}) than the original (${was})"
    fail=1
  fi
done
id_was="$(git show "${base}:src-tauri/src/main.rs" | grep -m1 '^const IDENTIFIER' || true)"
id_now="$(grep -m1 '^const IDENTIFIER' src-tauri/src/main.rs || true)"
if [ -z "$id_was" ] || [ "$id_was" != "$id_now" ]; then
  echo "::error file=src-tauri/src/main.rs::the app's identifier changed: '${id_now}' (the original: '${id_was}')"
  fail=1
fi
if [ "$fail" = 0 ]; then
  echo "guard: memory.rs, sync.rs, sign.rs, the key's uses and the identifier are the original's (${base})"
fi
exit "$fail"
