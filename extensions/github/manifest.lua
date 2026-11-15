return {
    name = "github",
    title = "GitHub",
    description = "Search repositories and read their README",
    version = "1.0.0",
    api_version = 1,

    commands = {
        { name = "search", title = "Search GitHub Repositories", description = "Type to search; Enter shows the details", icon = "git-branch", mode = "view" },
    },

    -- network: the GitHub API; browser/clipboard: the actions. Only these hosts are reachable
    -- (change `api_url` in the preferences and add its host here for GitHub Enterprise).
    permissions = { "network", "browser.open", "clipboard.write" },
    allowed_hosts = { "api.github.com", "avatars.githubusercontent.com" },
}
