#!/bin/sh
# Builds the two source archives the PKGBUILD needs, deterministically:
#   flux-launcher-<ver>.tar.gz          the project (tracked + untracked, minus target/ and vendor/)
#   flux-launcher-vendor-<ver>.tar.gz   every dependency, from `cargo vendor --locked` (Cargo.lock decides)
# and prints their sha256 for the PKGBUILD. Same inputs => same bytes (fixed mtime, owner, order, gzip -n).
# Usage: dist/make-source.sh [OUTDIR]
set -eu
here="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$here/dist/out}"
ver="$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)"
epoch="${SOURCE_DATE_EPOCH:-$(git -C "$here" log -1 --format=%ct 2>/dev/null || echo 0)}"
mkdir -p "$out"
tar_det() { # tar_det PREFIX  (file list on stdin, relative to cwd)
  tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner --pax-option=delete=atime,delete=ctime \
      --transform "s,^,$1/," -cf - --files-from=- | gzip -9n
}
cd "$here"
git ls-files -co --exclude-standard | grep -v -e '^vendor/' -e '^dist/out/' | sort \
  | tar_det "flux-launcher-$ver" > "$out/flux-launcher-$ver.tar.gz"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/.cargo"
# `cargo vendor` prints the [source] replacement; point it at the vendor/ folder next to Cargo.toml.
cargo vendor --locked "$tmp/vendor" | sed 's|^directory = .*|directory = "vendor"|' > "$tmp/.cargo/config.toml"
( cd "$tmp" && { find vendor -type f -o -type l; echo .cargo/config.toml; } | sort | tar_det "flux-launcher-$ver" ) > "$out/flux-launcher-vendor-$ver.tar.gz"
( cd "$out" && sha256sum "flux-launcher-$ver.tar.gz" "flux-launcher-vendor-$ver.tar.gz" )
