# Extension lifecycle

Events (`RuntimeEvent`): `ExtensionLoaded{generation}`, `ExtensionUnloaded`, `ExtensionReloaded`, `ExtensionCrashed{reason}`
(the last three carry the ids of the screens the old worker owned), `CommandStarted`, `CommandFinished`, `CommandCancelled`.

* **Generations.** Every start of an extension gets a new generation. Events and screen ownership of an outdated worker are dropped, so
  a replaced or abandoned worker can never affect its successor.
* **Screens.** When an extension is reloaded, disabled, removed or crashes, the launcher closes every screen it owned (and its loading
  placeholders) and says why in the footer. Nothing stays on screen pointing at a dead worker.
* **Commands.** A command whose task is dropped before finishing (cancelled, unloaded) reports `CommandCancelled`.
* **Unload order:** cancel tasks → `on_unload` hooks (1 s each, 3 s in total) → drop screens → stop the host (kill after the grace period).
  `on_unload` does **not** run when an extension crashes or is killed.
* **Crashes.** Restart with exponential backoff (`[restart]`: `base_ms=1000`, `max_ms=60000`); `max_crashes=5` within `window_s=300`
  puts it in *Failed* until reloaded by hand; staying up `stable_s=30` forgives earlier crashes. A host that cannot be confined is *Failed*
  immediately (retrying would not help). Commands for an extension that is restarting/failed get an explicit message.
* **Hot reload** (`--dev`, `flux reload`): per extension; the old worker is unloaded, the new one starts with a new generation.
