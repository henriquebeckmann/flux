local ui = flux.ui

function main()
    return ui.view {
        state = { progress = 0 },

        init = function(self)
            self:task("progress", function()
                for i = 1, 20 do
                    flux.task.sleep(0.25)
                    self:set { progress = i / 20 }
                end
            end)
        end,

        render = function(self)
            local M = ui.Detail.Metadata
            local done = self.state.progress >= 1
            return ui.Detail {
                title = "Demo detail",
                is_loading = not done,
                markdown = [[
# Detail pages

Render **Markdown**: *emphasis*, `inline code`, ~~strikethrough~~ and [links](https://example.com).

- bullet lists
  - nested ones
- [x] task lists

1. numbered
2. lists

> A block quote.

```lua
local ui = flux.ui
return ui.Detail { markdown = "# Hello" }
```

| Component | Status |
|-----------|--------|
| List      | ✓ |
| Grid      | ✓ |
| Detail    | ✓ |
| Form      | ✓ |
]],
                ui.ProgressBar { value = self.state.progress, label = done and "Done" or "Working…" },
                metadata = M {
                    M.Label { title = "Component", text = "Detail", icon = "info" },
                    M.Label { title = "Updates", text = "live (state + render)" },
                    M.Separator {},
                    M.TagList { title = "Features", M.TagList.Item { text = "markdown", color = "blue" }, M.TagList.Item { text = "metadata", color = "green" }, M.TagList.Item { text = "progress", color = "orange" } },
                    M.Link { title = "Docs", text = "docs/ui-components.md", url = "https://example.com" },
                },
                actions = { ui.Action { title = "Restart progress", on_action = function() self:set { progress = 0 }; self:task("progress", function() for i = 1, 20 do flux.task.sleep(0.25); self:set { progress = i / 20 } end end) end } },
            }
        end,
    }
end
