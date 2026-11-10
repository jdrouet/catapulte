use std::time::Duration;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let payload = serde_json::json!({
        "sender": "noreply@example.com",
        "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
        "body": { "kind": "plain", "text": "hello", "html": null },
        "unsubscribe": {
            "url": "https://example.com/unsubscribe/42",
            "mailto": "unsubscribe@example.com"
        }
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

    let headers: serde_json::Value = ctx
        .client
        .get(format!(
            "{}/api/v1/message/{msg_id}/headers",
            ctx.mailpit_api_base
        ))
        .send()
        .await
        .expect("GET headers failed")
        .json()
        .await
        .expect("headers are not JSON");
    assert_eq!(
        headers["List-Unsubscribe"][0],
        "<https://example.com/unsubscribe/42>, <mailto:unsubscribe@example.com>",
        "headers: {headers}"
    );
    assert_eq!(
        headers["List-Unsubscribe-Post"][0], "List-Unsubscribe=One-Click",
        "headers: {headers}"
    );
}
