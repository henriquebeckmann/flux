-- Reads the process table through `ps` and parses it. `parse` is pure so it can be tested on its own.
local M = {}

local COLUMNS = "pid=,pcpu=,rss=,user=,comm=,args="

--- Parses `ps -eo pid=,pcpu=,rss=,user=,comm=,args=` output.
--- Each row: { pid, cpu (percent), rss_kb, user, name, command }
function M.parse(text)
    local rows = {}
    for line in text:gmatch("[^\n]+") do
        local pid, cpu, rss, user, rest = line:match("^%s*(%d+)%s+([%d%.]+)%s+(%d+)%s+(%S+)%s+(.+)$")
        if pid then
            local name, args = rest:match("^(%S+)%s*(.*)$")
            rows[#rows + 1] = {
                pid = tonumber(pid), cpu = tonumber(cpu) or 0, rss_kb = tonumber(rss) or 0,
                user = user, name = name or rest, command = (args and args ~= "") and args or (name or rest),
            }
        end
    end
    return rows
end

--- Lists processes (sorted by `sort`: "cpu" or "mem"), at most `limit` of them.
function M.list(sort, limit)
    local res, err = flux.process.exec("ps", { "-eo", COLUMNS, sort == "mem" and "--sort=-rss" or "--sort=-pcpu" }, { timeout = 5 })
    if not res then return nil, tostring(err) end
    if not res.success then return nil, "ps failed: " .. res.stderr end
    local rows = M.parse(res.stdout)
    local out = {}
    for i = 1, math.min(#rows, limit or 300) do out[i] = rows[i] end
    return out
end

--- One process, or nil when it is gone.
function M.one(pid)
    local res = flux.process.exec("ps", { "-p", tostring(pid), "-o", COLUMNS }, { timeout = 5 })
    if not res or not res.success then return nil end
    return M.parse(res.stdout)[1]
end

function M.mem(kb)
    if kb >= 1048576 then return ("%.1f GB"):format(kb / 1048576) end
    if kb >= 1024 then return ("%.0f MB"):format(kb / 1024) end
    return kb .. " KB"
end

return M
