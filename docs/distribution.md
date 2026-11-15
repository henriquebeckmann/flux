# Installing and packaging (Linux)

* **From source, for your user:** `cargo build --release && dist/install.sh [--autostart]` → `~/.local/bin/flux`, desktop entry, icon, example
  extensions in `~/.local/share/flux/extensions` (discovered via `$XDG_DATA_DIRS`/`~/.local/share`). `--autostart` installs and enables a
  systemd user unit (`dist/flux.service`). `dist/uninstall.sh [--purge]` removes it (data is kept without `--purge`).
* **Arch Linux:** `dist/PKGBUILD` (`cd dist && makepkg`) installs to `/usr` with the unit in `/usr/lib/systemd/user/`.
* **Not provided:** `.deb` and AppImage (no tooling was available to build or test them). The release binary is the same one
  (`flux` hosts extensions as `flux host`), so those are packaging work only.

Runtime dependencies: `wl-clipboard` (clipboard), `xdg-desktop-portal` + a backend (global shortcut, file chooser), a Secret Service provider
(secure storage), Vulkan/mesa drivers (GPUI), a Wayland compositor with layer-shell (falls back to a pop-up). Process isolation needs
Linux ≥ 5.13 (Landlock); `systemd --user` for per-extension memory/CPU limits.

Autostart without systemd: add `exec-once = flux daemon` (Hyprland) or the equivalent of your compositor. The daemon does not fork; run it
under a supervisor or from your session startup.

## Reproducible builds
`dist/make-source.sh` creates two deterministic archives (project; `cargo vendor` of every dependency incl. the Zed git sources) and prints their
sha256. `dist/repro-build.sh` unpacks them in two different directories, builds offline (`--locked --frozen --offline`) with
`SOURCE_DATE_EPOCH`, `--remap-path-prefix` and `-ffile-prefix-map` (the bundled Lua/SQLite C code embeds paths too) and compares the binaries.
`dist/lto-bench.sh` measures `lto = off | thin | fat` (time, peak memory, size). `dist/PKGBUILD` builds from the same archives, offline.

Two things made the first attempts non-reproducible; both are fixed in `dist/lib-build.sh` and `dist/PKGBUILD`:
- **Remap order.** rustc and gcc apply the *last* matching prefix, so the broad `$HOME=/home/user` rule has to come first or it shadows
  the specific ones and leaks the build directory into panic/location strings.
- **`const-random`.** `ahash` is built with `compile-time-rng` (enabled by `zed-xim`, a gpui dependency), and its proc macro draws a fresh random
  seed per build, embedding it as constants. `CONST_RANDOM_SEED=flux-launcher-<version>` pins it. Runtime hashing is still randomized.

### Results (rustc 1.99.1, x86_64, 8 CPUs, thin LTO, `--locked --frozen --offline`)
Same sources built in `.../work-a/some/deep/path` and `.../elsewhere/w-b`:

| build | seconds | peak RSS | bytes | sha256 |
|---|---|---|---|---|
| A | 880 | 2416 MB | 40271048 | `c6e3d01261a47b1f2c8b50933cb1eea30a5737724a88dcd9f45a70a7e8dd0d58` |
| B | 850 | 2258 MB | 40271048 | `c6e3d01261a47b1f2c8b50933cb1eea30a5737724a88dcd9f45a70a7e8dd0d58` |

Identical. Not verified: a different machine, a different rustc/linker version, or a clean chroot (`extra-x86_64-build` is not installed here).

### `makepkg` (clean directory, no chroot)
`makepkg -f` with the two archives and real `sha256sums` built `flux-launcher-0.1.0-1-x86_64.pkg.tar.zst` (16 MB; `check()` ran 28 + 17 unit
tests, all passing). Verified on the package without installing it: file list and modes (`usr/bin/flux`, `.desktop`, icon, user unit,
extensions, docs), `desktop-file-validate` clean, `systemd-analyze --user verify` clean once `/usr/bin/flux` exists, extracted binary
runs (`flux --version`), no missing shared libraries, no build path left in the binary.
Not run: `namcap` (not installed), a clean chroot (`extra-x86_64-build` not installed), `pacman -U`. `clang` was dropped from `makedepends`
because the build never needed it. The binary inside the package (sha256 `dfef4512…`) differs from the `repro-build.sh` one: the PKGBUILD
uses its own `RUSTFLAGS`/work dir and newer sources, so the two are not meant to match; reproducibility was shown with `repro-build.sh`
only. Keep `sha256sums=('SKIP' …)` in the repo (the project archive contains the PKGBUILD itself) and fill in the values printed by
`dist/make-source.sh` when releasing.

### LTO
| `lto` | build time | peak RSS | binary |
|---|---|---|---|
| off | 854 s | 1569 MB | 39,991,744 B |
| thin | 881 s | 2325 MB | 40,344,776 B |
| fat | 939 s | 3619 MB | 37,813,832 B |

`thin` is kept: build time is within 3% of `off` and memory is 36% lower than `fat`, while `fat` saves only 6% of the binary. Startup and
runtime performance were **not** measured, so this is a build-cost decision, not a speed one.

## Visual walkthrough
`scripts/visual/run.sh` drives a real launcher with `wtype` and captures it with `grim` (an isolated daemon, your data untouched).
It refuses to run on a locked session and stops if the launcher window disappears.
