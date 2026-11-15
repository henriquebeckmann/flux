# Extension Host protocol

Launcher ⇄ host over a socketpair the host inherits as fd 3. Frames: `u32` little-endian length + JSON
`{ "v": <protocol>, "id": <n>, "corr": <id answered>?, "body": { "kind": …, … } }`. Max frame 16 MiB. Types: `flux_api::wire`.

**Handshake.** Core sends `init` (`proto_min`, `proto_max`, extension, generation, folders, limits, the effective policy, sandbox mode…).
The host picks `min(proto_max, its own)`; if that is below `proto_min` it answers `failed`. It confines itself, loads the extension, then
answers `ready { proto, sandbox }` where `sandbox` says what was actually applied.

**Core → host:** `run`, `invoke`, `submit`, `search`, `selection`, `form_changed`, `form_submit`, `view_closed`, `cancel_command`,
`unload`, `ping`, `reply` (answer to a call, `corr` = the call's `id`).
**Host → core:** `ready`, `failed`, `event` (a `RuntimeEvent`), `call` (storage, secrets, `privileged { namespace, function, args }`),
`cancel_call`, `pong { busy_ms, stats }`, `unloaded`.

* Binary strings in `privileged` arguments/results travel as `{"$b64": "…"}`. A reply that does not fit in a frame is replaced by an error reply, never dropped.
* The extension's identity is the **connection**; no message names it. A `privileged` call can only name functions on the broker's allow-list.
* **Cancellation:** `cancel_command` for commands; `cancel_call` aborts a brokered call (a running process is killed).
* **Timeouts:** blocking storage calls fail after 15 s; pings every 500 ms, no `pong` for 5 s ⇒ the host is killed.
* **Unexpected exit:** reader sees EOF ⇒ crash event with the exit status and the host's last stderr lines.
* **Evolution:** receivers ignore unknown fields and unknown `kind`/`op` values (decoded as `unknown`); additive changes keep the protocol
  version. Anything removed or re-purposed bumps `PROTOCOL_VERSION`; hosts and launchers negotiate the highest common one.
* **Violations** (oversized/truncated/non-JSON frame, version below the minimum, a host that is not as confined as strict mode demands)
  drop and kill the host; they are reported as the crash reason.
