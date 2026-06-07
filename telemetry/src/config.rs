use std::collections::HashMap;

/// Transport protocol for the OTLP exporter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OtlpProtocol {
    Grpc,
    HttpProtobuf,
}

/// Runtime configuration for the telemetry subsystem.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    pub traces_enabled: bool,
    pub metrics_enabled: bool,
    pub metric_export_interval: std::time::Duration,
    pub protocol: OtlpProtocol,
    pub endpoint: Option<String>,
    pub headers: Vec<(String, String)>,
    pub service_name: String,
    pub service_version: String,
    /// `service.instance.id` resource attribute — distinguishes replicas in
    /// metrics/traces. `None` here means "let `init` generate one" (see
    /// `from_map` for the resolution order).
    pub service_instance_id: Option<String>,
}

impl TelemetryConfig {
    /// Reads telemetry configuration from environment variables.
    ///
    /// `{prefix}`-prefixed variables take precedence over the standard
    /// `OTEL_*` equivalents when both are set.
    ///
    /// # Errors
    ///
    /// Returns an error when a required variable is missing (endpoint when
    /// traces are enabled) or a variable has an unrecognised value.
    pub fn from_env(prefix: &str, service_version: &str) -> anyhow::Result<Self> {
        let env: HashMap<String, String> = std::env::vars().collect();
        Self::from_map(&env, prefix, service_version)
    }

    fn from_map(
        env: &HashMap<String, String>,
        prefix: &str,
        service_version: &str,
    ) -> anyhow::Result<Self> {
        use anyhow::Context as _;

        let lookup = |primary: &str, fallback: &str| -> Option<String> {
            env.get(primary).or_else(|| env.get(fallback)).cloned()
        };

        let traces_exporter = lookup(&format!("{prefix}_TRACES_EXPORTER"), "OTEL_TRACES_EXPORTER");
        let traces_enabled = match traces_exporter.as_deref() {
            None | Some("none") => false,
            Some("otlp") => true,
            Some(other) => {
                anyhow::bail!("unsupported traces exporter {other:?}; accepted values: otlp, none");
            }
        };

        let protocol_raw = lookup(
            &format!("{prefix}_EXPORTER_OTLP_PROTOCOL"),
            "OTEL_EXPORTER_OTLP_PROTOCOL",
        )
        .unwrap_or_else(|| String::from("grpc"));
        let protocol = match protocol_raw.as_str() {
            "grpc" => OtlpProtocol::Grpc,
            "http/protobuf" => OtlpProtocol::HttpProtobuf,
            other => {
                anyhow::bail!(
                    "unsupported OTLP protocol {other:?}; accepted values: grpc, http/protobuf"
                );
            }
        };

        let metrics_exporter = lookup(
            &format!("{prefix}_METRICS_EXPORTER"),
            "OTEL_METRICS_EXPORTER",
        );
        let metrics_enabled = match metrics_exporter.as_deref() {
            None | Some("none") => false,
            Some("otlp") => true,
            Some(other) => {
                anyhow::bail!(
                    "unsupported metrics exporter {other:?}; accepted values: otlp, none"
                );
            }
        };

        let endpoint_key = format!("{prefix}_EXPORTER_OTLP_ENDPOINT");
        let endpoint = lookup(&endpoint_key, "OTEL_EXPORTER_OTLP_ENDPOINT");
        if (traces_enabled || metrics_enabled) && endpoint.is_none() {
            anyhow::bail!(
                "OTLP export requires an endpoint; set {endpoint_key} or OTEL_EXPORTER_OTLP_ENDPOINT"
            );
        }

        let interval_key = format!("{prefix}_METRIC_EXPORT_INTERVAL_SECS");
        let metric_export_interval = match env.get(&interval_key) {
            None => std::time::Duration::from_mins(1),
            Some(v) => {
                let secs = v
                    .parse::<u64>()
                    .with_context(|| format!("invalid {interval_key}: {v:?}"))?;
                if secs == 0 {
                    anyhow::bail!("{interval_key} must be greater than 0");
                }
                std::time::Duration::from_secs(secs)
            }
        };

        let headers_raw = lookup(
            &format!("{prefix}_EXPORTER_OTLP_HEADERS"),
            "OTEL_EXPORTER_OTLP_HEADERS",
        )
        .unwrap_or_default();
        let headers = parse_headers(&headers_raw).context("parsing OTLP headers")?;

        let service_name = lookup(&format!("{prefix}_SERVICE_NAME"), "OTEL_SERVICE_NAME")
            .unwrap_or_else(|| String::from("catapulte"));

        // Resolution order for the per-replica id: an explicit var, else the
        // container/pod HOSTNAME, else `init` generates a random UUID.
        let service_instance_id = lookup(
            &format!("{prefix}_SERVICE_INSTANCE_ID"),
            "OTEL_SERVICE_INSTANCE_ID",
        )
        .or_else(|| env.get("HOSTNAME").cloned())
        .filter(|s| !s.is_empty());

        Ok(Self {
            traces_enabled,
            metrics_enabled,
            metric_export_interval,
            protocol,
            endpoint,
            headers,
            service_name,
            service_version: service_version.to_owned(),
            service_instance_id,
        })
    }
}

