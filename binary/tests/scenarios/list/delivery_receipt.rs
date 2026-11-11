use std::time::Duration;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let resp = ctx
        .submit(ctx.simple_payload("recipient@example.com", "receipt test"))
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

    let messages = ctx
        .wait_for_mailpit_messages(1, Duration::from_secs(15))
        .await;
    assert_eq!(messages.len(), 1, "expected exactly one delivered email");
    let msg_id = messages[0]["ID"]
        .as_str()
        .expect("message ID missing")
        .to_owned();
    let msg = ctx.fetch_mailpit_message(&msg_id).await;

    let sent = ctx
        .wait_for_event(&id, "delivery.succeeded", Duration::from_secs(10))
        .await
        .expect("no sent event arrived");
    let payload = &sent["payload"];

    let response = payload["smtp_response"]
        .as_str()
        .unwrap_or_else(|| panic!("smtp_response missing: {sent}"));
    assert!(response.starts_with("250"), "smtp_response: {response}");

    // Mailpit reports the Message-ID without the angle brackets.
    let message_id = payload["message_id"]
        .as_str()
        .unwrap_or_else(|| panic!("message_id missing: {sent}"));
    assert_eq!(
        message_id.trim_start_matches('<').trim_end_matches('>'),
        msg["MessageID"]
            .as_str()
            .expect("Mailpit MessageID missing"),
    );
    assert_eq!(message_id, format!("<{id}@example.com>"));
}
