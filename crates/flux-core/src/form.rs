//! Form state and validation, independent of any UI toolkit.
//!
//! The extension describes fields; the user edits values; the core owns the values in between.
//! A re-render (patch) must never clobber what the user typed: `default_value` is applied once,
//! a controlled `value` only when the *prop itself* changes between renders.

use std::collections::HashMap;

use flux_api::{Field, FieldKind, FieldValue, FormView, TextFormat, valid_iso};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FormState {
    values: HashMap<String, FieldValue>,
    /// Last controlled `value` prop seen per field, to detect when the extension changes it.
    controlled: HashMap<String, FieldValue>,
    /// Errors from validation (shown after the user touched the field or tried to submit).
    errors: HashMap<String, String>,
    /// Errors sent by the extension; cleared when the field changes.
    external: HashMap<String, String>,
    touched: std::collections::HashSet<String>,
    focus: usize,
    submitted_once: bool,
}

fn default_for(f: &Field) -> Option<FieldValue> {
    match (&f.default, &f.kind) {
        (Some(v), _) => Some(v.clone()),
        (None, FieldKind::Checkbox { .. }) => Some(FieldValue::Bool(false)),
        (None, FieldKind::Dropdown { options }) => options.first().map(|o| FieldValue::Text(o.value.clone())),
        (None, FieldKind::TagPicker { .. } | FieldKind::FilePicker { .. }) => Some(FieldValue::List(vec![])),
        (None, FieldKind::Separator | FieldKind::Description { .. }) => None,
        (None, _) => Some(FieldValue::Text(String::new())),
    }
}

impl FormState {
    pub fn new(view: &FormView) -> Self {
        let mut s = Self::default();
        s.reconcile(view);
        s.focus = view.fields.iter().position(|f| f.autofocus && is_focusable(f)).or_else(|| first_focusable(view)).unwrap_or(0);
        s
    }

    /// Brings the state in line with a (patched) view: new fields get their defaults, removed
    /// fields are dropped, controlled values that changed are applied, everything else is kept.
    pub fn reconcile(&mut self, view: &FormView) {
        let ids: std::collections::HashSet<&str> = view.fields.iter().map(|f| f.id.as_str()).collect();
        self.values.retain(|k, _| ids.contains(k.as_str()));
        self.controlled.retain(|k, _| ids.contains(k.as_str()));
        self.errors.retain(|k, _| ids.contains(k.as_str()));
        self.external.retain(|k, _| ids.contains(k.as_str()));
        self.touched.retain(|k| ids.contains(k.as_str()));
        for f in &view.fields {
            if !f.kind.has_value() {
                continue;
            }
            match &f.value {
                Some(v) => {
                    if self.controlled.get(&f.id) != Some(v) {
                        self.values.insert(f.id.clone(), v.clone());
                        self.controlled.insert(f.id.clone(), v.clone());
                    }
                }
                None => {
                    self.controlled.remove(&f.id);
                    if !self.values.contains_key(&f.id) {
                        if let Some(d) = default_for(f) {
                            self.values.insert(f.id.clone(), d);
                        }
                    }
                }
            }
            // a dropdown whose selected option disappeared falls back to the first one
            if let (FieldKind::Dropdown { options }, Some(FieldValue::Text(cur))) = (&f.kind, self.values.get(&f.id)) {
                if !options.iter().any(|o| &o.value == cur) {
                    if let Some(first) = options.first() {
                        self.values.insert(f.id.clone(), FieldValue::Text(first.value.clone()));
                    }
                }
            }
            match &f.error {
                Some(e) => {
                    self.external.insert(f.id.clone(), e.clone());
                }
                None => {
                    self.external.remove(&f.id);
                }
            }
        }
        self.focus = self.focus.min(view.fields.len().saturating_sub(1));
    }

    pub fn value(&self, id: &str) -> Option<&FieldValue> {
        self.values.get(id)
    }

    pub fn text(&self, id: &str) -> &str {
        self.values.get(id).and_then(FieldValue::as_text).unwrap_or("")
    }

    pub fn bool(&self, id: &str) -> bool {
        matches!(self.values.get(id), Some(FieldValue::Bool(true)))
    }

