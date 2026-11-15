return {
    name = "{{id}}",
    title = "{{title}}",
    description = "A searchable list",
    version = "0.1.0",
    api_version = 1,

    commands = {
        { name = "main", title = "{{title}}", description = "Search the list", icon = "list", mode = "view" },
    },

    -- Add "network" (and allowed_hosts) to call APIs with flux.http.
    permissions = { "clipboard.write" },
}
