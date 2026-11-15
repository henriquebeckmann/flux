local ui = flux.ui

function main()
    return ui.view {
        state = { saved = nil, log = {} },

        render = function(self)
            if self.state.saved then
                local v = self.state.saved
                local lines = {}
                for _, k in ipairs { "name", "email", "age", "role", "tags", "when", "files", "notes", "newsletter", "secret" } do
                    local val = v[k]
                    if type(val) == "table" then val = table.concat(val, ", ") end
                    if k == "secret" and val and val ~= "" then val = "•••••• (not shown)" end
                    lines[#lines + 1] = ("| %s | %s |"):format(k, tostring(val == nil and "" or val):gsub("|", "\\|"))
                end
                return ui.Detail {
                    title = "Submitted",
                    markdown = "# Submitted\n\n| field | value |\n|---|---|\n" .. table.concat(lines, "\n"),
                    actions = { ui.Action { title = "Edit again", on_action = function() self:set { saved = nil } end } },
                }
            end
            return ui.Form {
                title = "Demo form",
                submit_title = "Submit",
                validate = function(values)
                    if values.name == "root" then return { name = "'root' is reserved" } end
                end,
                on_submit = function(values) self:set { saved = values } end,

                ui.Form.Description { title = "About", text = "Required fields are marked *. Tab moves between fields, Ctrl+Enter submits." },
                ui.Form.TextField { id = "name", title = "Name", placeholder = "Ada Lovelace", required = true, min_length = 2, autofocus = true,
                    on_change = function(v) flux.logger.debug("name changed:", v) end },
                ui.Form.TextField { id = "email", title = "Email", format = "email", info = "Validated as you type" },
                ui.Form.TextField { id = "age", title = "Age", format = "integer", min = 0, max = 150 },
                ui.Form.PasswordField { id = "secret", title = "Secret" },
                ui.Form.Separator {},
                ui.Form.Dropdown { id = "role", title = "Role", default_value = "dev",
                    ui.Form.Dropdown.Section { title = "Engineering",
                        ui.Form.Dropdown.Item { value = "dev", title = "Developer" },
                        ui.Form.Dropdown.Item { value = "ops", title = "Operations" } },
                    ui.Form.Dropdown.Section { title = "Other",
                        ui.Form.Dropdown.Item { value = "design", title = "Design" },
                        ui.Form.Dropdown.Item { value = "pm", title = "Product" } } },
                ui.Form.TagPicker { id = "tags", title = "Interests", default_value = { "lua" },
                    ui.Form.TagPicker.Item { value = "lua", title = "Lua" },
                    ui.Form.TagPicker.Item { value = "rust", title = "Rust" },
                    ui.Form.TagPicker.Item { value = "ui", title = "Interfaces" },
                    ui.Form.TagPicker.Item { value = "ops", title = "Ops" } },
                ui.Form.DatePicker { id = "when", title = "Start", type = "datetime", min = "2024-01-01T00:00" },
                ui.Form.FilePicker { id = "files", title = "Attachments", multiple = true, info = "Browse… or type an absolute path and press Enter" },
                ui.Form.TextArea { id = "notes", title = "Notes", rows = 4, placeholder = "Anything else?" },
                ui.Form.Checkbox { id = "newsletter", title = "Updates", label = "Send me the newsletter", default_value = true },
            }
        end,
    }
end
