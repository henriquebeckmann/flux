#!/bin/sh
# Builds the release binary twice from the source archives, in two *different* directories, and
# compares the results byte for byte. Needs dist/make-source.sh output (default: dist/out).
# Usage: dist/repro-build.sh [SRC_DIR] [WORK_DIR]
set -eu
here="$(cd "$(dirname "$0")/.." && pwd)"
src="${1:-$here/dist/out}"; base="${2:-${XDG_CACHE_HOME:-$HOME/.cache}/flux-repro}"
. "$here/dist/lib-build.sh"
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$here" log -1 --format=%ct 2>/dev/null || echo 1)}"
a="$(build_from_source "$src" "$base/out-a" "$base/work-a/some/deep/path")"
b="$(build_from_source "$src" "$base/out-b" "$base/elsewhere/w-b")"
echo "build A: $a"; echo "build B: $b"
if [ "$(echo "$a" | cut -d' ' -f4)" = "$(echo "$b" | cut -d' ' -f4)" ]; then
  echo "REPRODUCIBLE: identical sha256"
else
  echo "NOT REPRODUCIBLE"; cmp -l "$base/out-a/flux" "$base/out-b/flux" | head -20 || true; exit 1
fi
