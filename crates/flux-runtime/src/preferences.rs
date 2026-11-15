//! `preferences.lua`: declaration of an extension's settings.

use std::collections::HashSet;

use flux_api::{PrefValue, PreferenceKind, PreferenceOption, PreferenceSpec};
use mlua::{Lua, LuaSerdeExt, Table};
use serde::Deserialize;

#[derive(Deserialize)]
struct RawOption {
    title: Option<String>,
    value: String,
}

#[derive(Deserialize)]
struct RawPref {
    name: String,
    title: Option<String>,
    description: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    required: bool,
    default: Option<PrefValue>,
    #[serde(default)]
    options: Vec<RawOption>,
}

pub fn parse(lua: &Lua, table: Table) -> Result<Vec<PreferenceSpec>, String> {
    let raw: Vec<RawPref> = lua
        .from_value(mlua::Value::Table(table))
        .map_err(|e| format!("preferences.lua must return a list of preference tables: {e}"))?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in raw {
        if p.name.is_empty() || !p.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err(format!("invalid preference name '{}'", p.name));
        }
        if !seen.insert(p.name.clone()) {
            return Err(format!("duplicate preference '{}'", p.name));
        }
        let kind = match p.kind.as_str() {
            "text" => PreferenceKind::Text,
            "password" => PreferenceKind::Password,
            "checkbox" => PreferenceKind::Checkbox,
            "dropdown" => PreferenceKind::Dropdown,
            other => return Err(format!("preference '{}': unknown type '{other}' (text, password, checkbox, dropdown)", p.name)),
        };
        if kind == PreferenceKind::Dropdown && p.options.is_empty() {
            return Err(format!("preference '{}': dropdown needs options", p.name));
        }
        if kind == PreferenceKind::Password && p.default.is_some() {
            return Err(format!("preference '{}': password preferences cannot have a default", p.name));
        }
        match (&kind, &p.default) {
            (PreferenceKind::Checkbox, Some(PrefValue::Text(_))) => return Err(format!("preference '{}': checkbox default must be a boolean", p.name)),
            (PreferenceKind::Text | PreferenceKind::Dropdown, Some(PrefValue::Bool(_))) => return Err(format!("preference '{}': default must be a string", p.name)),
            _ => {}
        }
        out.push(PreferenceSpec {
            title: p.title.unwrap_or_else(|| p.name.clone()),
            name: p.name,
            description: p.description,
            kind,
            required: p.required,
            default: p.default,
            options: p.options.into_iter().map(|o| PreferenceOption { title: o.title.unwrap_or_else(|| o.value.clone()), value: o.value }).collect(),
        });
    }
    Ok(out)
}
