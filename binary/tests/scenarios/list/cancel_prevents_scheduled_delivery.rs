use std::time::Duration;

use reqwest::StatusCode;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let send_at = ctx.now_ms() + 8_000;
    let mut payload = ctx.simple_payload("recipient@example.com", "never sent");
    payload["send_at_ms"] = serde_json::json!(send_at);
    payload["idempotency_key"] = serde_json::json!("cancel-scenario");
    let resp = ctx.submit(payload.clone()).await;
    assert!(resp.status().is_success(), "POST failed: {}", resp.status());
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    assert_eq!(ctx.cancel(&id).await, StatusCode::NO_CONTENT);
    assert_eq!(ctx.cancel(&id).await, StatusCode::NO_CONTENT);
    assert!(
        ctx.wait_for_event(&id, "cancelled", Duration::from_secs(10))
            .await
            .is_some(),
        "no cancelled event arrived"
    );
    assert_eq!(ctx.email_record(&id).await["status"], "cancelled");

    // A retry with the same key returns the original id and does not revive the email.
    let again = ctx.submit(payload).await;
    assert!(again.status().is_success(), "resubmit: {}", again.status());
    let again_id = again.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(again_id, id);
    assert_eq!(ctx.email_record(&id).await["status"], "cancelled");

    ctx.sleep_until_ms(send_at + 5_000).await;
    assert_eq!(ctx.mailpit_message_count().await, 0, "cancelled email sent");
    assert_eq!(ctx.email_record(&id).await["status"], "cancelled");

    assert_eq!(
        ctx.cancel("00000000-0000-0000-0000-000000000001").await,
        StatusCode::NOT_FOUND
    );

    let resp = ctx
        .submit(ctx.simple_payload("recipient@example.com", "immediate"))
        .await;
    let delivered = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        ctx.wait_for_event(&delivered, "delivery.succeeded", Duration::from_secs(30))
            .await
            .is_some(),
        "immediate email was not delivered"
    );
    assert_eq!(ctx.cancel(&delivered).await, StatusCode::CONFLICT);
}
