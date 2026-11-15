//! Keyboard shortcuts as plain data, so matching can be tested without a UI toolkit.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A key press as reported by the UI (already normalized: lowercase key name).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyInput {
    pub key: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Shortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    pub key: String,
}

fn normalize_key(k: &str) -> String {
    match k.to_lowercase().as_str() {
        "return" => "enter".into(),
        "esc" => "escape".into(),
        "del" => "delete".into(),
        "arrowup" => "up".into(),
        "arrowdown" => "down".into(),
        "arrowleft" => "left".into(),
        "arrowright" => "right".into(),
        other => other.to_string(),
    }
}

impl Shortcut {
    pub fn new(key: &str) -> Self {
        Self { ctrl: false, alt: false, shift: false, meta: false, key: normalize_key(key) }
    }

    pub fn ctrl(mut self) -> Self {
        self.ctrl = true;
        self
    }

    /// Parses strings such as `"enter"`, `"ctrl+enter"`, `"Ctrl+Shift+C"`, `"alt+space"`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut out = Shortcut::new("");
        let mut key = None;
        for part in s.split('+').map(str::trim) {
            match part.to_lowercase().as_str() {
                "" => return Err(format!("invalid shortcut '{s}'")),
                "ctrl" | "control" => out.ctrl = true,
                "alt" | "option" => out.alt = true,
                "shift" => out.shift = true,
                "super" | "meta" | "cmd" | "win" => out.meta = true,
                _ => {
                    if key.replace(normalize_key(part)).is_some() {
                        return Err(format!("shortcut '{s}' has more than one key"));
                    }
                }
            }
        }
        out.key = key.ok_or_else(|| format!("shortcut '{s}' has no key"))?;
        Ok(out)
    }

    pub fn matches(&self, input: &KeyInput) -> bool {
        self.key == normalize_key(&input.key)
            && self.ctrl == input.ctrl
            && self.alt == input.alt
            && self.shift == input.shift
            && self.meta == input.meta
    }

    /// Human-readable form for chips, e.g. `Ctrl+Shift+C`, `↵`.
    pub fn pretty(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl { parts.push("Ctrl".into()) }
        if self.alt { parts.push("Alt".into()) }
        if self.shift { parts.push("Shift".into()) }
        if self.meta { parts.push("Super".into()) }
        parts.push(match self.key.as_str() {
            "enter" => "↵".into(),
            "escape" => "Esc".into(),
            "up" => "↑".into(),
            "down" => "↓".into(),
            "left" => "←".into(),
            "right" => "→".into(),
            "backspace" => "⌫".into(),
            "space" => "Space".into(),
            k if k.chars().count() == 1 => k.to_uppercase(),
            k => {
                let mut c = k.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            }
        });
        parts.join("+")
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl { f.write_str("ctrl+")? }
        if self.alt { f.write_str("alt+")? }
        if self.shift { f.write_str("shift+")? }
        if self.meta { f.write_str("super+")? }
        f.write_str(&self.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(key: &str, ctrl: bool, shift: bool) -> KeyInput {
        KeyInput { key: key.into(), ctrl, shift, ..Default::default() }
    }

    #[test]
    fn parses_and_matches() {
        let s = Shortcut::parse("Ctrl+Shift+C").unwrap();
        assert!(s.matches(&input("c", true, true)));
        assert!(!s.matches(&input("c", true, false)));
        assert!(!s.matches(&input("c", false, true)));
        assert_eq!(s.to_string(), "ctrl+shift+c");
        assert_eq!(s.pretty(), "Ctrl+Shift+C");
    }

    #[test]
    fn aliases_and_errors() {
        assert_eq!(Shortcut::parse("return").unwrap(), Shortcut::new("enter"));
        assert!(Shortcut::parse("ctrl+").is_err());
        assert!(Shortcut::parse("ctrl").is_err());
        assert!(Shortcut::parse("a+b").is_err());
        assert_eq!(Shortcut::parse("alt+space").unwrap().pretty(), "Alt+Space");
    }
}
