use reqwest::StatusCode;

use crate::scenarios::context::TestContext;

pub async fn scenario(ctx: TestContext) {
    let mut payload = ctx.simple_payload("recipient@example.com", "scheduled");
    payload["send_at_ms"] = serde_json::json!(ctx.now_ms() + 60_000);

    let resp = ctx.submit(payload).await;

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
