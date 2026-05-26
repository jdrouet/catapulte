use axum::Json;
use axum::body::Body;
use axum::extract::{FromRequest, Multipart, Query, Request, State};
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::port::email_repository::ListEmailsParams;
use catapulte_domain::use_case::list_emails::ListEmailsUseCase;
use catapulte_domain::use_case::submit_email::{AttachmentInput, SubmitEmailUseCase};
use futures_util::TryStreamExt;

use crate::HttpServerState;
use crate::dto::{
    BatchItemResultDto, BatchSubmitEmailRequest, BatchSubmitEmailResponse, DEFAULT_EMAILS_LIMIT,
    EmailRecordDto, EnvelopeCoreDto, ListEmailsQuery, ListEmailsResponse, MAX_ATTACHMENT_BYTES,
    MAX_ATTACHMENTS_PER_EMAIL, MAX_EMAILS_LIMIT, MAX_EMAILS_PER_BATCH, MAX_ENVELOPE_BYTES,
    MAX_REQUEST_BODY_BYTES, SubmitEmailRequest, SubmitEmailResponse,
};
use crate::error::AppError;
use crate::limited_reader::LimitedReader;

fn is_multipart_form_data(content_type: Option<&axum::http::HeaderValue>) -> bool {
    content_type.and_then(|v| v.to_str().ok()).is_some_and(|s| {
        let head = s.split(';').next().unwrap_or("").trim();
        head.eq_ignore_ascii_case("multipart/form-data")
    })
}

/// # Errors
///
/// Returns `AppError::BadRequest` on invalid input or `AppError::Submit` on use case failure.
#[tracing::instrument(skip_all)]
pub async fn submit_email<S: HttpServerState>(
    State(state): State<S>,
    request: Request<Body>,
) -> Result<Json<SubmitEmailResponse>, AppError> {
    let content_type = request.headers().get(axum::http::header::CONTENT_TYPE);

    let input = if is_multipart_form_data(content_type) {
        handle_multipart(request).await?
    } else {
        handle_json(request).await?
    };

    let id = state.submit_email().execute(input).await?;
    Ok(Json(SubmitEmailResponse { id }))
}

/// # Errors
///
/// Returns `AppError::BadRequestRaw` when the batch exceeds the maximum size limit.
#[tracing::instrument(skip_all, fields(batch_size = request.emails.len()))]
pub async fn submit_email_batch<S: HttpServerState>(
    State(state): State<S>,
    Json(request): Json<BatchSubmitEmailRequest>,
) -> Result<Json<BatchSubmitEmailResponse>, AppError> {
    if request.emails.len() > MAX_EMAILS_PER_BATCH {
        return Err(AppError::BadRequestRaw(format!(
            "batch exceeds maximum of {MAX_EMAILS_PER_BATCH} emails"
        )));
    }
    let mut results = Vec::with_capacity(request.emails.len());
    for email_req in request.emails {
        match email_req.into_submit_input() {
            Err(validation_err) => results.push(BatchItemResultDto::Rejected {
                error: validation_err.to_string(),
            }),
            Ok(input) => {
                let id = state.submit_email().execute(input).await?;
                results.push(BatchItemResultDto::Accepted {
                    id: id.as_uuid().to_string(),
                });
            }
        }
    }
    Ok(Json(BatchSubmitEmailResponse { results }))
}

async fn handle_json(
    request: Request<Body>,
) -> Result<catapulte_domain::use_case::submit_email::SubmitEmailInput, AppError> {
    let body_bytes = axum::body::to_bytes(request.into_body(), MAX_REQUEST_BODY_BYTES)
        .await
        .map_err(|e| AppError::BadRequestRaw(format!("failed to read request body: {e}")))?;
    let req: SubmitEmailRequest = serde_json::from_slice(&body_bytes)
        .map_err(|e| AppError::BadRequestRaw(format!("invalid JSON: {e}")))?;
    Ok(req.into_submit_input()?)
}

