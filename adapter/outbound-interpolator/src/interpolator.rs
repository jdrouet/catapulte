use anyhow::Context;
use catapulte_domain::entity::body::{InterpolatedBody, Plain, ResolvedBody};
use catapulte_domain::port::template_interpolator::{InterpolateError, TemplateInterpolator};
use catapulte_domain::port::template_validator::TemplateValidator;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use minijinja::value::{Rest, Value, ValueKind};
use minijinja::{AutoEscape, Environment, Output, State, UndefinedBehavior};

const HTML_FIVE: &str = "html-five";

/// Most bytes one render may write through the output formatter, and most a single container
/// argument may expand to when a filter stringifies it. Fuel cannot meter stringification (a
/// nested list that reuses itself expands exponentially inside one `Display` call), so this
/// bound stops the expansion early. ponytail: a constant, make it config if a template needs more.
const MAX_RENDER_BYTES: usize = 1024 * 1024;

/// A `fmt::Write` that fails once the shared total passes `MAX_RENDER_BYTES`, so an expanding
/// `Display` stops at the cap instead of building the whole string.
struct Bounded<'a, W> {
    inner: W,
    total: &'a AtomicUsize,
}

impl<W: std::fmt::Write> std::fmt::Write for Bounded<'_, W> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        if self.total.fetch_add(s.len(), Ordering::Relaxed) + s.len() > MAX_RENDER_BYTES {
            return Err(std::fmt::Error);
        }
        self.inner.write_str(s)
    }
}

struct OutputWriter<'a, 'b>(&'a mut Output<'b>);

impl std::fmt::Write for OutputWriter<'_, '_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.write_str(s)
    }
}

/// Escapes only `& < > " '`, so `/` stays literal and URLs remain valid after attribute decoding.
struct HtmlFive<W>(W);

impl<W: std::fmt::Write> std::fmt::Write for HtmlFive<W> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        for c in s.chars() {
            match c {
                '&' => self.0.write_str("&amp;")?,
                '<' => self.0.write_str("&lt;")?,
                '>' => self.0.write_str("&gt;")?,
                '"' => self.0.write_str("&quot;")?,
                '\'' => self.0.write_str("&#39;")?,
                _ => self.0.write_char(c)?,
            }
        }
        Ok(())
    }
}

fn out_of_budget() -> minijinja::Error {
    minijinja::Error::from(minijinja::ErrorKind::OutOfFuel)
}

fn format_value(
    total: &AtomicUsize,
    out: &mut Output,
    state: &mut State,
    value: &Value,
) -> Result<(), minijinja::Error> {
    let escape = !value.is_safe()
        && matches!(state.auto_escape(), AutoEscape::Custom(name) if name == HTML_FIVE);
    let mut bounded = Bounded {
        inner: OutputWriter(out),
        total,
    };
    let written = if escape {
        write!(HtmlFive(bounded), "{value}")
    } else {
        write!(bounded, "{value}")
    };
    written.map_err(|_| out_of_budget())
}

struct Discard;

impl std::fmt::Write for Discard {
    fn write_str(&mut self, _: &str) -> std::fmt::Result {
        Ok(())
    }
}

/// Fails when stringifying any container argument would exceed the budget, before the filter
/// stringifies it without a bound.
fn check_container_args(args: &[Value]) -> Result<(), minijinja::Error> {
    for arg in args {
        if matches!(
            arg.kind(),
            ValueKind::Seq | ValueKind::Map | ValueKind::Iterable | ValueKind::Plain
        ) {
            let total = AtomicUsize::new(0);
            let mut sink = Bounded {
                inner: Discard,
                total: &total,
            };
            write!(sink, "{arg}").map_err(|_| out_of_budget())?;
        }
    }
    Ok(())
}

/// Registers a builtin filter behind the container-size check.
fn add_guarded_filter<F, Rv, Args>(env: &mut Environment<'_>, names: &[&'static str], filter: F)
where
    F: minijinja::functions::Function<Rv, Args>,
    Rv: minijinja::value::FunctionResult,
    Args: for<'a> minijinja::value::FunctionArgs<'a>,
{
    let original = Value::from_function(filter);
    for name in names {
        let original = original.clone();
        env.add_filter(*name, move |state: &mut State, args: Rest<Value>| {
            check_container_args(&args)?;
            original.call(state, &args)
        });
    }
}

/// Render budget shared by sends, validation and previews, in minijinja fuel units (roughly one
/// per instruction). ponytail: a constant, make it config if a real template ever needs more.
const TEMPLATE_FUEL: u64 = 500_000;

fn build_env<'a>(escape: bool, undefined: UndefinedBehavior) -> Environment<'a> {
    let mut env = Environment::new();
    env.set_fuel(Some(TEMPLATE_FUEL));
    // Variables are HTML-escaped unless the part is plain text; `|safe` opts out.
    env.set_auto_escape_callback(move |_| {
        if escape {
            AutoEscape::Custom(HTML_FIVE.into())
        } else {
            AutoEscape::None
        }
    });
    let written = Arc::new(AtomicUsize::new(0));
    env.set_formatter(move |out, state, value| format_value(&written, out, state, value));
    env.set_undefined_behavior(undefined);
    harden_filters(&mut env);
    env
}

