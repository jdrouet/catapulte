use std::time::Duration;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let payload = serde_json::json!({
        "idempotency_key": "scenario-idem-key",
        "sender": "sender@example.com",
        "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
        "body": { "kind": "plain", "text": "Idempotency test" },
        "variables": {}
    });
    let resp1 = ctx.submit(payload.clone()).await;
    assert!(
        resp1.status().is_success(),
        "first POST failed: {}",
        resp1.status()
    );
    let id1 = resp1.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp2 = ctx.submit(payload).await;
    assert!(
        resp2.status().is_success(),
        "second POST failed: {}",
        resp2.status()
    );
    let id2 = resp2.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    assert_eq!(id1, id2, "both requests must return the same email id");

    let messages = ctx
        .wait_for_mailpit_messages(1, Duration::from_secs(15))
        .await;
    assert_eq!(messages.len(), 1, "expected exactly one delivered email");
}
