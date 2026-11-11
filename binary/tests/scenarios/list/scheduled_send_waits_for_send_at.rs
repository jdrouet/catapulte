use std::time::Duration;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let send_at = ctx.now_ms() + 5_000;
    let mut payload = ctx.simple_payload("recipient@example.com", "scheduled");
    payload["send_at_ms"] = serde_json::json!(send_at);
    let resp = ctx.submit(payload).await;
    assert!(resp.status().is_success(), "POST failed: {}", resp.status());
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let record = ctx.email_record(&id).await;
    assert_eq!(record["status"], "scheduled", "{record}");
    assert_eq!(record["send_at_ms"], send_at, "{record}");

    // Nothing may be delivered before the scheduled time.
    ctx.sleep_until_ms(send_at - 1_000).await;
    assert_eq!(ctx.mailpit_message_count().await, 0, "delivered too early");

    let messages = ctx
        .wait_for_mailpit_messages(1, Duration::from_secs(30))
        .await;
    assert_eq!(messages.len(), 1, "expected exactly one delivered email");

    let sending = ctx
        .wait_for_event(&id, "sending", Duration::from_secs(10))
        .await
        .expect("no sending event arrived");
    let started = sending["created_at_ms"].as_i64().unwrap();
    assert!(
        started >= send_at,
        "sending started at {started}, before send_at {send_at}"
    );

    let record = ctx
        .wait_for_status(&id, "sent", Duration::from_secs(30))
        .await;
    assert_eq!(record["status"], "sent", "{record}");
}