/// Filters that stringify a container argument in one call expand it before fuel can see it.
/// The string filters below check the argument first. The `~` operator cannot be guarded this
/// way and still expands a self-referencing container. `format`, `pprint` and `indent` stringify
/// values or produce output far larger than their input in ways that cannot be checked, and
/// no reasonable email template needs them, so they are removed.
fn harden_filters(env: &mut Environment<'_>) {
    for name in ["format", "pprint", "indent"] {
        env.remove_filter(name);
    }
    add_guarded_filter(env, &["string"], minijinja::filters::string);
    add_guarded_filter(env, &["join"], minijinja::filters::join);
    add_guarded_filter(env, &["escape", "e"], minijinja::filters::escape);
    add_guarded_filter(env, &["upper"], minijinja::filters::upper);
    add_guarded_filter(env, &["lower"], minijinja::filters::lower);
    add_guarded_filter(env, &["title"], minijinja::filters::title);
    add_guarded_filter(env, &["capitalize"], minijinja::filters::capitalize);
    add_guarded_filter(env, &["trim"], minijinja::filters::trim);
    add_guarded_filter(env, &["split"], minijinja::filters::split);
    add_guarded_filter(env, &["lines"], minijinja::filters::lines);
    add_guarded_filter(env, &["replace"], minijinja::filters::replace);
}

fn render_with<'a>(
    env: &mut Environment<'a>,
    source: &'a str,
    variables: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, InterpolateError> {
    env.add_template("t", source)
        .context("adding template")
        .map_err(|source| InterpolateError::Engine { source })?;
    let tmpl = env
        .get_template("t")
        .context("getting template")
        .map_err(|source| InterpolateError::Engine { source })?;
    tmpl.render(minijinja::value::Serde(variables))
        .map_err(|e| {
            if e.kind() == minijinja::ErrorKind::OutOfFuel {
                InterpolateError::BudgetExceeded
            } else {
                InterpolateError::Engine {
                    source: anyhow::Error::new(e).context("rendering template"),
                }
            }
        })
}

