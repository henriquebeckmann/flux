//! APIs that still work but have a replacement. Each entry says since when, what replaces it and
//! when it may go away, so authors get a message instead of a surprise (see docs/api-versioning.md).

/// A deprecated name found by looking for `pattern` in Lua source (comments excluded).
pub struct Deprecated {
    pub pattern: &'static str,
    pub since: &'static str,
    /// First API version in which it may stop working.
    pub removal_api: u32,
    pub replacement: &'static str,
}

pub const DEPRECATED: &[Deprecated] = &[
    Deprecated { pattern: "flux.ui.list", since: "0.1", removal_api: 2, replacement: "flux.ui.List" },
    Deprecated { pattern: "flux.ui.input", since: "0.1", removal_api: 2, replacement: "flux.ui.Input" },
];

impl Deprecated {
    pub fn message(&self) -> String {
        format!(
            "{} is deprecated since Flux {}; use {} (it may be removed in API {})",
            self.pattern, self.since, self.replacement, self.removal_api
        )
    }
}

/// Deprecated names used by `src` (once each).
pub fn scan(src: &str) -> Vec<&'static Deprecated> {
    let code: String = src.lines().map(|l| l.split("--").next().unwrap_or("")).collect::<Vec<_>>().join("\n");
    DEPRECATED.iter().filter(|d| code.contains(d.pattern)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_old_names_but_not_their_replacements_or_comments() {
        assert_eq!(scan("return flux.ui.list { }").len(), 1);
        assert!(scan("return flux.ui.List { }").is_empty());
        assert!(scan("-- flux.ui.list is old\nreturn 1").is_empty());
    }
}
