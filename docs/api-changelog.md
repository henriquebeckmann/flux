# API changelog

## API 1 (Flux 0.1)
Additions since the first API 1 builds (all backwards compatible):
* Permissions split into capabilities (`process.execute`, `clipboard.write`, …) and `grants`; old names still load with a warning.
* `flux.filesystem.write_text`, `flux.process.exec` result field `truncated`, `flux.http` error kind `too_many_requests`.
* `api_version` accepts a string; `min_flux_version`.

Deprecated (still working): `flux.ui.list` → `flux.ui.List`, `flux.ui.input` → `flux.ui.Input`; legacy permission names.

Behaviour that now has limits (see [resources.md](resources.md)): live coroutines, concurrent HTTP requests, background processes,
`exec` output size and timeout, `http` `max_bytes`, tasks running at once.

* `flux.process.exec/spawn`: in process-isolated deployments commands run confined; list the ones that need the desktop in `grants.exec_unconfined`.
* `flux.process.kill`: only processes the extension started, unless `grants.kill = "user"`. **This narrows behaviour**: extensions that signal other processes must declare it (the Process Manager example does).
* `flux.http`: bodies are capped at 8 MB (`http_max_mb`) because the launcher returns them in a single message.

API 2 (not released): `process.execute` without `grants.exec` is rejected.
