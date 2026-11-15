-- Runs once per runtime, with access to the real _G (passed as `...`).
-- Builds the extensions' sandbox environment and the "idiomatic" part of the
-- `flux` API that is more natural to write in Lua than in Rust.
local G, MAX_COROUTINES = ...

local M = {}

-- Base environment: only what is safe (no io, no os.execute, no debug, no package).
--
-- pcall / xpcall / coroutine.resume are replaced by versions that RE-RAISE when the CPU-slice guard
-- has tripped: otherwise `while true do pcall(function() while true do end end) end` would swallow
-- the limit error forever (see limits.rs). `is_tripped` is provided by Rust.
function M.safe_base(is_tripped)
  local b = {}
  for _, k in ipairs {
    "assert", "error", "ipairs", "pairs", "next", "select",
    "tonumber", "tostring", "type", "setmetatable",
    "rawget", "rawset", "rawequal", "rawlen",
  } do b[k] = G[k] end
  for _, lib in ipairs { "string", "table", "math", "utf8", "coroutine" } do
    local t = {}
    for k, v in pairs(G[lib]) do t[k] = v end
    b[lib] = t
  end

  local gpcall, gxpcall, gerror = G.pcall, G.xpcall, G.error
  local function guarded(ok, ...)
    if not ok and is_tripped() then gerror((...), 0) end
    return ok, ...
  end
  b.pcall = function(f, ...) return guarded(gpcall(f, ...)) end
  -- A message handler runs even for errors raised by the limit hook, with hooks disabled (Lua
  -- semantics): a looping handler would be uninterruptible. Once tripped, user handlers are skipped.
  b.xpcall = function(f, handler, ...)
    local function h(e)
      if is_tripped() then return e end
      return handler(e)
    end
    return guarded(gxpcall(f, h, ...))
  end

  -- Coroutines: only ones created through the sandbox may yield. A bare `coroutine.yield()` in the
  -- command's own thread would be misread by the async driver and could spin it forever.
  local gcreate, gresume, gyield, grunning = G.coroutine.create, G.coroutine.resume, G.coroutine.yield, G.coroutine.running
  local mine = setmetatable({}, { __mode = "k" })
  local co = b.coroutine
  -- A limit on live coroutines. Finished and collected ones do not count, so the table is only
  -- walked (and the collector given a chance) when the cheap counter says we may be at the limit.
  local live = 0
  local function recount()
    collectgarbage("step", 1000)
    local n = 0
    for c in next, mine do
      if G.coroutine.status(c) ~= "dead" then n = n + 1 end
    end
    live = n
  end
  co.create = function(f)
    if live >= MAX_COROUTINES then
      recount()
      if live >= MAX_COROUTINES then gerror("too many coroutines (max " .. MAX_COROUTINES .. ")", 2) end
    end
    local c = gcreate(f)
    mine[c] = true
    live = live + 1
    return c
  end
  co.resume = function(c, ...) return guarded(gresume(c, ...)) end
  co.wrap = function(f)
    local c = co.create(f)
    return function(...)
      local r = table.pack(co.resume(c, ...))
      if not r[1] then gerror(r[2], 0) end
      return table.unpack(r, 2, r.n)
    end
  end
  co.yield = function(...)
    if not mine[(grunning())] then gerror("coroutine.yield can only be used inside coroutines created with coroutine.create/wrap", 2) end
    return gyield(...)
  end

  -- Strings share one metatable per Lua state; do not hand it out.
  b.getmetatable = function(x)
    if type(x) == "string" then return nil end
    return G.getmetatable(x)
  end
  b.os = { time = G.os.time, date = G.os.date, clock = G.os.clock, difftime = G.os.difftime }
  b._VERSION = G._VERSION
  b._G = b
  b.load = function(src, name, _, env) return G.load(src, name, "t", env or b) end
  return b
end

-- A command's environment: `require("flux")` returns this run's `flux`
-- and `require("x.y")` loads `<extension dir>/x/y.lua`.
function M.make_env(base, flux, dir, read_module)
  local env = setmetatable({}, { __index = base })
  local loaded = {}
  env.require = function(name)
    if name == "flux" then return flux end
    if loaded[name] ~= nil then return loaded[name] end
    local src, chunkname = read_module(dir, name)
    if not src then error("module '" .. tostring(name) .. "' not found in the extension", 2) end
    local fn, err = G.load(src, chunkname, "t", env)
    if not fn then error(err, 0) end
    local r = fn(name)
    if r == nil then r = true end
    loaded[name] = r
    return r
  end
  env.flux = flux
  env.print = flux.logger.info
  return env
end


-- ---------------------------------------------------------------------------
-- UI components. Each is a callable table: `ui.List{ ... }` tags the props table with its kind;
-- validation (types, unknown props, keys) happens in Rust with path-qualified messages.
-- ---------------------------------------------------------------------------
local function component(kind, subs)
  local c = subs or {}
  return setmetatable(c, {
    __call = function(_, props)
      if props == nil then props = {} end
      if type(props) ~= "table" then error(kind .. ": expected a table of props", 2) end
      props.__kind = kind
      return props
    end,
  })
end

