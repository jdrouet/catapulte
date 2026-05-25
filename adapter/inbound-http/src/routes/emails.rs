use axum::Json;
use axum::extract::{Query, State};
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::port::email_repository::{EmailRepository, ListEmailsParams};
use catapulte_domain::use_case::submit_email::{SubmitEmailUseCase, SubmitParams};

use crate::HttpServerState;
use crate::dto::{
    DEFAULT_EMAILS_LIMIT, EmailRecordDto, ListEmailsQuery, ListEmailsResponse, MAX_EMAILS_LIMIT,
    SubmitEmailRequest, SubmitEmailResponse,
};
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

/// # Errors
///
/// Returns `AppError::InvalidEmailId` when the `id` query param is not a valid UUID.
/// Returns `AppError::ListEmails` when the repository query fails.
#[tracing::instrument(skip_all)]
pub async fn list_emails<S: HttpServerState>(
    State(state): State<S>,
    Query(query): Query<ListEmailsQuery>,
) -> Result<Json<ListEmailsResponse>, AppError> {
    let id = match query.id.as_deref() {
        Some(raw) => Some(EmailId::from(
            uuid::Uuid::parse_str(raw).map_err(|_| AppError::InvalidEmailId)?,
        )),
        None => None,
    };
    let limit = query
        .limit
        .unwrap_or(DEFAULT_EMAILS_LIMIT)
        .min(MAX_EMAILS_LIMIT);
    let offset = query.offset.unwrap_or(0);
    let params = ListEmailsParams {
        status: query.status.map(Into::into),
        after_ms: query.after_ms,
        before_ms: query.before_ms,
        recipient: query.recipient,
        id,
        limit,
        offset,
    };
    let emails = state
        .email_repository()
        .list_emails(params)
        .await
        .map_err(AppError::ListEmails)?
        .into_iter()
        .map(EmailRecordDto::from)
        .collect();
    Ok(Json(ListEmailsResponse {
        emails,
        limit,
        offset,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::port::email_repository::{
        EmailRecord, EmailRepository, EmailRepositoryError, EmailStatus, ListEmailsParams,
        SaveResult,
    };
    use catapulte_domain::port::event_repository::{
        EventRecord, EventRepository, EventRepositoryError, ListEventsParams,
    };
    use catapulte_domain::use_case::submit_email::{
        SubmitEmailError, SubmitEmailUseCase, SubmitParams,
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use catapulte_domain::use_case::list_senders::{
        ListSendersError, ListSendersUseCase, SenderUsage,
    };

    use crate::HttpServerState;
    use crate::dto::{DEFAULT_EMAILS_LIMIT, MAX_EMAILS_LIMIT};
    use crate::router;

    struct NoopListSenders;

    #[allow(async_fn_in_trait)]
    impl ListSendersUseCase for NoopListSenders {
        async fn execute(&self) -> Result<Vec<SenderUsage>, ListSendersError> {
            Ok(vec![])
        }
    }

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
    struct FakeEmailRepository {
        captured_params: Arc<Mutex<Option<ListEmailsParams>>>,
        result: Arc<Vec<EmailRecord>>,
    }

    impl FakeEmailRepository {
        fn new() -> Self {
            Self {
                captured_params: Arc::new(Mutex::new(None)),
                result: Arc::new(vec![]),
            }
        }

        fn with_records(records: Vec<EmailRecord>) -> Self {
            Self {
                captured_params: Arc::new(Mutex::new(None)),
                result: Arc::new(records),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl EmailRepository for FakeEmailRepository {
        async fn save(
            &self,
            id: EmailId,
            _envelope: &Envelope,
        ) -> Result<SaveResult, EmailRepositoryError> {
            Ok(SaveResult::Created(id))
        }

        async fn list_emails(
            &self,
            params: ListEmailsParams,
        ) -> Result<Vec<EmailRecord>, EmailRepositoryError> {
            *self.captured_params.lock().unwrap() = Some(params);
            Ok((*self.result).clone())
        }
    }

    struct FailingEmailRepository;

    #[allow(async_fn_in_trait)]
    impl EmailRepository for FailingEmailRepository {
        async fn save(
            &self,
            id: EmailId,
            _envelope: &Envelope,
        ) -> Result<SaveResult, EmailRepositoryError> {
            Ok(SaveResult::Created(id))
        }

        async fn list_emails(
            &self,
            _params: ListEmailsParams,
        ) -> Result<Vec<EmailRecord>, EmailRepositoryError> {
            Err(EmailRepositoryError::Storage {
                source: anyhow::anyhow!("db down"),
            })
        }
    }

    #[derive(Clone)]
    struct TestState {
        submit: Arc<FakeSubmit>,
        event_repo: Arc<FakeEventRepository>,
        email_repo: Arc<FakeEmailRepository>,
    }

    impl HttpServerState for TestState {
        fn submit_email(&self) -> &impl SubmitEmailUseCase {
            self.submit.as_ref()
        }

        fn event_repository(&self) -> &impl EventRepository {
            self.event_repo.as_ref()
        }

        fn email_repository(&self) -> &impl EmailRepository {
            self.email_repo.as_ref()
        }

        fn list_senders(&self) -> &impl ListSendersUseCase {
            &NoopListSenders
        }
    }

    #[derive(Clone)]
    struct FailingTestState {
        submit: Arc<FailingSubmit>,
        event_repo: Arc<FakeEventRepository>,
        email_repo: Arc<FakeEmailRepository>,
    }

    impl HttpServerState for FailingTestState {
        fn submit_email(&self) -> &impl SubmitEmailUseCase {
            self.submit.as_ref()
        }

        fn event_repository(&self) -> &impl EventRepository {
            self.event_repo.as_ref()
        }

        fn email_repository(&self) -> &impl EmailRepository {
            self.email_repo.as_ref()
        }

        fn list_senders(&self) -> &impl ListSendersUseCase {
            &NoopListSenders
        }
    }

    #[derive(Clone)]
    struct FailingEmailRepoState {
        submit: Arc<FakeSubmit>,
        event_repo: Arc<FakeEventRepository>,
        email_repo: Arc<FailingEmailRepository>,
    }

    impl HttpServerState for FailingEmailRepoState {
        fn submit_email(&self) -> &impl SubmitEmailUseCase {
            self.submit.as_ref()
        }

        fn event_repository(&self) -> &impl EventRepository {
            self.event_repo.as_ref()
        }

        fn email_repository(&self) -> &impl EmailRepository {
            self.email_repo.as_ref()
        }

        fn list_senders(&self) -> &impl ListSendersUseCase {
            &NoopListSenders
        }
    }

    fn make_router() -> axum::Router {
        router(TestState {
            submit: Arc::new(FakeSubmit),
            event_repo: Arc::new(FakeEventRepository),
            email_repo: Arc::new(FakeEmailRepository::new()),
        })
    }

    fn make_failing_router() -> axum::Router {
        router(FailingTestState {
            submit: Arc::new(FailingSubmit),
            event_repo: Arc::new(FakeEventRepository),
            email_repo: Arc::new(FakeEmailRepository::new()),
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

    fn get_emails(query: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(format!("/emails{query}"))
            .body(Body::empty())
            .unwrap()
    }

    fn sample_email_record() -> EmailRecord {
        EmailRecord {
            id: EmailId::default(),
            idempotency_key: None,
            subject: None,
            sender: "sender@example.com".to_owned(),
            recipients: vec![],
            created_at_ms: 1000,
            status: EmailStatus::Queued,
        }
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

    #[tokio::test]
    async fn list_emails_returns_200_with_emails_array() {
        let email_repo = Arc::new(FakeEmailRepository::with_records(vec![
            sample_email_record(),
        ]));
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            event_repo: Arc::new(FakeEventRepository),
            email_repo,
        });
        let response = app.oneshot(get_emails("")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["emails"].as_array().is_some());
        assert_eq!(json["emails"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn list_emails_with_invalid_id_returns_400() {
        let app = make_router();
        let response = app.oneshot(get_emails("?id=not-a-uuid")).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_emails_status_filter_forwarded() {
        let email_repo = Arc::new(FakeEmailRepository::new());
        let captured = email_repo.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            event_repo: Arc::new(FakeEventRepository),
            email_repo,
        });
        app.oneshot(get_emails("?status=sent")).await.unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().status, Some(EmailStatus::Sent));
    }

    #[tokio::test]
    async fn list_emails_recipient_filter_forwarded() {
        let email_repo = Arc::new(FakeEmailRepository::new());
        let captured = email_repo.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            event_repo: Arc::new(FakeEventRepository),
            email_repo,
        });
        app.oneshot(get_emails("?recipient=alice")).await.unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().recipient.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn list_emails_caps_limit_at_max() {
        let email_repo = Arc::new(FakeEmailRepository::new());
        let captured = email_repo.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            event_repo: Arc::new(FakeEventRepository),
            email_repo,
        });
        app.oneshot(get_emails("?limit=500")).await.unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().limit, MAX_EMAILS_LIMIT);
    }

    #[tokio::test]
    async fn list_emails_applies_default_limit() {
        let email_repo = Arc::new(FakeEmailRepository::new());
        let captured = email_repo.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            event_repo: Arc::new(FakeEventRepository),
            email_repo,
        });
        app.oneshot(get_emails("")).await.unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().limit, DEFAULT_EMAILS_LIMIT);
    }

    #[tokio::test]
    async fn list_emails_500_when_repository_errors() {
        let app = router(FailingEmailRepoState {
            submit: Arc::new(FakeSubmit),
            event_repo: Arc::new(FakeEventRepository),
            email_repo: Arc::new(FailingEmailRepository),
        });
        let response = app.oneshot(get_emails("")).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
