# Writing Lua extensions

Everything is Lua; you never touch Rust or GPUI. Start here, then follow the links.

```sh
flux create my-extension --template list   # list | grid | form | detail
cd my-extension && flux dev                # loads it in the launcher, reloads on save, shows logs and errors
flux validate && flux package && flux install my-extension-0.1.0.fluxext
```

| Guide | What it covers |
|---|---|
| [ui-components.md](ui-components.md) | `List`, `Grid`, `Detail`, `Form`, `Image`, `Icon`, `ProgressBar`, `LoadingIndicator`, actions |
| [reactivity.md](reactivity.md) | `ui.view{state, render}`, events, tasks, cancellation, how updates stay cheap |
| [http.md](http.md) | `flux.http`, `flux.task`, `flux.json` |
| [manifest.md](manifest.md) | `manifest.lua`, permissions, `allowed_hosts`, preferences |
| [cli.md](cli.md) | `flux create/dev/validate/package/install/list/remove/enable/disable/logs/reload` |
| [management.md](management.md) | installing, enabling, errors and logs, "Manage Extensions" |
| [security.md](security.md) | sandbox, limits, `pcall` hardening, what is *not* a sandbox, proposed process isolation |
| [daemon.md](daemon.md) | resident daemon, global shortcut, `config.toml` |
| [adding-apis.md](adding-apis.md) | for Flux contributors: new functions, namespaces and components |

## Layout
```
my-ext/
├── manifest.lua       # required
├── preferences.lua    # optional: settings shown by "Configure <ext>"
├── init.lua           # optional: runs when the extension (re)loads
├── commands/<name>.lua  # one file per command, defines `function main()`
└── any/other.lua      # modules: `require("any.other")` (never leaves the extension folder)
```
Extensions are looked up in `./extensions`, `~/.config/flux/extensions` and the install directory
(`~/.local/share/flux/installed`); `FLUX_EXTENSIONS_DIR` (`:`-separated) overrides the first two.

## A first command
```lua
local ui = flux.ui

function main()
    return ui.view {
        state = { n = 0 },
        render = function(self)
            return ui.List {
                ui.List.Item {
                    key = "counter", title = "Clicked " .. self.state.n .. " times",
                    actions = { ui.Action { title = "Add one", on_action = function() self:set { n = self.state.n + 1 } end } },
                },
            }
        end,
    }
end
```
`main()` returns a screen; callbacks may be asynchronous (HTTP, processes, `flux.task.sleep`… suspend the coroutine and never
block the launcher). Legacy `flux.ui.list{ items = … }` / `flux.ui.input{…}` keep working.

## Actions and the Action Panel (Ctrl+K)
```lua
actions = {
    ui.Action { title = "Open", description = "In the browser", on_action = open },        -- 1st = Enter
    ui.Action { title = "Copy link", on_action = copy },                                   -- 2nd = Ctrl+Enter
    ui.ActionPanel.Section { title = "Danger",
        ui.Action { title = "Delete", style = "destructive", shortcut = "ctrl+d",
                    confirm = { title = "Delete it?", message = "Cannot be undone" }, on_action = delete } },
}
```
- Props: `title`, `description`, `shortcut` (`"ctrl+shift+c"`, `"alt+up"`…), `style` (`default|secondary|destructive`),
  `confirm` (`true`, a string, or `{title, message}`: the launcher asks **before** running; Enter confirms, Esc cancels), `on_action`.
- First action = **Enter**, second = **Ctrl+Enter**, unless they declare a shortcut. Shortcuts fire directly from the list.
- `actions` on a view = *global* actions, shown after the focused item's own. Forms use Ctrl+Enter to submit.
- Helpers: `flux.action.copy{ title, text }`, `flux.action.open{ title, url }`.

## Navigation
`self:push(view)` / `self:pop()` inside a `ui.view`, or `flux.navigation.push/replace/pop`. `main()` returning a view opens the first
screen; **an action returning a view replaces the current screen**. Esc goes back; each screen keeps its search text, selection and
form values. Leaving a screen cancels its tasks (including HTTP requests and child processes).
`on_appear` / `on_disappear` props run when a screen opens / closes.

## Preferences
`preferences.lua` returns a list:
```lua
return {
    { name = "username", title = "Username", type = "text", default = "friend" },
    { name = "token", title = "API token", type = "password", required = true },
    { name = "verbose", title = "Verbose", type = "checkbox", default = false },
    { name = "theme", title = "Theme", type = "dropdown", default = "dark",
      options = { { title = "Dark", value = "dark" }, { title = "Light", value = "light" } } },
}
```
- Edited in the launcher with **Configure <Extension>** (also in the Ctrl+K panel of the extension's commands).
- If a `required` preference has no value, running any command of the extension opens that screen instead.
- `flux.preferences.get(name)`, `flux.preferences.getAll()`, `flux.preferences.set(name, value)` (values are validated against the declaration).
- Values are stored per extension in SQLite, **except `password` preferences, which live in the system keyring** (Secret Service).

## Secure storage
`flux.storage.secure.get(key)`, `.set(key, value)`, `.remove(key)` — per-extension, backed by the Secret Service
(gnome-keyring, KWallet, KeePassXC). If no Secret Service is available the calls raise
`secure storage unavailable: …`; **nothing is ever written to disk in plaintext as a fallback**.

## Secure storage
`flux.storage.secure.get/set/remove` — per-extension, backed by the Secret Service. Without one the calls raise
`secure storage unavailable: …`; nothing is ever written to disk in plaintext. Optional credentials should be read with `pcall`.

## Available API
| Namespace | Functions | Permission |
|---|---|---|
| `flux.ui` | components (see [ui-components.md](ui-components.md)), `view`, `close()` | – |
| `flux.http` | `get/post/put/patch/delete/request` | `network` (+ `allowed_hosts`) |
| `flux.task` | `token()`, `sleep(seconds)` | – |
| `flux.json` | `encode(v, pretty?)`, `decode(text)` | – |
| `flux.navigation` | `push`, `replace`, `pop` | – |
| `flux.clipboard` | `getText()`, `setText(s)`, `clear()` | `clipboard` |
| `flux.storage` | `get/set/remove/clear`, `cache.get/set(k, v, ttl)`, `secure.*` | – |
| `flux.preferences` | `get`, `getAll`, `set` | – |
| `flux.process` | `exec(cmd, args?, {cwd?, timeout?, signal?})`, `spawn(cmd, args?)`, `kill(pid, signal?)` | `process` |
| `flux.filesystem` | read-only: `home`, `list`, `stat`, `exists`, `read_text`, `open`, `join/basename/dirname/extension` | `filesystem` |
| `flux.browser` | `open(url)` (http/https/mailto) | `browser` |
| `flux.logger` | `debug/info/warn/error(...)` (rate-limited, shown by `flux logs`) | – |
| `flux.environment` | `app_version, extension_id, command_id, extension_dir, data_dir, os` | – |
| `flux.lifecycle` | `on_unload(fn)` | – |

Still unimplemented (raise `"… is not implemented yet"`): `application, window, keyboard, notification, dialog, scheduler, ai`,
`clipboard.getImage/setImage`, `process.getOutput/getRunningProcesses`.
`local` is reserved in Lua, so LocalStorage is directly `flux.storage`.

## Example extensions
`extensions/`: **github** (HTTP, debounced search, README detail), **calculator** (safe expression parser, history),
**files** (list/grid, previews, thumbnails), **processes** (live list, confirmed SIGTERM/SIGKILL), **demo** (every component).
