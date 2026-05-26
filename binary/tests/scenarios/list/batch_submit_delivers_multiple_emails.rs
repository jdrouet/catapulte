use std::time::Duration;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let resp = ctx
        .client
        .post(format!("{}/emails/batch", ctx.http_base))
        .json(&serde_json::json!({
            "emails": [
                {
                    "sender": "sender@example.com",
                    "recipients": [{ "kind": "to", "address": "alice@example.com" }],
                    "body": { "kind": "plain", "text": "batch email 1" }
                },
                {
                    "sender": "sender@example.com",
                    "recipients": [{ "kind": "to", "address": "bob@example.com" }],
                    "body": { "kind": "plain", "text": "batch email 2" }
                },
                {
                    "sender": "sender@example.com",
                    "recipients": [{ "kind": "to", "address": "carol@example.com" }],
                    "body": { "kind": "plain", "text": "batch email 3" }
                }
            ]
        }))
        .send()
        .await
        .expect("POST /emails/batch failed");

    assert!(
        resp.status().is_success(),
        "unexpected status: {}",
        resp.status()
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 3, "expected 3 results");

    let mut seen_ids = std::collections::HashSet::new();
    for result in results {
        assert_eq!(
            result["status"].as_str(),
            Some("accepted"),
            "all results should be accepted"
        );
        let id = result["id"].as_str().expect("id field");
        uuid::Uuid::parse_str(id).expect("id should be a valid UUID");
        assert!(seen_ids.insert(id.to_owned()), "UUIDs should be distinct");
    }

    let messages = ctx
        .wait_for_mailpit_messages(3, Duration::from_secs(15))
        .await;
    assert_eq!(
        messages.len(),
        3,
        "expected 3 messages delivered to mailpit, got {}",
        messages.len()
    );
}
