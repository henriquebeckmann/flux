local fs = flux.filesystem
local ui = flux.ui

local IMAGE = { png = true, jpg = true, jpeg = true, gif = true, webp = true, svg = true }
local TEXT_ICON = { lua = "code", rs = "code", py = "code", js = "code", ts = "code", c = "code", h = "code", sh = "terminal",
    md = "file-text", txt = "file-text", toml = "file-text", json = "file-text", yaml = "file-text", yml = "file-text",
    zip = "archive", gz = "archive", tar = "archive", mp3 = "music", flac = "music", mp4 = "film", mkv = "film", pdf = "book" }

local function ext_of(name)
    return (fs.extension(name) or ""):lower()
end

local function icon_for(e)
    if e.is_dir then return "folder" end
    local ext = ext_of(e.name)
    if IMAGE[ext] then return "image" end
    return TEXT_ICON[ext] or "file"
end

local function human(n)
    if n < 1024 then return n .. " B" end
    local units = { "KB", "MB", "GB", "TB" }
    local v = n
    for _, u in ipairs(units) do
        v = v / 1024
        if v < 1024 or u == "TB" then return ("%.1f %s"):format(v, u) end
    end
end

local function when(ts)
    return ts and os.date("%Y-%m-%d %H:%M", ts) or "—"
end

local function expand(path)
    if path == "~" then return fs.home() end
    if path:sub(1, 2) == "~/" then return fs.join(fs.home(), path:sub(3)) end
    return path
end

--- Text preview of up to 64 KB; binary files are not shown.
local function preview(path, size)
    local text, truncated = fs.read_text(path, { max_bytes = 64 * 1024 })
    if not text then return nil, truncated end
    if text:find("\0", 1, true) then return nil, "binary file" end
    return text, truncated and size or nil
end

local function detail_view(entry)
    local ext = ext_of(entry.name)
    local text, note = preview(entry.path, entry.size)
    local md
    if entry.is_dir then
        md = "# " .. entry.name .. "\n\nA folder."
    elseif text and (ext == "md" or ext == "markdown") then
        md = text
    elseif text then
        local fence = text:find("```", 1, true) and "~~~" or "```"
        md = "# " .. entry.name .. "\n\n" .. fence .. "\n" .. text .. "\n" .. fence
        if note then md = md .. "\n_Showing the first 64 KB of " .. human(entry.size) .. "._" end
    elseif IMAGE[ext] then
        md = "# " .. entry.name .. "\n\nImage file. Use **Open** to view it."
    else
        md = "# " .. entry.name .. "\n\nNo preview (" .. tostring(note or "unsupported type") .. ")."
    end
    local M = ui.Detail.Metadata
    return ui.Detail {
        title = entry.name,
        markdown = md,
        metadata = M {
            M.Label { title = "Name", text = entry.name, icon = icon_for(entry) },
            M.Label { title = "Kind", text = entry.is_dir and "Folder" or (ext ~= "" and ext:upper() .. " file" or "File") },
            M.Label { title = "Size", text = entry.is_dir and "—" or human(entry.size) },
            M.Label { title = "Modified", text = when(entry.modified) },
            M.Separator {},
            M.Label { title = "Path", text = entry.path },
        },
        actions = {
            ui.Action { title = "Open", on_action = function() fs.open(entry.path); flux.ui.close() end },
            ui.Action { title = "Copy path", shortcut = "ctrl+c", on_action = function() flux.clipboard.setText(entry.path); flux.ui.close() end },
        },
    }
end

