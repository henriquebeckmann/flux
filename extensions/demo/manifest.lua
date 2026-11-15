return {
    name = "demo",
    title = "Demo",
    description = "Flux demo extension",
    version = "1.0.0",

    commands = {
        { name = "hello", title = "Hello from Lua", description = "Example list with actions", icon = "👋", mode = "view" },
        { name = "sync", title = "Demo: Sync", description = "Increments a counter in storage", icon = "🔄", mode = "no-view" },
        { name = "form", title = "Demo: Form", description = "Every form field, with validation", icon = "file-text", mode = "view" },
        { name = "grid", title = "Demo: Grid", description = "Icons and images in a grid", icon = "image", mode = "view" },
        { name = "detail", title = "Demo: Detail", description = "Markdown, metadata, progress", icon = "info", mode = "view" },
    },

    permissions = { "clipboard.read", "clipboard.write", "process.execute", "browser.open" },
    -- notify-send talks to the desktop session, so it is the one command that runs unconfined
    grants = { exec = { "uname", "sleep", "notify-send" }, exec_unconfined = { "notify-send" } },
}
