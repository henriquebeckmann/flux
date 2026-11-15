local ui = flux.ui

function main()
    return ui.view {
        state = { saved = nil },
        render = function(self)
            if self.state.saved then
                return ui.Detail {
                    markdown = "# Saved\n\nHello **" .. self.state.saved.name .. "**!",
                    actions = { ui.Action { title = "Back to the form", on_action = function() self:set { saved = nil } end } },
                }
            end
            return ui.Form {
                title = "{{title}}",
                submit_title = "Save",
                -- rules run instantly in the launcher; validate() runs on submit and can look at several fields
                validate = function(values)
                    if values.name == "root" then return { name = "That name is reserved" } end
                end,
                on_submit = function(values) self:set { saved = values } end,

                ui.Form.TextField { id = "name", title = "Name", required = true, min_length = 2, autofocus = true },
                ui.Form.TextField { id = "email", title = "Email", format = "email" },
                ui.Form.Dropdown { id = "role", title = "Role", default_value = "dev",
                    ui.Form.Dropdown.Item { value = "dev", title = "Developer" },
                    ui.Form.Dropdown.Item { value = "ops", title = "Operations" } },
                ui.Form.DatePicker { id = "start", title = "Start date" },
                ui.Form.TextArea { id = "notes", title = "Notes", rows = 4 },
                ui.Form.Checkbox { id = "tos", title = "Terms", label = "I agree", required = true },
            }
        end,
    }
end
