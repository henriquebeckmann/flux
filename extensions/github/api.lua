-- Thin wrapper over the GitHub REST API. Every call returns `data, nil` or `nil, message`.
local M = {}

local function describe_error(res)
    if res.status == 403 or res.status == 429 then
        local remaining = res.headers["x-ratelimit-remaining"]
        if remaining == "0" then
            return "GitHub rate limit reached. Add a personal access token in the extension preferences."
        end
    end
    if res.status == 401 then return "GitHub rejected the token (401). Check it in the extension preferences." end
    if res.status == 404 then return "Not found (404)." end
    local ok, body = pcall(res.json, res)
    local detail = ok and type(body) == "table" and body.message or nil
    return ("GitHub answered %d%s"):format(res.status, detail and (": " .. detail) or "")
end

--- @param opts { base: string, token?: string, signal?: any }
function M.new(opts)
    local self = {}

    local function request(path, extra)
        local headers = { Accept = "application/vnd.github+json", ["X-GitHub-Api-Version"] = "2022-11-28" }
        if opts.token and opts.token ~= "" then headers.Authorization = "Bearer " .. opts.token end
        for k, v in pairs(extra.headers or {}) do headers[k] = v end
        local res, err = flux.http.get(opts.base .. path, { headers = headers, query = extra.query, timeout = 15, signal = opts.signal })
        if not res then return nil, "Request failed: " .. err.message, nil end
        if not res.ok then return nil, describe_error(res), res end
        return res, nil, res
    end

    function self.search(text)
        local res, err = request("/search/repositories", { query = { q = text, per_page = 30 } })
        if not res then return nil, err end
        local data = res:json()
        return data.items or {}, nil, data.total_count
    end

    --- Raw README (Markdown), cut to a size the launcher renders comfortably.
    function self.readme(full_name)
        local res, err, raw = request("/repos/" .. full_name .. "/readme", { headers = { Accept = "application/vnd.github.raw+json" } })
        if not res then
            if raw and raw.status == 404 then return "_This repository has no README._" end
            return nil, err
        end
        local text = res.body
        if #text > 20000 then text = text:sub(1, 20000) .. "\n\n_… README truncated._" end
        return text
    end

    return self
end

return M
