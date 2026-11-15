# Adding an API to the runtime

Crates: `flux-cli` (command line, no GPUI) sits beside `flux-core`.

Architecture (each arrow is a crate dependency; the runtime does **not** know GPUI):
```
flux (bin, ui/ GPUI) ─┬→ flux-core ─→ flux-runtime ─→ flux-api (pure contracts)
                      └→ flux-platform (OS integration: ipc socket, portal hotkey, keyring) ─→ flux-api
```
The UI and runtime only exchange `RuntimeRequest` / `RuntimeEvent` (`crates/flux-api`). Navigation, the Action Panel
model, search history and show/hide state live in `flux-core` (`Navigator`, `ActionPanelModel`, `Core`) so they are testable
without GPUI; `src/ui` only draws them. Each extension runs in its own thread/Lua state (`flux-runtime/src/worker.rs`).

## New function in an existing namespace
1. In `crates/flux-runtime/src/api.rs`, add the function to the namespace's builder (`clipboard`, `process`…). Per-run data (extension, permissions, storage, secrets, views) is in `CmdCtx` (`ctx.rs`). Use `lua.create_function` (sync) or `create_async_function` (does not block the runtime).
2. If it is sensitive, call `ctx.require(Permission::X)?` at the start.
3. If it is more natural in Lua (sugar, defaults), write it in `src/prelude.lua`, inside `M.extend`.
4. Remove the namespace from the `STUBS` list in `prelude.lua` once it really exists.
5. Add a test in `crates/flux-runtime/tests/runtime.rs` and document it in `docs/extensions.md`.

## New namespace
Create `fn my_ns(lua, ctx) -> Result<Table>`, register it in `build_flux` and remove its name from `STUBS`. For a new permission, add a variant to `flux_api::Permission` (`parse` and `as_str`).
If you need persistent state, extend the `flux_api::Storage` trait (implemented by `flux_core::Db`).

## New UI component
1. `flux-api/src/view.rs`: the data structs (and a `ViewNode` variant for a whole screen). Give collection items a `key` (`Keyed`).
2. `flux-api/src/patch.rs`: teach `diff`/`apply` about it (keyed ops); extend the property test.
3. `flux-runtime/src/prelude.lua`: a `component("Kind")` constructor under `ui`; `flux-runtime/src/build.rs`: convert and validate it (use `Props::new` with the allowed prop list so typos are reported; register callbacks with `b.callback(path, fn)` using a stable path).
4. `flux-core`: model/behaviour that needs no window (`visible.rs`, `gridnav.rs`, `form.rs`, `markdown.rs`, `Core`); add tests there.
5. `src/ui`: render it (`launcher.rs`, `form_ui.rs`, `markdown_view.rs`, `icons.rs`). The UI only draws core state.
6. Tests in `crates/flux-runtime/tests/components.rs`, docs in `docs/ui-components.md`.

## Compatibility
Adding functions/fields is safe; do not rename or remove existing APIs without keeping an alias.
