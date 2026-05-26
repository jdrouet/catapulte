use std::time::Duration;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let mjml_source = r"<mjml>
  <mj-head>
    <mj-preview>Hello {{ name }}!</mj-preview>
  </mj-head>
  <mj-body>
    <mj-section>
      <mj-column>
        <mj-text>Hello {{ name }}!</mj-text>
      </mj-column>
    </mj-section>
  </mj-body>
</mjml>";

    let payload = serde_json::json!({
        "sender": "sender@example.com",
        "recipients": [{ "kind": "to", "address": "recipient@example.com" }],
        "body": { "kind": "mjml_inline", "source": mjml_source },
        "variables": { "name": "World" }
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

    let html = msg["HTML"].as_str().unwrap_or("");
    assert!(
        html.contains("Hello World!"),
        "expected rendered HTML to contain 'Hello World!', got: {html}"
    );
    let text = msg["Text"].as_str().unwrap_or("");
    assert!(
        text.contains("Hello World!"),
        "expected text part to contain 'Hello World!', got: {text}"
    );
}
