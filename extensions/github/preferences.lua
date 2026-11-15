return {
    { name = "token", title = "Personal access token", type = "password",
      description = "Optional. Raises the API rate limit from 60 to 5000 requests per hour." },
    { name = "api_url", title = "API URL", type = "text", default = "https://api.github.com",
      description = "Change for GitHub Enterprise (and add the host to allowed_hosts in manifest.lua)." },
    { name = "avatars", title = "Show owner avatars", type = "checkbox", default = true },
}
