use axum::http::StatusCode;

/// Check the status of Catapulte
///
/// Just answers if everything is going fine.
// TODO check the smtp connection
#[utoipa::path(
    operation_id = "status",
    head,
    path = "/status",
    responses(
        (status = 204, description = "Everything is running smoothly"),
    )
)]
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
