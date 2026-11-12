mod support;

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
        inline_cid_image,
        batch_submit_delivers_multiple_emails,
        list_emails_returns_submitted,
        display_name_sender,
        delivery_receipt,
        reply_to_and_headers,
        list_unsubscribe,
        scheduled_send_waits_for_send_at,
        cancel_prevents_scheduled_delivery,
    ],
    backends: [sqlite_storage, sqlite_memory, sqlite_nats, postgres_storage, postgres_memory, postgres_nats],
}
