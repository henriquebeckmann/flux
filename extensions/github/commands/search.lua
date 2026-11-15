local gh = require("api")
local ui = flux.ui

local function client()
    -- The token is optional: without a Secret Service the lookup fails and we simply search anonymously.
    local ok, token = pcall(flux.preferences.get, "token")
    return gh.new {
        base = (flux.preferences.get("api_url") or "https://api.github.com"):gsub("/+$", ""),
        token = ok and token or nil,
    }
end

local function compact(n)
    if n >= 1000000 then return ("%.1fM"):format(n / 1000000) end
    if n >= 1000 then return ("%.1fk"):format(n / 1000) end
    return tostring(n)
end

local function short_date(iso)
    return iso and iso:sub(1, 10) or "—"
end

local function accessories_of(r)
    local list = {}
    if r.language then list[#list + 1] = { tag = { text = r.language, color = "purple" } } end
    list[#list + 1] = { icon = "star", text = compact(r.stargazers_count or 0) }
    return list
end

--- The detail page of one repository; it loads the README by itself.
local function details_view(repo)
    return ui.view {
        state = { readme = nil, error = nil },

        init = function(self)
            self:task("readme", function()
                local text, err = client().readme(repo.full_name)
                if text then self:set { readme = text } else self:set { error = err } end
            end)
        end,

        render = function(self)
            local s = self.state
            local body
            if s.readme then
                body = s.readme
            elseif s.error then
                body = "# Could not load the README\n\n" .. s.error
            else
                body = "_Loading the README…_"
            end
            local M = ui.Detail.Metadata
            local meta = {
                M.Label { title = "Stars", text = tostring(repo.stargazers_count or 0), icon = "star" },
                M.Label { title = "Forks", text = tostring(repo.forks_count or 0), icon = "git-branch" },
                M.Label { title = "Open issues", text = tostring(repo.open_issues_count or 0) },
                M.Label { title = "Language", text = repo.language or "—" },
                M.Label { title = "License", text = repo.license and repo.license.spdx_id or "—" },
                M.Label { title = "Updated", text = short_date(repo.pushed_at) },
                M.Separator {},
            }
            if repo.topics and #repo.topics > 0 then
                local tags = {}
                for _, t in ipairs(repo.topics) do tags[#tags + 1] = M.TagList.Item { text = t, color = "blue" } end
                meta[#meta + 1] = M.TagList { title = "Topics", table.unpack(tags) }
            end
            meta[#meta + 1] = M.Link { title = "Repository", text = "github.com/" .. repo.full_name, url = repo.html_url }
            if type(repo.homepage) == "string" and repo.homepage:match("^https?://") then
                meta[#meta + 1] = M.Link { title = "Homepage", text = repo.homepage, url = repo.homepage }
            end
            return ui.Detail {
                title = repo.full_name,
                is_loading = not s.readme and not s.error,
                markdown = "# " .. repo.full_name .. "\n\n" .. (repo.description or "") .. "\n\n---\n\n" .. body,
                metadata = M(meta),
                actions = {
                    ui.Action { title = "Open in browser", on_action = function() flux.browser.open(repo.html_url); flux.ui.close() end },
                    ui.Action { title = "Copy URL", shortcut = "ctrl+c", on_action = function() flux.clipboard.setText(repo.html_url); flux.ui.close() end },
                    ui.Action { title = "Copy clone command", shortcut = "ctrl+shift+c", on_action = function() flux.clipboard.setText("git clone " .. repo.clone_url); flux.ui.close() end },
                },
            }
        end,
    }
end

function main()
    local avatars = flux.preferences.get("avatars") ~= false

    return ui.view {
        state = { query = "", repos = {}, total = 0, loading = false, error = nil },

        render = function(self)
            local s = self.state
            local items = {}
            for _, r in ipairs(s.repos) do
                items[#items + 1] = ui.List.Item {
                    key = r.full_name,
                    title = r.full_name,
                    subtitle = r.description,
                    icon = avatars and r.owner and r.owner.avatar_url
                        and ui.Icon { image = ui.Image { url = r.owner.avatar_url, mask = "circle", fallback = "git-branch" } } or "git-branch",
                    accessories = accessories_of(r),
                    actions = {
                        ui.Action { title = "Show details", on_action = function() self:push(details_view(r)) end },
                        ui.Action { title = "Open in browser", on_action = function() flux.browser.open(r.html_url); flux.ui.close() end },
                        ui.Action { title = "Copy URL", shortcut = "ctrl+c", on_action = function() flux.clipboard.setText(r.html_url); flux.ui.close() end },
                        ui.Action { title = "Copy clone command", shortcut = "ctrl+shift+c", on_action = function() flux.clipboard.setText("git clone " .. r.clone_url); flux.ui.close() end },
                    },
                }
            end

            local empty
            if s.error then
                empty = ui.List.EmptyView { title = "Search failed", description = s.error, icon = ui.Icon { name = "warning", tint = "red" } }
            elseif s.query == "" then
                empty = ui.List.EmptyView { title = "Search GitHub", description = "Start typing a repository name or topic", icon = "search" }
            else
                empty = ui.List.EmptyView { title = "No repositories found", icon = "search" }
            end

            return ui.List {
                filtering = false,
                is_loading = s.loading,
                search_bar_placeholder = "Search repositories…",
                on_search_change = function(text)
                    text = text:match("^%s*(.-)%s*$")
                    if text == "" then
                        self:cancel("search")
                        self:set { query = "", repos = {}, loading = false, error = nil, total = 0 }
                        return
                    end
                    self:set { query = text, loading = true, error = nil }
                    -- "latest wins": typing again cancels the pending request
                    self:task("search", function()
                        flux.task.sleep(0.35) -- debounce
                        local ok, repos, err, total = pcall(function() return client().search(text) end)
                        if not ok then repos, err = nil, tostring(repos) end
                        if repos then
                            self:set { repos = repos, total = total or #repos, loading = false }
                        else
                            self:set { repos = {}, loading = false, error = err }
                        end
                    end)
                end,
                ui.List.Section {
                    key = "repos",
                    title = s.total > 0 and ("%d repositories"):format(s.total) or nil,
                    items = items,
                },
                empty,
            }
        end,
    }
end
