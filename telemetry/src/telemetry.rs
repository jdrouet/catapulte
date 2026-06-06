use anyhow::Context as _;
use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::Layer;

use crate::config::{OtlpProtocol, TelemetryConfig};

/// Handle to the initialised telemetry subsystem.
///
/// Call [`Telemetry::tracing_layer`] to obtain the layer for the
/// `tracing_subscriber` registry, and [`Telemetry::shutdown`] to flush and
/// tear down the provider on process exit.
pub struct Telemetry {
    provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
    metric_export_interval: std::time::Duration,
}

impl Telemetry {
    /// Returns an `OpenTelemetryLayer` bound to the active tracer provider,
    /// or `None` when traces are disabled.
    ///
    /// The returned `Option<impl Layer<S>>` composes as a no-op when `None`.
    #[must_use]
    pub fn tracing_layer<S>(&self) -> Option<impl Layer<S>>
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        self.provider.as_ref().map(|p| {
            let tracer = opentelemetry::trace::TracerProvider::tracer(p, "catapulte");
            OpenTelemetryLayer::new(tracer)
        })
    }

    /// Returns `true` when the metrics pipeline is active.
    #[must_use]
    pub fn metrics_enabled(&self) -> bool {
        self.meter_provider.is_some()
    }

    /// Returns the configured metric export interval.
    #[must_use]
    pub fn metric_export_interval(&self) -> std::time::Duration {
        self.metric_export_interval
    }

    /// Flushes pending spans and shuts down the tracer provider.
    pub fn shutdown(self) {
        if let Some(provider) = self.provider
            && let Err(e) = provider.shutdown()
        {
            tracing::warn!(error = %e, "tracer provider shutdown error");
        }
        if let Some(provider) = self.meter_provider
            && let Err(e) = provider.shutdown()
        {
            tracing::warn!(error = %e, "meter provider shutdown error");
        }
    }
}

/// Initialises the telemetry subsystem according to the supplied configuration.
///
/// When traces are enabled this builds the OTLP exporter, wires it into an
/// `SdkTracerProvider`, and installs it as the global tracer provider.
///
/// When metrics are enabled this builds an OTLP `MetricExporter`, wraps it in
/// a `PeriodicReader` with the configured interval, builds an
/// `SdkMeterProvider`, and installs it as the global meter provider.
///
/// Both providers share the same `Resource` (service name + version).
///
/// # Errors
///
/// Returns an error when the OTLP exporter cannot be constructed (e.g. an
/// invalid endpoint URL).
///
/// # Panics
///
/// Panics if `config.traces_enabled` is `true` but `config.endpoint` is
/// `None`. This is an invariant enforced by [`TelemetryConfig::from_env`].
pub fn init(config: &TelemetryConfig) -> anyhow::Result<Telemetry> {
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );

    let resource = Resource::builder()
        .with_attribute(KeyValue::new(
            opentelemetry_semantic_conventions::resource::SERVICE_NAME,
            config.service_name.clone(),
        ))
        .with_attribute(KeyValue::new(
            opentelemetry_semantic_conventions::resource::SERVICE_VERSION,
            config.service_version.clone(),
        ))
        .build();

    let provider = if config.traces_enabled {
        let endpoint = config.endpoint.as_deref().expect(
            "endpoint is required when traces are enabled; validated by TelemetryConfig::from_env",
        );

        let exporter = build_span_exporter(&config.protocol, endpoint, &config.headers)
            .context("building OTLP span exporter")?;

        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource.clone())
            .with_sampler(opentelemetry_sdk::trace::Sampler::ParentBased(Box::new(
                opentelemetry_sdk::trace::Sampler::AlwaysOn,
            )))
            .build();

        opentelemetry::global::set_tracer_provider(provider.clone());
        Some(provider)
    } else {
        None
    };

    let meter_provider = if config.metrics_enabled {
        let endpoint = config.endpoint.as_deref().unwrap_or_default();
        let metric_exporter = build_metric_exporter(&config.protocol, endpoint, &config.headers)
            .context("building OTLP metric exporter")?;

        let reader = PeriodicReader::builder(metric_exporter)
            .with_interval(config.metric_export_interval)
            .build();

        let mp = SdkMeterProvider::builder()
            .with_reader(reader)
            .with_resource(resource)
            .build();

        opentelemetry::global::set_meter_provider(mp.clone());
        Some(mp)
    } else {
        None
    };

    Ok(Telemetry {
        provider,
        meter_provider,
        metric_export_interval: config.metric_export_interval,
    })
}

