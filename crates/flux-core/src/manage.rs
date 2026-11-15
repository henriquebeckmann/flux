//! Built-in screens for managing extensions ("Manage Extensions"). They are ordinary view trees
//! built by the core (no Lua); their actions are [`NativeAction`]s the core executes itself.

use std::collections::HashMap;

use flux_api::*;

use crate::logs::LogLine;

#[derive(Debug, Clone, PartialEq)]
pub enum NativeAction {
    ShowInfo(String),
    Toggle { name: String, enable: bool },
    Reload(String),
    ShowLogs(String),
    ShowErrors(String),
    Remove(String),
    OpenFolder(String),
    CopyPath(String),
    /// Take a permission away from an extension (`revoke`) or give it back; reloads the extension.
    SetPermission { name: String, cap: String, revoke: bool },
}

pub struct Native {
    pub node: ViewNode,
    pub actions: HashMap<u64, NativeAction>,
}

struct Ids {
    next: u64,
    map: HashMap<u64, NativeAction>,
}

impl Ids {
    fn new() -> Self {
        Self { next: 0, map: HashMap::new() }
    }

    fn action(&mut self, title: &str, a: NativeAction) -> ActionSpec {
        self.next += 1;
        self.map.insert(self.next, a);
        ActionSpec { id: self.next, title: title.into(), ..Default::default() }
    }
}

fn icon(name: &str, tint: &str) -> Icon {
    Icon { kind: IconKind::Named(name.into()), tint: Some(tint.into()) }
}

fn status_text(s: &ExtensionStatus) -> (&'static str, &'static str, &'static str, &'static str) {
    // (label, icon, color, tag color)
    match s {
        ExtensionStatus::Loaded => ("Running", "check", "green", "green"),
        ExtensionStatus::Disabled => ("Disabled", "x", "gray", "gray"),
        ExtensionStatus::Invalid { .. } => ("Invalid", "warning", "red", "red"),
        ExtensionStatus::Crashed { .. } => ("Restarted", "warning", "orange", "orange"),
        ExtensionStatus::Failed { .. } => ("Failed", "warning", "red", "red"),
    }
}

fn tags(items: &[String]) -> Vec<Tag> {
    items.iter().map(|t| Tag { text: t.clone(), color: None }).collect()
}

/// The "Manage Extensions" list. `can_remove` tells which extensions `flux remove` could delete.
pub fn extensions_list(infos: &[ExtensionInfo], can_remove: impl Fn(&ExtensionInfo) -> bool) -> Native {
    let mut ids = Ids::new();
    let mut items = Vec::new();
    for i in infos {
        let (label, icon_name, tint, tag_color) = status_text(&i.status);
        let mut sections = vec![ActionSection {
            title: None,
            actions: vec![ids.action("Show Details", NativeAction::ShowInfo(i.name.clone())), ids.action(if i.enabled { "Disable" } else { "Enable" }, NativeAction::Toggle { name: i.name.clone(), enable: !i.enabled })],
        }];
        let mut more = vec![ids.action("Reload", NativeAction::Reload(i.name.clone())), ids.action("Show Logs", NativeAction::ShowLogs(i.name.clone()))];
        if matches!(i.status, ExtensionStatus::Invalid { .. } | ExtensionStatus::Crashed { .. } | ExtensionStatus::Failed { .. }) {
            more.insert(0, ids.action("Show Errors", NativeAction::ShowErrors(i.name.clone())));
        }
        sections.push(ActionSection { title: Some("Extension".into()), actions: more });
        let mut files = vec![ids.action("Open Folder", NativeAction::OpenFolder(i.dir.clone())), ids.action("Copy Path", NativeAction::CopyPath(i.dir.clone()))];
        if can_remove(i) {
            let mut remove = ids.action("Remove Extension", NativeAction::Remove(i.name.clone()));
            remove.style = ActionStyle::Destructive;
            remove.confirm = Some(Confirm { title: format!("Remove {}?", i.title), message: Some("The extension's files are deleted. Its stored data is kept.".into()) });
            files.push(remove);
        }
        sections.push(ActionSection { title: Some("Files".into()), actions: files });
        let mut accessories = Vec::new();
        if !i.version.is_empty() {
            accessories.push(Accessory { text: Some(format!("v{}", i.version)), ..Default::default() });
        }
        accessories.push(Accessory { tag: Some(Tag { text: if i.source == ExtensionSource::Dev { "dev".into() } else { "installed".into() }, color: None }), ..Default::default() });
        accessories.push(Accessory { tag: Some(Tag { text: label.into(), color: Some(tag_color.into()) }), ..Default::default() });
        items.push(ListItem {
            key: i.name.clone(),
            title: i.title.clone(),
            subtitle: Some(match &i.description {
                Some(d) => format!("{} · {d}", i.name),
                None => i.name.clone(),
            }),
            icon: Some(icon(icon_name, tint)),
            accessories,
            keywords: i.commands.clone(),
            actions: sections,
        });
    }
    let node = ViewNode::List(ListView {
        title: Some("Manage Extensions".into()),
        search_bar_placeholder: Some("Search extensions…".into()),
        empty: EmptyView { title: Some("No extensions found".into()), description: Some("Install one with `flux install <path>`".into()), icon: Some(icon("package", "gray")) },
        sections: vec![Section { key: "all".into(), items, ..Default::default() }],
        ..Default::default()
    });
    Native { node, actions: ids.map }
}

