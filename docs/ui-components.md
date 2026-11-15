# UI components

Components are callable tables: `ui.List{ props…, child, child }`. Children are positional; props are named. Mistakes raise
errors with the full path, e.g. `List.Item[3].accessories[1].progress: expected a number between 0 and 1, got 2`, and unknown
props suggest the right name (`unknown prop 'titel' (did you mean 'title'?)`). Every view is **keyed**: give items a unique
`key` so selection, scroll position and form input survive updates (items without one get a positional key).

Return a component from `main()` (static screen) or wrap it in [`ui.view`](reactivity.md) (reactive screen).

## List
```lua
ui.List {
  title = "Things", search_bar_placeholder = "Search…",
  filtering = true,          -- false: show exactly what you send (remote search)
  is_loading = false,        -- thin progress bar under the search field
  selected_key = "b",
  on_search_change = function(text) end, on_selection_change = function(key) end,
  on_key = { ["ctrl+d"] = function() end },   -- view-scoped shortcuts
  actions = { … },           -- global actions
  ui.List.Section { key = "a", title = "Recent", subtitle = "…", items = { … } },   -- or positional children
  ui.List.Item { key = "x", title = "…", subtitle = "…", icon = "folder", keywords = { "alias" },
                 accessories = { { text = "2m" }, { tag = { text = "new", color = "green" } },
                                 { icon = "star", progress = 0.4, tooltip = "…" }, "plain text" },
                 actions = { … } },
  ui.List.EmptyView { title = "Nothing here", description = "…", icon = "search" },
}
```
Fuzzy filtering searches title, subtitle and `keywords` (title matches rank first); empty sections disappear.

## Grid
```lua
ui.Grid { size = "small"|"medium"|"large"|{ width = 120, height = 80 }, columns = 4, fit = "contain"|"fill",
  ui.Grid.Section { title = "Pictures", items = { ui.Grid.Item { key = "a", title = "A", subtitle = "…",
      content = ui.Image { path = "assets/a.png", mask = "circle" } | ui.Icon { name = "folder" } | "text", actions = { … } } } },
  ui.Grid.EmptyView { … } }
```
Arrow keys move in two dimensions (a shorter row clamps to its last cell), sections start on a new row, columns follow the
window width unless forced. A string `content` is an image when it looks like a path/URL, an icon if it is a built-in name,
else text.

## Detail
```lua
ui.Detail { title = "…", markdown = "# Hi", is_loading = false,
  ui.Detail.Markdown { "more markdown" }, ui.ProgressBar { value = 0.4, label = "Copying" }, ui.LoadingIndicator { label = "…" },
  metadata = ui.Detail.Metadata {
    ui.Detail.Metadata.Label { title = "Stars", text = "12", icon = "star" },
    ui.Detail.Metadata.Separator {},
    ui.Detail.Metadata.TagList { title = "Topics", ui.Detail.Metadata.TagList.Item { text = "lua", color = "blue" } },
    ui.Detail.Metadata.Link { title = "Site", text = "example.com", url = "https://example.com" } },
  actions = { … } }
```
Markdown: headings, emphasis, strikethrough, `code`, links, lists (nested, ordered, tasks), quotes, code blocks, tables, rules.
Raw HTML is dropped and images become their alt text. Links open only if they are `http(s)` or `mailto`. ↑/↓/PgUp/PgDn scroll.

## Form
```lua
ui.Form { title = "…", submit_title = "Save",
  validate = function(values) if values.name == "root" then return { name = "reserved" } end end,   -- cross-field, runs on submit
  on_submit = function(values) … end,
  ui.Form.TextField { id = "name", title = "Name", placeholder = "…", default_value = "", required = true,
                      min_length = 2, max_length = 40, format = "email"|"url"|"integer"|"number", min = 0, max = 10,
                      info = "help text", autofocus = true, on_change = function(v) end, validate = function(v) end },
  ui.Form.PasswordField { id = "pw" },  ui.Form.TextArea { id = "bio", rows = 4 },
  ui.Form.Checkbox { id = "tos", label = "I agree", required = true },
  ui.Form.Dropdown { id = "role", ui.Form.Dropdown.Item { value = "a", title = "A" }, ui.Form.Dropdown.Section { title = "G", … } },
  ui.Form.TagPicker { id = "tags", default_value = { "a" }, ui.Form.TagPicker.Item { value = "a", title = "A" } },
  ui.Form.DatePicker { id = "when", type = "date"|"datetime", min = "2026-01-01" },   -- ISO-8601 values
  ui.Form.FilePicker { id = "files", multiple = true, directories = false, files = true },
  ui.Form.Separator {}, ui.Form.Description { text = "…" } }
```
`values` is a table keyed by field id (text, boolean, or list of strings). Rules (`required`, lengths, `format`, `min/max`, date
bounds) are evaluated **instantly in the launcher**; only a valid form reaches `validate` and `on_submit`. Errors returned by
`validate` (`{ field = "message" }`) appear under the fields and clear when the user edits them.
`default_value` is applied once; `value` makes a field *controlled* (applied whenever the prop changes between renders) —
re-rendering never overwrites what the user typed. Keys: **Tab/Shift+Tab** move, **Ctrl+Enter** submits (Enter on the last
single-line field too), calendar: arrows/PgUp/PgDn/Home, Enter selects. FilePicker: *Browse…* uses the desktop's file-chooser
portal; if none exists, type an absolute path and press Enter (existence and file/folder kind are checked).

## Image, Icon, ProgressBar, LoadingIndicator
- `ui.Image{ path = "assets/a.png" }` (inside the extension folder; absolute paths need `filesystem`) or `ui.Image{ url = "https://…" }`
  (needs `network` **and** the host in `allowed_hosts`; downloaded to a cache with a 5 MB cap, public addresses only). Options:
  `mask = "circle"|"rounded"`, `fallback = "image"` (icon/text while loading or on failure).
- `ui.Icon{ name = "folder" }` (built-in), `ui.Icon{ text = "👋" }`, `ui.Icon{ image = ui.Image{…} }`, all with `tint = "red"|"#rrggbb"`.
  A bare string is a built-in name when it is one, otherwise text. Built-in names: app archive bell book bookmark calculator calendar check chevron-right clipboard clock code copy cpu download external-link file file-text film folder folder-open gear git-branch globe hard-drive history image info key link list lock music package plus refresh search star tag terminal trash user warning x.
- Colors: red orange yellow green blue purple pink gray, or `#rgb`/`#rrggbb`.
- `ui.ProgressBar{ value = 0..1 | nil (indeterminate), label }` and `ui.LoadingIndicator{ label }` are Detail blocks; `is_loading = true` on any
  view shows the indeterminate bar under the search field.
