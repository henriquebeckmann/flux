local flux = require("flux")

local kernel = nil -- state: filled asynchronously by the "Show kernel" action

local function details(title, lines)
    local items = {}
    for _, line in ipairs(lines) do
        items[#items + 1] = { title = line, icon = "•" }
    end
    return flux.ui.list({ title = title, items = items })
end

local function build()
    local user = flux.preferences.get("username")
    -- The keyring may be missing/locked: show that instead of failing the whole command.
    local keyring_ok, token = pcall(flux.storage.secure.get, "session")
    if not keyring_ok then token = nil end

    return flux.ui.list({
        title = "Demo",
        search_bar_placeholder = "Search the demo…",

        -- Global actions: shown in the Action Panel (Ctrl+K) for every item.
        actions = {
            flux.action.section({
                title = "Demo",
                actions = {
                    flux.action.new({
                        title = "Reset kernel info",
                        description = "Clears the cached `uname` output",
                        shortcut = "ctrl+r",
                        on_action = function()
                            kernel = nil
                            return build()
                        end,
                    }),
                },
            }),
        },

        items = {
            {
                title = "Greet " .. tostring(user),
                subtitle = "Copies a greeting to the clipboard (Ctrl+Enter shows it in a new screen)",
                icon = "📋",
                actions = {
                    -- First action = Enter, second = Ctrl+Enter.
                    flux.action.new({
                        title = "Copy greeting",
                        description = "Hello, " .. tostring(user) .. "!",
                        on_action = function()
                            flux.clipboard.setText("Hello, " .. tostring(user) .. "!")
                            flux.ui.close()
                        end,
                    }),
                    flux.action.new({
                        title = "Show greeting",
                        on_action = function()
                            flux.navigation.push({ component = details("Greeting", { "Hello, " .. tostring(user) .. "!" }) })
                        end,
                    }),
                    flux.action.section({
                        title = "More",
                        actions = {
                            flux.action.new({
                                title = "Open Lua website",
                                shortcut = "ctrl+o",
                                on_action = function() flux.browser.open("https://www.lua.org") end,
                            }),
                        },
                    }),
                },
            },
            {
                title = "Show kernel",
                subtitle = kernel or "Press Enter to run `uname -a` without blocking the UI",
                icon = "🐧",
                actions = {
                    flux.action.new({
                        title = "Run uname",
                        on_action = function()
                            local r = flux.process.exec("uname", { "-a" })
                            kernel = (r.stdout:gsub("%s+$", ""))
                            flux.logger.info("uname:", kernel)
                            return build() -- returning a view replaces the current screen
                        end,
                    }),
                },
            },
            {
                title = "Slow task",
                subtitle = "Runs `sleep 20`; press Esc to cancel it (the process is killed)",
                icon = "⏳",
                actions = {
                    flux.action.new({
                        title = "Run slow task",
                        on_action = function()
                            flux.process.exec("sleep", { "20" }, { timeout = 60 }) -- exec defaults to a 30 s timeout
                            flux.process.exec("notify-send", { "Slow task finished" })
                            flux.logger.info("slow task finished")
                        end,
                    }),
                },
            },
            {
                title = "Session token",
                subtitle = (not keyring_ok) and "Keyring unavailable" or (token and "Saved in the keyring" or "Not saved"),
                icon = "🔑",
                actions = {
                    flux.action.new({
                        title = "Save a token",
                        on_action = function()
                            flux.navigation.push(flux.ui.input({
                                title = "Session token",
                                placeholder = "Paste a token (kept in the keyring)",
                                secure = true,
                                on_submit = function(text)
                                    flux.storage.secure.set("session", text)
                                    flux.navigation.pop()
                                    flux.navigation.replace(build())
                                end,
                            }))
                        end,
                    }),
                    flux.action.new({
                        title = "Forget token",
                        style = "destructive",
                        on_action = function()
                            flux.storage.secure.remove("session")
                            return build()
                        end,
                    }),
                },
            },
        },
    })
end

function main()
    return build()
end