fn fence(text: &str) -> String {
    format!("```\n{}\n```", text.replace("```", "'''"))
}

pub fn logs_detail(name: &str, lines: &[LogLine], file_hint: &str) -> Native {
    let body = if lines.is_empty() {
        "_No log lines yet._".to_string()
    } else {
        let tail = &lines[lines.len().saturating_sub(200)..];
        fence(&tail.iter().map(LogLine::render).collect::<Vec<_>>().join("\n"))
    };
    let md = format!("# {name}: logs\n\n{body}\n\nFull log: `{file_hint}`");
    Native { node: ViewNode::Detail(DetailView { title: Some(format!("{name}: logs")), blocks: vec![DetailBlock::Markdown(md)], ..Default::default() }), actions: HashMap::new() }
}

pub fn errors_detail(info: &ExtensionInfo, recent: &[LogLine]) -> Native {
    let mut md = format!("# {}: errors\n", info.title);
    match &info.status {
        ExtensionStatus::Invalid { errors } => {
            md.push_str("\nThe extension did not load:\n\n");
            for e in errors {
                md.push_str(&format!("- {}\n", e.replace('\n', " ")));
            }
        }
        ExtensionStatus::Crashed { message } => md.push_str(&format!("\nThe extension was restarted: {message}\n")),
        ExtensionStatus::Failed { message } => md.push_str(&format!("\nThe extension keeps failing and was stopped: {message}\nReload it to try again.\n")),
        _ => md.push_str("\nNo load problems.\n"),
    }
    if !recent.is_empty() {
        md.push_str("\n## Runtime errors\n\n");
        md.push_str(&fence(&recent.iter().map(LogLine::render).collect::<Vec<_>>().join("\n")));
    }
    Native { node: ViewNode::Detail(DetailView { title: Some(format!("{}: errors", info.title)), blocks: vec![DetailBlock::Markdown(md)], ..Default::default() }), actions: HashMap::new() }
}

