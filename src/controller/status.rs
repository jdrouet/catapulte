use crate::error::ServerError;
use crate::service::smtp::SmtpPool;
use axum::extract::Extension;
use axum::http::StatusCode;

/// Check the status of Catapulte
///
/// Just answers if everything is going fine.
#[utoipa::path(
    operation_id = "status",
    head,
    path = "/status",
    responses(
        (status = 204, description = "Everything is running smoothly."),
        (status = 500, description = "The SMTP server cannot be reached."),
    )
)]
pub(crate) async fn handler(
    Extension(smtp_pool): Extension<SmtpPool>,
) -> Result<StatusCode, ServerError> {
    metrics::counter!("status_check").increment(1);
    smtp_pool.test_connection()?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::handler;
    use axum::extract::Extension;
    use axum::http::StatusCode;

    #[tokio::test]
    async fn success() {
        crate::try_init_logs();

        let smtp_pool = crate::service::smtp::Configuration::secure()
            .build()
            .unwrap();
        let result = handler(Extension(smtp_pool)).await.unwrap();
        assert_eq!(result, StatusCode::NO_CONTENT);
    }
}

#[cfg(test)]
mod integration_tests {
    use crate::service::server::Server;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    fn create_app() -> axum::Router {
        crate::try_init_logs();

        Server::default_insecure().app()
    }

    #[tokio::test]
    async fn success() {
        let res = create_app()
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .method("HEAD")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::NO_CONTENT);
    }
}
