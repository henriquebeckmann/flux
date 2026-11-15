#!/bin/sh
# Installs Flux for the current user (no root): binary, desktop entry, icon, example extensions.
# Usage: dist/install.sh [--prefix DIR] [--autostart] [--binary PATH]
set -eu
prefix="${HOME}/.local"; autostart=0; bin=""
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) prefix="$2"; shift 2;;
    --autostart) autostart=1; shift;;
    --binary) bin="$2"; shift 2;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
done
here="$(cd "$(dirname "$0")/.." && pwd)"
[ -n "$bin" ] || bin="$here/target/release/flux"
[ -x "$bin" ] || { echo "no binary at $bin (run: cargo build --release)" >&2; exit 1; }
install -Dm755 "$bin" "$prefix/bin/flux"
install -Dm644 "$here/dist/dev.flux.Launcher.desktop" "$prefix/share/applications/dev.flux.Launcher.desktop"
install -Dm644 "$here/dist/dev.flux.Launcher.svg" "$prefix/share/icons/hicolor/scalable/apps/dev.flux.Launcher.svg"
mkdir -p "$prefix/share/flux/extensions"
cp -r "$here"/extensions/. "$prefix/share/flux/extensions/"
if [ "$autostart" = 1 ]; then
  unit="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
  mkdir -p "$unit"
  sed "s|%h/.local|$prefix|" "$here/dist/flux.service" > "$unit/flux.service"
  systemctl --user daemon-reload && systemctl --user enable --now flux.service
fi
echo "Installed to $prefix. Bind a key to: $prefix/bin/flux toggle"
echo "Needs at runtime: wl-clipboard, xdg-desktop-portal (+ a backend), a Secret Service provider, mesa/vulkan drivers."
