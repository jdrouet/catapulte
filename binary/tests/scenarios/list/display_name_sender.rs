use std::time::Duration;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let payload = serde_json::json!({
        "sender": "Café Ünïcode <noreply@example.com>",
        "recipients": [{ "kind": "to", "name": "Alice", "address": "recipient@example.com" }],
        "body": { "kind": "plain", "text": "hello", "html": null }
    });

    let resp = ctx.submit(payload).await;
    assert!(
        resp.status().is_success(),
        "POST /emails failed: {}",
        resp.status()
    );

    let messages = ctx
        .wait_for_mailpit_messages(1, Duration::from_secs(15))
        .await;
    assert_eq!(messages.len(), 1, "expected exactly one delivered email");

    let msg_id = messages[0]["ID"]
        .as_str()
        .expect("message ID missing")
        .to_owned();
    let msg = ctx.fetch_mailpit_message(&msg_id).await;

    assert_eq!(msg["From"]["Name"], "Café Ünïcode", "From: {}", msg["From"]);
    assert_eq!(msg["From"]["Address"], "noreply@example.com");
    assert_eq!(msg["To"][0]["Name"], "Alice", "To: {}", msg["To"]);
    assert_eq!(msg["To"][0]["Address"], "recipient@example.com");
}