async fn handle_multipart(
    request: Request<Body>,
) -> Result<catapulte_domain::use_case::submit_email::SubmitEmailInput, AppError> {
    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|e| AppError::BadRequestRaw(format!("invalid multipart: {e}")))?;

    let mut envelope: Option<EnvelopeCoreDto> = None;
    let mut attachments: Vec<AttachmentInput> = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequestRaw(format!("multipart read error: {e}")))?
    {
        let name = field.name().unwrap_or("").to_owned();
        match name.as_str() {
            "envelope" => {
                use futures_util::StreamExt;
                let stream = field.map(|r| r.map_err(std::io::Error::other));
                let reader = tokio_util::io::StreamReader::new(stream);
                let limited = LimitedReader::new(reader, MAX_ENVELOPE_BYTES as u64);
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut Box::pin(limited), &mut buf)
                    .await
                    .map_err(|_| {
                        AppError::BadRequestRaw("envelope field exceeds size limit".to_owned())
                    })?;
                let dto: EnvelopeCoreDto = serde_json::from_slice(&buf)
                    .map_err(|e| AppError::BadRequestRaw(format!("invalid envelope JSON: {e}")))?;
                envelope = Some(dto);
            }
            "attachment" => {
                if attachments.len() >= MAX_ATTACHMENTS_PER_EMAIL {
                    return Err(AppError::BadRequestRaw("too many attachments".to_owned()));
                }

                let filename = field
                    .file_name()
                    .ok_or_else(|| {
                        AppError::BadRequestRaw(
                            "attachment field missing filename in Content-Disposition".to_owned(),
                        )
                    })?
                    .to_owned();

                let content_type = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_owned();

                // Buffer through LimitedReader to enforce the per-attachment size cap.
                // The axum Field borrows from Multipart and cannot be made 'static, so
                // we must drain it here before advancing to the next field.
                let stream = field.map_err(std::io::Error::other);
                let stream_reader = tokio_util::io::StreamReader::new(stream);
                let mut limited = LimitedReader::new(stream_reader, MAX_ATTACHMENT_BYTES);
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut limited, &mut buf)
                    .await
                    .map_err(|e| {
                        AppError::BadRequestRaw(format!("attachment too large or read error: {e}"))
                    })?;

                attachments.push(AttachmentInput::Inline {
                    filename,
                    content_type,
                    bytes: bytes::Bytes::from(buf),
                });
            }
            _ => {
                // Ignore unknown fields.
            }
        }
    }

    let envelope = envelope
        .ok_or_else(|| AppError::BadRequestRaw("missing required 'envelope' field".to_owned()))?;

    Ok(envelope.into_submit_input(attachments)?)
}

