use std::time::Duration;

use base64::Engine as _;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nfake");

    let payload = serde_json::json!({
        "sender": "sender@example.com",
        "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
        "body": { "kind": "plain", "text": "logo", "html": "<p><img src=\"cid:logo\"></p>" },
        "attachments": [{
            "filename": "logo.png",
            "content_type": "image/png",
            "inline_base64": png,
            "content_id": "logo"
        }]
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

    let inline = msg["Inline"].as_array().expect("Inline array");
    assert_eq!(inline.len(), 1, "inline parts: {}", msg["Inline"]);
    assert_eq!(inline[0]["ContentID"], "logo");
    assert!(
        msg["HTML"].as_str().unwrap_or("").contains("cid:logo"),
        "HTML: {}",
        msg["HTML"]
    );
}
