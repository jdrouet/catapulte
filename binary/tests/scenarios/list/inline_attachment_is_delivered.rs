use std::time::Duration;

use base64::Engine as _;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let attachment_content = b"Hello attachment";
    let inline_base64 = base64::engine::general_purpose::STANDARD.encode(attachment_content);

    let payload = serde_json::json!({
        "sender": "sender@example.com",
        "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
        "body": { "kind": "plain", "text": "Email with attachment" },
        "variables": {},
        "attachments": [{
            "filename": "test.txt",
            "content_type": "text/plain",
            "inline_base64": inline_base64
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

    let attachments = msg["Attachments"].as_array().expect("Attachments array");
    assert_eq!(attachments.len(), 1, "expected exactly one attachment");

    let att = &attachments[0];
    assert_eq!(
        att["FileName"].as_str(),
        Some("test.txt"),
        "attachment filename mismatch"
    );
    let content_type = att["ContentType"].as_str().unwrap_or("");
    assert!(
        content_type.starts_with("text/plain"),
        "expected content type to start with text/plain, got: {content_type}"
    );
}
