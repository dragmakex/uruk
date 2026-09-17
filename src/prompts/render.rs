//! Variable binding and rendering (SPEC §5.1).
//!
//! "Missing inputs, unresolved template variables, or invalid structured
//! outputs fail validation before a scientific record is accepted."

use super::PromptTemplate;
use crate::records::ContentHash;
use std::collections::BTreeMap;

/// Why rendering failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("template {template}: no value bound for {{{var}}}")]
    MissingBinding { template: String, var: String },
    #[error("template {template}: rendered output still contains {{{var}}}")]
    UnresolvedVariable { template: String, var: String },
    #[error("template {template}: bound {{{var}}}, which the template does not use")]
    UnknownBinding { template: String, var: String },
}

/// Values bound to a template's variables.
#[derive(Debug, Clone, Default)]
pub struct Bindings(BTreeMap<String, String>);

impl Bindings {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a value. An absent optional input is bound explicitly rather than
    /// left out: SPEC §15.1 requires an optional source hypothesis to be
    /// "explicitly absent rather than silently invented".
    pub fn set(mut self, var: &str, value: impl Into<String>) -> Self {
        self.0.insert(var.to_string(), value.into());
        self
    }

    /// Bind a value that may be absent, with the standard absence marker.
    pub fn set_optional(self, var: &str, value: Option<impl Into<String>>) -> Self {
        match value {
            Some(v) => self.set(var, v),
            None => self.set(var, "(none supplied)"),
        }
    }

    pub fn get(&self, var: &str) -> Option<&str> {
        self.0.get(var).map(|s| s.as_str())
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.keys()
    }
}

/// A rendered prompt with the provenance a task must record (SPEC §5.1).
#[derive(Debug, Clone)]
pub struct Rendered {
    pub template_id: String,
    pub template_hash: ContentHash,
    pub text: String,
    /// Hash of the rendered text: exactly what was sent.
    pub input_hash: ContentHash,
}

/// A variable placeholder is `{name}` where `name` is a lowercase identifier,
/// optionally with spaces or digits — `{goal}`, `{hypothesis 1}`, `{review 2}`.
///
/// This deliberately excludes the JSON response schemas embedded in the
/// templates: `{"title": ...}` and `{` on its own line are not placeholders.
fn is_var_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == ' ')
        && s.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && !s.ends_with(' ')
}