pub(crate) fn build_span_exporter(
    protocol: &OtlpProtocol,
    endpoint: &str,
    headers: &[(String, String)],
) -> anyhow::Result<opentelemetry_otlp::SpanExporter> {
    use opentelemetry_otlp::WithExportConfig as _;

    match protocol {
        OtlpProtocol::Grpc => {
            let mut builder = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint);
            if !headers.is_empty() {
                use opentelemetry_otlp::WithTonicConfig as _;
                let mut map = tonic::metadata::MetadataMap::new();
                for (k, v) in headers {
                    let key: tonic::metadata::MetadataKey<tonic::metadata::Ascii> = k
                        .parse()
                        .with_context(|| format!("invalid gRPC metadata key {k:?}"))?;
                    let val: tonic::metadata::MetadataValue<tonic::metadata::Ascii> = v
                        .parse()
                        .with_context(|| format!("invalid gRPC metadata value for {k:?}"))?;
                    map.insert(key, val);
                }
                builder = builder.with_metadata(map);
            }
            builder.build().context("building gRPC OTLP span exporter")
        }
        OtlpProtocol::HttpProtobuf => {
            let mut builder = opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_endpoint(endpoint);
            if !headers.is_empty() {
                use opentelemetry_otlp::WithHttpConfig as _;
                let header_map: std::collections::HashMap<String, String> =
                    headers.iter().cloned().collect();
                builder = builder.with_headers(header_map);
            }
            builder.build().context("building HTTP OTLP span exporter")
        }
    }
}

pub(crate) fn build_metric_exporter(
    protocol: &OtlpProtocol,
    endpoint: &str,
    headers: &[(String, String)],
) -> anyhow::Result<opentelemetry_otlp::MetricExporter> {
    use opentelemetry_otlp::WithExportConfig as _;

    match protocol {
        OtlpProtocol::Grpc => {
            let mut builder = opentelemetry_otlp::MetricExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint);
            if !headers.is_empty() {
                use opentelemetry_otlp::WithTonicConfig as _;
                let mut map = tonic::metadata::MetadataMap::new();
                for (k, v) in headers {
                    let key: tonic::metadata::MetadataKey<tonic::metadata::Ascii> = k
                        .parse()
                        .with_context(|| format!("invalid gRPC metadata key {k:?}"))?;
                    let val: tonic::metadata::MetadataValue<tonic::metadata::Ascii> = v
                        .parse()
                        .with_context(|| format!("invalid gRPC metadata value for {k:?}"))?;
                    map.insert(key, val);
                }
                builder = builder.with_metadata(map);
            }
            builder
                .build()
                .context("building gRPC OTLP metric exporter")
        }
        OtlpProtocol::HttpProtobuf => {
            let mut builder = opentelemetry_otlp::MetricExporter::builder()
                .with_http()
                .with_endpoint(endpoint);
            if !headers.is_empty() {
                use opentelemetry_otlp::WithHttpConfig as _;
                let header_map: std::collections::HashMap<String, String> =
                    headers.iter().cloned().collect();
                builder = builder.with_headers(header_map);
            }
            builder
                .build()
                .context("building HTTP OTLP metric exporter")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_metric_exporter, build_span_exporter};
    use crate::config::OtlpProtocol;

    #[tokio::test]
    async fn build_exporter_grpc_with_header_returns_ok() {
        let result = build_span_exporter(
            &OtlpProtocol::Grpc,
            "http://localhost:4317",
            &[("Authorization".to_owned(), "Bearer tok".to_owned())],
        );
        assert!(result.is_ok(), "grpc exporter build failed: {result:?}");
    }

    #[test]
    fn build_exporter_http_protobuf_with_header_returns_ok() {
        let result = build_span_exporter(
            &OtlpProtocol::HttpProtobuf,
            "http://localhost:4318",
            &[("X-Custom".to_owned(), "val".to_owned())],
        );
        assert!(
            result.is_ok(),
            "http/protobuf exporter build failed: {result:?}"
        );
    }

    #[tokio::test]
    async fn build_exporter_grpc_no_headers_returns_ok() {
        let result = build_span_exporter(&OtlpProtocol::Grpc, "http://localhost:4317", &[]);
        assert!(
            result.is_ok(),
            "grpc exporter (no headers) build failed: {result:?}"
        );
    }

    #[test]
    fn build_exporter_http_protobuf_no_headers_returns_ok() {
        let result = build_span_exporter(&OtlpProtocol::HttpProtobuf, "http://localhost:4318", &[]);
        assert!(
            result.is_ok(),
            "http/protobuf exporter (no headers) build failed: {result:?}"
        );
    }

    #[tokio::test]
    async fn build_metric_exporter_grpc_returns_ok() {
        let result = build_metric_exporter(&OtlpProtocol::Grpc, "http://localhost:4317", &[]);
        assert!(
            result.is_ok(),
            "grpc metric exporter build failed: {result:?}"
        );
    }

    #[test]
    fn build_metric_exporter_http_protobuf_returns_ok() {
        let result =
            build_metric_exporter(&OtlpProtocol::HttpProtobuf, "http://localhost:4318", &[]);
        assert!(
            result.is_ok(),
            "http/protobuf metric exporter build failed: {result:?}"
        );
    }
}
