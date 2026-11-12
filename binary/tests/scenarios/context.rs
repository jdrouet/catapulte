use std::time::Duration;

pub struct TestContext {
    pub client: reqwest::Client,
    pub http_base: String,
    pub mailpit_api_base: String,
}

impl TestContext {
    #[allow(clippy::unused_self)]
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
                && let Ok(body) = resp.json::<serde_json::Value>().await
                && let Some(msgs) = body["messages"].as_array()
                && msgs.len() >= expected
            {
                return msgs.clone();
            }
            if tokio::time::Instant::now() >= deadline {
                return vec![];
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn fetch_mailpit_message(&self, message_id: &str) -> serde_json::Value {
        self.client
            .get(format!(
                "{}/api/v1/message/{message_id}",
                self.mailpit_api_base
            ))
            .send()
            .await
            .expect("GET /api/v1/message/{id} failed")
            .json()
            .await
            .expect("failed to parse mailpit message response")
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
            if let Ok(resp) = self.client.get(&url).send().await
                && let Ok(body) = resp.json::<serde_json::Value>().await
                && let Some(events) = body["events"].as_array()
                && let Some(ev) = events
                    .iter()
                    .find(|e| e["event_type"].as_str() == Some(event_type))
            {
                return Some(ev.clone());
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

impl TestContext {
    /// Current time in epoch milliseconds, for building `send_at_ms` values.
    #[allow(clippy::unused_self)]
    pub fn now_ms(&self) -> i64 {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time before the epoch")
                .as_millis(),
        )
        .expect("time fits in i64")
    }

    /// Sleeps until `at_ms` (epoch milliseconds) has passed; returns at once when it has.
    pub async fn sleep_until_ms(&self, at_ms: i64) {
        let wait = at_ms - self.now_ms();
        if let Ok(wait) = u64::try_from(wait) {
            tokio::time::sleep(Duration::from_millis(wait)).await;
        }
    }

    pub async fn mailpit_message_count(&self) -> usize {
        let body: serde_json::Value = self
            .client
            .get(format!("{}/api/v1/messages", self.mailpit_api_base))
            .send()
            .await
            .expect("GET /api/v1/messages failed")
            .json()
            .await
            .expect("failed to parse mailpit messages");
        body["messages"].as_array().map_or(0, Vec::len)
    }

    /// The `GET /emails` record of one email.
    pub async fn email_record(&self, email_id: &str) -> serde_json::Value {
        let body: serde_json::Value = self
            .client
            .get(format!("{}/emails?id={email_id}", self.http_base))
            .send()
            .await
            .expect("GET /emails failed")
            .json()
            .await
            .expect("failed to parse /emails response");
        body["emails"][0].clone()
    }

    pub async fn cancel(&self, email_id: &str) -> reqwest::StatusCode {
        self.client
            .post(format!("{}/emails/{email_id}/cancel", self.http_base))
            .send()
            .await
            .expect("POST cancel failed")
            .status()
    }

    /// Polls until the email's status equals `status`; returns the last record seen.
    pub async fn wait_for_status(
        &self,
        email_id: &str,
        status: &str,
        timeout: Duration,
    ) -> serde_json::Value {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let record = self.email_record(email_id).await;
            if record["status"].as_str() == Some(status) || tokio::time::Instant::now() >= deadline
            {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}