/// Extract the variable names a template uses, in first-appearance order.
pub fn extract_vars(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'{'
            && let Some(end) = text[i + 1..].find('}')
        {
            {
                let inner = &text[i + 1..i + 1 + end];
                if is_var_name(inner) && !out.iter().any(|v| v == inner) {
                    out.push(inner.to_string());
                }
                i += end + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Render a template, requiring every variable to be bound.
pub fn render(template: &PromptTemplate, bindings: &Bindings) -> Result<Rendered, RenderError> {
    // Every variable the template uses must have a value.
    for var in &template.required_vars {
        if bindings.get(var).is_none() {
            return Err(RenderError::MissingBinding {
                template: template.id.to_string(),
                var: var.clone(),
            });
        }
    }

    // A binding the template does not use is a caller mistake: it usually
    // means the input was meant for a different template revision.
    for key in bindings.keys() {
        if !template.required_vars.contains(key) {
            return Err(RenderError::UnknownBinding {
                template: template.id.to_string(),
                var: key.clone(),
            });
        }
    }

    let mut text = String::with_capacity(template.text.len() * 2);
    let bytes = template.text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'{'
            && let Some(end) = template.text[i + 1..].find('}')
        {
            {
                let inner = &template.text[i + 1..i + 1 + end];
                if is_var_name(inner) {
                    // A required variable is always bound by the check above.
                    text.push_str(bindings.get(inner).unwrap_or(""));
                    i += end + 2;
                    continue;
                }
            }
        }
        // Not a placeholder: copy the byte through, JSON braces included.
        let ch_len = template.text[i..]
            .chars()
            .next()
            .map_or(1, |c| c.len_utf8());
        text.push_str(&template.text[i..i + ch_len]);
        i += ch_len;
    }

    // A substituted value could itself contain an unresolved placeholder.
    for var in &template.required_vars {
        let needle = format!("{{{var}}}");
        if text.contains(&needle) {
            return Err(RenderError::UnresolvedVariable {
                template: template.id.to_string(),
                var: var.clone(),
            });
        }
    }

    Ok(Rendered {
        template_id: template.id.to_string(),
        template_hash: template.hash.clone(),
        input_hash: ContentHash::of_str(&text),
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompts::{PromptRegistry, ids};

    fn t(text: &'static str) -> PromptTemplate {
        PromptTemplate {
            id: "test",
            text,
            hash: ContentHash::of_str(text),
            source: None,
            required_vars: extract_vars(text),
        }
    }

    #[test]
    fn json_braces_are_not_variables() {
        let vars = extract_vars(
            r#"Goal: {goal}
            Respond with:
            {
              "title": "<x>",
              "claims": [{"basis": "source_reported"}]
            }
            Compare {hypothesis 1} and {hypothesis 2}."#,
        );
        assert_eq!(vars, vec!["goal", "hypothesis 1", "hypothesis 2"]);
    }

    #[test]
    fn missing_binding_fails_before_dispatch() {
        let tpl = t("Goal: {goal}\nItem: {item}");
        let err = render(&tpl, &Bindings::new().set("goal", "g")).unwrap_err();
        assert_eq!(
            err,
            RenderError::MissingBinding {
                template: "test".into(),
                var: "item".into()
            }
        );
    }

    #[test]
    fn unknown_binding_is_rejected() {
        let tpl = t("Goal: {goal}");
        let err = render(
            &tpl,
            &Bindings::new().set("goal", "g").set("stale_var", "x"),
        )
        .unwrap_err();
        assert!(matches!(err, RenderError::UnknownBinding { .. }), "{err}");
    }

    #[test]
    fn absent_optional_is_explicit_not_silent() {
        let tpl = t("Existing: {source_hypothesis}\nGoal: {goal}");
        let r = render(
            &tpl,
            &Bindings::new()
                .set("goal", "g")
                .set_optional("source_hypothesis", None::<String>),
        )
        .unwrap();
        assert!(r.text.contains("(none supplied)"));
    }

    #[test]
    fn rendered_hash_pins_the_exact_input() {
        let tpl = t("Goal: {goal}");
        let a = render(&tpl, &Bindings::new().set("goal", "x")).unwrap();
        let b = render(&tpl, &Bindings::new().set("goal", "y")).unwrap();
        assert_ne!(a.input_hash, b.input_hash);
        assert_eq!(a.template_hash, b.template_hash, "same template revision");
    }

    #[test]
    fn every_shipped_template_renders_with_its_declared_vars() {
        let reg = PromptRegistry::new();
        for id in reg.ids() {
            let tpl = reg.get(id).unwrap();
            let mut b = Bindings::new();
            for var in &tpl.required_vars {
                b = b.set(var, format!("<{var}>"));
            }
            let rendered =
                render(tpl, &b).unwrap_or_else(|e| panic!("template {id} failed to render: {e}"));
            // The JSON response schema must survive rendering intact.
            assert!(
                rendered.text.contains('{'),
                "{id}: response schema was eaten by the renderer"
            );
        }
    }

    #[test]
    fn debate_templates_bind_turn_budget() {
        let reg = PromptRegistry::new();
        for id in [ids::GENERATION_DEBATE, ids::RANKING_DEBATE] {
            let vars = &reg.get(id).unwrap().required_vars;
            for needed in ["turn", "max_turns", "turns_remaining", "transcript"] {
                assert!(
                    vars.iter().any(|v| v == needed),
                    "{id} must bind {{{needed}}} (SPEC §15.1)"
                );
            }
        }
    }

    #[test]
    fn ranking_templates_never_bind_scores_or_elo() {
        let reg = PromptRegistry::new();
        for id in [ids::RANKING_PAIRWISE, ids::RANKING_DEBATE] {
            let tpl = reg.get(id).unwrap();
            for forbidden in ["score", "scores", "elo", "rating", "credits"] {
                assert!(
                    !tpl.required_vars.iter().any(|v| v == forbidden),
                    "{id} must not bind {{{forbidden}}} (SPEC §7 step 2)"
                );
            }
        }
    }
}
