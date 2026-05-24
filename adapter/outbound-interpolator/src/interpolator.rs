use anyhow::Context;
use catapulte_domain::entity::body::{InterpolatedBody, Plain, ResolvedBody};
use catapulte_domain::port::template_interpolator::{InterpolateError, TemplateInterpolator};
use minijinja::Environment;

fn render_str(
    source: &str,
    variables: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, InterpolateError> {
    let mut env = Environment::new();
    env.add_template("t", source)
        .context("adding template")
        .map_err(|source| InterpolateError::Engine { source })?;
    let tmpl = env
        .get_template("t")
        .context("getting template")
        .map_err(|source| InterpolateError::Engine { source })?;
    tmpl.render(variables)
        .context("rendering template")
        .map_err(|source| InterpolateError::Engine { source })
}

pub struct MiniJinjaInterpolator;

impl MiniJinjaInterpolator {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for MiniJinjaInterpolator {
    fn default() -> Self {
        Self::new()
    }
}

fn interpolate_plain(
    plain: Plain,
    variables: &serde_json::Map<String, serde_json::Value>,
) -> Result<InterpolatedBody, InterpolateError> {
    let (text, html) = plain.into_parts();
    let text = text.map(|t| render_str(&t, variables)).transpose()?;
    let html = html.map(|h| render_str(&h, variables)).transpose()?;
    let plain = Plain::try_new(text, html)
        .context("reconstructing plain body")
        .map_err(|source| InterpolateError::Engine { source })?;
    Ok(InterpolatedBody::Plain(plain))
}

impl TemplateInterpolator for MiniJinjaInterpolator {
    fn interpolate(
        &self,
        body: ResolvedBody,
        variables: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<InterpolatedBody, InterpolateError> {
        match body {
            ResolvedBody::Mjml(source) => {
                render_str(&source, variables).map(InterpolatedBody::Mjml)
            }
            ResolvedBody::Plain(plain) => interpolate_plain(plain, variables),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    use catapulte_domain::entity::body::{Plain, ResolvedBody};
    use catapulte_domain::port::template_interpolator::{InterpolateError, TemplateInterpolator};

    use super::MiniJinjaInterpolator;

    #[test]
    fn interpolate_plain_text_substitutes_variable() {
        let interpolator = MiniJinjaInterpolator::new();
        let plain = Plain::try_new(Some("hello {{ name }}".to_string()), None).unwrap();
        let body = ResolvedBody::Plain(plain);
        let mut variables = Map::new();
        variables.insert("name".to_string(), Value::String("world".to_string()));

        let result = interpolator.interpolate(body, &variables).unwrap();

        match result {
            catapulte_domain::entity::body::InterpolatedBody::Plain(p) => {
                assert_eq!(p.text(), Some("hello world"));
                assert_eq!(p.html(), None);
            }
            catapulte_domain::entity::body::InterpolatedBody::Mjml(_) => {
                panic!("expected Plain variant")
            }
        }
    }

    #[test]
    fn interpolate_plain_html_substitutes_variable() {
        let interpolator = MiniJinjaInterpolator::new();
        let plain = Plain::try_new(None, Some("<p>{{ greeting }}</p>".to_string())).unwrap();
        let body = ResolvedBody::Plain(plain);
        let mut variables = Map::new();
        variables.insert("greeting".to_string(), Value::String("hi".to_string()));

        let result = interpolator.interpolate(body, &variables).unwrap();

        match result {
            catapulte_domain::entity::body::InterpolatedBody::Plain(p) => {
                assert_eq!(p.html(), Some("<p>hi</p>"));
                assert_eq!(p.text(), None);
            }
            catapulte_domain::entity::body::InterpolatedBody::Mjml(_) => {
                panic!("expected Plain variant")
            }
        }
    }

    #[test]
    fn interpolate_plain_both_parts_substituted() {
        let interpolator = MiniJinjaInterpolator::new();
        let plain = Plain::try_new(
            Some("text: {{ val }}".to_string()),
            Some("html: {{ val }}".to_string()),
        )
        .unwrap();
        let body = ResolvedBody::Plain(plain);
        let mut variables = Map::new();
        variables.insert("val".to_string(), Value::String("ok".to_string()));

        let result = interpolator.interpolate(body, &variables).unwrap();

        match result {
            catapulte_domain::entity::body::InterpolatedBody::Plain(p) => {
                assert_eq!(p.text(), Some("text: ok"));
                assert_eq!(p.html(), Some("html: ok"));
            }
            catapulte_domain::entity::body::InterpolatedBody::Mjml(_) => {
                panic!("expected Plain variant")
            }
        }
    }

    #[test]
    fn interpolate_mjml_substitutes_variable() {
        let interpolator = MiniJinjaInterpolator::new();
        let body = ResolvedBody::Mjml("<mj-text>{{ msg }}</mj-text>".to_string());
        let mut variables = Map::new();
        variables.insert("msg".to_string(), Value::String("hello".to_string()));

        let result = interpolator.interpolate(body, &variables).unwrap();

        match result {
            catapulte_domain::entity::body::InterpolatedBody::Mjml(s) => {
                assert!(s.contains("hello"), "expected 'hello' in rendered output");
            }
            catapulte_domain::entity::body::InterpolatedBody::Plain(_) => {
                panic!("expected Mjml variant")
            }
        }
    }

    #[test]
    fn interpolate_plain_no_variables_passthrough() {
        let interpolator = MiniJinjaInterpolator::new();
        let plain = Plain::try_new(Some("no variables here".to_string()), None).unwrap();
        let body = ResolvedBody::Plain(plain);
        let variables = Map::new();

        let result = interpolator.interpolate(body, &variables).unwrap();

        match result {
            catapulte_domain::entity::body::InterpolatedBody::Plain(p) => {
                assert_eq!(p.text(), Some("no variables here"));
            }
            catapulte_domain::entity::body::InterpolatedBody::Mjml(_) => {
                panic!("expected Plain variant")
            }
        }
    }

    #[test]
    fn interpolate_invalid_template_returns_error() {
        let interpolator = MiniJinjaInterpolator::new();
        let plain = Plain::try_new(Some("{{ unclosed".to_string()), None).unwrap();
        let body = ResolvedBody::Plain(plain);
        let variables = Map::new();

        let result = interpolator.interpolate(body, &variables);

        assert!(matches!(result, Err(InterpolateError::Engine { .. })));
    }
}
