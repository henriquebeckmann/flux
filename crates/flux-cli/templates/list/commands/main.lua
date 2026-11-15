local ui = flux.ui

local FRUITS = { "Apple", "Banana", "Cherry", "Dragon fruit", "Elderberry", "Fig", "Grape" }

function main()
    return ui.view {
        state = { picked = nil },

        -- render is a pure function of self.state: Flux diffs the result and updates only what changed.
        render = function(self)
            local items = {}
            for i, name in ipairs(FRUITS) do
                items[#items + 1] = ui.List.Item {
                    key = name, -- keys keep the selection when the list changes
                    title = name,
                    subtitle = "Fruit #" .. i,
                    icon = "star",
                    accessories = { { tag = { text = tostring(#name) .. " letters", color = "blue" } } },
                    actions = {
                        ui.Action {
                            title = "Copy name",
                            on_action = function()
                                flux.clipboard.setText(name)
                                flux.ui.close()
                            end,
                        },
                        ui.Action {
                            title = "Mark as picked",
                            shortcut = "ctrl+p",
                            on_action = function() self:set { picked = name } end,
                        },
                    },
                }
            end
            return ui.List {
                search_bar_placeholder = "Search fruit…",
                ui.List.Section { title = self.state.picked and ("Picked: " .. self.state.picked) or "All fruit", items = items },
                ui.List.EmptyView { title = "No fruit matches", icon = "search" },
            }
        end,
    }
end