    pub fn list(&self, id: &str) -> &[String] {
        match self.values.get(id) {
            Some(FieldValue::List(v)) => v,
            _ => &[],
        }
    }

    /// The user changed a field. Returns the new validation error (if any).
    pub fn set(&mut self, view: &FormView, id: &str, value: FieldValue) -> Option<String> {
        let Some(field) = view.fields.iter().find(|f| f.id == id) else { return None };
        self.values.insert(id.to_string(), value);
        self.touched.insert(id.to_string());
        self.external.remove(id); // the extension's complaint is stale once the user edits
        self.revalidate(field);
        self.errors.get(id).cloned()
    }

    fn revalidate(&mut self, field: &Field) {
        match validate(field, self.values.get(&field.id)) {
            Some(e) => {
                self.errors.insert(field.id.clone(), e);
            }
            None => {
                self.errors.remove(&field.id);
            }
        }
    }

    /// Error to display for a field: the extension's, or validation after interaction.
    pub fn error(&self, id: &str) -> Option<&str> {
        if let Some(e) = self.external.get(id) {
            return Some(e);
        }
        if self.touched.contains(id) || self.submitted_once {
            return self.errors.get(id).map(String::as_str);
        }
        None
    }

    pub fn set_external_errors(&mut self, errors: &[(String, String)]) {
        for (id, msg) in errors {
            self.external.insert(id.clone(), msg.clone());
        }
    }

    /// Validates every field (marking them as shown). Returns the errors, in field order.
    pub fn validate_all(&mut self, view: &FormView) -> Vec<(String, String)> {
        self.submitted_once = true;
        let mut out = Vec::new();
        for f in view.fields.iter().filter(|f| f.kind.has_value()) {
            self.revalidate(f);
            if let Some(e) = self.errors.get(&f.id) {
                out.push((f.id.clone(), e.clone()));
            }
        }
        if let Some((id, _)) = out.first() {
            if let Some(i) = view.fields.iter().position(|f| &f.id == id) {
                self.focus = i;
            }
        }
        out
    }

    /// Values for `on_submit`, in field order.
    pub fn submit_values(&self, view: &FormView) -> Vec<(String, FieldValue)> {
        view.fields.iter().filter(|f| f.kind.has_value()).filter_map(|f| self.values.get(&f.id).map(|v| (f.id.clone(), v.clone()))).collect()
    }

    pub fn is_dirty(&self, view: &FormView) -> bool {
        view.fields.iter().filter(|f| f.kind.has_value()).any(|f| default_for(f).as_ref() != self.values.get(&f.id) && f.value.is_none())
    }

    // ------------------------------------------------------------- focus

    pub fn focus(&self) -> usize {
        self.focus
    }

    pub fn focused_id<'a>(&self, view: &'a FormView) -> Option<&'a str> {
        view.fields.get(self.focus).map(|f| f.id.as_str())
    }

    pub fn set_focus(&mut self, view: &FormView, id: &str) {
        if let Some(i) = view.fields.iter().position(|f| f.id == id && is_focusable(f)) {
            self.focus = i;
        }
    }

    pub fn focus_next(&mut self, view: &FormView) {
        self.step_focus(view, 1);
    }

    pub fn focus_prev(&mut self, view: &FormView) {
        self.step_focus(view, -1);
    }

    fn step_focus(&mut self, view: &FormView, dir: isize) {
        let n = view.fields.len() as isize;
        if n == 0 {
            return;
        }
        for step in 1..=n {
            let i = (self.focus as isize + dir * step).rem_euclid(n) as usize;
            if is_focusable(&view.fields[i]) {
                self.focus = i;
                return;
            }
        }
    }
}

pub fn is_focusable(f: &Field) -> bool {
    f.kind.has_value()
}

fn first_focusable(view: &FormView) -> Option<usize> {
    view.fields.iter().position(is_focusable)
}

fn empty(v: Option<&FieldValue>) -> bool {
    match v {
        None => true,
        Some(FieldValue::Text(s)) => s.trim().is_empty(),
        Some(FieldValue::List(l)) => l.is_empty(),
        Some(FieldValue::Bool(_)) => false,
    }
}

