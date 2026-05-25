use axum::Json;
use axum::extract::State;
use catapulte_domain::use_case::list_senders::ListSendersUseCase;

use crate::HttpServerState;
use crate::dto::{ListSendersResponse, SenderDto, SenderQuotaDto};
use crate::error::AppError;

/// # Errors
///
/// Returns `AppError::ListSenders` when the sender usage query fails.
#[tracing::instrument(skip_all)]
pub async fn list_senders<S: HttpServerState>(
    State(state): State<S>,
) -> Result<Json<ListSendersResponse>, AppError> {
    let usage_list = state.list_senders().execute().await?;

    let senders = usage_list
        .into_iter()
        .map(|u| {
            let quota_dto = u.config.quota.as_ref().map(|q| SenderQuotaDto {
                count: q.count,
                range: q.range.to_string(),
            });
            SenderDto {
                name: u.config.name.as_str().to_owned(),
                sent_in_range: u.sent_in_range,
                failed_in_range: u.failed_in_range,
                quota: quota_dto,
            }
        })
        .collect();

    Ok(Json(ListSendersResponse { senders }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use catapulte_domain::entity::email::EmailId;
    use catapulte_domain::entity::envelope::Envelope;
    use catapulte_domain::entity::sender::{QuotaRange, SenderConfig, SenderName, SenderQuota};
    use catapulte_domain::port::email_repository::{
        EmailRecord, EmailRepository, EmailRepositoryError, ListEmailsParams, SaveResult,
    };
    use catapulte_domain::port::event_repository::{
        EventRecord, EventRepository, EventRepositoryError, ListEventsParams,
    };
    use catapulte_domain::use_case::list_senders::{
        ListSendersError, ListSendersUseCase, SenderUsage,
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
    struct FakeEmailRepository;

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
            _params: ListEmailsParams,
        ) -> Result<Vec<EmailRecord>, EmailRepositoryError> {
            Ok(vec![])
        }
    }

    #[derive(Clone)]
    struct FakeListSenders {
        result: Arc<Vec<SenderUsage>>,
    }

    impl FakeListSenders {
        fn empty() -> Self {
            Self {
                result: Arc::new(vec![]),
            }
        }

        fn with_usage(usage: Vec<SenderUsage>) -> Self {
            Self {
                result: Arc::new(usage),
            }
        }
    }

    impl ListSendersUseCase for FakeListSenders {
        async fn execute(&self) -> Result<Vec<SenderUsage>, ListSendersError> {
            Ok((*self.result).clone())
        }
    }

    #[derive(Clone)]
    struct FailingListSenders;

    impl ListSendersUseCase for FailingListSenders {
        async fn execute(&self) -> Result<Vec<SenderUsage>, ListSendersError> {
            Err(ListSendersError::Usage {
                source: catapulte_domain::port::sender_usage::SenderUsageError::Storage {
                    source: anyhow::anyhow!("db down"),
                },
            })
        }
    }

    #[derive(Clone)]
    struct TestState {
        list_senders: Arc<FakeListSenders>,
    }

    impl HttpServerState for TestState {
        fn submit_email(&self) -> &impl SubmitEmailUseCase {
            &FakeSubmit
        }

        fn event_repository(&self) -> &impl EventRepository {
            &FakeEventRepository
        }

        fn email_repository(&self) -> &impl EmailRepository {
            &FakeEmailRepository
        }

        fn list_senders(&self) -> &impl ListSendersUseCase {
            self.list_senders.as_ref()
        }
    }

    #[derive(Clone)]
    struct FailingState;

    impl HttpServerState for FailingState {
        fn submit_email(&self) -> &impl SubmitEmailUseCase {
            &FakeSubmit
        }

        fn event_repository(&self) -> &impl EventRepository {
            &FakeEventRepository
        }

        fn email_repository(&self) -> &impl EmailRepository {
            &FakeEmailRepository
        }

        fn list_senders(&self) -> &impl ListSendersUseCase {
            &FailingListSenders
        }
    }

    fn get_senders() -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri("/senders")
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn list_senders_returns_200_with_empty_array_when_no_senders_configured() {
        let state = TestState {
            list_senders: Arc::new(FakeListSenders::empty()),
        };
        let app = router(state);
        let response = app.oneshot(get_senders()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["senders"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_senders_returns_sender_with_stats() {
        let name = SenderName::new("primary");
        let usage = vec![SenderUsage {
            config: SenderConfig {
                name: name.clone(),
                quota: None,
            },
            sent_in_range: 42,
            failed_in_range: 3,
        }];
        let state = TestState {
            list_senders: Arc::new(FakeListSenders::with_usage(usage)),
        };
        let app = router(state);
        let response = app.oneshot(get_senders()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let senders = json["senders"].as_array().unwrap();
        assert_eq!(senders.len(), 1);
        assert_eq!(senders[0]["name"], "primary");
        assert_eq!(senders[0]["sent_in_range"], 42);
        assert_eq!(senders[0]["failed_in_range"], 3);
        assert!(senders[0]["quota"].is_null());
    }

    #[tokio::test]
    async fn list_senders_includes_quota_when_configured() {
        let name = SenderName::new("bulk");
        let usage = vec![SenderUsage {
            config: SenderConfig {
                name: name.clone(),
                quota: Some(SenderQuota {
                    count: 1000,
                    range: QuotaRange::Daily,
                }),
            },
            sent_in_range: 0,
            failed_in_range: 0,
        }];
        let state = TestState {
            list_senders: Arc::new(FakeListSenders::with_usage(usage)),
        };
        let app = router(state);
        let response = app.oneshot(get_senders()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let senders = json["senders"].as_array().unwrap();
        assert_eq!(senders.len(), 1);
        assert_eq!(senders[0]["quota"]["count"], 1000);
        assert_eq!(senders[0]["quota"]["range"], "daily");
    }

    #[tokio::test]
    async fn list_senders_returns_500_when_repository_fails() {
        let app = router(FailingState);
        let response = app.oneshot(get_senders()).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
