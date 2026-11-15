use std::collections::HashMap;

use flux_api::CommandInfo;

/// Fuzzy score (subsequence, case-insensitive). `None` if it does not match.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut qi, mut score, mut prev_match) = (0, 0, false);
    for (i, &c) in t.iter().enumerate() {
        if qi < q.len() && c == q[qi] {
            score += 10;
            if prev_match { score += 15 }
            if i == 0 || !t[i - 1].is_alphanumeric() { score += 20 } // start of word
            if i == qi { score += 10 } // prefixo
            qi += 1;
            prev_match = true;
        } else {
            prev_match = false;
        }
    }
    (qi == q.len()).then(|| score - (t.len() as i32 / 4))
}

/// Filters and ranks commands. `history`: `(id, count, last_used)`.
/// With an empty query, the most used commands come first.
pub fn search(commands: &[CommandInfo], query: &str, history: &[(String, u32, i64)]) -> Vec<CommandInfo> {
    let usage: HashMap<&str, u32> = history.iter().map(|(id, count, _)| (id.as_str(), *count)).collect();
    let mut scored: Vec<(i32, &CommandInfo)> = commands
        .iter()
        .filter_map(|c| {
            let title = fuzzy_score(query, &c.title);
            let sub = c.subtitle.as_deref().and_then(|s| fuzzy_score(query, s)).map(|s| s / 3);
            let base = title.or(sub)?;
            let frecency = ((1.0 + *usage.get(c.id.as_str()).unwrap_or(&0) as f32).ln() * 12.0) as i32;
            Some((base + frecency, c))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
    scored.into_iter().map(|(_, c)| c.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_api::CommandMode;

    fn cmd(id: &str, title: &str) -> CommandInfo {
        CommandInfo { id: id.into(), title: title.into(), subtitle: None, icon: None, mode: CommandMode::View, extension: None }
    }

    #[test]
    fn matches_subsequence_and_prefers_prefix() {
        assert!(fuzzy_score("hl", "Hello").is_some());
        assert!(fuzzy_score("xyz", "Hello").is_none());
        assert!(fuzzy_score("he", "Hello").unwrap() > fuzzy_score("he", "The end").unwrap());
    }

    #[test]
    fn frecency_breaks_ties_and_orders_empty_query() {
        let cmds = [cmd("a", "Alpha"), cmd("b", "Beta")];
        let hist = [("b".to_string(), 5, 0)];
        let r = search(&cmds, "", &hist);
        assert_eq!(r[0].id, "b");
        assert_eq!(search(&cmds, "al", &hist).len(), 1);
    }
}