/// # Errors
///
/// Returns `AppError::InvalidEmailId` when the `id` query param is not a valid UUID.
/// Returns `AppError::ListEmails` when the use case fails.
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
        .list_emails()
        .execute(params)
        .await?
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
    use catapulte_domain::port::email_repository::{
        EmailRecord, EmailRepositoryError, EmailStatus, ListEmailsParams,
    };
    use catapulte_domain::port::event_repository::{EventRecord, ListEventsParams};
    use catapulte_domain::use_case::list_emails::{ListEmailsError, ListEmailsUseCase};
    use catapulte_domain::use_case::list_events::{ListEventsError, ListEventsUseCase};
    use catapulte_domain::use_case::submit_email::{
        SubmitEmailError, SubmitEmailInput, SubmitEmailUseCase,
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use catapulte_domain::use_case::list_senders::{
        ListSendersError, ListSendersUseCase, SenderSnapshot,
    };

    use crate::HttpServerState;
    use crate::dto::{DEFAULT_EMAILS_LIMIT, MAX_EMAILS_LIMIT};
    use crate::router;

    struct NoopListSenders;

    #[allow(async_fn_in_trait)]
    impl ListSendersUseCase for NoopListSenders {
        async fn execute(&self) -> Result<Vec<SenderSnapshot>, ListSendersError> {
            Ok(vec![])
        }
    }

    struct NoopListEvents;

    #[allow(async_fn_in_trait)]
    impl ListEventsUseCase for NoopListEvents {
        async fn execute(
            &self,
            _params: ListEventsParams,
        ) -> Result<Vec<EventRecord>, ListEventsError> {
            Ok(vec![])
        }
    }

    #[derive(Clone)]
    struct FakeSubmit;

    impl SubmitEmailUseCase for FakeSubmit {
        async fn execute(&self, _input: SubmitEmailInput) -> Result<EmailId, SubmitEmailError> {
            Ok(EmailId::default())
        }
    }

    #[derive(Clone)]
    struct FailingSubmit;

    impl SubmitEmailUseCase for FailingSubmit {
        async fn execute(&self, _input: SubmitEmailInput) -> Result<EmailId, SubmitEmailError> {
            Err(SubmitEmailError::Persist(EmailRepositoryError::Storage {
                source: anyhow::anyhow!("nope"),
            }))
        }
    }

    #[derive(Clone)]
    struct FakeListEmails {
        captured_params: Arc<Mutex<Option<ListEmailsParams>>>,
        result: Arc<Vec<EmailRecord>>,
    }

    impl FakeListEmails {
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
    impl ListEmailsUseCase for FakeListEmails {
        async fn execute(
            &self,
            params: ListEmailsParams,
        ) -> Result<Vec<EmailRecord>, ListEmailsError> {
            *self.captured_params.lock().unwrap() = Some(params);
            Ok((*self.result).clone())
        }
    }

    struct FailingListEmails;

    #[allow(async_fn_in_trait)]
    impl ListEmailsUseCase for FailingListEmails {
        async fn execute(
            &self,
            _params: ListEmailsParams,
        ) -> Result<Vec<EmailRecord>, ListEmailsError> {
            Err(ListEmailsError::Repository(EmailRepositoryError::Storage {
                source: anyhow::anyhow!("db down"),
            }))
        }
    }

    #[derive(Clone)]
    struct TestState {
        submit: Arc<FakeSubmit>,
        list_emails: Arc<FakeListEmails>,
    }

    impl HttpServerState for TestState {
        fn submit_email(&self) -> &impl SubmitEmailUseCase {
            self.submit.as_ref()
        }

        fn list_emails(&self) -> &impl ListEmailsUseCase {
            self.list_emails.as_ref()
        }

        fn list_events(&self) -> &impl ListEventsUseCase {
            &NoopListEvents
        }

        fn list_senders(&self) -> &impl ListSendersUseCase {
            &NoopListSenders
        }
    }

    #[derive(Clone)]
    struct FailingTestState {
        submit: Arc<FailingSubmit>,
        list_emails: Arc<FakeListEmails>,
    }

    impl HttpServerState for FailingTestState {
        fn submit_email(&self) -> &impl SubmitEmailUseCase {
            self.submit.as_ref()
        }

        fn list_emails(&self) -> &impl ListEmailsUseCase {
            self.list_emails.as_ref()
        }

        fn list_events(&self) -> &impl ListEventsUseCase {
            &NoopListEvents
        }

        fn list_senders(&self) -> &impl ListSendersUseCase {
            &NoopListSenders
        }
    }

    #[derive(Clone)]
    struct FailingEmailRepoState {
        submit: Arc<FakeSubmit>,
    }

    impl HttpServerState for FailingEmailRepoState {
        fn submit_email(&self) -> &impl SubmitEmailUseCase {
            self.submit.as_ref()
        }

        fn list_emails(&self) -> &impl ListEmailsUseCase {
            &FailingListEmails
        }

        fn list_events(&self) -> &impl ListEventsUseCase {
            &NoopListEvents
        }

        fn list_senders(&self) -> &impl ListSendersUseCase {
            &NoopListSenders
        }
    }

    fn make_router() -> axum::Router {
        router(TestState {
            submit: Arc::new(FakeSubmit),
            list_emails: Arc::new(FakeListEmails::new()),
        })
    }

    fn make_failing_router() -> axum::Router {
        router(FailingTestState {
            submit: Arc::new(FailingSubmit),
            list_emails: Arc::new(FakeListEmails::new()),
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
        let list_emails = Arc::new(FakeListEmails::with_records(vec![sample_email_record()]));
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_emails,
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
        let list_emails = Arc::new(FakeListEmails::new());
        let captured = list_emails.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_emails,
        });
        app.oneshot(get_emails("?status=sent")).await.unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().status, Some(EmailStatus::Sent));
    }

    #[tokio::test]
    async fn list_emails_recipient_filter_forwarded() {
        let list_emails = Arc::new(FakeListEmails::new());
        let captured = list_emails.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_emails,
        });
        app.oneshot(get_emails("?recipient=alice")).await.unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().recipient.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn list_emails_caps_limit_at_max() {
        let list_emails = Arc::new(FakeListEmails::new());
        let captured = list_emails.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_emails,
        });
        app.oneshot(get_emails("?limit=500")).await.unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().limit, MAX_EMAILS_LIMIT);
    }

    #[tokio::test]
    async fn list_emails_applies_default_limit() {
        let list_emails = Arc::new(FakeListEmails::new());
        let captured = list_emails.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_emails,
        });
        app.oneshot(get_emails("")).await.unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().limit, DEFAULT_EMAILS_LIMIT);
    }

    #[tokio::test]
    async fn list_emails_500_when_repository_errors() {
        let app = router(FailingEmailRepoState {
            submit: Arc::new(FakeSubmit),
        });
        let response = app.oneshot(get_emails("")).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn submit_with_invalid_sender_returns_400() {
        let app = make_router();
        let payload = serde_json::json!({
            "sender": "not-an-email",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"}
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn submit_with_empty_recipients_returns_400() {
        let app = make_router();
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [],
            "body": {"kind": "plain", "text": "hi"}
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn submit_with_invalid_recipient_returns_400() {
        let app = make_router();
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "bad"}],
            "body": {"kind": "plain", "text": "hi"}
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn submit_with_one_attachment_returns_200() {
        use base64::Engine;
        let app = make_router();
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"hello from attachment");
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"},
            "attachments": [{"filename": "test.txt", "content_type": "text/plain", "inline_base64": encoded}]
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn submit_with_too_many_attachments_returns_400() {
        use crate::dto::MAX_ATTACHMENTS_PER_EMAIL;
        use base64::Engine;
        let app = make_router();
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"x");
        let attachments: Vec<_> = (0..=MAX_ATTACHMENTS_PER_EMAIL)
            .map(|i| {
                serde_json::json!({"filename": format!("f{i}.txt"), "content_type": "text/plain", "inline_base64": encoded})
            })
            .collect();
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"},
            "attachments": attachments
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn submit_with_oversize_attachment_returns_400() {
        use crate::dto::MAX_ATTACHMENT_BYTES;
        use base64::Engine;
        let app = make_router();
        let big = vec![0u8; usize::try_from(MAX_ATTACHMENT_BYTES + 1).unwrap()];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&big);
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"},
            "attachments": [{"filename": "big.bin", "content_type": "application/octet-stream", "inline_base64": encoded}]
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn submit_with_invalid_base64_returns_400() {
        let app = make_router();
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"},
            "attachments": [{"filename": "bad.txt", "content_type": "text/plain", "inline_base64": "not valid base64!!!"}]
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn submit_with_large_inline_attachment_within_limit_succeeds_through_router() {
        use base64::Engine;
        // ~10 MiB raw — above axum's 2 MiB default limit, well below the 25 MiB per-attachment cap.
        let raw = vec![0u8; 10 * 1024 * 1024];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&raw);
        let app = make_router();
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"},
            "attachments": [{"filename": "large.bin", "content_type": "application/octet-stream", "inline_base64": encoded}]
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_ne!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "router body limit must not reject a 10 MiB attachment"
        );
    }

    #[tokio::test]
    async fn submit_with_remote_attachment_returns_200() {
        let app = make_router();
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"},
            "attachments": [{"filename": "remote.pdf", "content_type": "application/pdf", "url": "https://example.com/file.pdf"}]
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn submit_with_both_inline_and_url_returns_400() {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"x");
        let app = make_router();
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"},
            "attachments": [{"filename": "both.txt", "content_type": "text/plain", "inline_base64": encoded, "url": "https://example.com/file.txt"}]
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn submit_with_neither_inline_nor_url_returns_400() {
        let app = make_router();
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"},
            "attachments": [{"filename": "neither.txt", "content_type": "text/plain"}]
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn submit_with_malformed_url_returns_400() {
        let app = make_router();
        let payload = serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"},
            "attachments": [{"filename": "bad.txt", "content_type": "text/plain", "url": "not a url at all"}]
        });
        let response = app
            .oneshot(post_json(Body::from(serde_json::to_vec(&payload).unwrap())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // -- batch helpers and tests --

    fn post_batch_json(body: impl Into<Body>) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/emails/batch")
            .header("content-type", "application/json")
            .body(body.into())
            .unwrap()
    }

    fn valid_email_payload(recipient: &str) -> serde_json::Value {
        serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": recipient}],
            "body": {"kind": "plain", "text": "hi"}
        })
    }

    #[tokio::test]
    async fn batch_submit_with_three_valid_emails_returns_three_accepted() {
        let app = make_router();
        let payload = serde_json::json!({
            "emails": [
                valid_email_payload("r1@x.y"),
                valid_email_payload("r2@x.y"),
                valid_email_payload("r3@x.y"),
            ]
        });
        let response = app
            .oneshot(post_batch_json(Body::from(
                serde_json::to_vec(&payload).unwrap(),
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let results = json["results"].as_array().expect("results array");
        assert_eq!(results.len(), 3);
        for result in results {
            assert_eq!(result["status"].as_str(), Some("accepted"));
            let id = result["id"].as_str().expect("id field");
            uuid::Uuid::parse_str(id).expect("id should be a valid UUID");
        }
    }

    #[tokio::test]
    async fn batch_submit_with_one_invalid_email_returns_mixed_results() {
        let app = make_router();
        let payload = serde_json::json!({
            "emails": [
                valid_email_payload("r1@x.y"),
                // middle item has empty recipients — should be rejected
                {
                    "sender": "a@b.c",
                    "recipients": [],
                    "body": {"kind": "plain", "text": "hi"}
                },
                valid_email_payload("r3@x.y"),
            ]
        });
        let response = app
            .oneshot(post_batch_json(Body::from(
                serde_json::to_vec(&payload).unwrap(),
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let results = json["results"].as_array().expect("results array");
        assert_eq!(results.len(), 3);
        assert_eq!(results[0]["status"].as_str(), Some("accepted"));
        assert_eq!(results[2]["status"].as_str(), Some("accepted"));
        assert_eq!(results[1]["status"].as_str(), Some("rejected"));
        assert!(
            results[1]["error"].as_str().is_some_and(|e| !e.is_empty()),
            "rejected entry should have a non-empty error message"
        );
    }

    #[tokio::test]
    async fn batch_submit_over_limit_returns_400() {
        use crate::dto::MAX_EMAILS_PER_BATCH;
        let app = make_router();
        let emails: Vec<_> = (0..=MAX_EMAILS_PER_BATCH)
            .map(|i| valid_email_payload(&format!("r{i}@x.y")))
            .collect();
        let payload = serde_json::json!({ "emails": emails });
        let response = app
            .oneshot(post_batch_json(Body::from(
                serde_json::to_vec(&payload).unwrap(),
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn batch_submit_empty_array_returns_200_with_empty_results() {
        let app = make_router();
        let payload = serde_json::json!({ "emails": [] });
        let response = app
            .oneshot(post_batch_json(Body::from(
                serde_json::to_vec(&payload).unwrap(),
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let results = json["results"].as_array().expect("results array");
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn batch_submit_aborts_with_500_on_use_case_failure() {
        let app = make_failing_router();
        let payload = serde_json::json!({
            "emails": [valid_email_payload("r@x.y")]
        });
        let response = app
            .oneshot(post_batch_json(Body::from(
                serde_json::to_vec(&payload).unwrap(),
            )))
            .await
            .unwrap();
        // Infrastructure failure must abort the entire batch and return 500.
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn batch_submit_validation_errors_do_not_abort_the_batch() {
        let app = make_router();
        let payload = serde_json::json!({
            "emails": [
                valid_email_payload("r1@x.y"),
                // invalid sender — triggers EnvelopeConversionError
                {
                    "sender": "not-an-email",
                    "recipients": [{"kind": "to", "address": "r2@x.y"}],
                    "body": {"kind": "plain", "text": "hi"}
                },
                valid_email_payload("r3@x.y"),
            ]
        });
        let response = app
            .oneshot(post_batch_json(Body::from(
                serde_json::to_vec(&payload).unwrap(),
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let results = json["results"].as_array().expect("results array");
        assert_eq!(results.len(), 3);
        assert_eq!(results[0]["status"].as_str(), Some("accepted"));
        assert_eq!(results[1]["status"].as_str(), Some("rejected"));
        assert!(
            results[1]["error"].as_str().is_some_and(|e| !e.is_empty()),
            "rejected entry should have a non-empty sender-related error message"
        );
        assert_eq!(results[2]["status"].as_str(), Some("accepted"));
    }

    // -- multipart helpers and tests --

    #[allow(clippy::type_complexity)]
    fn multipart_body(boundary: &str, parts: &[(&str, Option<&str>, Option<&str>, &[u8])]) -> Body {
        let mut out = Vec::new();
        for (name, filename, ct, body) in parts {
            out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            let cd = match filename {
                Some(f) => {
                    format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n")
                }
                None => format!("Content-Disposition: form-data; name=\"{name}\"\r\n"),
            };
            out.extend_from_slice(cd.as_bytes());
            if let Some(ct) = ct {
                out.extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
            }
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(body);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        Body::from(out)
    }

    fn post_multipart(boundary: &str, body: Body) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/emails")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .unwrap()
    }

    fn envelope_json() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"}
        }))
        .unwrap()
    }

    /// A `SubmitEmailUseCase` that reads all stream attachment bytes and stores them for inspection.
    #[derive(Clone)]
    struct CapturingSubmit {
        captured_bytes: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl CapturingSubmit {
        fn new() -> Self {
            Self {
                captured_bytes: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl SubmitEmailUseCase for CapturingSubmit {
        async fn execute(&self, input: SubmitEmailInput) -> Result<EmailId, SubmitEmailError> {
            use catapulte_domain::use_case::submit_email::AttachmentInput;
            for att in input.attachments {
                if let AttachmentInput::Inline { bytes, .. } = att {
                    self.captured_bytes.lock().unwrap().push(bytes.to_vec());
                }
            }
            Ok(EmailId::default())
        }
    }

    #[derive(Clone)]
    struct CapturingTestState {
        submit: Arc<CapturingSubmit>,
        list_emails: Arc<FakeListEmails>,
    }

    impl HttpServerState for CapturingTestState {
        fn submit_email(&self) -> &impl SubmitEmailUseCase {
            self.submit.as_ref()
        }

        fn list_emails(&self) -> &impl ListEmailsUseCase {
            self.list_emails.as_ref()
        }

        fn list_events(&self) -> &impl ListEventsUseCase {
            &NoopListEvents
        }

        fn list_senders(&self) -> &impl ListSendersUseCase {
            &NoopListSenders
        }
    }

    #[tokio::test]
    async fn submit_multipart_with_one_attachment_returns_200() {
        let boundary = "testboundary123";
        let file_content = b"streamed file content";
        let envelope = envelope_json();
        let body = multipart_body(
            boundary,
            &[
                ("envelope", None, Some("application/json"), &envelope),
                (
                    "attachment",
                    Some("test.txt"),
                    Some("text/plain"),
                    file_content,
                ),
            ],
        );

        let submit = Arc::new(CapturingSubmit::new());
        let captured = submit.captured_bytes.clone();
        let app = router(CapturingTestState {
            submit,
            list_emails: Arc::new(FakeListEmails::new()),
        });

        let response = app.oneshot(post_multipart(boundary, body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let blobs = captured.lock().unwrap();
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0], file_content);
    }

    #[tokio::test]
    async fn submit_multipart_missing_envelope_returns_400() {
        let boundary = "testboundary456";
        let body = multipart_body(
            boundary,
            &[("attachment", Some("file.txt"), Some("text/plain"), b"data")],
        );
        let app = make_router();
        let response = app.oneshot(post_multipart(boundary, body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn submit_multipart_too_many_attachments_returns_400() {
        use crate::dto::MAX_ATTACHMENTS_PER_EMAIL;
        let boundary = "testboundary789";
        let envelope = envelope_json();
        // Build MAX_ATTACHMENTS_PER_EMAIL + 1 attachment parts.
        #[allow(clippy::type_complexity)]
        let mut parts: Vec<(&str, Option<&str>, Option<&str>, &[u8])> =
            vec![("envelope", None, Some("application/json"), &envelope)];
        for _ in 0..=MAX_ATTACHMENTS_PER_EMAIL {
            parts.push(("attachment", Some("x.txt"), Some("text/plain"), b"x"));
        }
        let body = multipart_body(boundary, &parts);
        let app = make_router();
        let response = app.oneshot(post_multipart(boundary, body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn multipart_dispatch_is_case_insensitive() {
        // Content-Type with mixed case must still route to the multipart handler.
        let boundary = "caseboundary001";
        let envelope = envelope_json();
        let body = multipart_body(
            boundary,
            &[("envelope", None, Some("application/json"), &envelope)],
        );
        let request = Request::builder()
            .method("POST")
            .uri("/emails")
            .header(
                "content-type",
                format!("Multipart/Form-Data; boundary={boundary}"),
            )
            .body(body)
            .unwrap();
        let app = make_router();
        let response = app.oneshot(request).await.unwrap();
        // A well-formed multipart request with a valid envelope must succeed.
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn submit_multipart_envelope_too_large_returns_400() {
        use crate::dto::MAX_ENVELOPE_BYTES;
        let boundary = "largeenvboundary";
        // Build an envelope JSON whose total size exceeds MAX_ENVELOPE_BYTES by padding
        // the subject field with a very long string.
        let padding = "x".repeat(MAX_ENVELOPE_BYTES + 1);
        let oversized_envelope = serde_json::to_vec(&serde_json::json!({
            "sender": "a@b.c",
            "recipients": [{"kind": "to", "address": "t@x.y"}],
            "body": {"kind": "plain", "text": "hi"},
            "subject": padding
        }))
        .unwrap();
        let body = multipart_body(
            boundary,
            &[(
                "envelope",
                None,
                Some("application/json"),
                &oversized_envelope,
            )],
        );
        let request = Request::builder()
            .method("POST")
            .uri("/emails")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .unwrap();
        let app = make_router();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
