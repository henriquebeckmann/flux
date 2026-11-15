local ui = flux.ui

local ICONS = { "star", "folder", "globe", "bell", "book", "calendar", "clock", "code", "key", "lock", "music", "tag" }

function main()
    local cells = {}
    for _, name in ipairs(ICONS) do
        cells[#cells + 1] = ui.Grid.Item {
            key = name,
            title = name,
            content = ui.Icon { name = name },
            actions = {
                ui.Action {
                    title = "Copy icon name",
                    on_action = function()
                        flux.clipboard.setText(name)
                        flux.ui.close()
                    end,
                },
            },
        }
    end
    return ui.Grid {
        size = "small", -- small | medium | large | { width = 100, height = 80 }
        search_bar_placeholder = "Search icons…",
        ui.Grid.Section { title = "Built-in icons", items = cells },
    }
end
