# manifest.lua

```lua
return {
  name = "github",              -- unique id: a-z 0-9 - _ (max 64); prefix of every command id
  title = "GitHub", description = "…", version = "1.2.0",   -- semver (default 0.0.0)
  author = "…", license = "MIT", homepage = "…", icon = "git-branch",
  api_version = 1,              -- major API version (see api-versioning.md); min_flux_version = "0.1.0" optional
  commands = { { name = "search", title = "Search", description = "…", icon = "search", mode = "view"|"no-view" } },
  permissions = { "network", "clipboard.write", "process.execute", "filesystem.read", "browser.open" },
  grants = { exec = { "xdg-open" }, fs_read = { "~/Documents" } },   -- see permissions.md
  allowed_hosts = { "api.github.com", "*.githubusercontent.com" },
}
```
The manifest is evaluated in the sandbox (no `flux`, no I/O). **All** problems are reported at once with the offending key
(`commands[3].name: …`). Unknown keys are warned about by `flux validate`. Each declared command needs `commands/<name>.lua`.

Permissions, grants, user overrides and the legacy names: see [permissions.md](permissions.md).

`preferences.lua` (see [extensions.md](extensions.md)) declares text/password/checkbox/dropdown settings.
