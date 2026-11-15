local ui = flux.ui

function main()
    return ui.Detail {
        title = "{{title}}",
        markdown = [[
# {{title}}

Detail pages render **Markdown**: headings, lists, `code`, [links](https://example.com) and tables.

| Component | Use it for |
|-----------|------------|
| List      | Searchable rows |
| Grid      | Images and icons |
| Detail    | This page |
| Form      | Input with validation |
]],
        metadata = ui.Detail.Metadata {
            ui.Detail.Metadata.Label { title = "Version", text = "0.1.0", icon = "tag" },
            ui.Detail.Metadata.Separator {},
            ui.Detail.Metadata.TagList { title = "Components",
                ui.Detail.Metadata.TagList.Item { text = "List", color = "blue" },
                ui.Detail.Metadata.TagList.Item { text = "Detail", color = "green" } },
            ui.Detail.Metadata.Link { title = "Docs", text = "example.com", url = "https://example.com" },
        },
        actions = {
            ui.Action { title = "Open website", on_action = function() flux.browser.open("https://example.com") end },
        },
    }
end
