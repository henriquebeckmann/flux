return {
    name = "processes",
    title = "Process Manager",
    description = "See what is running and stop it",
    version = "1.0.0",
    api_version = 1,

    commands = {
        { name = "list", title = "Manage Processes", description = "Live list with CPU and memory; terminate with confirmation", icon = "cpu", mode = "view" },
    },
    -- process.execute: runs `ps` (and only `ps`). process.kill: may signal processes of the same user.
    permissions = { "process.execute", "process.kill", "clipboard.write" },
    -- kill = "user": the whole point of this extension is to stop processes it did not start.
    grants = { exec = { "ps" }, kill = "user" },
}
