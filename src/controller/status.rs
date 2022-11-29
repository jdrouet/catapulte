use axum::http::StatusCode;

pub(super) async fn handler() -> StatusCode {
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::handler;
    use axum::http::StatusCode;

    #[tokio::test]
    async fn success() {
        let result = handler().await;
        assert_eq!(result, StatusCode::NO_CONTENT);
    }
}