fn render_str(
    source: &str,
    variables: &serde_json::Map<String, serde_json::Value>,
    escape: bool,
) -> Result<String, InterpolateError> {
    render_with(
        &mut build_env(escape, UndefinedBehavior::Lenient),
        source,
        variables,
    )
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

impl TemplateValidator for MiniJinjaInterpolator {
    /// Same escaping as a send, but missing variables and attributes render as empty.
    fn validate(
        &self,
        source: &str,
        variables: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<String, InterpolateError> {
        render_with(
            &mut build_env(true, UndefinedBehavior::Chainable),
            source,
            variables,
        )
    }
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

    #[test]
    fn validation_accepts_missing_nested_attributes_but_sending_still_fails() {
        use catapulte_domain::port::template_validator::TemplateValidator;

        let source = "<mj-text>{{ customer.name }}</mj-text>";
        let none = Map::new();

        let validated = MiniJinjaInterpolator::new()
            .validate(source, &none)
            .unwrap();
        assert_eq!(validated, "<mj-text></mj-text>");

        let sent =
            MiniJinjaInterpolator::new().interpolate(ResolvedBody::Mjml(source.to_owned()), &none);
        assert!(sent.is_err(), "send-time behaviour is unchanged");
    }

    #[test]
    fn validation_needs_representative_data_for_value_requiring_expressions() {
        use catapulte_domain::port::template_validator::TemplateValidator;

        let source = "<mj-text>{{ total + 1 }}</mj-text>";
        let interpolator = MiniJinjaInterpolator::new();

        assert!(interpolator.validate(source, &Map::new()).is_err());
        let mut variables = Map::new();
        variables.insert("total".to_owned(), Value::from(1));
        assert_eq!(
            interpolator.validate(source, &variables).unwrap(),
            "<mj-text>2</mj-text>"
        );
    }

    #[test]
    fn validation_reports_syntax_errors_and_escapes_like_a_send() {
        use catapulte_domain::port::template_validator::TemplateValidator;

        let interpolator = MiniJinjaInterpolator::new();
        assert!(interpolator.validate("{% if %}", &Map::new()).is_err());

        let mut variables = Map::new();
        variables.insert("name".to_owned(), Value::String("<script>".to_owned()));
        let source = "<mj-text>{{ name }}</mj-text>";
        let validated = interpolator.validate(source, &variables).unwrap();
        let sent = interpolator
            .interpolate(ResolvedBody::Mjml(source.to_owned()), &variables)
            .unwrap();
        assert_eq!(validated, "<mj-text>&lt;script&gt;</mj-text>");
        assert!(matches!(sent, InterpolatedBody::Mjml(m) if m == validated));
    }

    const HOSTILE: &str = "<mj-text>{% for a in range(100000) %}{% for b in range(100000) %}x{% endfor %}{% endfor %}</mj-text>";

    #[test]
    fn a_runaway_loop_exhausts_the_budget_quickly_when_sending_and_validating() {
        use catapulte_domain::port::template_validator::TemplateValidator;

        let interpolator = MiniJinjaInterpolator::new();
        let started = std::time::Instant::now();

        let sent = interpolator.interpolate(ResolvedBody::Mjml(HOSTILE.to_owned()), &Map::new());
        let validated = interpolator.validate(HOSTILE, &Map::new());

        let elapsed = started.elapsed();
        eprintln!("two runaway renders took {elapsed:?}");
        assert!(matches!(sent, Err(InterpolateError::BudgetExceeded)));
        assert!(matches!(validated, Err(InterpolateError::BudgetExceeded)));
        assert!(elapsed < std::time::Duration::from_secs(1), "{elapsed:?}");
    }

    #[test]
    fn a_large_realistic_table_still_renders_within_the_budget() {
        let source = "<mj-table>{% for r in rows %}<tr><td>{{ r.sku }}</td><td>{{ r.name | upper }}</td><td>{{ r.qty }} x {{ r.price }}</td><td>{% if r.qty > 5 %}bulk{% else %}single{% endif %}</td></tr>{% endfor %}</mj-table>";
        let rows: Vec<Value> = (0..5000)
            .map(|i| serde_json::json!({ "sku": format!("S{i}"), "name": "widget", "qty": i % 9, "price": 3.5 }))
            .collect();
        let mut variables = Map::new();
        variables.insert("rows".to_owned(), Value::Array(rows));

        let result = MiniJinjaInterpolator::new()
            .interpolate(ResolvedBody::Mjml(source.to_owned()), &variables)
            .unwrap();

        assert!(matches!(result, InterpolatedBody::Mjml(m) if m.contains("<td>S4999</td>")));
    }

    fn doubling(output: &str) -> String {
        let mut source = String::from("{% set a = ['x'] %}");
        for _ in 0..22 {
            source.push_str("{% set a = [a, a] %}");
        }
        source.push_str(output);
        source
    }

    #[test]
    fn exponentially_expanding_values_are_cut_off_by_every_stringifier() {
        use catapulte_domain::port::template_validator::TemplateValidator;

        let interpolator = MiniJinjaInterpolator::new();
        for output in [
            "{{ a }}",
            "{{ a|string }}",
            "{{ a|join(',') }}",
            "{{ a|upper }}",
            "{{ a|replace('x', 'y') }}",
            "{{ a|e }}",
            "{{ a|format() }}",
        ] {
            let source = doubling(output);
            let started = std::time::Instant::now();

            let sent = interpolator.interpolate(ResolvedBody::Mjml(source.clone()), &Map::new());
            let validated = interpolator.validate(&source, &Map::new());

            let elapsed = started.elapsed();
            eprintln!("{output}: send+validate took {elapsed:?}");
            assert!(sent.is_err() && validated.is_err(), "{output}");
            assert!(
                elapsed < std::time::Duration::from_secs(1),
                "{output}: {elapsed:?}"
            );
        }
    }

    #[test]
    fn the_reproducer_reports_the_budget_and_plain_text_parts_are_bounded_too() {
        let source = doubling("{{ a }}");
        let sent = MiniJinjaInterpolator::new()
            .interpolate(ResolvedBody::Mjml(source.clone()), &Map::new());
        assert!(
            matches!(sent, Err(InterpolateError::BudgetExceeded)),
            "{sent:?}"
        );

        let plain = Plain::try_new(Some(source), None).unwrap();
        let text =
            MiniJinjaInterpolator::new().interpolate(ResolvedBody::Plain(plain), &Map::new());
        assert!(
            matches!(text, Err(InterpolateError::BudgetExceeded)),
            "{text:?}"
        );
    }

    #[test]
    fn a_big_realistic_table_stays_far_below_the_output_cap() {
        let source = "<mj-table>{% for r in rows %}<tr><td>{{ r.sku }}</td><td>{{ r.name | upper }}</td><td>{{ r.tags | join(', ') }}</td></tr>{% endfor %}</mj-table>";
        let rows: Vec<Value> = (0..5000)
            .map(|i| serde_json::json!({ "sku": format!("S{i}"), "name": "widget", "tags": ["a", "b"] }))
            .collect();
        let mut variables = Map::new();
        variables.insert("rows".to_owned(), Value::Array(rows));

        let InterpolatedBody::Mjml(out) = MiniJinjaInterpolator::new()
            .interpolate(ResolvedBody::Mjml(source.to_owned()), &variables)
            .unwrap()
        else {
            panic!("expected mjml");
        };

        assert!(out.contains("<td>a, b</td>"));
        assert!(
            out.len() < super::MAX_RENDER_BYTES / 2,
            "{} bytes",
            out.len()
        );
    }
}
