use std::time::Duration;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let resp = ctx
        .submit(ctx.simple_payload("recipient@example.com", "Hello!"))
        .await;
    assert!(
        resp.status().is_success(),
        "POST /emails failed: {}",
        resp.status()
    );
    let messages = ctx
        .wait_for_mailpit_messages(1, Duration::from_secs(15))
        .await;
    assert_eq!(messages.len(), 1, "expected exactly one delivered email");
}
