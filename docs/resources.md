# Resource limits

Every limit is configurable; a request over a limit fails with a message naming the limit (nothing is queued silently).

`[limits]` (Lua state and brokered operations), `[host]` (process isolation), in `~/.config/flux/config.toml`:

| Key | Default | Why |
|---|---|---|
| `memory_mb` | 64 | Lua heap of one extension; a leak or allocation bomb fails inside the VM |
| `slice_ms` | 500 | max CPU between suspension points; interactive code never needs more |
| `command_timeout_s` | 60 | wall-clock of one command/callback |
| `max_tasks` | 64 | commands + callbacks + `self:task` running at once (per-screen cap of 32 also applies) |
| `max_coroutines` | 1000 | live coroutines; finished ones do not count |
| `max_http_concurrent` | 8 | requests in flight; browsers use 6 per host |
| `http_max_mb` | 8 | ceiling for a request's `max_bytes`; the launcher returns the body in one message (≤ 16 MiB frame) |
| `max_children` | 16 | live `flux.process.spawn` children |
| `exec_output_kb` | 10240 (4096 when brokered) | kept per stream of `flux.process.exec`; the rest is read and dropped (`truncated = true`) |
| `exec_timeout_max_s` | 600 | ceiling for `exec` timeouts |
| `[host] memory_mb` | 512 | cgroup `MemoryMax` of a host process (covers Rust buffers the Lua limit cannot see) |
| `[host] cpu_percent` | 100 | cgroup `CPUQuota` (100 = one core) |
| `[host] max_tasks` | 64 | cgroup `TasksMax` (threads) |
| `[host] max_open_files` | 256 | `RLIMIT_NOFILE` |
| `[host] ping_timeout_ms` | 5000 | a host silent this long is killed |
| `[host] isolation` / `sandbox` / `cgroup` | process / strict / true | see [security.md](security.md) |

The defaults are deliberately generous for launcher-style extensions (a list, a few requests, a few commands) and small compared to
the machine; they are starting points, not measurements. The cgroup limits only apply when a systemd user manager is present;
otherwise `flux status` still shows usage but memory/CPU are not capped (the Lua limits and `RLIMIT_*` still are).

`flux status [--json]` shows, per extension: state, host pid, resident memory, CPU time, tasks, requests in flight, background processes,
recent crashes and isolation.
