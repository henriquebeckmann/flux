# State, events and updates

A reactive screen is declared once and re-rendered when its state changes:

```lua
return ui.view {
  state = { query = "", repos = {}, loading = false },     -- initial local state
  init = function(self) end,                                -- optional, runs once (sync: start work with self:task)
  render = function(self) return ui.List { … } end,         -- a PURE function of self.state
}
```

## `self`
| | |
|---|---|
| `self.state` | read-only view of the state (`self.state.x = 1` raises) |
| `self:set{ k = v }` / `self:unset("k")` / `self:update(fn)` | change state; `update`'s `fn(state)` returns a patch |
| `self:task(name, fn)` | run `fn` in the background. Starting a task with the same name **cancels the previous one** ("latest wins"); closing the screen cancels all of them. No wall-clock limit, but every stretch of Lua between two `await`s is CPU-limited |
| `self:cancel(name)` | cancel a task |
| `self:push(view)`, `self:pop()`, `self:close()` | navigate / hide the launcher |

`self:set` inside `render` is an error. Calling async APIs (`flux.http`, `flux.preferences.get`…) is only possible in callbacks and
tasks, not in `render`/`init` (they are synchronous).

## How an update works
1. `set` marks the screen dirty; renders are **coalesced** (all `set`s in a callback, or within ~8 ms, produce one render).
2. `render` returns a component tree, which is validated and converted to a typed tree.
3. The runtime diffs it against the previous tree **by key** (sections, items, form fields) and sends only the patch
   (`Update`/`Insert`/`Remove`/`Move` operations; a kind change replaces the screen). An unchanged render sends nothing.
4. The launcher applies the patch in place: the selected item stays selected **by key** even if it moves, form input and scroll
   position are kept, and only changed rows are re-drawn.
Callback ids are derived from the position/key of the prop in the tree, so unchanged items produce identical data and a click on
a row drawn before the re-render still works. A failing `render` keeps the last good screen and reports the error (footer + log).
Diffing 5 000 items with one change costs one operation (tested).

## Events (props on List/Grid/Detail/Form)
| Prop | When |
|---|---|
| `on_search_change(text)` | the search field changed (only sent when declared); the previous handler still running is cancelled |
| `on_selection_change(key)` | the selected item changed (`nil` when nothing matches) |
| `on_key = { ["ctrl+d"] = fn }` | view-scoped shortcuts, checked before item actions |
| `on_action` | an action ran |
| form fields: `on_change(value)`, `validate(value)`; form: `validate(values)`, `on_submit(values)` | see [ui-components.md](ui-components.md) |
| `on_appear`, `on_disappear` | the screen opened / closed |

## Loading and cancellation
`is_loading = true` shows the bar. `flux.task.token()` returns a cancellation token (`:cancel()`, `:is_cancelled()`) accepted as
`signal` by `flux.http` and `flux.process.exec`; closing a screen, Esc during loading, reloads and hiding-with-reset cancel
automatically (HTTP requests are aborted and child processes killed *and reaped*). `flux.task.sleep(s)` is the non-blocking
wait for debouncing and polling loops.

```lua
on_search_change = function(text)
  self:set { query = text, loading = true }
  self:task("search", function()
    flux.task.sleep(0.3)                                    -- debounce
    local res, err = flux.http.get(URL, { query = { q = text } })
    self:set { repos = res and res:json().items or {}, loading = false, error = err and err.message }
  end)
end
```
