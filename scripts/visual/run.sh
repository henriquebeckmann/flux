#!/bin/sh
# Visual walkthrough of the launcher on a real Wayland (Hyprland) session: starts an isolated daemon,
# drives it with wtype and takes grim screenshots of the launcher area.
#
#   scripts/visual/run.sh [OUTDIR] [SCENARIO...]       (default: all scenarios)
#
# Safety: refuses to run while a lock screen is up, and stops if the launcher's layer disappears.
# It uses a throw-away XDG_DATA/CONFIG/STATE (your database and permissions.toml are not touched),
# the real XDG_RUNTIME_DIR (the daemon socket), and quits only the daemon it started.
set -u
here="$(cd "$(dirname "$0")/../.." && pwd)"
out="${1:-$here/target/visual}"; [ $# -gt 0 ] && shift
want="${*:-root search detail form validation grid manage manage_panel processes files calculator empty chooser}"
flux="${FLUX_BIN:-$here/target/debug/flux}"
mkdir -p "$out"
for lock in hyprlock swaylock gtklock; do pgrep -x "$lock" > /dev/null && { echo "a lock screen ($lock) is running; unlock first" >&2; exit 3; }; done
tmp="$(mktemp -d)"
export XDG_DATA_HOME="$tmp/data" XDG_CONFIG_HOME="$tmp/config" XDG_STATE_HOME="$tmp/state" FLUX_EXTENSIONS_DIR="$here/extensions"

if "$flux" status > /dev/null 2>&1; then echo "a Flux daemon is already running on this session; quit it first (flux quit)" >&2; exit 4; fi
RUST_LOG=info "$flux" daemon > "$out/daemon.log" 2>&1 &
daemon=$!
cleanup() { "$flux" quit > /dev/null 2>&1; sleep 0.5; kill "$daemon" 2> /dev/null; rm -rf "$tmp"; }
trap cleanup EXIT INT TERM
for i in 1 2 3 4 5 6 7 8 9 10; do "$flux" status > /dev/null 2>&1 && break; sleep 1; done

# --- geometry of the focused monitor (logical pixels) and the area the launcher occupies
mon="$(hyprctl monitors -j | jq -c '[.[] | select(.focused)][0]')"
mx=$(echo "$mon" | jq '.x'); my=$(echo "$mon" | jq '.y')
mw=$(echo "$mon" | jq '(.width / .scale) | floor'); mh=$(echo "$mon" | jq '(.height / .scale) | floor')
rw=760; rh=520
rx=$((mx + (mw - rw) / 2)); ry=$((my + (mh - rh) / 2))
echo "monitor ${mw}x${mh}; capturing ${rw}x${rh} at $rx,$ry" | tee "$out/geometry.txt"

n=0
shot() { n=$((n + 1)); f="$out/$(printf %02d $n)-$1.png"; sleep "${2:-0.6}"; grim -g "$rx,$ry ${rw}x${rh}" "$f" && echo "$f"; }
layer_up() { hyprctl layers | grep -q "namespace: flux"; }
guard() { layer_up || { echo "the launcher window is gone; stopping" >&2; exit 5; }; }
key() { for k in "$@"; do wtype -k "$k"; sleep 0.12; done; }
typ() { wtype -- "$1"; sleep 0.25; }
ctrl() { wtype -M ctrl -k "$1" -m ctrl; sleep 0.25; }
back_to_root() { for i in 1 2 3 4 5 6; do key Escape; done; sleep 0.4; "$flux" show; sleep 1.0; ctrl a; key BackSpace; sleep 0.3; }
open_cmd() { typ "$1"; sleep 0.5; key Return; sleep "${2:-1.2}"; }
show() { "$flux" show; sleep 1.2; guard; }

show
for s in $want; do
  guard
  case "$s" in
    root)        shot root ;;
    search)      typ "demo"; shot search; back_to_root ;;
    detail)      open_cmd "Demo: Detail"; shot detail; back_to_root ;;
    form)        open_cmd "Demo: Form"; shot form-empty; typ "A"; key Tab; typ "not-an-email"; key Tab; typ "abc"; shot form-typed; back_to_root ;;
    validation)  open_cmd "Demo: Form"; wtype -M ctrl -k Return -m ctrl; sleep 0.6; shot form-validation; key Tab Tab Tab Tab Tab Tab Tab; shot form-files-focused; back_to_root ;;
    grid)        open_cmd "Demo: Grid" 2.5; shot grid; key Right Right; shot grid-moved; back_to_root ;;
    manage)      open_cmd "Manage Extensions"; shot manage-list; key Return; sleep 0.8; shot manage-detail; back_to_root ;;
    manage_panel) open_cmd "Manage Extensions"; ctrl k; shot manage-actions; key Escape; key Down Down; ctrl k; shot manage-actions-2; back_to_root ;;
    processes)   open_cmd "Manage Processes" 3; shot processes; key Down Down; shot processes-selected; ctrl k; shot processes-actions; back_to_root ;;
    files)       open_cmd "Browse Files" 2.5; shot files; key Return; sleep 1; shot files-entered; key Left; sleep 0.5; shot files-back; back_to_root ;;
    calculator)  open_cmd "Calculator"; typ "2+3*4"; shot calculator; back_to_root ;;
    empty)       typ "zzzzzz"; shot empty-root; back_to_root ;;
    chooser)
      for round in 1 2 3; do
        open_cmd "Demo: Form"; key Tab Tab Tab Tab Tab Tab Tab; key Return; sleep 3
        shot "chooser-$round-dialog" 0.2
        grim -o "$(echo "$mon" | jq -r .name)" "$out/chooser-$round-fullscreen.png"
        echo "round $round: layers while the dialog is open: $(hyprctl layers | grep -c 'namespace: flux') ; active: $(hyprctl activewindow -j | jq -r '.class + " / " + .title')"
        hyprctl dispatch focuswindow "class:xdg-desktop-portal-gtk" > /dev/null; sleep 0.5; key Escape; sleep 2
        # wtype's Escape does not always reach the GTK dialog: close that one window (the test's own dialog) as the user's "Cancel"
        hyprctl clients -j | jq -e '[.[] | select(.class | test("portal"))] | length > 0' > /dev/null && { echo "round $round: Escape did not close the dialog; closing it with hyprctl"; hyprctl dispatch closewindow "class:xdg-desktop-portal-gtk" > /dev/null; }
        waited=0; while hyprctl clients -j | jq -e '[.[] | select(.class | test("portal"))] | length > 0' > /dev/null && [ $waited -lt 20 ]; do sleep 0.5; waited=$((waited + 1)); done
        echo "round $round: the dialog took $((waited / 2)) s to close after Escape"; sleep 1.5
        echo "round $round: after closing the dialog, launcher layer present: $(layer_up && echo yes || echo NO)"
        layer_up || { echo "the launcher did not come back after the file chooser" >&2; shot "chooser-$round-no-launcher" 0.2; exit 6; }
        shot "chooser-$round-back" 0.2
        typ "x"; shot "chooser-$round-typed-after-dialog" 0.3   # does the keyboard still reach the launcher?
        back_to_root
      done ;;
    *) echo "unknown scenario: $s" >&2 ;;
  esac
done
echo "done: $n screenshots in $out"
