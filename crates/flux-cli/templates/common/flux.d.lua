---@meta
-- Type stubs for the Flux extension API (editor completion only; nothing here runs).
-- Full reference: docs/ui-components.md, docs/reactivity.md, docs/http.md

---@class FluxAction
---@field title string
---@field description? string
---@field shortcut? string        e.g. "ctrl+shift+c"; the first action is Enter, the second Ctrl+Enter
---@field style? "default"|"secondary"|"destructive"
---@field confirm? boolean|string|{title?: string, message?: string}
---@field on_action fun(): any    may return a view (replaces the screen)

---@class FluxIcon
---@field name? string            built-in icon name (see docs/ui-components.md)
---@field text? string            emoji or short text
---@field image? any
---@field tint? string            color name or #rrggbb

---@class FluxAccessory
---@field text? string
---@field icon? string|FluxIcon
---@field tag? string|{text: string, color?: string}
---@field progress? number        0..1
---@field tooltip? string

---@class FluxView<S>
---@field state S                 read-only; change it with self:set
local View = {}
---@param patch table
function View:set(patch) end
---@param ... string
function View:unset(...) end
---@param fn fun(state: table): table?
function View:update(fn) end
---Runs `fn` in the background. A task with the same name replaces the running one; closing the screen cancels it.
---@param name string
---@param fn fun()
function View:task(name, fn) end
---@param name? string
function View:cancel(name) end
---@param view any
function View:push(view) end
function View:pop() end
function View:close() end

---@class FluxHttpError
---@field kind "timeout"|"cancelled"|"network"|"too_large"|"denied"|"invalid"
---@field message string

---@class FluxResponse
---@field status integer
---@field ok boolean
---@field url string
---@field redirected boolean
---@field headers table<string, string>
---@field body string
local Response = {}
---@return any
function Response:json() end
---@return string
function Response:text() end

---@class FluxHttpOptions
---@field headers? table<string, string|number>
---@field query? table<string, string|number|boolean|(string|number)[]>
---@field json? any
---@field body? string
---@field form? table<string, string|number|boolean>
---@field timeout? number         seconds, default 30
---@field max_bytes? integer      default 10 MB
---@field follow_redirects? boolean
---@field signal? any             from flux.task.token()

---@class Flux
flux = {}

flux.ui = {}
---@param def {state?: table, init?: fun(self: FluxView), render: fun(self: FluxView): any}
function flux.ui.view(def) end
---@param props table
function flux.ui.List(props) end
flux.ui.List.Section = function(props) end
flux.ui.List.Item = function(props) end
flux.ui.List.EmptyView = function(props) end
---@param props table
function flux.ui.Grid(props) end
flux.ui.Grid.Section = function(props) end
flux.ui.Grid.Item = function(props) end
flux.ui.Grid.EmptyView = function(props) end
---@param props table
function flux.ui.Detail(props) end
flux.ui.Detail.Markdown = function(props) end
flux.ui.Detail.Metadata = function(props) end
---@param props table
function flux.ui.Form(props) end
flux.ui.Form.TextField = function(props) end
flux.ui.Form.PasswordField = function(props) end
flux.ui.Form.TextArea = function(props) end
flux.ui.Form.Checkbox = function(props) end
flux.ui.Form.Dropdown = function(props) end
flux.ui.Form.TagPicker = function(props) end
flux.ui.Form.DatePicker = function(props) end
flux.ui.Form.FilePicker = function(props) end
flux.ui.Form.Separator = function(props) end
flux.ui.Form.Description = function(props) end
---@param props FluxAction
function flux.ui.Action(props) end
function flux.ui.ActionPanel(actions) end
---@param props FluxIcon
function flux.ui.Icon(props) end
function flux.ui.Image(props) end
function flux.ui.ProgressBar(props) end
function flux.ui.LoadingIndicator(props) end
function flux.ui.close() end

---@param url string
---@param opts? FluxHttpOptions
---@return FluxResponse?, FluxHttpError?
function flux.http.get(url, opts) end
function flux.http.post(url, opts) end
function flux.http.put(url, opts) end
function flux.http.patch(url, opts) end
function flux.http.delete(url, opts) end
function flux.http.request(opts) end

flux.json = {}
---@return string
function flux.json.encode(value, pretty) end
---@return any
function flux.json.decode(text) end

flux.task = {}
---@return {cancel: fun(self), is_cancelled: fun(self): boolean}
function flux.task.token() end
---Waits without blocking anything; cancelled together with the task.
---@param seconds number
function flux.task.sleep(seconds) end

flux.storage = {}
function flux.storage.get(key) end
function flux.storage.set(key, value) end
function flux.storage.remove(key) end
function flux.storage.clear() end
flux.storage.cache = {}
flux.storage.secure = {}

flux.preferences = {}
function flux.preferences.get(name) end
function flux.preferences.getAll() end
function flux.preferences.set(name, value) end

flux.clipboard = {}
function flux.clipboard.getText() end
function flux.clipboard.setText(text) end

flux.process = {}
function flux.process.exec(cmd, args, opts) end
function flux.process.spawn(cmd, args) end
function flux.process.kill(pid, signal) end

flux.filesystem = {}
function flux.filesystem.home() end
function flux.filesystem.list(path, opts) end
function flux.filesystem.stat(path) end
function flux.filesystem.exists(path) end
function flux.filesystem.read_text(path, opts) end
function flux.filesystem.open(path) end
function flux.filesystem.join(...) end
function flux.filesystem.basename(path) end
function flux.filesystem.dirname(path) end
function flux.filesystem.extension(path) end

flux.browser = {}
function flux.browser.open(url) end

flux.navigation = {}
function flux.navigation.push(view) end
function flux.navigation.pop() end
function flux.navigation.replace(view) end

flux.logger = {}
function flux.logger.debug(...) end
function flux.logger.info(...) end
function flux.logger.warn(...) end
function flux.logger.error(...) end

flux.environment = {}
flux.lifecycle = {}
function flux.lifecycle.on_unload(fn) end
