use std::collections::HashMap;

use axum::Json;
use axum::extract::State;
use catapulte_domain::entity::sender::{QuotaRange, SenderName};
use catapulte_domain::port::sender_repository::{SenderRepository, SenderStats};

use crate::HttpServerState;
use crate::dto::{ListSendersResponse, SenderDto, SenderQuotaDto};
use crate::error::AppError;

fn since_ms_for_range(range: &QuotaRange) -> i64 {
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX);
    match range {
        QuotaRange::Hourly => now_ms - 3_600_000,
        QuotaRange::Daily => now_ms - 86_400_000,
        QuotaRange::Weekly => now_ms - 604_800_000,
        QuotaRange::Monthly => now_ms - 2_592_000_000,
    }
}

/// # Errors
///
/// Returns `AppError::ListSenders` when the sender repository query fails.
#[tracing::instrument(skip_all)]
pub async fn list_senders<S: HttpServerState>(
    State(state): State<S>,
) -> Result<Json<ListSendersResponse>, AppError> {
    let configured = state.configured_senders();

    if configured.is_empty() {
        return Ok(Json(ListSendersResponse { senders: vec![] }));
    }

    // Group senders by their since_ms value (senders with no quota use since_ms=0).
    // This way we make one DB call per distinct time window.
    let mut groups: HashMap<i64, Vec<SenderName>> = HashMap::new();
    for (name, quota) in configured {
        let since_ms = quota.as_ref().map_or(0, |q| since_ms_for_range(&q.range));
        groups.entry(since_ms).or_default().push(name.clone());
    }

    // Collect all stats, keyed by sender name.
    let mut stats_map: HashMap<SenderName, SenderStats> = HashMap::new();
    let repo = state.sender_repository();
    for (since_ms, names) in &groups {
        let results = repo.get_stats(names, *since_ms).await?;
        for s in results {
            stats_map.insert(s.name.clone(), s);
        }
    }

    let senders = configured
        .iter()
        .map(|(name, quota)| {
            let sender_stats = stats_map.get(name);
            let sent_in_range = sender_stats.map_or(0, |s| s.sent_in_range);
            let failed_in_range = sender_stats.map_or(0, |s| s.failed_in_range);
            let quota_dto = quota.as_ref().map(|q| SenderQuotaDto {
                count: q.count,
                range: q.range.to_string(),
            });
            SenderDto {
                name: name.as_str().to_owned(),
                sent_in_range,
                failed_in_range,
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
    use catapulte_domain::entity::sender::{QuotaRange, SenderName, SenderQuota};
    use catapulte_domain::port::email_repository::{
        EmailRecord, EmailRepository, EmailRepositoryError, ListEmailsParams, SaveResult,
    };
    use catapulte_domain::port::event_repository::{
        EventRecord, EventRepository, EventRepositoryError, ListEventsParams,
    };
    use catapulte_domain::port::sender_repository::{
        SenderRepository, SenderRepositoryError, SenderStats,
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
    struct FakeSenderRepository {
        stats: Arc<Vec<SenderStats>>,
    }

    impl FakeSenderRepository {
        fn empty() -> Self {
            Self {
                stats: Arc::new(vec![]),
            }
        }

        fn with_stats(stats: Vec<SenderStats>) -> Self {
            Self {
                stats: Arc::new(stats),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    impl SenderRepository for FakeSenderRepository {
        async fn get_stats(
            &self,
            _names: &[SenderName],
            _since_ms: i64,
        ) -> Result<Vec<SenderStats>, SenderRepositoryError> {
            Ok((*self.stats).clone())
        }
    }

    struct FailingSenderRepository;

    #[allow(async_fn_in_trait)]
    impl SenderRepository for FailingSenderRepository {
        async fn get_stats(
            &self,
            _names: &[SenderName],
            _since_ms: i64,
        ) -> Result<Vec<SenderStats>, SenderRepositoryError> {
            Err(SenderRepositoryError::Storage {
                source: anyhow::anyhow!("db down"),
            })
        }
    }

    #[derive(Clone)]
    struct TestState {
        sender_repo: Arc<FakeSenderRepository>,
        configured: Arc<Vec<(SenderName, Option<SenderQuota>)>>,
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

        fn sender_repository(&self) -> &impl SenderRepository {
            self.sender_repo.as_ref()
        }

        fn configured_senders(&self) -> &[(SenderName, Option<SenderQuota>)] {
            &self.configured
        }
    }

    #[derive(Clone)]
    struct FailingState {
        configured: Arc<Vec<(SenderName, Option<SenderQuota>)>>,
    }

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

        fn sender_repository(&self) -> &impl SenderRepository {
            &FailingSenderRepository
        }

        fn configured_senders(&self) -> &[(SenderName, Option<SenderQuota>)] {
            &self.configured
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
            sender_repo: Arc::new(FakeSenderRepository::empty()),
            configured: Arc::new(vec![]),
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
        let stats = vec![SenderStats {
            name: name.clone(),
            sent_in_range: 42,
            failed_in_range: 3,
        }];
        let state = TestState {
            sender_repo: Arc::new(FakeSenderRepository::with_stats(stats)),
            configured: Arc::new(vec![(name, None)]),
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
        let state = TestState {
            sender_repo: Arc::new(FakeSenderRepository::empty()),
            configured: Arc::new(vec![(
                name,
                Some(SenderQuota {
                    count: 1000,
                    range: QuotaRange::Daily,
                }),
            )]),
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
        let name = SenderName::new("default");
        let state = FailingState {
            configured: Arc::new(vec![(name, None)]),
        };
        let app = router(state);
        let response = app.oneshot(get_senders()).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
