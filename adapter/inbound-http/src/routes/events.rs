use axum::Json;
use axum::extract::{Path, Query, State};
use catapulte_domain::entity::email::EmailId;
use catapulte_domain::port::event_repository::ListEventsParams;
use catapulte_domain::use_case::list_events::ListEventsUseCase;

use crate::HttpServerState;
use crate::dto::{
    DEFAULT_EVENTS_LIMIT, EventRecordDto, ListEventsQuery, ListEventsResponse, MAX_EVENTS_LIMIT,
};
use crate::error::AppError;

/// # Errors
///
/// Returns `AppError::InvalidEmailId` when the `email_id` query param is not a valid UUID.
/// Returns `AppError::ListEvents` when the use case fails.
#[tracing::instrument(skip_all)]
pub async fn list_events<S: HttpServerState>(
    State(state): State<S>,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<ListEventsResponse>, AppError> {
    let email_id = match query.email_id.as_deref() {
        Some(raw) => Some(EmailId::from(
            uuid::Uuid::parse_str(raw).map_err(|_| AppError::InvalidEmailId)?,
        )),
        None => None,
    };
    let limit = query
        .limit
        .unwrap_or(DEFAULT_EVENTS_LIMIT)
        .min(MAX_EVENTS_LIMIT);
    let offset = query.offset.unwrap_or(0);
    let params = ListEventsParams {
        email_id,
        event_type: query.event_type,
        after_ms: query.after_ms,
        before_ms: query.before_ms,
        limit,
        offset,
    };
    let events = state
        .list_events()
        .execute(params)
        .await?
        .into_iter()
        .map(EventRecordDto::from)
        .collect();
    Ok(Json(ListEventsResponse {
        events,
        limit,
        offset,
    }))
}

/// # Errors
///
/// Returns `AppError::InvalidEmailId` when the path segment is not a valid UUID.
/// Returns `AppError::ListEvents` when the use case fails.
#[tracing::instrument(skip_all, fields(email_id = %email_id))]
pub async fn list_events_for_email<S: HttpServerState>(
    State(state): State<S>,
    Path(email_id): Path<String>,
    Query(query): Query<ListEventsQuery>,
) -> Result<Json<ListEventsResponse>, AppError> {
    let uuid = uuid::Uuid::parse_str(&email_id).map_err(|_| AppError::InvalidEmailId)?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_EVENTS_LIMIT)
        .min(MAX_EVENTS_LIMIT);
    let offset = query.offset.unwrap_or(0);
    let params = ListEventsParams {
        email_id: Some(EmailId::from(uuid)),
        event_type: query.event_type,
        after_ms: query.after_ms,
        before_ms: query.before_ms,
        limit,
        offset,
    };
    let events = state
        .list_events()
        .execute(params)
        .await?
        .into_iter()
        .map(EventRecordDto::from)
        .collect();
    Ok(Json(ListEventsResponse {
        events,
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
    use catapulte_domain::port::email_repository::{EmailRecord, ListEmailsParams};
    use catapulte_domain::port::event_repository::{
        EventRecord, EventRepositoryError, ListEventsParams,
    };
    use catapulte_domain::use_case::list_emails::{ListEmailsError, ListEmailsUseCase};
    use catapulte_domain::use_case::list_events::{ListEventsError, ListEventsUseCase};
    use catapulte_domain::use_case::list_senders::{
        ListSendersError, ListSendersUseCase, SenderUsage,
    };
    use catapulte_domain::use_case::submit_email::{
        SubmitEmailError, SubmitEmailUseCase, SubmitParams,
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::HttpServerState;
    use crate::dto::{DEFAULT_EVENTS_LIMIT, MAX_EVENTS_LIMIT};
    use crate::router;

    struct NoopListSenders;

    #[allow(async_fn_in_trait)]
    impl ListSendersUseCase for NoopListSenders {
        async fn execute(&self) -> Result<Vec<SenderUsage>, ListSendersError> {
            Ok(vec![])
        }
    }

    struct NoopListEmails;

    #[allow(async_fn_in_trait)]
    impl ListEmailsUseCase for NoopListEmails {
        async fn execute(
            &self,
            _params: ListEmailsParams,
        ) -> Result<Vec<EmailRecord>, ListEmailsError> {
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
    struct FakeListEvents {
        captured_params: Arc<Mutex<Option<ListEventsParams>>>,
        result: Arc<Vec<EventRecord>>,
    }

    impl FakeListEvents {
        fn new() -> Self {
            Self {
                captured_params: Arc::new(Mutex::new(None)),
                result: Arc::new(vec![]),
            }
        }

        fn with_records(records: Vec<EventRecord>) -> Self {
            Self {
                captured_params: Arc::new(Mutex::new(None)),
                result: Arc::new(records),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl ListEventsUseCase for FakeListEvents {
        async fn execute(
            &self,
            params: ListEventsParams,
        ) -> Result<Vec<EventRecord>, ListEventsError> {
            *self.captured_params.lock().unwrap() = Some(params);
            Ok((*self.result).clone())
        }
    }

    struct FailingListEvents;

    #[allow(async_fn_in_trait)]
    impl ListEventsUseCase for FailingListEvents {
        async fn execute(
            &self,
            _params: ListEventsParams,
        ) -> Result<Vec<EventRecord>, ListEventsError> {
            Err(ListEventsError::Repository(EventRepositoryError::Storage {
                source: anyhow::anyhow!("db down"),
            }))
        }
    }

    #[derive(Clone)]
    struct TestState {
        submit: Arc<FakeSubmit>,
        list_events: Arc<FakeListEvents>,
    }

    impl HttpServerState for TestState {
        fn submit_email(&self) -> &impl SubmitEmailUseCase {
            self.submit.as_ref()
        }

        fn list_emails(&self) -> &impl ListEmailsUseCase {
            &NoopListEmails
        }

        fn list_events(&self) -> &impl ListEventsUseCase {
            self.list_events.as_ref()
        }

        fn list_senders(&self) -> &impl ListSendersUseCase {
            &NoopListSenders
        }
    }

    #[derive(Clone)]
    struct FailingRepoState {
        submit: Arc<FakeSubmit>,
    }

    impl HttpServerState for FailingRepoState {
        fn submit_email(&self) -> &impl SubmitEmailUseCase {
            self.submit.as_ref()
        }

        fn list_emails(&self) -> &impl ListEmailsUseCase {
            &NoopListEmails
        }

        fn list_events(&self) -> &impl ListEventsUseCase {
            &FailingListEvents
        }

        fn list_senders(&self) -> &impl ListSendersUseCase {
            &NoopListSenders
        }
    }

    fn valid_email_id() -> String {
        uuid::Uuid::now_v7().to_string()
    }

    fn get_events(email_id: &str, query: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(format!("/emails/{email_id}/events{query}"))
            .body(Body::empty())
            .unwrap()
    }

    fn get_all_events(query: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(format!("/events{query}"))
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn list_events_returns_200_with_events_array() {
        let email_id = EmailId::default();
        let record = EventRecord {
            id: uuid::Uuid::now_v7(),
            email_id,
            event_type: "queued".to_owned(),
            payload: None,
            sender_name: None,
            created_at_ms: 1000,
        };
        let list_events = Arc::new(FakeListEvents::with_records(vec![record]));
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_events,
        });
        let response = app
            .oneshot(get_events(&email_id.as_uuid().to_string(), ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["events"].as_array().is_some());
        assert_eq!(json["events"].as_array().unwrap().len(), 1);
        assert_eq!(json["events"][0]["event_type"], "queued");
    }

    #[tokio::test]
    async fn list_events_with_invalid_uuid_returns_400() {
        let list_events = Arc::new(FakeListEvents::new());
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_events,
        });
        let response = app.oneshot(get_events("not-a-uuid", "")).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_events_caps_limit_at_max() {
        let list_events = Arc::new(FakeListEvents::new());
        let captured = list_events.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_events,
        });
        app.oneshot(get_events(&valid_email_id(), "?limit=500"))
            .await
            .unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().limit, MAX_EVENTS_LIMIT);
    }

    #[tokio::test]
    async fn list_events_applies_default_limit() {
        let list_events = Arc::new(FakeListEvents::new());
        let captured = list_events.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_events,
        });
        app.oneshot(get_events(&valid_email_id(), ""))
            .await
            .unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().limit, DEFAULT_EVENTS_LIMIT);
    }

    #[tokio::test]
    async fn list_events_forwards_event_type_filter() {
        let list_events = Arc::new(FakeListEvents::new());
        let captured = list_events.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_events,
        });
        app.oneshot(get_events(&valid_email_id(), "?event_type=sent"))
            .await
            .unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().event_type.as_deref(), Some("sent"));
    }

    #[tokio::test]
    async fn list_events_500_when_repository_errors() {
        let app = router(FailingRepoState {
            submit: Arc::new(FakeSubmit),
        });
        let response = app
            .oneshot(get_events(&valid_email_id(), ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn list_events_without_email_id_passes_none_to_repo() {
        let list_events = Arc::new(FakeListEvents::new());
        let captured = list_events.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_events,
        });
        app.oneshot(get_all_events("")).await.unwrap();
        let params = captured.lock().unwrap();
        assert!(params.as_ref().unwrap().email_id.is_none());
    }

    #[tokio::test]
    async fn list_events_with_email_id_param_forwards_to_repo() {
        let uuid = uuid::Uuid::now_v7();
        let list_events = Arc::new(FakeListEvents::new());
        let captured = list_events.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_events,
        });
        app.oneshot(get_all_events(&format!("?email_id={uuid}")))
            .await
            .unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().email_id, Some(EmailId::from(uuid)));
    }

    #[tokio::test]
    async fn list_events_with_invalid_email_id_query_returns_400() {
        let list_events = Arc::new(FakeListEvents::new());
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_events,
        });
        let response = app
            .oneshot(get_all_events("?email_id=not-a-uuid"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_events_global_applies_default_limit() {
        let list_events = Arc::new(FakeListEvents::new());
        let captured = list_events.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_events,
        });
        app.oneshot(get_all_events("")).await.unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().limit, DEFAULT_EVENTS_LIMIT);
    }

    #[tokio::test]
    async fn list_events_global_caps_limit_at_max() {
        let list_events = Arc::new(FakeListEvents::new());
        let captured = list_events.captured_params.clone();
        let app = router(TestState {
            submit: Arc::new(FakeSubmit),
            list_events,
        });
        app.oneshot(get_all_events("?limit=999")).await.unwrap();
        let params = captured.lock().unwrap();
        assert_eq!(params.as_ref().unwrap().limit, MAX_EVENTS_LIMIT);
    }
}
