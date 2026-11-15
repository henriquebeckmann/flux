# Security model

Each extension runs in **its own process** (an *Extension Host*, `flux host`) that confines itself with
Landlock, seccomp and resource limits, and that can only reach the outside world through a **broker** in the
launcher which re-checks every permission. This page says what that does and does **not** protect against.
It describes only what is implemented and tested; `isolation = "thread"` (no isolation) exists for development.

```
flux (launcher: Core + UI + Platform + Extension Manager)
 ├─ broker A ◄─ socketpair ─► flux host A  [Lua VM of extension A, confined]
 └─ broker B ◄─ socketpair ─► flux host B  [Lua VM of extension B, confined]
```

## What a host process can and cannot do (verified on the real kernel, `tests/host.rs`)

Applied by the host to itself *before* it reads extension code (`confine.rs`). `flux status` and Manage Extensions show
`process, confined` only when **all** of this is in effect, otherwise `process, partly confined: <what is missing>`.

| Mechanism | Effect |
|---|---|
| `PR_SET_NO_NEW_PRIVS`, `RLIMIT_CORE=0`, `RLIMIT_NOFILE`, `RLIMIT_FSIZE`, `PDEATHSIG` | no privilege gain, no core dumps, bounded fds and file size, dies with the launcher |
| **Landlock** (filesystem) | reads only: the extension folder, minimal system files (`/usr`, `/lib*`, TLS certs, resolver config, `/proc/self`). Writes only: its own data folder. Not `$HOME`, `/etc/passwd`, other processes' `/proc` |
| **Landlock** (network, ABI ≥ 4, Linux 6.7) | no TCP connect at all (second layer behind seccomp) |
| **seccomp** (deny list) | no `execve`, no `fork`/`vfork`, `clone` only for threads, no `ptrace`, `mount`, `bpf`, namespaces, kernel modules, `io_uring`, `kill`; `tgkill` only to itself; **sockets: none, ever** — no network (HTTP is brokered) and no `AF_UNIX`, so a host cannot reach the launcher's control socket |
| cgroup scope (`systemd-run --user --scope`) | `MemoryMax`, `TasksMax`, `CPUQuota` per host, when a systemd user manager is available (`[host] cgroup`) |
| Broker | **HTTP**, process execution (confined, see below), clipboard, browser, files outside the extension's folders, storage and secrets are performed **by the launcher**, with the policy *the launcher* holds for that extension |

**Strict by default.** If a required restriction cannot be applied (no Landlock, no seccomp, …) the host refuses to run the
extension and it is shown as *Failed*, not run unconfined. `[host] sandbox = "best-effort"` runs it and reports what is missing;
`"off"` disables confinement.

## Failure handling
* The launcher notices a dead host immediately (connection closes) and a stuck one by pings (`Pong` carries how long the Lua worker has been
  inside one poll); a stuck or silent host is `SIGKILL`ed — **this includes loops inside C functions**, which a thread could never be
  stopped from. The extension restarts with backoff (see [lifecycle.md](lifecycle.md)).
* Child processes the launcher started for an extension (`flux.process.exec`) are killed when the command is cancelled, the extension
  is disabled/reloaded, or the host crashes. `flux.process.spawn` children are by design allowed to outlive a command (launching apps);
  they are counted and capped (`max_children`) but not killed on unload.
* A host that sends malformed, oversized, unknown-version or non-confined frames is dropped (and killed) without affecting the launcher
  ([protocol.md](protocol.md)).

## Permissions
Declared in the manifest, narrowed by the user, enforced twice (host stub + broker). See [permissions.md](permissions.md).

## Credentials
Password preferences and `storage.secure` live only in the Secret Service (served by the broker); never in SQLite or logs.

## Network
Redirects are re-checked per hop; private ranges are denied by default (also at DNS time); response size is capped; only http/https.
Image URLs in views obey the same host rules and are downloaded by the launcher with the same address checks and a 5 MB cap.

## Remaining risks (read this before trusting the sandbox)
1. **Commands the extension may run are only as safe as their confinement.** Every command runs through `flux host --exec`: `no_new_privs`,
   Landlock (reads system files and the extension's `fs_read` grants, writes only its data folder and `fs_write` grants), seccomp (no sockets
   without `network`, never `AF_UNIX`, no `ptrace`/`mount`/`bpf`/signals to others), so `ps`, `uname`, `head`… work and `cat ~/.ssh/id_rsa` does not.
   Commands the manifest lists in `grants.exec_unconfined` (for example `notify-send`, which needs the desktop session) run with your full access.
   Without the wrapper (thread mode, `sandbox = "off"`) nothing is confined. Programs that need to write outside their data folder, reach
   D-Bus/X11/Wayland, or signal other processes (`timeout`, `pkill`) fail when confined: that is the point, and `exec_unconfined` is the opt-out.
2. **`process.kill`** reaches only processes the extension started, unless the manifest says `grants.kill = "user"` (then any process of
   yours, except Flux's own `flux*` programs). The Process Manager example declares it; treat it like any high-risk permission.
3. **`flux.http` is performed by the launcher.** The host has no network at all (seccomp refuses `socket()`, Landlock allows no TCP), so a
   native-code escape inside a host cannot reach the network. The launcher enforces `allowed_hosts`, the SSRF rules and size limits. Results
   come back in one message: bodies are capped at 8 MB (`http_max_mb`), command output at 4 MB per stream.
4. **Declarations are evaluated in a throw-away confined process** (`flux host --eval`): read-only access to the extension folder, no sockets,
   no `exec`, 3 s and 64 MB. What comes back is data. (`flux validate`/install use the same evaluator when run from the real `flux`
   binary, best-effort where the kernel has no Landlock.) In thread mode, declarations run in the launcher's own Lua state.
5. **The seccomp filter is a deny list**: syscalls added to Linux in the future are allowed until listed. Landlock is the primary barrier.
6. **Filesystem reads outside the extension's folders are brokered and use a denylist** (`~/.ssh`, Flux data, …) plus `grants.fs_read`;
   a denylist is defence in depth. An extension with `exec_unconfined` commands can read what those commands can.
7. Kernel vulnerabilities, side channels and denial of service by the launcher's own resources (event floods from an extension are rate-limited
   only for logs) are out of scope. UI spoofing (an extension controls the text it shows) is not prevented.
8. On kernels without Landlock (or in `best-effort`/`off`/`thread` modes) the guarantees above do **not** hold.
9. A stuck host burns CPU until detected (`slice_ms*4+2 s`), capped by the cgroup quota when available.
10. `spawn`ed children may outlive the extension by design (launching applications); they are counted and capped.

## Lua VM hardening (unchanged)
Memory limit, CPU slice (500 ms and ~50 M VM instructions), wall-clock timeout, and `pcall`/`xpcall`/`coroutine.*` that re-raise once the
guard tripped (so limits cannot be swallowed). Live coroutines are capped (`max_coroutines`).
