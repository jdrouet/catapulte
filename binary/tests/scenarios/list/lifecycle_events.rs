use std::time::Duration;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let resp = ctx
        .submit(ctx.simple_payload("recipient@example.com", "lifecycle test"))
        .await;
    assert!(
        resp.status().is_success(),
        "POST /emails failed: {}",
        resp.status()
    );
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let _ = ctx
        .wait_for_mailpit_messages(1, Duration::from_secs(15))
        .await;

    let sent = ctx
        .wait_for_event(&id, "delivery.succeeded", Duration::from_secs(10))
        .await;
    assert!(sent.is_some(), "no sent event arrived");
}