fn valid_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else { return false };
    !local.is_empty()
        && !s.contains(char::is_whitespace)
        && !domain.contains('@')
        && domain.contains('.')
        && domain.split('.').all(|p| !p.is_empty())
}

fn valid_url(s: &str) -> bool {
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://"));
    rest.is_some_and(|r| !r.is_empty() && !r.starts_with('/') && !s.contains(char::is_whitespace))
}

/// Declarative validation of one field. `None` = valid.
pub fn validate(f: &Field, v: Option<&FieldValue>) -> Option<String> {
    match &f.kind {
        FieldKind::Separator | FieldKind::Description { .. } => return None,
        FieldKind::Checkbox { .. } => {
            if f.required && !matches!(v, Some(FieldValue::Bool(true))) {
                return Some("This box must be checked".into());
            }
            return None;
        }
        _ => {}
    }
    if empty(v) {
        return f.required.then(|| "This field is required".to_string());
    }
    match (&f.kind, v) {
        (FieldKind::Dropdown { options }, Some(FieldValue::Text(t))) => {
            if !options.iter().any(|o| &o.value == t) {
                return Some("Choose one of the options".into());
            }
        }
        (FieldKind::TagPicker { options }, Some(FieldValue::List(l))) => {
            if l.iter().any(|x| !options.iter().any(|o| &o.value == x)) {
                return Some("Contains an unknown option".into());
            }
        }
        (FieldKind::DatePicker { include_time, min, max }, Some(FieldValue::Text(t))) => {
            if !valid_iso(t, *include_time) {
                return Some(if *include_time { "Use the format YYYY-MM-DD HH:MM".into() } else { "Use the format YYYY-MM-DD".into() });
            }
            if min.as_ref().is_some_and(|m| t < m) {
                return Some(format!("Must be on or after {}", min.as_ref().unwrap().replace('T', " ")));
            }
            if max.as_ref().is_some_and(|m| t > m) {
                return Some(format!("Must be on or before {}", max.as_ref().unwrap().replace('T', " ")));
            }
        }
        (FieldKind::Text | FieldKind::Password | FieldKind::TextArea { .. }, Some(FieldValue::Text(t))) => {
            let n = t.chars().count();
            if let Some(m) = f.rules.min_length {
                if n < m {
                    return Some(format!("Enter at least {m} character{}", if m == 1 { "" } else { "s" }));
                }
            }
            if let Some(m) = f.rules.max_length {
                if n > m {
                    return Some(format!("Enter at most {m} character{}", if m == 1 { "" } else { "s" }));
                }
            }
            match f.rules.format {
                Some(TextFormat::Email) if !valid_email(t.trim()) => return Some("Enter a valid email address".into()),
                Some(TextFormat::Url) if !valid_url(t.trim()) => return Some("Enter a valid http(s) URL".into()),
                Some(TextFormat::Integer) => match t.trim().parse::<i64>() {
                    Ok(n) => return range(f, n as f64),
                    Err(_) => return Some("Enter a whole number".into()),
                },
                Some(TextFormat::Number) => match t.trim().parse::<f64>() {
                    Ok(n) if n.is_finite() => return range(f, n),
                    _ => return Some("Enter a number".into()),
                },
                _ => {}
            }
        }
        _ => {}
    }
    None
}

