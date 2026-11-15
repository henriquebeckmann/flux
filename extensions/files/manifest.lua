return {
    name = "files",
    title = "File Explorer",
    description = "Browse folders, preview files, open and copy paths",
    version = "1.0.0",
    api_version = 1,

    commands = {
        { name = "browse", title = "Browse Files", description = "Folders, previews and image thumbnails", icon = "folder", mode = "view" },
    },
    -- read-only access to the filesystem; clipboard for "Copy path". Flux's own data and
    -- credential folders (~/.ssh, ~/.gnupg…) are refused by the API regardless.
    permissions = { "filesystem.read", "clipboard.write" },
}
