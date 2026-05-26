use std::time::Duration;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let resp = ctx
        .submit(ctx.simple_payload("recipient@example.com", "Hello list!"))
        .await;
    assert!(
        resp.status().is_success(),
        "POST /emails failed: {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let id = body["id"].as_str().expect("id field missing").to_owned();

    let _ = ctx
        .wait_for_mailpit_messages(1, Duration::from_secs(15))
        .await;

    // GET /emails -> contains the submitted id
    let resp = ctx
        .client
        .get(format!("{}/emails", ctx.http_base))
        .send()
        .await
        .expect("GET /emails failed");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let emails = body["emails"].as_array().expect("emails array");
    assert!(
        emails.iter().any(|e| e["id"].as_str() == Some(id.as_str())),
        "submitted email id not found in GET /emails response"
    );

    // GET /emails?id={id} -> exactly one entry
    let resp = ctx
        .client
        .get(format!("{}/emails?id={id}", ctx.http_base))
        .send()
        .await
        .expect("GET /emails?id failed");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let emails = body["emails"].as_array().expect("emails array");
    assert_eq!(emails.len(), 1, "expected exactly one email for id filter");
    assert_eq!(emails[0]["id"].as_str(), Some(id.as_str()));
}
