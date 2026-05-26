mod scenarios {
    pub mod backends;
    pub mod context;
    pub mod list;
    pub mod runner;
}

crate::e2e_matrix! {
    scenarios: [plain_email, idempotency, lifecycle_events],
    backends: [sqlite_storage, sqlite_memory, sqlite_nats, postgres_storage, postgres_memory, postgres_nats],
}
