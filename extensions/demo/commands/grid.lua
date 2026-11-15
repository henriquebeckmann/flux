local ui = flux.ui

local ICONS = { "app", "archive", "bell", "book", "bookmark", "calculator", "calendar", "clipboard", "clock", "code", "cpu", "download",
    "file", "film", "folder", "gear", "globe", "image", "key", "lock", "music", "package", "star", "tag", "terminal", "user" }
local COLORS = { "red", "orange", "yellow", "green", "blue", "purple", "pink", "gray" }

function main()
    return ui.view {
        state = { size = "medium" },

        render = function(self)
            local icons = {}
            for i, name in ipairs(ICONS) do
                icons[#icons + 1] = ui.Grid.Item {
                    key = "icon-" .. name, title = name,
                    content = ui.Icon { name = name, tint = COLORS[(i % #COLORS) + 1] },
                    actions = { ui.Action { title = "Copy name", on_action = function() flux.clipboard.setText(name); flux.ui.close() end } },
                }
            end
            local swatches = {}
            for _, c in ipairs(COLORS) do
                swatches[#swatches + 1] = ui.Grid.Item { key = "color-" .. c, title = c, content = ui.Icon { text = "●", tint = c } }
            end
            local sizes = {}
            for _, size in ipairs { "small", "medium", "large" } do
                sizes[#sizes + 1] = ui.Action { title = "Cell size: " .. size, on_action = function() self:set { size = size } end }
            end
            return ui.Grid {
                size = self.state.size,
                search_bar_placeholder = "Filter icons…",
                actions = sizes,
                ui.Grid.Section { key = "icons", title = "Icons", items = icons },
                ui.Grid.Section { key = "colors", title = "Colors", items = swatches },
            }
        end,
    }
end
