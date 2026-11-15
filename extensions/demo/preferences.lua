-- Declared preferences. Edit them with the "Configure Demo" command (or Ctrl+K on a demo command).
return {
    { name = "username", title = "Username", description = "Used by the greeting", type = "text", default = "friend" },
    { name = "api_token", title = "API token", description = "Stored in the system keyring, never in the database", type = "password" },
    { name = "verbose", title = "Verbose logging", type = "checkbox", default = false },
    {
        name = "theme", title = "Theme", type = "dropdown", default = "dark",
        options = { { title = "Dark", value = "dark" }, { title = "Light", value = "light" } },
    },
}
