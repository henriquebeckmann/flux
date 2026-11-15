# Shared by repro-build.sh and lto-bench.sh (sourced, not executed).
# build_from_source SRC_DIR OUT_DIR WORKDIR [VAR=value ...]
#   Unpacks the two archives from make-source.sh into WORKDIR (any path), builds `flux` offline from
#   the vendored sources with path remapping, and copies the binary to OUT_DIR/flux.
#   Prints:  <seconds> <peak-rss-MB> <bytes> <sha256>
build_from_source() {
  src="$1"; out="$2"; work="$3"; shift 3
  ver="$(ls "$src"/flux-launcher-[0-9]*.tar.gz | sed 's|.*/flux-launcher-\(.*\)\.tar\.gz|\1|' | head -1)"
  rm -rf "$work"; mkdir -p "$work" "$out"
  tar -xzf "$src/flux-launcher-$ver.tar.gz" -C "$work"
  tar -xzf "$src/flux-launcher-vendor-$ver.tar.gz" -C "$work"
  tree="$work/flux-launcher-$ver"
  epoch="${SOURCE_DATE_EPOCH:-1}"
  # const-random (pulled in by ahash/compile-time-rng via zed-xim) draws a fresh random seed per build
  # unless this is set; it ends up as constants in the binary. Fixed per release -> same bytes.
  seed="flux-launcher-$ver"
  cargo_home="$work/cargo-home"; mkdir -p "$cargo_home"
  # rustc and gcc let the LAST matching prefix win, so the broad $HOME rule must come first,
  # otherwise it shadows the specific ones (the work dir lives under $HOME) and leaks the build path.
  remap="--remap-path-prefix=$HOME=/home/user --remap-path-prefix=$cargo_home=/cargo --remap-path-prefix=$tree=/build/flux"
  cmap="-ffile-prefix-map=$HOME=/home/user -ffile-prefix-map=$cargo_home=/cargo -ffile-prefix-map=$tree=/build/flux"
  (
    cd "$tree"
    # run under a wrapper that reports wall time and peak RSS of the whole process tree
    env "$@" SOURCE_DATE_EPOCH="$epoch" CONST_RANDOM_SEED="$seed" CARGO_HOME="$cargo_home" CARGO_TARGET_DIR="$work/target" \
        RUSTFLAGS="$remap" CFLAGS="$cmap" CXXFLAGS="$cmap" CARGO_INCREMENTAL=0 \
        python3 -I -c '
import resource, subprocess, sys, time
t = time.time()
r = subprocess.run(["cargo", "build", "--release", "--locked", "--frozen", "--offline", "-p", "flux"])
rss = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss / 1024
open(sys.argv[1], "w").write("%d %d\n" % (time.time() - t, rss))
sys.exit(r.returncode)' "$work/time.txt"
  ) >&2
  cp "$work/target/release/flux" "$out/flux"
  read -r secs rss < "$work/time.txt"
  echo "$secs $rss $(stat -c %s "$out/flux") $(sha256sum "$out/flux" | cut -d' ' -f1)"
  [ -n "${KEEP_WORK:-}" ] || rm -rf "$work"
}
