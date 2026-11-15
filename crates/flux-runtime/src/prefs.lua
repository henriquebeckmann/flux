-- Built-in "Configure <extension>" command. It is ordinary Lua built on the public API
-- (flux.ui / flux.navigation / flux.preferences), so there is no special UI path for settings.
local flux = require("flux")

local specs = flux.preferences.spec()

local function display(spec)
  local ok, value = pcall(flux.preferences.get, spec.name)
  if not ok then
    -- e.g. no Secret Service for a password preference: keep the screen usable
    return tostring(value):find("secure storage unavailable", 1, true) and "Keyring unavailable" or "Could not read value"
  end
  local text
  if spec.type == "password" then
    text = value and "••••••••" or "Not set"
  elseif spec.type == "checkbox" then
    text = value and "Enabled" or "Disabled"
  elseif spec.type == "dropdown" then
    text = value
    for _, o in ipairs(spec.options) do if o.value == value then text = o.title end end
    text = text or "Not set"
  else
    text = (value ~= nil and value ~= "") and tostring(value) or "Not set"
  end
  if spec.required and (value == nil or value == "") then text = text .. "  (required)" end
  return text
end

local build

local function refresh()
  flux.navigation.pop()
  flux.navigation.replace(build())
end

local function edit(spec)
  if spec.type == "checkbox" then
    flux.preferences.set(spec.name, flux.preferences.get(spec.name) ~= true)
    return build() -- returning a view replaces the current screen
  elseif spec.type == "dropdown" then
    local items = {}
    for _, o in ipairs(spec.options) do
      items[#items + 1] = {
        title = o.title,
        subtitle = o.value,
        actions = { flux.action.new { title = "Select", on_action = function()
          flux.preferences.set(spec.name, o.value)
          refresh()
        end } },
      }
    end
    flux.navigation.push(flux.ui.list { title = spec.title, search_bar_placeholder = "Select " .. spec.title, items = items })
  else
    local secure = spec.type == "password"
    flux.navigation.push(flux.ui.input {
      title = spec.title,
      placeholder = spec.description or spec.title,
      secure = secure,
      value = (not secure) and flux.preferences.get(spec.name) or nil,
      on_submit = function(text)
        flux.preferences.set(spec.name, text)
        refresh()
      end,
    })
  end
end

function build()
  local items = {}
  for _, spec in ipairs(specs) do
    items[#items + 1] = {
      title = spec.title,
      subtitle = display(spec),
      icon = ({ text = "✎", password = "🔑", checkbox = "☑", dropdown = "▾" })[spec.type],
      actions = { flux.action.new {
        title = (spec.type == "checkbox") and "Toggle" or "Edit",
        description = spec.description,
        on_action = function() return edit(spec) end,
      } },
    }
  end
  return flux.ui.list { title = "Preferences", search_bar_placeholder = "Search preferences…", items = items }
end

function main()
  return build()
end