fn range(f: &Field, n: f64) -> Option<String> {
    if f.rules.min.is_some_and(|m| n < m) {
        return Some(format!("Must be at least {}", f.rules.min.unwrap()));
    }
    if f.rules.max.is_some_and(|m| n > m) {
        return Some(format!("Must be at most {}", f.rules.max.unwrap()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_api::{FieldOption, Rules};

    fn text(id: &str) -> Field {
        Field::new(id, FieldKind::Text)
    }

    fn form(fields: Vec<Field>) -> FormView {
        FormView { fields, ..Default::default() }
    }

    fn opt(v: &str) -> FieldOption {
        FieldOption { value: v.into(), title: v.into(), icon: None, section: None }
    }

    #[test]
    fn defaults_are_applied_once_and_never_clobber_typing() {
        let mut f = text("a");
        f.default = Some(FieldValue::Text("init".into()));
        let mut view = form(vec![f, Field::new("c", FieldKind::Checkbox { label: "x".into() }), Field::new("d", FieldKind::Dropdown { options: vec![opt("r"), opt("g")] })]);
        let mut st = FormState::new(&view);
        assert_eq!((st.text("a"), st.bool("c"), st.text("d")), ("init", false, "r"));
        st.set(&view, "a", FieldValue::Text("typed".into()));
        // the extension re-renders with a *different* default: the user's text wins
        view.fields[0].default = Some(FieldValue::Text("other".into()));
        st.reconcile(&view);
        assert_eq!(st.text("a"), "typed");
    }

    #[test]
    fn controlled_values_follow_the_prop_only_when_it_changes() {
        let mut f = text("a");
        f.value = Some(FieldValue::Text("one".into()));
        let mut view = form(vec![f]);
        let mut st = FormState::new(&view);
        assert_eq!(st.text("a"), "one");
        st.set(&view, "a", FieldValue::Text("user".into()));
        st.reconcile(&view); // same prop -> keep what the user typed (optimistic)
        assert_eq!(st.text("a"), "user");
        view.fields[0].value = Some(FieldValue::Text("two".into()));
        st.reconcile(&view); // prop changed -> the extension wins
        assert_eq!(st.text("a"), "two");
    }

    #[test]
    fn fields_can_appear_and_disappear() {
        let mut view = form(vec![text("a"), text("b")]);
        let mut st = FormState::new(&view);
        st.set(&view, "a", FieldValue::Text("keep".into()));
        view.fields = vec![text("a"), text("n")];
        st.reconcile(&view);
        assert_eq!((st.text("a"), st.text("n")), ("keep", ""));
        assert!(st.value("b").is_none());
    }

    #[test]
    fn required_and_length_and_format_rules() {
        let mut f = text("e");
        f.required = true;
        assert_eq!(validate(&f, Some(&FieldValue::Text("  ".into()))).as_deref(), Some("This field is required"));
        f.rules = Rules { min_length: Some(3), max_length: Some(5), ..Default::default() };
        assert!(validate(&f, Some(&FieldValue::Text("ab".into()))).unwrap().contains("at least 3"));
        assert!(validate(&f, Some(&FieldValue::Text("abcdef".into()))).unwrap().contains("at most 5"));
        assert!(validate(&f, Some(&FieldValue::Text("héllo".into()))).is_none(), "length counts characters, not bytes");

        let mut m = text("m");
        m.rules.format = Some(TextFormat::Email);
        for ok in ["a@b.co", "first.last@sub.example.org"] {
            assert!(validate(&m, Some(&FieldValue::Text(ok.into()))).is_none(), "{ok}");
        }
        for bad in ["a", "a@", "@b.c", "a@b", "a b@c.d", "a@@b.c", "a@b..c"] {
            assert!(validate(&m, Some(&FieldValue::Text(bad.into()))).is_some(), "{bad}");
        }
        m.rules.format = Some(TextFormat::Url);
        assert!(validate(&m, Some(&FieldValue::Text("https://x.org/a".into()))).is_none());
        assert!(validate(&m, Some(&FieldValue::Text("javascript:alert(1)".into()))).is_some());
        assert!(validate(&m, Some(&FieldValue::Text("ftp://x".into()))).is_some());
        m.rules = Rules { format: Some(TextFormat::Integer), min: Some(1.0), max: Some(10.0), ..Default::default() };
        assert!(validate(&m, Some(&FieldValue::Text("5".into()))).is_none());
        assert!(validate(&m, Some(&FieldValue::Text("5.5".into()))).unwrap().contains("whole number"));
        assert!(validate(&m, Some(&FieldValue::Text("0".into()))).unwrap().contains("at least 1"));
        assert!(validate(&m, Some(&FieldValue::Text("11".into()))).unwrap().contains("at most 10"));
        m.rules.format = Some(TextFormat::Number);
        assert!(validate(&m, Some(&FieldValue::Text("2.5".into()))).is_none());
        assert!(validate(&m, Some(&FieldValue::Text("NaN".into()))).is_some());
    }

    #[test]
    fn checkbox_dropdown_tags_and_dates() {
        let mut c = Field::new("c", FieldKind::Checkbox { label: "x".into() });
        c.required = true;
        assert!(validate(&c, Some(&FieldValue::Bool(false))).is_some());
        assert!(validate(&c, Some(&FieldValue::Bool(true))).is_none());

        let d = Field::new("d", FieldKind::Dropdown { options: vec![opt("a")] });
        assert!(validate(&d, Some(&FieldValue::Text("zzz".into()))).is_some());
        let t = Field::new("t", FieldKind::TagPicker { options: vec![opt("a"), opt("b")] });
        assert!(validate(&t, Some(&FieldValue::List(vec!["a".into(), "b".into()]))).is_none());
        assert!(validate(&t, Some(&FieldValue::List(vec!["a".into(), "x".into()]))).is_some());
        let mut req = t.clone();
        req.required = true;
        assert!(validate(&req, Some(&FieldValue::List(vec![]))).is_some());

        let dp = Field::new("p", FieldKind::DatePicker { include_time: false, min: Some("2026-01-10".into()), max: Some("2026-12-31".into()) });
        assert!(validate(&dp, Some(&FieldValue::Text("2026-06-15".into()))).is_none());
        assert!(validate(&dp, Some(&FieldValue::Text("2026-02-30".into()))).unwrap().contains("YYYY-MM-DD"));
        assert!(validate(&dp, Some(&FieldValue::Text("2026-01-09".into()))).unwrap().contains("on or after"));
        assert!(validate(&dp, Some(&FieldValue::Text("2027-01-01".into()))).unwrap().contains("on or before"));
        assert!(validate(&dp, Some(&FieldValue::Text("".into()))).is_none(), "optional empty date is fine");
    }

    #[test]
    fn errors_show_after_interaction_or_submit_and_extension_errors_clear_on_edit() {
        let mut f = text("a");
        f.required = true;
        let view = form(vec![f]);
        let mut st = FormState::new(&view);
        assert_eq!(st.error("a"), None, "pristine fields are not nagged");
        let errs = st.validate_all(&view);
        assert_eq!(errs.len(), 1);
        assert!(st.error("a").is_some(), "after a submit attempt errors are shown");
        st.set(&view, "a", FieldValue::Text("ok".into()));
        assert_eq!(st.error("a"), None);
        st.set_external_errors(&[("a".into(), "taken".into())]);
        assert_eq!(st.error("a"), Some("taken"));
        st.set(&view, "a", FieldValue::Text("other".into()));
        assert_eq!(st.error("a"), None, "editing clears the extension's complaint");
    }

    #[test]
    fn focus_skips_separators_wraps_and_honours_autofocus() {
        let mut b = text("b");
        b.autofocus = true;
        let view = form(vec![Field::new("desc", FieldKind::Description { text: "d".into() }), text("a"), Field::new("sep", FieldKind::Separator), b, text("c")]);
        let mut st = FormState::new(&view);
        assert_eq!(st.focused_id(&view), Some("b"));
        st.focus_next(&view);
        assert_eq!(st.focused_id(&view), Some("c"));
        st.focus_next(&view);
        assert_eq!(st.focused_id(&view), Some("a"), "wraps and skips the description");
        st.focus_prev(&view);
        assert_eq!(st.focused_id(&view), Some("c"));
        let plain = form(vec![Field::new("d", FieldKind::Separator), text("x")]);
        assert_eq!(FormState::new(&plain).focused_id(&plain), Some("x"));
    }

    #[test]
    fn submit_values_skip_decorative_fields() {
        let view = form(vec![Field::new("d", FieldKind::Description { text: "d".into() }), text("a"), Field::new("c", FieldKind::Checkbox { label: "".into() })]);
        let mut st = FormState::new(&view);
        st.set(&view, "a", FieldValue::Text("v".into()));
        assert_eq!(st.submit_values(&view), vec![("a".into(), FieldValue::Text("v".into())), ("c".into(), FieldValue::Bool(false))]);
    }
}