fn parse_headers(raw: &str) -> anyhow::Result<Vec<(String, String)>> {
    let mut headers = Vec::new();
    for pair in raw.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("header pair {pair:?} is not in k=v format"))?;
        headers.push((k.trim().to_owned(), v.trim().to_owned()));
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "CATAPULTE_TEST_OTEL";

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn cfg(pairs: &[(&str, &str)]) -> anyhow::Result<TelemetryConfig> {
        TelemetryConfig::from_map(&env(pairs), P, "1.0.0")
    }

    #[test]
    fn disabled_by_default() {
        let c = cfg(&[]).unwrap();
        assert!(!c.traces_enabled);
    }

    #[test]
    fn none_disables() {
        let c = cfg(&[(&format!("{P}_TRACES_EXPORTER"), "none")]).unwrap();
        assert!(!c.traces_enabled);
    }

    #[test]
    fn otlp_enables_with_endpoint() {
        let c = cfg(&[
            (&format!("{P}_TRACES_EXPORTER"), "otlp"),
            (
                &format!("{P}_EXPORTER_OTLP_ENDPOINT"),
                "http://localhost:4317",
            ),
        ])
        .unwrap();
        assert!(c.traces_enabled);
        assert_eq!(c.endpoint.as_deref(), Some("http://localhost:4317"));
    }

    #[test]
    fn otlp_without_endpoint_errors() {
        let err = cfg(&[(&format!("{P}_TRACES_EXPORTER"), "otlp")]).unwrap_err();
        assert!(err.to_string().contains("endpoint"));
    }

    #[test]
    fn unknown_exporter_errors() {
        let err = cfg(&[(&format!("{P}_TRACES_EXPORTER"), "prometheus")]).unwrap_err();
        assert!(err.to_string().contains("prometheus"));
    }

    #[test]
    fn grpc_protocol() {
        let c = cfg(&[
            (&format!("{P}_TRACES_EXPORTER"), "otlp"),
            (
                &format!("{P}_EXPORTER_OTLP_ENDPOINT"),
                "http://localhost:4317",
            ),
            (&format!("{P}_EXPORTER_OTLP_PROTOCOL"), "grpc"),
        ])
        .unwrap();
        assert_eq!(c.protocol, OtlpProtocol::Grpc);
    }

    #[test]
    fn http_protobuf_protocol() {
        let c = cfg(&[
            (&format!("{P}_TRACES_EXPORTER"), "otlp"),
            (
                &format!("{P}_EXPORTER_OTLP_ENDPOINT"),
                "http://localhost:4318",
            ),
            (&format!("{P}_EXPORTER_OTLP_PROTOCOL"), "http/protobuf"),
        ])
        .unwrap();
        assert_eq!(c.protocol, OtlpProtocol::HttpProtobuf);
    }

    #[test]
    fn unknown_protocol_errors() {
        let err = cfg(&[
            (&format!("{P}_TRACES_EXPORTER"), "otlp"),
            (
                &format!("{P}_EXPORTER_OTLP_ENDPOINT"),
                "http://localhost:4317",
            ),
            (&format!("{P}_EXPORTER_OTLP_PROTOCOL"), "http/json"),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("http/json"));
    }

    #[test]
    fn otel_fallback_for_exporter() {
        let c = TelemetryConfig::from_map(
            &env(&[
                ("OTEL_TRACES_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4317"),
            ]),
            P,
            "1.0.0",
        )
        .unwrap();
        assert!(c.traces_enabled);
        assert_eq!(c.endpoint.as_deref(), Some("http://collector:4317"));
    }

    #[test]
    fn catapulte_prefix_overrides_otel() {
        let c = TelemetryConfig::from_map(
            &env(&[
                ("OTEL_TRACES_EXPORTER", "none"),
                (&format!("{P}_TRACES_EXPORTER"), "otlp"),
                (
                    &format!("{P}_EXPORTER_OTLP_ENDPOINT"),
                    "http://localhost:4317",
                ),
            ]),
            P,
            "1.0.0",
        )
        .unwrap();
        assert!(c.traces_enabled);
    }

    #[test]
    fn header_parsing() {
        let c = cfg(&[
            (&format!("{P}_TRACES_EXPORTER"), "otlp"),
            (
                &format!("{P}_EXPORTER_OTLP_ENDPOINT"),
                "http://localhost:4317",
            ),
            (
                &format!("{P}_EXPORTER_OTLP_HEADERS"),
                "Authorization=Bearer tok, X-Custom=val",
            ),
        ])
        .unwrap();
        assert_eq!(c.headers.len(), 2);
        assert_eq!(
            c.headers[0],
            ("Authorization".to_owned(), "Bearer tok".to_owned())
        );
        assert_eq!(c.headers[1], ("X-Custom".to_owned(), "val".to_owned()));
    }

    #[test]
    fn empty_headers_ok() {
        let c = cfg(&[
            (&format!("{P}_TRACES_EXPORTER"), "otlp"),
            (
                &format!("{P}_EXPORTER_OTLP_ENDPOINT"),
                "http://localhost:4317",
            ),
            (&format!("{P}_EXPORTER_OTLP_HEADERS"), ""),
        ])
        .unwrap();
        assert!(c.headers.is_empty());
    }

    #[test]
    fn service_name_default() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.service_name, "catapulte");
    }

    #[test]
    fn service_name_from_prefix() {
        let c = cfg(&[(&format!("{P}_SERVICE_NAME"), "my-app")]).unwrap();
        assert_eq!(c.service_name, "my-app");
    }

    #[test]
    fn service_version_from_arg() {
        let c = TelemetryConfig::from_map(&env(&[]), P, "2.3.4").unwrap();
        assert_eq!(c.service_version, "2.3.4");
    }

    #[test]
    fn service_instance_id_from_prefix() {
        let c = cfg(&[(&format!("{P}_SERVICE_INSTANCE_ID"), "inst-1")]).unwrap();
        assert_eq!(c.service_instance_id.as_deref(), Some("inst-1"));
    }

    #[test]
    fn service_instance_id_falls_back_to_hostname() {
        let c = cfg(&[("HOSTNAME", "pod-abc")]).unwrap();
        assert_eq!(c.service_instance_id.as_deref(), Some("pod-abc"));
    }

    #[test]
    fn service_instance_id_prefers_explicit_over_hostname() {
        let c = cfg(&[
            (&format!("{P}_SERVICE_INSTANCE_ID"), "inst-1"),
            ("HOSTNAME", "pod-abc"),
        ])
        .unwrap();
        assert_eq!(c.service_instance_id.as_deref(), Some("inst-1"));
    }

    #[test]
    fn service_instance_id_none_when_unset() {
        // None here means `init` will generate a random UUID.
        let c = cfg(&[]).unwrap();
        assert_eq!(c.service_instance_id, None);
    }

    #[test]
    fn otel_fallback_for_protocol() {
        let c = cfg(&[
            (&format!("{P}_TRACES_EXPORTER"), "otlp"),
            (
                &format!("{P}_EXPORTER_OTLP_ENDPOINT"),
                "http://localhost:4318",
            ),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
        ])
        .unwrap();
        assert_eq!(c.protocol, OtlpProtocol::HttpProtobuf);
    }

    #[test]
    fn otel_fallback_for_headers() {
        let c = cfg(&[
            (&format!("{P}_TRACES_EXPORTER"), "otlp"),
            (
                &format!("{P}_EXPORTER_OTLP_ENDPOINT"),
                "http://localhost:4317",
            ),
            ("OTEL_EXPORTER_OTLP_HEADERS", "Authorization=Bearer tok"),
        ])
        .unwrap();
        assert_eq!(
            c.headers,
            vec![("Authorization".to_owned(), "Bearer tok".to_owned())]
        );
    }

    #[test]
    fn otel_fallback_for_service_name() {
        let c = cfg(&[("OTEL_SERVICE_NAME", "from-otel")]).unwrap();
        assert_eq!(c.service_name, "from-otel");
    }

    #[test]
    fn catapulte_prefix_overrides_otel_service_name() {
        let c = cfg(&[
            ("OTEL_SERVICE_NAME", "from-otel"),
            (&format!("{P}_SERVICE_NAME"), "from-catapulte"),
        ])
        .unwrap();
        assert_eq!(c.service_name, "from-catapulte");
    }

    #[test]
    fn metrics_disabled_by_default() {
        let c = cfg(&[]).unwrap();
        assert!(!c.metrics_enabled);
    }

    #[test]
    fn metrics_none_disables() {
        let c = cfg(&[(&format!("{P}_METRICS_EXPORTER"), "none")]).unwrap();
        assert!(!c.metrics_enabled);
    }

    #[test]
    fn metrics_otlp_enables() {
        let c = cfg(&[
            (&format!("{P}_METRICS_EXPORTER"), "otlp"),
            (
                &format!("{P}_EXPORTER_OTLP_ENDPOINT"),
                "http://localhost:4317",
            ),
        ])
        .unwrap();
        assert!(c.metrics_enabled);
    }

    #[test]
    fn metrics_otel_fallback_enables() {
        let c = TelemetryConfig::from_map(
            &env(&[
                ("OTEL_METRICS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4317"),
            ]),
            P,
            "1.0.0",
        )
        .unwrap();
        assert!(c.metrics_enabled);
    }

    #[test]
    fn metrics_enabled_without_endpoint_errors() {
        let err = cfg(&[(&format!("{P}_METRICS_EXPORTER"), "otlp")]).unwrap_err();
        assert!(err.to_string().contains("endpoint"));
    }

    #[test]
    fn zero_metric_export_interval_errors() {
        let err = cfg(&[
            (&format!("{P}_METRICS_EXPORTER"), "otlp"),
            (
                &format!("{P}_EXPORTER_OTLP_ENDPOINT"),
                "http://localhost:4317",
            ),
            (&format!("{P}_METRIC_EXPORT_INTERVAL_SECS"), "0"),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("greater than 0"));
    }

    #[test]
    fn metrics_unknown_exporter_errors() {
        let err = cfg(&[(&format!("{P}_METRICS_EXPORTER"), "prometheus")]).unwrap_err();
        assert!(err.to_string().contains("prometheus"));
    }

    #[test]
    fn metric_export_interval_default_is_60s() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.metric_export_interval, std::time::Duration::from_mins(1));
    }

    #[test]
    fn metric_export_interval_parses_secs() {
        let c = cfg(&[(&format!("{P}_METRIC_EXPORT_INTERVAL_SECS"), "30")]).unwrap();
        assert_eq!(c.metric_export_interval, std::time::Duration::from_secs(30));
    }

    #[test]
    fn metric_export_interval_invalid_errors() {
        let err =
            cfg(&[(&format!("{P}_METRIC_EXPORT_INTERVAL_SECS"), "not-a-number")]).unwrap_err();
        assert!(err.to_string().contains("METRIC_EXPORT_INTERVAL"));
    }
}
