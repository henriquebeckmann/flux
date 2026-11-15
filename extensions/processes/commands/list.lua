local ps = require("ps")
local ui = flux.ui

local REFRESH_SECONDS = 2

local function cpu_color(cpu)
    if cpu >= 50 then return "red" elseif cpu >= 10 then return "orange" end
    return "gray"
end

--- Live page of one process: refreshed by its own task, which stops when the page closes.
local function details_view(proc)
    return ui.view {
        state = { proc = proc, gone = false },

        init = function(self)
            self:task("watch", function()
                while true do
                    flux.task.sleep(REFRESH_SECONDS)
                    local p = ps.one(proc.pid)
                    if p then self:set { proc = p } else self:set { gone = true }; return end
                end
            end)
        end,

        render = function(self)
            local p = self.state.proc
            local M = ui.Detail.Metadata
            return ui.Detail {
                title = p.name .. " (" .. p.pid .. ")",
                markdown = "# " .. p.name .. "\n\n```\n" .. p.command .. "\n```\n"
                    .. (self.state.gone and "\n**This process has exited.**\n" or ""),
                ui.ProgressBar { value = math.min(p.cpu, 100) / 100, label = ("CPU %.1f%%"):format(p.cpu) },
                metadata = M {
                    M.Label { title = "PID", text = tostring(p.pid) },
                    M.Label { title = "User", text = p.user },
                    M.Label { title = "CPU", text = ("%.1f%%"):format(p.cpu) },
                    M.Label { title = "Memory", text = ps.mem(p.rss_kb) },
                },
                actions = self.state.gone and {} or {
                    ui.Action {
                        title = "Terminate (SIGTERM)", style = "destructive",
                        confirm = { title = "Terminate " .. p.name .. "?", message = "PID " .. p.pid .. " is asked to exit." },
                        on_action = function() flux.process.kill(p.pid, "TERM") end,
                    },
                    ui.Action {
                        title = "Force kill (SIGKILL)", style = "destructive", shortcut = "ctrl+shift+k",
                        confirm = { title = "Force kill " .. p.name .. "?", message = "SIGKILL cannot be handled: unsaved data is lost." },
                        on_action = function() flux.process.kill(p.pid, "KILL") end,
                    },
                    ui.Action { title = "Copy PID", on_action = function() flux.clipboard.setText(tostring(p.pid)); flux.ui.close() end },
                },
            }
        end,
    }
end

function main()
    return ui.view {
        state = { procs = {}, sort = "cpu", loading = true, error = nil },

        init = function(self)
            -- Refresh loop tied to this screen: it is cancelled when the list is closed.
            self:task("refresh", function()
                while true do
                    local rows, err = ps.list(self.state.sort)
                    if rows then self:set { procs = rows, loading = false, error = nil } else self:set { loading = false, error = err } end
                    flux.task.sleep(REFRESH_SECONDS)
                end
            end)
        end,

        render = function(self)
            local s = self.state
            local items = {}
            for _, p in ipairs(s.procs) do
                items[#items + 1] = ui.List.Item {
                    key = tostring(p.pid),
                    title = p.name,
                    subtitle = p.command,
                    icon = ui.Icon { name = "cpu", tint = cpu_color(p.cpu) },
                    keywords = { tostring(p.pid), p.user },
                    accessories = {
                        { tag = { text = ("%.1f%%"):format(p.cpu), color = cpu_color(p.cpu) } },
                        { text = ps.mem(p.rss_kb) },
                        { text = "pid " .. p.pid },
                    },
                    actions = {
                        ui.Action { title = "Show details", on_action = function() self:push(details_view(p)) end },
                        ui.Action {
                            title = "Terminate (SIGTERM)", style = "destructive", shortcut = "ctrl+k",
                            confirm = { title = "Terminate " .. p.name .. "?", message = "PID " .. p.pid .. " is asked to exit." },
                            on_action = function() flux.process.kill(p.pid, "TERM") end,
                        },
                        ui.Action {
                            title = "Force kill (SIGKILL)", style = "destructive", shortcut = "ctrl+shift+k",
                            confirm = { title = "Force kill " .. p.name .. "?", message = "SIGKILL cannot be handled: unsaved data is lost." },
                            on_action = function() flux.process.kill(p.pid, "KILL") end,
                        },
                        ui.Action { title = "Copy PID", shortcut = "ctrl+c", on_action = function() flux.clipboard.setText(tostring(p.pid)); flux.ui.close() end },
                        ui.Action { title = "Copy command", shortcut = "ctrl+shift+c", on_action = function() flux.clipboard.setText(p.command); flux.ui.close() end },
                    },
                }
            end
            return ui.List {
                is_loading = s.loading,
                search_bar_placeholder = "Filter by name, command, user or pid…",
                actions = {
                    ui.Action {
                        title = s.sort == "cpu" and "Sort by memory" or "Sort by CPU", shortcut = "ctrl+s",
                        on_action = function()
                            self:set { sort = s.sort == "cpu" and "mem" or "cpu" }
                        end,
                    },
                },
                ui.List.Section { key = "procs", title = ("%d processes · sorted by %s"):format(#s.procs, s.sort == "cpu" and "CPU" or "memory"), items = items },
                ui.List.EmptyView { title = s.error or "No processes", icon = s.error and "warning" or "cpu" },
            }
        end,
    }
end
