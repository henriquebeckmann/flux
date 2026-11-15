# The `flux` command line

```
flux create <id> [--template list|grid|form|detail] [--dir D]   new project (manifest, command, README, .luarc.json, flux.d.lua)
flux dev [path]                    load the folder in the launcher, reload on save, print logs and errors
flux validate [path] [--json]      manifest, structure, permissions, Lua syntax (file:line)
flux package [path] [-o out]       <name>-<version>.fluxext (reproducible zip)
flux install <folder|.fluxext> [--force]   validate, then install atomically (newer versions replace, equal/older need --force)
flux list [--json]                 installed + development extensions with status
flux remove <id> [--purge]         delete an installed extension (--purge also removes its data and preferences)
flux enable|disable <id>           persisted; applies immediately if the launcher runs
flux logs <id> [-n 50] [-f]        the extension's log file
flux reload [id]                   ask the running launcher to reload
flux [toggle|show|hide|daemon|quit|install-desktop] [--dev]
```
Exit codes: **0** success · **1** failed operation (invalid extension, no launcher for `reload`…) · **2** usage error.
Commands that change something notify the launcher over its socket; if it is not running they say so and the change applies on
its next start.

## flux dev
No launcher running: starts it in the foreground (hot reload on) with the folder attached and the window shown. Launcher
already running: attaches the folder over the socket and keeps running until Ctrl+C (then detaches). Either way the terminal shows
`[dev] ok` / `[dev] error: commands/x.lua:12: …` within ~250 ms of every save (the same validator as `flux validate`), and the
extension's log lines as they are written. A reload only restarts the edited extension.
Tips: `flux.logger.info(...)` and `print` are visible in `flux logs`/`flux dev`; errors also appear in the launcher footer.

```
flux permissions <name> [--revoke cap] [--restore cap] [--json]   what an extension may do; take away / give back
flux status [--json]                                             resource use and isolation of every extension
```