pub fn info_detail(info: &ExtensionInfo) -> Native {
    let (label, ..) = status_text(&info.status);
    let mut md = format!("# {}\n", info.title);
    if let Some(d) = &info.description {
        md.push_str(&format!("\n{d}\n"));
    }
    if let ExtensionStatus::Invalid { errors } = &info.status {
        md.push_str("\n## Errors\n\n");
        for e in errors {
            md.push_str(&format!("- {}\n", e.replace('\n', " ")));
        }
    }
    let label_item = |title: &str, text: &str| MetaItem::Label { title: title.into(), text: text.into(), icon: None };
    let mut meta = vec![label_item("Id", &info.name), label_item("Version", if info.version.is_empty() { "—" } else { &info.version }), label_item("Status", label)];
    if let Some(a) = &info.author {
        meta.push(label_item("Author", a));
    }
    if let Some(l) = &info.license {
        meta.push(label_item("License", l));
    }
    meta.push(label_item("Source", if info.source == ExtensionSource::Dev { "development folder" } else { "installed" }));
    meta.push(label_item("Folder", &info.dir));
    if !info.isolation.is_empty() {
        meta.push(label_item("Isolation", &info.isolation));
    }
    meta.push(MetaItem::Separator);
    meta.push(MetaItem::TagList { title: "Permissions".into(), tags: if info.permissions.is_empty() { vec![Tag { text: "none".into(), color: Some("gray".into()) }] } else { tags(&info.permissions) } });
    if !info.revoked_permissions.is_empty() {
        meta.push(MetaItem::TagList { title: "Revoked by you".into(), tags: info.revoked_permissions.iter().map(|t| Tag { text: t.clone(), color: Some("red".into()) }).collect() });
    }
    if !info.allowed_hosts.is_empty() {
        meta.push(MetaItem::TagList { title: "Allowed hosts".into(), tags: tags(&info.allowed_hosts) });
    }
    if !info.commands.is_empty() {
        meta.push(MetaItem::TagList { title: "Commands".into(), tags: tags(&info.commands) });
    }
    // One action per permission: take it away, or give it back.
    let mut ids = Ids::new();
    let mut revoke = Vec::new();
    for cap in info.permissions.iter().filter(|p| !p.starts_with("credentials.")) {
        let mut a = ids.action(&format!("Revoke {cap}"), NativeAction::SetPermission { name: info.name.clone(), cap: cap.clone(), revoke: true });
        a.style = ActionStyle::Destructive;
        a.confirm = Some(Confirm { title: format!("Revoke {cap} from {}?", info.title), message: Some("The extension is reloaded without it. You can restore it here later.".into()) });
        revoke.push(a);
    }
    let restore: Vec<ActionSpec> = info.revoked_permissions.iter().map(|cap| ids.action(&format!("Restore {cap}"), NativeAction::SetPermission { name: info.name.clone(), cap: cap.clone(), revoke: false })).collect();
    // Enter runs the first action: it must be harmless, never a revoke.
    let mut sections = vec![ActionSection {
        title: None,
        actions: vec![ids.action("Reload", NativeAction::Reload(info.name.clone())), ids.action("Show Logs", NativeAction::ShowLogs(info.name.clone()))],
    }];
    if !restore.is_empty() {
        sections.push(ActionSection { title: Some("Restore".into()), actions: restore });
    }
    if !revoke.is_empty() {
        sections.push(ActionSection { title: Some("Permissions".into()), actions: revoke });
    }
    Native { node: ViewNode::Detail(DetailView { title: Some(info.title.clone()), blocks: vec![DetailBlock::Markdown(md)], metadata: meta, actions: sections, ..Default::default() }), actions: ids.map }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn info(name: &str, status: ExtensionStatus, enabled: bool, source: ExtensionSource) -> ExtensionInfo {
        ExtensionInfo {
            name: name.into(),
            title: name.to_uppercase(),
            version: "1.2.3".into(),
            description: Some("desc".into()),
            author: None,
            license: None,
            dir: format!("/x/{name}"),
            source,
            enabled,
            status,
            permissions: vec!["network".into()],
            allowed_hosts: vec!["api.x.com".into()],
            commands: vec!["go".into()],
            isolation: "process, confined".into(),
            revoked_permissions: vec![],
        }
    }

    #[test]
    fn list_shows_status_toggle_wording_and_only_removable_extensions_get_remove() {
        let infos = vec![
            info("on", ExtensionStatus::Loaded, true, ExtensionSource::Installed),
            info("off", ExtensionStatus::Disabled, false, ExtensionSource::Dev),
            info("bad", ExtensionStatus::Invalid { errors: vec!["manifest.lua: x".into()] }, true, ExtensionSource::Installed),
        ];
        let n = extensions_list(&infos, |i| i.source == ExtensionSource::Installed);
        let ViewNode::List(l) = &n.node else { panic!() };
        let items = &l.sections[0].items;
        let titles = |i: usize| -> Vec<String> { items[i].actions.iter().flat_map(|s| s.actions.iter().map(|a| a.title.clone())).collect() };
        assert!(titles(0).contains(&"Disable".to_string()) && titles(0).contains(&"Remove Extension".to_string()));
        assert!(titles(1).contains(&"Enable".to_string()) && !titles(1).contains(&"Remove Extension".to_string()), "dev extensions are not removable from the UI");
        assert!(titles(2).contains(&"Show Errors".to_string()));
        assert_eq!(items[0].accessories.last().unwrap().tag.as_ref().unwrap().text, "Running");
        assert_eq!(items[2].accessories.last().unwrap().tag.as_ref().unwrap().color.as_deref(), Some("red"));
        let remove = items[0].actions.iter().flat_map(|s| &s.actions).find(|a| a.title == "Remove Extension").unwrap();
        assert!(remove.confirm.is_some() && remove.style == ActionStyle::Destructive);
        // every action id is mapped to something the core can execute
        let all: Vec<u64> = items.iter().flat_map(|i| i.actions.iter().flat_map(|s| s.actions.iter().map(|a| a.id))).collect();
        assert!(all.iter().all(|id| n.actions.contains_key(id)) && all.len() == n.actions.len());
        assert_eq!(n.actions[&items[1].actions[0].actions[1].id], NativeAction::Toggle { name: "off".into(), enable: true });
    }

    #[test]
    fn detail_screens_describe_the_extension() {
        let i = info("x", ExtensionStatus::Invalid { errors: vec!["manifest.lua: bad".into()] }, true, ExtensionSource::Dev);
        let ViewNode::Detail(d) = info_detail(&i).node else { panic!() };
        assert!(matches!(&d.blocks[0], DetailBlock::Markdown(m) if m.contains("manifest.lua: bad")));
        assert!(d.metadata.iter().any(|m| matches!(m, MetaItem::TagList { title, tags } if title == "Permissions" && tags[0].text == "network")));
        assert!(d.metadata.iter().any(|m| matches!(m, MetaItem::TagList { title, .. } if title == "Allowed hosts")));

        let lines = vec![LogLine { ts_ms: 0, level: flux_api::LogLevel::Error, message: "boom ``` fence".into() }];
        let ViewNode::Detail(l) = logs_detail("x", &lines, "/logs/x.log").node else { panic!() };
        let DetailBlock::Markdown(md) = &l.blocks[0] else { panic!() };
        assert!(md.contains("1970-01-01T00:00:00Z ERROR boom ''' fence") && md.matches("```").count() == 2, "{md}");
        let ViewNode::Detail(e) = errors_detail(&i, &lines).node else { panic!() };
        let DetailBlock::Markdown(md) = &e.blocks[0] else { panic!() };
        assert!(md.contains("did not load") && md.contains("Runtime errors"));
    }
}
