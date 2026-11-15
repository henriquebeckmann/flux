//! Project templates for `flux create`, embedded in the binary.

pub const TEMPLATES: &[&str] = &["list", "grid", "form", "detail"];

pub struct File {
    pub path: &'static str,
    pub content: &'static str,
}

const COMMON: &[File] = &[
    File { path: "README.md", content: include_str!("../templates/common/README.md") },
    File { path: ".luarc.json", content: include_str!("../templates/common/luarc.json") },
    File { path: "flux.d.lua", content: include_str!("../templates/common/flux.d.lua") },
];

pub fn files(template: &str) -> Option<Vec<&'static File>> {
    static LIST: [File; 2] = [
        File { path: "manifest.lua", content: include_str!("../templates/list/manifest.lua") },
        File { path: "commands/main.lua", content: include_str!("../templates/list/commands/main.lua") },
    ];
    static GRID: [File; 2] = [
        File { path: "manifest.lua", content: include_str!("../templates/grid/manifest.lua") },
        File { path: "commands/main.lua", content: include_str!("../templates/grid/commands/main.lua") },
    ];
    static FORM: [File; 2] = [
        File { path: "manifest.lua", content: include_str!("../templates/form/manifest.lua") },
        File { path: "commands/main.lua", content: include_str!("../templates/form/commands/main.lua") },
    ];
    static DETAIL: [File; 2] = [
        File { path: "manifest.lua", content: include_str!("../templates/detail/manifest.lua") },
        File { path: "commands/main.lua", content: include_str!("../templates/detail/commands/main.lua") },
    ];
    let specific: &[File] = match template {
        "list" => &LIST,
        "grid" => &GRID,
        "form" => &FORM,
        "detail" => &DETAIL,
        _ => return None,
    };
    Some(specific.iter().chain(COMMON.iter()).collect())
}

/// `my-ext` -> `My Ext`
pub fn title_of(id: &str) -> String {
    id.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn render(content: &str, id: &str) -> String {
    content.replace("{{id}}", id).replace("{{title}}", &title_of(id))
}
