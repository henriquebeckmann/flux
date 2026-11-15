#!/bin/sh
# Measures the release build with each LTO setting: time, peak memory, size. One build per setting
# (this takes a long time). Usage: dist/lto-bench.sh [SRC_DIR] [WORK_DIR]
set -eu
here="$(cd "$(dirname "$0")/.." && pwd)"
src="${1:-$here/dist/out}"; base="${2:-${XDG_CACHE_HOME:-$HOME/.cache}/flux-repro}"
. "$here/dist/lib-build.sh"
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$here" log -1 --format=%ct 2>/dev/null || echo 1)}"
echo "lto seconds peak_rss_mb bytes sha256" | tee "$base/lto-bench.txt"
for lto in off thin fat; do
  r="$(build_from_source "$src" "$base/out-$lto" "$base/bench-$lto" CARGO_PROFILE_RELEASE_LTO=$lto)"
  echo "$lto $r" | tee -a "$base/lto-bench.txt"
done
