#!/usr/bin/env bash
#
# Fail when tracked files differ from HEAD.
#
# `src-tauri/build.rs` stamps `git describe --tags --always --dirty` into the
# binary and the About page displays it, so a build produced from a modified
# checkout advertises itself as `<version>-dirty`. The v26.8.1 Windows
# installer shipped exactly that. An artifact that cannot be mapped back to a
# commit should not reach users, so the release job runs this check instead of
# publishing one.
#
# `git update-index --refresh` runs first because `git diff-index` is plumbing:
# it compares the index's cached stat data and reports a difference without
# falling back to a content comparison, so a checkout whose mtimes moved — any
# fresh CI clone — can look modified when it is not. `git describe --dirty`
# refreshes the index itself (git's `builtin/describe.c`); this check has to.
#
# Usage: bash scripts/assert-pristine-tree.sh "<when this ran>"
set -euo pipefail

context="${1:-checkout}"

# Best effort: an unwritable or locked index leaves stale stat data, which can
# only produce a false failure below — with the diff printed, so it stays
# diagnosable.
git update-index -q --refresh || true

echo "describe: $(git describe --tags --always --dirty)"

if git diff-index --quiet HEAD --; then
  echo "Tracked files match HEAD ($context)."
  exit 0
fi

echo "::error::Tracked files differ from HEAD ($context); this build would be stamped -dirty."
git status --porcelain --untracked-files=no
git diff --stat HEAD --
exit 1
