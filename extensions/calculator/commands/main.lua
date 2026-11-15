local calc = require("calc")
local ui = flux.ui

local MAX_HISTORY = 50

local function load_history()
    return flux.storage.get("history") or {}
end

local function save_history(list)
    flux.storage.set("history", list)
end

--- Adds an entry on top; the same expression is kept only once.
local function remember(list, expr, result)
    local out = { { expr = expr, result = result } }
    for _, e in ipairs(list) do
        if e.expr ~= expr and #out < MAX_HISTORY then out[#out + 1] = e end
    end
    return out
end

function main()
    return ui.view {
        state = { query = "", history = load_history() },

        render = function(self)
            local s = self.state
            local sections = {}

            local text = s.query:match("^%s*(.-)%s*$")
            if text ~= "" then
                local value, err = calc.eval(text)
                local item
                if value then
                    local shown = calc.format(value)
                    item = ui.List.Item {
                        key = "result",
                        title = shown,
                        subtitle = text,
                        icon = ui.Icon { name = "calculator", tint = "green" },
                        accessories = { { text = "Enter to copy" } },
                        actions = {
                            ui.Action {
                                title = "Copy result",
                                on_action = function()
                                    flux.clipboard.setText(shown)
                                    save_history(remember(s.history, text, shown))
                                    flux.ui.close()
                                end,
                            },
                            ui.Action {
                                title = "Save to history",
                                shortcut = "ctrl+s",
                                on_action = function()
                                    local h = remember(s.history, text, shown)
                                    save_history(h)
                                    self:set { history = h }
                                end,
                            },
                            ui.Action {
                                title = "Copy expression and result",
                                shortcut = "ctrl+shift+c",
                                on_action = function()
                                    flux.clipboard.setText(text .. " = " .. shown)
                                    flux.ui.close()
                                end,
                            },
                        },
                    }
                else
                    item = ui.List.Item {
                        key = "error",
                        title = "Cannot evaluate",
                        subtitle = err,
                        icon = ui.Icon { name = "warning", tint = "red" },
                        accessories = { { tag = { text = "error", color = "red" } } },
                    }
                end
                sections[#sections + 1] = ui.List.Section { key = "result", title = "Result", item }
            end

            local rows = {}
            for i, e in ipairs(s.history) do
                rows[#rows + 1] = ui.List.Item {
                    key = "h" .. e.expr,
                    title = e.result,
                    subtitle = e.expr,
                    icon = "history",
                    actions = {
                        ui.Action {
                            title = "Copy result",
                            on_action = function()
                                flux.clipboard.setText(e.result)
                                flux.ui.close()
                            end,
                        },
                        ui.Action {
                            title = "Copy expression",
                            on_action = function()
                                flux.clipboard.setText(e.expr)
                                flux.ui.close()
                            end,
                        },
                        ui.Action {
                            title = "Remove from history",
                            style = "destructive",
                            shortcut = "ctrl+x",
                            on_action = function()
                                local h = {}
                                for j, other in ipairs(s.history) do
                                    if j ~= i then h[#h + 1] = other end
                                end
                                save_history(h)
                                self:set { history = h }
                            end,
                        },
                    },
                }
            end
            if #rows > 0 then
                sections[#sections + 1] = ui.List.Section { key = "history", title = "History", items = rows }
            end

            return ui.List {
                filtering = false, -- the extension decides what is shown for the typed text
                search_bar_placeholder = "Type an expression, e.g. 2 * (3 + 4) ^ 2",
                on_search_change = function(text) self:set { query = text } end,
                actions = #s.history == 0 and {} or {
                    ui.Action {
                        title = "Clear history",
                        style = "destructive",
                        confirm = { title = "Clear the whole history?", message = #s.history .. " entries will be deleted." },
                        on_action = function()
                            flux.storage.remove("history")
                            self:set { history = {} }
                        end,
                    },
                },
                ui.List.EmptyView { title = "Type something to calculate", description = "Operators + - * / % ^ !, functions sqrt sin ln …, constants pi e", icon = "calculator" },
                table.unpack(sections),
            }
        end,
    }
end
