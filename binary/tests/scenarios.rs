mod scenarios {
    pub mod backends;
    pub mod context;
    pub mod list;
    pub mod runner;
}

crate::e2e_matrix! {
    scenarios: [
        plain_email,
        idempotency,
        lifecycle_events,
        mjml_inline_renders_with_variables,
        inline_attachment_is_delivered,
        batch_submit_delivers_multiple_emails,
        list_emails_returns_submitted,
    ],
    backends: [sqlite_storage, sqlite_memory, sqlite_nats, postgres_storage, postgres_memory, postgres_nats],
}
