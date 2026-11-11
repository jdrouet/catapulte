use anyhow::Context;
use catapulte_domain::entity::body::{InterpolatedBody, Plain, ResolvedBody};
use catapulte_domain::port::template_interpolator::{InterpolateError, TemplateInterpolator};
use minijinja::value::Value;
use minijinja::{AutoEscape, Environment, Output, State};

const HTML_FIVE: &str = "html-five";

/// Escapes only `& < > " '`, so `/` stays literal and URLs remain valid after attribute decoding.
fn format_value(
    out: &mut Output,
    state: &mut State,
    value: &Value,
) -> Result<(), minijinja::Error> {
    if value.is_safe()
        || !matches!(state.auto_escape(), AutoEscape::Custom(name) if name == HTML_FIVE)
    {
        return minijinja::escape_formatter(out, state, value);
    }
    for c in value.to_string().chars() {
        match c {
            '&' => out.write_str("&amp;")?,
            '<' => out.write_str("&lt;")?,
            '>' => out.write_str("&gt;")?,
            '"' => out.write_str("&quot;")?,
            '\'' => out.write_str("&#39;")?,
            _ => out.write_str(c.encode_utf8(&mut [0; 4]))?,
        }
    }
    Ok(())
}

fn render_str(
    source: &str,
    variables: &serde_json::Map<String, serde_json::Value>,
    escape: bool,
) -> Result<String, InterpolateError> {
    let mut env = Environment::new();
    // Variables are HTML-escaped unless the part is plain text; `|safe` opts out.
    env.set_auto_escape_callback(move |_| {
        if escape {
            AutoEscape::Custom(HTML_FIVE.into())
        } else {
            AutoEscape::None
        }
    });
    env.set_formatter(format_value);
    env.add_template("t", source)
        .context("adding template")
        .map_err(|source| InterpolateError::Engine { source })?;
    let tmpl = env
        .get_template("t")
        .context("getting template")
        .map_err(|source| InterpolateError::Engine { source })?;
    tmpl.render(minijinja::value::Serde(variables))
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
    let text = text.map(|t| render_str(&t, variables, false)).transpose()?;
    let html = html.map(|h| render_str(&h, variables, true)).transpose()?;
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
                render_str(&source, variables, true).map(InterpolatedBody::Mjml)
            }
            ResolvedBody::Plain(plain) => interpolate_plain(plain, variables),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    use catapulte_domain::entity::body::{InterpolatedBody, Plain, ResolvedBody};
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

    fn vars(key: &str, value: &str) -> Map<String, Value> {
        let mut variables = Map::new();
        variables.insert(key.to_string(), Value::String(value.to_string()));
        variables
    }

    #[test]
    fn mjml_escapes_html_in_variables() {
        let body = ResolvedBody::Mjml("<mj-text>{{ v }}</mj-text>".to_string());

        let result = MiniJinjaInterpolator::new()
            .interpolate(body, &vars("v", "<b>"))
            .unwrap();

        match result {
            InterpolatedBody::Mjml(s) => assert_eq!(s, "<mj-text>&lt;b&gt;</mj-text>"),
            InterpolatedBody::Plain(_) => panic!("expected Mjml variant"),
        }
    }

    #[test]
    fn mjml_safe_filter_keeps_raw_html() {
        let body = ResolvedBody::Mjml("<mj-text>{{ v|safe }}</mj-text>".to_string());

        let result = MiniJinjaInterpolator::new()
            .interpolate(body, &vars("v", "<b>"))
            .unwrap();

        match result {
            InterpolatedBody::Mjml(s) => assert_eq!(s, "<mj-text><b></mj-text>"),
            InterpolatedBody::Plain(_) => panic!("expected Mjml variant"),
        }
    }

    #[test]
    fn plain_html_escapes_and_text_does_not() {
        let plain = Plain::try_new(
            Some("{{ v }}".to_string()),
            Some("<p>{{ v }}</p><i>{{ v|safe }}</i>".to_string()),
        )
        .unwrap();

        let result = MiniJinjaInterpolator::new()
            .interpolate(ResolvedBody::Plain(plain), &vars("v", "Tom & <Jerry>"))
            .unwrap();

        match result {
            InterpolatedBody::Plain(p) => {
                assert_eq!(p.text(), Some("Tom & <Jerry>"));
                assert_eq!(
                    p.html(),
                    Some("<p>Tom &amp; &lt;Jerry&gt;</p><i>Tom & <Jerry></i>")
                );
            }
            InterpolatedBody::Mjml(_) => panic!("expected Plain variant"),
        }
    }

    #[test]
    fn escapes_exactly_five_characters_and_keeps_slash() {
        let body = ResolvedBody::Mjml("{{ v }}|{{ v|e }}|{{ v|escape }}".to_string());

        let result = MiniJinjaInterpolator::new()
            .interpolate(body, &vars("v", r#"&<>"'/"#))
            .unwrap();

        let one = "&amp;&lt;&gt;&quot;&#39;/";
        match result {
            InterpolatedBody::Mjml(s) => assert_eq!(s, format!("{one}|{one}|{one}")),
            InterpolatedBody::Plain(_) => panic!("expected Mjml variant"),
        }
    }

    #[test]
    fn safe_numbers_and_escape_filters() {
        let mut variables = vars("v", "<b>");
        variables.insert("n".to_string(), Value::from(42));
        let body = ResolvedBody::Mjml(
            "{{ v|safe }}|{{ n }}|{{ v|safe|e }}|{{ v|safe|escape }}".to_string(),
        );

        let result = MiniJinjaInterpolator::new()
            .interpolate(body, &variables)
            .unwrap();

        // `|e` on an already-safe value is a no-op in minijinja, so the markup stays raw.
        match result {
            InterpolatedBody::Mjml(s) => assert_eq!(s, "<b>|42|<b>|<b>"),
            InterpolatedBody::Plain(_) => panic!("expected Mjml variant"),
        }
    }

    #[test]
    fn autoescape_block_switches_escaping_off_and_back_on() {
        let body = ResolvedBody::Mjml(
            "{{ v }}|{% autoescape false %}{{ v }}{% endautoescape %}|{{ v }}".to_string(),
        );

        let result = MiniJinjaInterpolator::new()
            .interpolate(body, &vars("v", "<&>"))
            .unwrap();

        match result {
            InterpolatedBody::Mjml(s) => assert_eq!(s, "&lt;&amp;&gt;|<&>|&lt;&amp;&gt;"),
            InterpolatedBody::Plain(_) => panic!("expected Mjml variant"),
        }
    }
}
