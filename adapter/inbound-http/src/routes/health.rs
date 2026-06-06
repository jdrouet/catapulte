use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct HealthResponse {
    status: &'static str,
}

pub(crate) async fn live() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub(crate) async fn ready<S: crate::ReadinessState>(
    axum::extract::State(state): axum::extract::State<S>,
) -> (axum::http::StatusCode, axum::Json<HealthResponse>) {
    use catapulte_domain::use_case::check_readiness::{CheckReadinessUseCase, Readiness};
    match state.check_readiness().check_readiness().await {
        Readiness::Ready => (
            axum::http::StatusCode::OK,
            axum::Json(HealthResponse { status: "ok" }),
        ),
        Readiness::NotReady => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(HealthResponse {
                status: "unavailable",
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use catapulte_domain::use_case::check_readiness::Readiness;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::live;

    #[derive(Clone)]
    struct MockReadiness(Readiness);

    impl crate::ReadinessState for MockReadiness {
        fn check_readiness(
            &self,
        ) -> &impl catapulte_domain::use_case::check_readiness::CheckReadinessUseCase {
            self
        }
    }

    impl catapulte_domain::use_case::check_readiness::CheckReadinessUseCase for MockReadiness {
        async fn check_readiness(&self) -> Readiness {
            self.0
        }
    }

    fn health_router(mock: MockReadiness) -> Router {
        Router::new()
            .route("/health/live", get(live))
            .route("/health/ready", get(super::ready::<MockReadiness>))
            .with_state(mock)
    }

    #[tokio::test]
    async fn live_returns_200_with_ok_status() {
        let app = health_router(MockReadiness(Readiness::Ready));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json, serde_json::json!({"status": "ok"}));
    }

    #[tokio::test]
    async fn ready_returns_200_when_ready() {
        let app = health_router(MockReadiness(Readiness::Ready));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json, serde_json::json!({"status": "ok"}));
    }

    #[tokio::test]
    async fn ready_returns_503_when_not_ready() {
        let app = health_router(MockReadiness(Readiness::NotReady));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json, serde_json::json!({"status": "unavailable"}));
    }
}
