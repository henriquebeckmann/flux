# Permissions

The manifest only **requests** permissions. What an extension may do is `manifest ∩ user overrides`, and it is checked on every call
twice: by the host's `flux.*` stub, and again by the launcher that performs the operation (using its own copy of the policy).

| Capability | Allows | Narrowed by |
|---|---|---|
| `network` | `flux.http`, remote images | `allowed_hosts` (exact or `*.suffix`) |
| `filesystem.read` | `flux.filesystem.list/stat/exists/read_text/open/home`, absolute image paths | `grants.fs_read` (directories) |
| `filesystem.write` | `flux.filesystem.write_text` | its own data folder + `grants.fs_write` |
| `process.execute` | `flux.process.exec/spawn` | `grants.exec` (exact command string: `xdg-open` or `/usr/bin/ps`); every command is confined unless listed in `grants.exec_unconfined` |
| `process.kill` | `flux.process.kill` | own processes only, unless `grants.kill = "user"` |
| `clipboard.read` / `clipboard.write` | `flux.clipboard.getText` / `setText, clear` | — |
| `browser.open` | `flux.browser.open` | — |
| `credentials.read` / `credentials.write` | `flux.storage.secure.get` / `set, remove` | — |

`application.read`, `window.control`, `notification.send` are **reserved**: no API uses them yet, so manifests asking for them are rejected.

```lua
permissions = { "network", "process.execute", "filesystem.read" },
allowed_hosts = { "api.github.com" },
grants = { exec = { "xdg-open", "ps" }, exec_unconfined = { "xdg-open" }, kill = "own", fs_read = { "~/Documents" }, fs_write = { "~/Downloads" } },
```

* **Compatibility.** Old names still load, with a warning: `process` → `process.execute` + `process.kill`, `clipboard` → read + write,
  `filesystem` → `filesystem.read`, `browser` → `browser.open`; `notifications`, `keyboard`, `application`, `window` never did anything.
  Without `grants.exec`, any command may run in API 1 (legacy; `flux validate` warns). **API 2 requires `grants.exec`.**
* **Credentials** have always worked without a declaration: until a manifest declares `credentials.read` or `credentials.write`, both are
  granted implicitly. Declaring either switches to the strict model.

## The user's side
`~/.config/flux/permissions.toml` (written by the CLI, read on every extension (re)load; a broken file is reported and ignored):

```toml
[extensions.github]
revoke = ["clipboard.write"]
hosts = ["api.github.com"]       # can only narrow what the manifest declared
```
```
flux permissions github                       # what it asked for / what it has
flux permissions github --revoke network      # take away (reloads the extension)
flux permissions github --restore network
flux permissions github --json
```
An override can only take away: entries the manifest never declared are ignored. Manage Extensions shows the effective permissions.
