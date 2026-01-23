pub fn init_logs(
    directive: &str,
    color: bool,
) -> Result<(), tracing_subscriber::util::TryInitError> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(directive))
        .with(tracing_subscriber::fmt::layer().with_ansi(color))
        .try_init()
}
