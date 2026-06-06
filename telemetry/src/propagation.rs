use std::collections::HashMap;

use tracing_opentelemetry::OpenTelemetrySpanExt as _;

/// Inject the current tracing span's W3C trace context into a carrier of header
/// pairs. Returns empty when there is no active `OTel` context (e.g. exporter
/// disabled) — a clean no-op.
#[must_use]
pub fn inject_current() -> Vec<(String, String)> {
    let cx = tracing::Span::current().context();
    let mut map = HashMap::new();
    opentelemetry::global::get_text_map_propagator(|p| p.inject_context(&cx, &mut map));
    map.into_iter().collect()
}

/// Extract a parent context from carrier pairs and set it as the parent of
/// `span`. No-op when the carrier is empty.
pub fn set_span_parent(span: &tracing::Span, carrier: &[(String, String)]) {
    if carrier.is_empty() {
        return;
    }
    let map: HashMap<String, String> = carrier.iter().cloned().collect();
    let parent = opentelemetry::global::get_text_map_propagator(|p| p.extract(&map));
    let _ = span.set_parent(parent);
}

#[cfg(test)]
mod tests {
    use super::{inject_current, set_span_parent};

    #[test]
    fn inject_current_with_no_active_otel_span_returns_empty() {
        // Without an installed propagator or an active OTel-backed span,
        // inject_current returns an empty vec rather than panicking.
        let pairs = inject_current();
        assert!(pairs.is_empty());
    }

    #[test]
    fn set_span_parent_with_empty_carrier_is_noop() {
        let span = tracing::info_span!("test.noop");
        // Must not panic.
        set_span_parent(&span, &[]);
    }
}
