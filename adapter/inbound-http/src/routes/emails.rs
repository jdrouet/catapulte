use axum::Json;
use axum::extract::State;
use catapulte_domain::use_case::submit_email::{SubmitEmailUseCase, SubmitParams};

use crate::HttpServerState;
use crate::dto::{SubmitEmailRequest, SubmitEmailResponse};
use crate::error::AppError;

/// # Errors
///
/// Returns `AppError::BadRequest` on invalid input or `AppError::Submit` on use case failure.
#[tracing::instrument(skip_all, fields(sender = %request.sender, recipients_count = request.recipients.len()))]
pub async fn submit_email<S: HttpServerState>(
    State(state): State<S>,
    Json(request): Json<SubmitEmailRequest>,
) -> Result<Json<SubmitEmailResponse>, AppError> {
    let envelope = request.into_envelope()?;
    let id = state
        .submit_email()
        .execute(envelope, SubmitParams {})
        .await?;
    Ok(Json(SubmitEmailResponse { id }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::port::email_repository::EmailRepositoryError;
    use catapulte_domain::port::event_repository::{
        EventRecord, EventRepository, EventRepositoryError, ListEventsParams,
    };
    use catapulte_domain::use_case::submit_email::{
        SubmitEmailError, SubmitEmailUseCase, SubmitParams,
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::HttpServerState;
    use crate::router;

    #[derive(Clone)]
    struct FakeSubmit;

    impl SubmitEmailUseCase for FakeSubmit {
        async fn execute(
            &self,
            _envelope: Envelope,
            _params: SubmitParams,
        ) -> Result<EmailId, SubmitEmailError> {
            Ok(EmailId::default())
        }
    }

    #[derive(Clone)]
    struct FailingSubmit;

    impl SubmitEmailUseCase for FailingSubmit {
        async fn execute(
            &self,
            _envelope: Envelope,
            _params: SubmitParams,
        ) -> Result<EmailId, SubmitEmailError> {
            Err(SubmitEmailError::Persist(EmailRepositoryError::Storage {
                source: anyhow::anyhow!("nope"),
            }))
        }
    }

    struct FakeEventRepository;

    #[allow(async_fn_in_trait)]
    impl EventRepository for FakeEventRepository {
        async fn list_events(
            &self,
            _params: ListEventsParams,
        ) -> Result<Vec<EventRecord>, EventRepositoryError> {
            Ok(vec![])
        }
    }

    #[derive(Clone)]
    struct TestState {
        submit: Arc<FakeSubmit>,
        event_repo: Arc<FakeEventRepository>,
    }

    impl HttpServerState for TestState {
        fn submit_email(&self) -> &impl SubmitEmailUseCase {
            self.submit.as_ref()
        }

        fn event_repository(&self) -> &impl EventRepository {
            self.event_repo.as_ref()
        }
    }

    #[derive(Clone)]
    struct FailingTestState {
        submit: Arc<FailingSubmit>,
        event_repo: Arc<FakeEventRepository>,
    }

    impl HttpServerState for FailingTestState {
        fn submit_email(&self) -> &impl SubmitEmailUseCase {
            self.submit.as_ref()
        }

        fn event_repository(&self) -> &impl EventRepository {
            self.event_repo.as_ref()
        }
    }

    fn make_router() -> axum::Router {
        router(TestState {
            submit: Arc::new(FakeSubmit),
            event_repo: Arc::new(FakeEventRepository),
        })
    }

    fn make_failing_router() -> axum::Router {
        router(FailingTestState {
            submit: Arc::new(FailingSubmit),
            event_repo: Arc::new(FakeEventRepository),
        })
    }

    fn post_json(body: impl Into<Body>) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/emails")
            .header("content-type", "application/json")
            .body(body.into())
            .unwrap()
    }

    #[tokio::test]
    async fn submit_plain_email_returns_200_with_id() {
        let app = make_router();
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"}
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let id_str = json.get("id").and_then(|v| v.as_str()).expect("id field");
        uuid::Uuid::parse_str(id_str).expect("id should be a valid UUID");
    }

    #[tokio::test]
    async fn submit_with_no_body_parts_returns_400() {
        let app = make_router();
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": null, "html": null}
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn submit_with_invalid_remote_url_returns_400() {
        let app = make_router();
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "mjml_remote", "url": "not a url"}
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn submit_propagates_use_case_failure_as_500() {
        let app = make_failing_router();
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"}
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
