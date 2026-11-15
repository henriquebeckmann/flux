# Running Flux as a daemon

```
flux              # toggle: show/hide (starts the daemon if it is not running)
flux show | hide | toggle
flux daemon       # start in the background without showing the window
flux quit         # stop the daemon (also: "Quit Flux" in the launcher)
flux --dev ...    # reload extensions when their files change
flux install-desktop   # create the desktop entry the global-shortcut portal needs (once)
```
Only one daemon runs: a second `flux` just forwards its command through a unix socket
(`$XDG_RUNTIME_DIR/flux.sock`, mode 0600; stale sockets from crashes are detected and replaced).

**Esc** goes back one layer: close the Action Panel → previous screen → clear the search → hide the window
(the process stays alive). When shown again the launcher restores the screen and search you left, unless it was hidden
for longer than `reset_after_secs`, in which case it opens at a clean root.

## Global shortcut
Wayland compositors do not let applications grab keys, and not every compositor offers the same mechanism:

1. **Portal (`GlobalShortcuts`)** — works where the portal backend implements it (xdg-desktop-portal-hyprland, -kde, -gnome).
   Flux registers the shortcut `dev.flux.Launcher:toggle` with the preferred trigger from `config.toml`; the **compositor still
   decides the actual keys**. Run `flux install-desktop` once (the portal identifies the app by `dev.flux.Launcher.desktop`),
   restart the daemon, then:
   * Hyprland (Lua config): `hl.bind("ALT + SPACE", hl.dsp.global("dev.flux.Launcher:toggle"))`
   * Hyprland (classic config): `bind = ALT, space, global, dev.flux.Launcher:toggle`
   * Check with `hyprctl globalshortcuts`; test without a key press: `hyprctl dispatch 'hl.dsp.global("dev.flux.Launcher:toggle")'`.
2. **Any compositor / X11 / no portal** — bind a key to the CLI, which works everywhere:
   * Hyprland: `hl.bind("ALT + SPACE", hl.dsp.exec_cmd("flux toggle"))` / `bind = ALT, space, exec, flux toggle`
   * sway/i3: `bindsym Mod1+space exec flux toggle`
   * KDE/GNOME: add a custom shortcut running `flux toggle`.

If the portal is missing, Flux logs one warning and keeps working through the CLI.

## `~/.config/flux/config.toml`
```toml
hotkey = "ALT+space"        # preferred trigger sent to the portal (a hint)
reset_after_secs = 60       # hidden longer than this => open at a clean root
reduce_motion = false       # true: loading indicators do not animate
extension_dirs = ["/path/to/extensions"]   # replaces the defaults (FLUX_EXTENSIONS_DIR overrides it)

[limits]                    # per extension
memory_mb = 64
slice_ms = 500              # max CPU time between two yields
command_timeout_s = 60      # max wall-clock per command/callback
```
Unknown keys are rejected (a broken file is reported and ignored; defaults are used).

## Secrets
Password preferences and `flux.storage.secure` use the Secret Service over D-Bus (`oo7`). Without one running and unlocked,
they fail with a clear error; they never fall back to plaintext.

```toml
[restart]            # see lifecycle.md
base_ms = 1000
max_ms = 60000
max_crashes = 5
window_s = 300
stable_s = 30

[host]               # see security.md and resources.md
isolation = "process"   # or "thread" (no isolation)
sandbox = "strict"      # or "best-effort" / "off"
cgroup = true
```
`flux status [--json]` asks the running daemon for per-extension resource use. Extension logs are in `$XDG_STATE_HOME/flux/logs`;
extensions are also discovered in `$XDG_DATA_DIRS/flux/extensions`. `flux daemon` does not fork (use your session startup or `dist/flux.service`).
