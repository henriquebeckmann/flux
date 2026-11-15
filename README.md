# Flux

Raycast/rofi-style launcher in Rust + GPUI (Zed, via git) with Lua extensions (mlua), SQLite and the system keyring.

```
cargo run -- daemon --dev     # start the resident daemon (--dev reloads extensions on change)
cargo run -- toggle           # show / hide it (bind this to a key, or use the portal shortcut)
cargo test --workspace
```
Keys: type to search · ↑/↓ or Ctrl+P/N navigate · Enter run · **Ctrl+K Action Panel** · Ctrl+Enter secondary action ·
Ctrl+↑/↓ search history · Esc back / clear / hide · full text editing (Ctrl+A/C/X/V, Home/End, Ctrl+←/→, Ctrl+Backspace, IME).

On Wayland it uses layer-shell (centered overlay); elsewhere it falls back to a pop-up window.
Authoring: `flux create my-ext && cd my-ext && flux dev` · `flux validate|package|install|list|remove` ([CLI](docs/cli.md)).
Examples in `extensions/`: github, calculator, files, processes, demo.
Docs: [extensions](docs/extensions.md) · [components](docs/ui-components.md) · [reactivity](docs/reactivity.md) · [http](docs/http.md) · [security](docs/security.md) · [permissions](docs/permissions.md) · [protocol](docs/protocol.md) · [lifecycle](docs/lifecycle.md) · [API versioning](docs/api-versioning.md) · [resources](docs/resources.md) · [installing](docs/distribution.md) · [daemon & global shortcut](docs/daemon.md) · [adding APIs](docs/adding-apis.md).
