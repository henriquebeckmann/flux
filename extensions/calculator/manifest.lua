return {
    name = "calculator",
    title = "Calculator",
    description = "Evaluate expressions as you type, with history",
    version = "1.0.0",
    api_version = 1,

    commands = {
        { name = "main", title = "Calculator", description = "Type an expression: 2 * (3 + 4) ^ 2", icon = "calculator", mode = "view" },
    },
    permissions = { "clipboard.write" },
}
