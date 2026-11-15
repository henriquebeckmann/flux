return {
    { name = "start_dir", title = "Start folder", type = "text", default = "~", description = "Where Browse Files opens (an absolute path or ~/…)." },
    { name = "view", title = "Default view", type = "dropdown", default = "list",
      options = { { title = "List", value = "list" }, { title = "Grid (thumbnails)", value = "grid" } } },
    { name = "hidden", title = "Show hidden files", type = "checkbox", default = false },
}
