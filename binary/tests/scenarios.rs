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
    ],
    backends: [sqlite_storage, sqlite_memory, sqlite_nats, postgres_storage, postgres_memory, postgres_nats],
}

// Scheduling and cancellation need a queue that can hold a row back: the storage queue.
crate::e2e_matrix! {
    scenarios: [
        scheduled_send_waits_for_send_at,
        cancel_prevents_scheduled_delivery,
    ],
    backends: [sqlite_storage, postgres_storage],
}

crate::e2e_matrix! {
    scenarios: [
        scheduled_send_rejected_without_storage_queue,
    ],
    backends: [sqlite_memory, sqlite_nats, postgres_memory, postgres_nats],
}
