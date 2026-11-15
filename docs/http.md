# flux.http

```lua
local res, err = flux.http.get("https://api.example.com/items", {
  query = { q = "lua", page = 2, tag = { "a", "b" } },      -- encoded; lists repeat the key
  headers = { Authorization = "Bearer " .. token },
  timeout = 10,                                             -- seconds (default 30, max 600)
  signal = token_obj,                                       -- flux.task.token()
  max_bytes = 1048576,                                      -- response cap (default 10 MB)
})
if not res then print(err.kind, err.message) return end
res.status; res.ok; res.url; res.redirected; res.headers["content-type"]; res.body
local data = res:json(); local text = res:text()
```
Functions: `get`, `post`, `put`, `patch`, `delete`, `head`, `request{ method, url, … }`. Body options (one of): `json = value`
(sets `Content-Type`), `body = "raw"`, `form = { k = "v" }`. `follow_redirects = false` disables redirects (max 5 otherwise).

**Errors**: a non-2xx status is *not* an error (`res.ok == false`). Transport problems return `nil, err` where
`err = { kind, message }` (prints as its message): `timeout`, `cancelled`, `network`, `too_large`, `denied`, `invalid`.
Wrong arguments (unknown option, bad header, missing `network` permission) **raise**.

**Async**: requests run on the extension's own runtime and never block the launcher or other extensions; the calling task being
cancelled (view closed, reload) drops the connection.

**Permissions**: needs `network`. `allowed_hosts` in the manifest restricts hosts (exact or `*.example.com`) — checked for the URL and for
**every redirect**. Loopback/private/link-local addresses (`127.0.0.1`, `10.x`, `169.254.169.254`, `::1`…) are refused unless the
host is listed *exactly* (the check also runs when DNS resolves, defeating rebinding). Only `http`/`https`. Authorization headers are
never logged.

`flux.json.encode(value, pretty?)` / `flux.json.decode(text)` (raises `invalid JSON: …`; JSON `null` becomes `nil`).
