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
    use crate::service::smtp::tests::{smtp_image_insecure, SMTP_PORT};
    use crate::service::{server::Server, smtp::tests::smtp_image_secure};
    use axum::{body::Body, http::Request};
    use testcontainers::clients::Cli as DockerCli;
    use tower::ServiceExt;

    #[tokio::test]
    async fn insecure() {
        crate::try_init_logs();

        let docker = DockerCli::default();
        let smtp_node = docker.run(smtp_image_insecure());
        let smtp_port = smtp_node.get_host_port_ipv4(SMTP_PORT);

        let res = Server::default_insecure(smtp_port)
            .app()
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

    #[tokio::test]
    async fn secure() {
        crate::try_init_logs();

        let docker = DockerCli::default();
        let smtp_node = docker.run(smtp_image_secure());
        let smtp_port = smtp_node.get_host_port_ipv4(SMTP_PORT);

        let res = Server::default_secure(smtp_port)
            .app()
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