function M.install_components(flux)
  local ui = flux.ui
  ui.List = component("List", {
    Section = component("ListSection"), Item = component("ListItem"), EmptyView = component("ListEmpty"),
  })
  ui.Grid = component("Grid", {
    Section = component("GridSection"), Item = component("GridItem"), EmptyView = component("GridEmpty"),
  })
  ui.Detail = component("Detail", {
    Markdown = component("DetailMarkdown"),
    Metadata = component("DetailMetadata", {
      Label = component("MetaLabel"),
      TagList = component("MetaTagList", { Item = component("MetaTag") }),
      Link = component("MetaLink"),
      Separator = component("MetaSeparator"),
    }),
  })
  ui.Form = component("Form", {
    TextField = component("FormText"),
    PasswordField = component("FormPassword"),
    TextArea = component("FormTextArea"),
    Checkbox = component("FormCheckbox"),
    Dropdown = component("FormDropdown", { Item = component("FormOption"), Section = component("FormOptionSection") }),
    TagPicker = component("FormTagPicker", { Item = component("FormOption") }),
    DatePicker = component("FormDatePicker"),
    FilePicker = component("FormFilePicker"),
    Separator = component("FormSeparator"),
    Description = component("FormDescription"),
  })
  ui.Image = component("Image")
  ui.Icon = component("Icon")
  ui.ProgressBar = component("ProgressBar")
  ui.LoadingIndicator = component("LoadingIndicator")
  ui.Action = component("action")
  ui.ActionPanel = component("actionpanel", { Section = component("section") })

  -- Reactive screen: `render` is a pure function of `self.state`; see docs/reactivity.md.
  ui.view = function(def)
    if type(def) ~= "table" then error("ui.view expects a table { state, render, ... }", 2) end
    if type(def.render) ~= "function" then error("ui.view needs a render(self) function", 2) end
    def.__kind = "view"
    return def
  end

  -- Legacy lowercase constructors keep working.
  ui.list = ui.List
  ui.input = component("Input")
  flux.action.new = ui.Action
  flux.action.section = ui.ActionPanel.Section
end

-- The `self` handed to render/init/callbacks of a `ui.view`.
function M.new_instance(def, host)
  local st = {}
  if def.state ~= nil then
    if type(def.state) ~= "table" then error("ui.view: state must be a table", 3) end
    for k, v in pairs(def.state) do st[k] = v end
  end
  local self = {}
  self.state = setmetatable({}, {
    __index = st,
    __newindex = function() error("self.state is read-only: use self:set{ ... }", 2) end,
    __pairs = function() return next, st, nil end,
  })
  function self:set(patch)
    if type(patch) ~= "table" then error("self:set expects a table", 2) end
    for k, v in pairs(patch) do st[k] = v end
    host.invalidate()
  end
  function self:unset(...)
    for i = 1, select("#", ...) do st[(select(i, ...))] = nil end
    host.invalidate()
  end
  function self:update(fn)
    local patch = fn(self.state)
    if patch ~= nil then self:set(patch) end
  end
  -- Runs `fn` in the background, tied to this screen. Starting a task with the same name cancels the
  -- previous one ("latest wins"); closing the screen cancels all of them.
  function self:task(name, fn)
    if type(name) == "function" then fn, name = name, "default" end
    if type(fn) ~= "function" then error("self:task(name, fn) needs a function", 2) end
    host.task(name, fn)
  end
  -- Replaces the text of the search field (e.g. clear it after navigating into a folder).
  function self:set_search(text) host.set_search(text or "") end
  function self:cancel(name) host.cancel(name or "default") end
  function self:push(view) host.push(view) end
  function self:pop() host.pop() end
  function self:close() host.close() end
  return self
end

local STUBS = {
  "application", "window", "keyboard", "notification",
  "dialog", "scheduler", "ai",
}

-- Completes the `flux` object created by Rust.
function M.extend(flux)
  M.install_components(flux)

  flux.action.copy = function(spec)
    return flux.action.new {
      title = spec.title or "Copy",
      shortcut = spec.shortcut,
      on_action = function()
        flux.clipboard.setText(spec.text)
        flux.ui.close()
      end,
    }
  end
  flux.action.open = function(spec)
    return flux.action.new {
      title = spec.title or "Open",
      shortcut = spec.shortcut,
      on_action = function()
        flux.browser.open(spec.url)
        flux.ui.close()
      end,
    }
  end

  local raw_log = flux.logger._log
  for _, level in ipairs { "debug", "info", "warn", "error" } do
    flux.logger[level] = function(...)
      local parts = {}
      for i = 1, select("#", ...) do parts[i] = tostring((select(i, ...))) end
      raw_log(level, table.concat(parts, " "))
    end
  end

  -- Path helpers for flux.filesystem (pure string functions).
  local fsn = flux.filesystem
  function fsn.join(...)
    local out = table.concat({ ... }, "/"):gsub("/+", "/")
    return out
  end
  function fsn.basename(p) return (p:gsub("/+$", ""):match("([^/]*)$")) end
  function fsn.dirname(p)
    local d = p:gsub("/+$", ""):match("^(.*)/[^/]*$")
    if d == nil then return "." end
    return d == "" and "/" or d
  end
  function fsn.extension(p) return p:match("%.([^./]+)$") end

  -- flux.http.get/post/put/patch/delete/request + response methods
  local raw_request = flux.http._request
  local Response = {}
  Response.__index = Response
  function Response:text() return self.body end
  function Response:json() return flux.json.decode(self.body) end
  local function request(method, url, opts)
    local res, err = raw_request(method, url, opts)
    if res then return setmetatable(res, Response) end
    return nil, err
  end
  local http = {}
  for _, m in ipairs { "get", "post", "put", "patch", "delete", "head" } do
    http[m] = function(url, opts) return request(m:upper(), url, opts) end
  end
  http.request = function(opts)
    if type(opts) ~= "table" or type(opts.url) ~= "string" then error("flux.http.request needs a table with a url", 2) end
    return request(opts.method or "GET", opts.url, opts)
  end
  flux.http = http

  -- Planned but not yet implemented APIs: they fail explicitly.
  for _, ns in ipairs(STUBS) do
    flux[ns] = setmetatable({}, {
      __index = function(_, fn)
        return function()
          error(("flux.%s.%s is not implemented yet"):format(ns, tostring(fn)), 2)
        end
      end,
    })
  end
end

return M
