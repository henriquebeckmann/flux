return {
    name = "{{id}}",
    title = "{{title}}",
    description = "A detail page with Markdown",
    version = "0.1.0",
    api_version = 1,

    commands = {
        { name = "main", title = "{{title}}", description = "Show the page", icon = "info", mode = "view" },
    },
    permissions = { "browser.open" },
}
