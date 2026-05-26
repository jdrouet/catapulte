use std::time::Duration;

pub struct TestContext {
    pub client: reqwest::Client,
    pub http_base: String,
    pub mailpit_api_base: String,
}

impl TestContext {
    pub fn simple_payload(&self, recipient: &str, text: &str) -> serde_json::Value {
        serde_json::json!({
            "sender": "sender@example.com",
            "recipients": [{ "kind": "to", "address": recipient }],
            "body": { "kind": "plain", "text": text },
            "variables": {}
        })
    }

    pub async fn submit(&self, payload: serde_json::Value) -> reqwest::Response {
        self.client
            .post(format!("{}/emails", self.http_base))
            .json(&payload)
            .send()
            .await
            .expect("POST /emails failed")
    }

    pub async fn wait_for_mailpit_messages(
        &self,
        expected: usize,
        timeout: Duration,
    ) -> Vec<serde_json::Value> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Ok(resp) = self
                .client
                .get(format!("{}/api/v1/messages", self.mailpit_api_base))
                .send()
                .await
            {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(msgs) = body["messages"].as_array() {
                        if msgs.len() >= expected {
                            return msgs.clone();
                        }
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return vec![];
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn wait_for_event(
        &self,
        email_id: &str,
        event_type: &str,
        timeout: Duration,
    ) -> Option<serde_json::Value> {
        let url = format!("{}/emails/{email_id}/events", self.http_base);
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Ok(resp) = self.client.get(&url).send().await {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(events) = body["events"].as_array() {
                        if let Some(ev) = events
                            .iter()
                            .find(|e| e["event_type"].as_str() == Some(event_type))
                        {
                            return Some(ev.clone());
                        }
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
