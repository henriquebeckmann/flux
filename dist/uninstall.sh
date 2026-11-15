#!/bin/sh
# Removes what install.sh installed. Your extensions and data are kept unless you pass --purge.
set -eu
prefix="${HOME}/.local"; purge=0
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) prefix="$2"; shift 2;;
    --purge) purge=1; shift;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
done
systemctl --user disable --now flux.service 2>/dev/null || true
rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/flux.service"
"$prefix/bin/flux" quit 2>/dev/null || true
rm -f "$prefix/bin/flux" "$prefix/share/applications/dev.flux.Launcher.desktop" "$prefix/share/icons/hicolor/scalable/apps/dev.flux.Launcher.svg"
rm -rf "$prefix/share/flux"
if [ "$purge" = 1 ]; then
  rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/flux" "${XDG_CONFIG_HOME:-$HOME/.config}/flux" "${XDG_STATE_HOME:-$HOME/.local/state}/flux"
fi
echo "Flux removed."
