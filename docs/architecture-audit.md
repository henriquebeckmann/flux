# Architecture audit (Stage 4 baseline)

Snapshot taken at the start of Stage 4, before any hardening change. It records
what the code did **then**; later sections of the docs describe the result.

## Crates and dependencies

```
flux-api       serde only; contracts (view tree, patches, requests/events)
flux-runtime   flux-api; mlua, tokio, reqwest  -> supervisor + one worker thread per extension
flux-platform  flux-api; ashpd, oo7, reqwest   -> portal, keyring, daemon socket, images
flux-core      flux-api, flux-runtime; rusqlite -> GPUI-free application logic
flux-cli       flux-api, flux-core, flux-platform, flux-runtime
flux (root)    flux-api, flux-core, flux-platform, flux-cli, gpui
```

UI -> Core is a direct in-process call. Core -> Runtime is an unbounded
`tokio::mpsc` of `RuntimeRequest`; Runtime -> Core is an unbounded
`async_channel` of `RuntimeEvent`. Nothing crossing those channels is
serialized. The daemon socket is a one-shot line protocol without versioning.

## Main risks found

| # | Risk | Where |
|---|------|-------|
| 1 | Threads are not a security boundary; a stuck thread is abandoned, never killed | `supervisor.rs` stop_worker |
| 2 | Abandoned/stale worker keeps emitting events and re-registers view owners (no generation) | `views.rs` instantiate, `Shared.owners` |
| 3 | Views of a reloaded/disabled/crashed extension stay on screen, requests are dropped silently | `worker.rs` unload, `supervisor.rs` owner_of, `navigator.rs` |
| 4 | Crash detected lazily on next send; no backoff; `Crashed` never clears; panic payload lost; init failure loops | `supervisor.rs` send, `worker.rs` |
| 5 | Children: `process.spawn`/`browser.open` untracked and unbounded; `kill` accepts any same-uid pid; `exec` output/timeout uncapped | `api.rs` |
| 6 | Permissions: 4 of 9 names have no enforcement; only runtime layer checks; absolute image paths skip the fs denylist; `max_bytes` uncapped; proxy env may bypass the resolver guard | `api.rs`, `build.rs`, `http.rs` |
| 7 | No lifecycle events (loaded/unloaded/reloaded/crashed, started/cancelled); aborted commands emit no `CommandFinished` | `flux-api/lib.rs` |
| 8 | `api_version`: only upper bound checked, missing means "current" | `manifest.rs` |
| 9 | Distribution: no release profile, packaging, static .desktop/icon/unit; `app_id` mismatch; extensions only from cwd | root |
| 10 | Test gaps: reload/crash with views on the stack, malformed IPC, serde round-trip of requests/events, UI | tests |

## Host capabilities (this machine)

- Landlock ABI **10** (`landlock_create_ruleset(VERSION)`), LSM list includes `landlock`.
- seccomp available through `prctl` (no filter inherited by the shell).
- cgroup v2; `cpu memory pids` delegated to the user manager.
- `systemd-run --user` available; user namespaces allowed.

Other machines may have older kernels. The design therefore reports what was
actually applied instead of assuming it.