function main()
    local start = expand(flux.preferences.get("start_dir") or "~")
    local view = flux.preferences.get("view") == "grid" and "grid" or "list"
    local hidden = flux.preferences.get("hidden") == true
    -- a missing (or protected) start folder falls back to home instead of failing
    local ok, found = pcall(fs.exists, start)
    if not ok or not found then start = fs.home() end

    -- The listing lives in the state: it is read when the folder changes, not on every render.
    local first, first_err = fs.list(start, { hidden = hidden })

    return ui.view {
        state = { path = start, view = view, hidden = hidden, entries = first or {}, error = first_err },

        render = function(self)
            local s = self.state
            local entries = s.entries

            local function navigate(path)
                local list, err = fs.list(path, { hidden = s.hidden })
                if not list then
                    self:set { error = tostring(err) }
                    return
                end
                self:set_search("") -- the filter was for the previous folder
                self:set { path = path, entries = list, error = nil }
            end
            local function refresh(patch)
                local hidden_now = s.hidden
                if patch and patch.hidden ~= nil then hidden_now = patch.hidden end -- note: `and/or` would lose `false`
                local list = fs.list(s.path, { hidden = hidden_now }) or {}
                local p = { entries = list }
                for k, v in pairs(patch or {}) do p[k] = v end
                self:set(p)
            end
            local parent = fs.dirname(s.path)

            local function actions_for(e)
                local list = {}
                if e.is_dir then
                    list[#list + 1] = ui.Action { title = "Open folder", on_action = function() navigate(e.path) end }
                    list[#list + 1] = ui.Action { title = "Show details", shortcut = "ctrl+i", on_action = function() self:push(detail_view(e)) end }
                    list[#list + 1] = ui.Action { title = "Open with default app", shortcut = "ctrl+o", on_action = function() fs.open(e.path); flux.ui.close() end }
                else
                    list[#list + 1] = ui.Action { title = "Show details", on_action = function() self:push(detail_view(e)) end }
                    list[#list + 1] = ui.Action { title = "Open", shortcut = "ctrl+o", on_action = function() fs.open(e.path); flux.ui.close() end }
                end
                list[#list + 1] = ui.Action { title = "Copy path", shortcut = "ctrl+c", on_action = function() flux.clipboard.setText(e.path); flux.ui.close() end }
                list[#list + 1] = ui.Action { title = "Copy name", shortcut = "ctrl+shift+c", on_action = function() flux.clipboard.setText(e.name); flux.ui.close() end }
                return list
            end

            local folders, files = {}, {}
            for _, e in ipairs(entries) do
                local item
                if s.view == "grid" then
                    local ext = ext_of(e.name)
                    local content = ui.Icon { name = icon_for(e) }
                    if not e.is_dir and IMAGE[ext] and e.size < 20 * 1024 * 1024 then
                        content = ui.Image { path = e.path, fallback = "image" }
                    end
                    item = ui.Grid.Item { key = e.path, title = e.name, subtitle = e.is_dir and nil or human(e.size), content = content, actions = actions_for(e) }
                else
                    item = ui.List.Item {
                        key = e.path,
                        title = e.name,
                        subtitle = e.is_dir and "Folder" or nil,
                        icon = icon_for(e),
                        accessories = e.is_dir and { { text = when(e.modified) } } or { { text = human(e.size) }, { text = when(e.modified) } },
                        actions = actions_for(e),
                    }
                end
                if e.is_dir then folders[#folders + 1] = item else files[#files + 1] = item end
            end

            local global = {
                ui.Action { title = "Go to parent folder", shortcut = "alt+up", on_action = function() if parent ~= s.path then navigate(parent) end end },
                ui.Action { title = "Go to home folder", shortcut = "alt+home", on_action = function() navigate(fs.home()) end },
                ui.Action { title = s.hidden and "Hide hidden files" or "Show hidden files", shortcut = "ctrl+h", on_action = function() refresh { hidden = not s.hidden } end },
                ui.Action { title = s.view == "grid" and "Switch to list" or "Switch to grid", shortcut = "ctrl+g", on_action = function() self:set { view = s.view == "grid" and "list" or "grid" } end },
                ui.Action { title = "Refresh", shortcut = "ctrl+r", on_action = function() refresh() end },
                ui.Action { title = "Copy current folder path", on_action = function() flux.clipboard.setText(s.path); flux.ui.close() end },
            }
            local empty = ui.List.EmptyView { title = s.error or "This folder is empty", icon = s.error and "warning" or "folder-open" }
            local keys = {
                ["alt+up"] = function() if parent ~= s.path then navigate(parent) end end,
                ["ctrl+h"] = function() refresh { hidden = not s.hidden } end,
                ["ctrl+g"] = function() self:set { view = s.view == "grid" and "list" or "grid" } end,
            }

            if s.view == "grid" then
                return ui.Grid {
                    title = s.path,
                    size = "medium",
                    search_bar_placeholder = "Filter " .. fs.basename(s.path) .. "…",
                    actions = global,
                    on_key = keys,
                    ui.Grid.Section { key = "folders", title = #folders > 0 and "Folders" or nil, items = folders },
                    ui.Grid.Section { key = "files", title = #files > 0 and "Files" or nil, items = files },
                    ui.Grid.EmptyView { title = s.error or "This folder is empty", icon = s.error and "warning" or "folder-open" },
                }
            end
            return ui.List {
                title = s.path,
                search_bar_placeholder = "Filter " .. fs.basename(s.path) .. "…",
                actions = global,
                on_key = keys,
                ui.List.Section { key = "folders", title = #folders > 0 and "Folders" or nil, items = folders },
                ui.List.Section { key = "files", title = #files > 0 and "Files" or nil, items = files },
                empty,
            }
        end,
    }
end
