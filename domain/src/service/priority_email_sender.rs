use std::collections::HashMap;
use std::time::SystemTime;

use crate::entity::sender::{QuotaRange, SenderName, SenderQuota};
use crate::port::email_sender::{EmailSender, OutboundEmail, SendError};
use crate::port::email_transport::EmailTransport;
use crate::port::sender_repository::{SenderRepository, SenderRepositoryError, SenderStats};

pub struct NoopSenderRepository;

impl SenderRepository for NoopSenderRepository {
    async fn get_stats(
        &self,
        names: &[SenderName],
        _since_ms: i64,
    ) -> Result<Vec<SenderStats>, SenderRepositoryError> {
        Ok(names
            .iter()
            .map(|n| SenderStats {
                name: n.clone(),
                sent_in_range: 0,
                failed_in_range: 0,
            })
            .collect())
    }
}

pub struct SenderRoute<T> {
    pub name: SenderName,
    pub priority: u8,
    pub quota: Option<SenderQuota>,
    pub transport: T,
}

pub struct PriorityEmailSender<T, R = NoopSenderRepository> {
    routes: Vec<SenderRoute<T>>,
    repo: R,
}

impl<T, R> PriorityEmailSender<T, R> {
    pub fn new(mut routes: Vec<SenderRoute<T>>, repo: R) -> Self {
        routes.sort_by_key(|r| r.priority);
        Self { routes, repo }
    }
}

fn quota_range_ms(range: &QuotaRange) -> i64 {
    match range {
        QuotaRange::Hourly => 3_600_000,
        QuotaRange::Daily => 86_400_000,
        QuotaRange::Weekly => 604_800_000,
        QuotaRange::Monthly => 2_592_000_000,
    }
}

impl<T, R> EmailSender for PriorityEmailSender<T, R>
where
    T: EmailTransport,
    R: SenderRepository,
{
    async fn send(&self, email: OutboundEmail) -> Result<SenderName, SendError> {
        let now_ms = i64::try_from(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system time is before Unix epoch")
                .as_millis(),
        )
        .unwrap_or(i64::MAX);

        let mut since_ms_to_names: HashMap<i64, Vec<SenderName>> = HashMap::new();
        for route in &self.routes {
            if let Some(quota) = &route.quota {
                let since_ms = now_ms - quota_range_ms(&quota.range);
                since_ms_to_names
                    .entry(since_ms)
                    .or_default()
                    .push(route.name.clone());
            }
        }

        let mut sent_map: HashMap<SenderName, u64> = HashMap::new();
        for (since_ms, names) in &since_ms_to_names {
            if let Ok(stats) = self.repo.get_stats(names, *since_ms).await {
                for stat in stats {
                    sent_map.insert(stat.name, stat.sent_in_range);
                }
            }
        }

        let mut last_err: Option<SendError> = None;

        for route in &self.routes {
            let over_quota = route
                .quota
                .as_ref()
                .is_some_and(|q| sent_map.get(&route.name).copied().unwrap_or(0) >= q.count);
            if over_quota {
                continue;
            }
            match route.transport.deliver(&email).await {
                Ok(()) => return Ok(route.name.clone()),
                Err(err) => {
                    last_err = Some(SendError::Send {
                        sender_name: route.name.clone(),
                        source: err,
                    });
                }
            }
        }

        if let Some(err) = last_err {
            return Err(err);
        }

        for route in &self.routes {
            match route.transport.deliver(&email).await {
                Ok(()) => return Ok(route.name.clone()),
                Err(err) => {
                    last_err = Some(SendError::Send {
                        sender_name: route.name.clone(),
                        source: err,
                    });
                }
            }
        }

        Err(last_err.expect("routes must not be empty"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{NoopSenderRepository, PriorityEmailSender, SenderRoute};
    use crate::entity::body::{Plain, RenderedBody};
    use crate::entity::sender::{QuotaRange, SenderName, SenderQuota};
    use crate::port::email_sender::{EmailSender, OutboundEmail};
    use crate::port::email_transport::EmailTransport;
    use crate::port::sender_repository::{SenderRepository, SenderRepositoryError, SenderStats};

    enum FakeTransport {
        Ok,
        Fail,
    }

    impl EmailTransport for FakeTransport {
        async fn deliver<'a>(&'a self, _email: &'a OutboundEmail) -> Result<(), anyhow::Error> {
            match self {
                Self::Ok => Ok(()),
                Self::Fail => Err(anyhow::anyhow!("simulated failure")),
            }
        }
    }

    fn ok_route(
        name: &str,
        priority: u8,
        quota: Option<SenderQuota>,
    ) -> SenderRoute<FakeTransport> {
        SenderRoute {
            name: SenderName::new(name),
            priority,
            quota,
            transport: FakeTransport::Ok,
        }
    }

    fn fail_route(
        name: &str,
        priority: u8,
        quota: Option<SenderQuota>,
    ) -> SenderRoute<FakeTransport> {
        SenderRoute {
            name: SenderName::new(name),
            priority,
            quota,
            transport: FakeTransport::Fail,
        }
    }

    fn make_email() -> OutboundEmail {
        OutboundEmail {
            sender: "test@example.com".into(),
            subject: None,
            recipients: vec![],
            body: RenderedBody::new(
                Plain::try_new(None, Some("<p>hi</p>".into())).expect("valid body"),
            ),
        }
    }

    struct FakeSenderRepository {
        stats: HashMap<String, u64>,
    }

    impl SenderRepository for FakeSenderRepository {
        async fn get_stats(
            &self,
            names: &[SenderName],
            _since_ms: i64,
        ) -> Result<Vec<SenderStats>, SenderRepositoryError> {
            Ok(names
                .iter()
                .map(|n| SenderStats {
                    name: n.clone(),
                    sent_in_range: self.stats.get(n.as_str()).copied().unwrap_or(0),
                    failed_in_range: 0,
                })
                .collect())
        }
    }

    struct ErrorSenderRepository;

    impl SenderRepository for ErrorSenderRepository {
        async fn get_stats(
            &self,
            _names: &[SenderName],
            _since_ms: i64,
        ) -> Result<Vec<SenderStats>, SenderRepositoryError> {
            Err(SenderRepositoryError::Storage {
                source: anyhow::anyhow!("db error"),
            })
        }
    }

    #[tokio::test]
    async fn first_sender_fails_second_succeeds() {
        let sender = PriorityEmailSender::new(
            vec![fail_route("first", 0, None), ok_route("second", 1, None)],
            NoopSenderRepository,
        );
        let result = sender.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "second");
    }

    #[tokio::test]
    async fn all_senders_fail_returns_last_error() {
        let sender = PriorityEmailSender::new(
            vec![fail_route("first", 0, None), fail_route("second", 1, None)],
            NoopSenderRepository,
        );
        let result = sender.send(make_email()).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().sender_name().as_str(), "second");
    }

    #[tokio::test]
    async fn first_sender_succeeds_returns_immediately() {
        let sender = PriorityEmailSender::new(
            vec![ok_route("first", 0, None), ok_route("second", 1, None)],
            NoopSenderRepository,
        );
        let result = sender.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "first");
    }

    #[tokio::test]
    async fn over_quota_sender_is_skipped_for_eligible_one() {
        let stats: HashMap<String, u64> = [("primary".into(), 1u64)].into_iter().collect();
        let sender = PriorityEmailSender::new(
            vec![
                ok_route(
                    "primary",
                    0,
                    Some(SenderQuota {
                        count: 1,
                        range: QuotaRange::Daily,
                    }),
                ),
                ok_route("backup", 1, None),
            ],
            FakeSenderRepository { stats },
        );
        let result = sender.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "backup");
    }

    #[tokio::test]
    async fn all_over_quota_still_delivers_via_fallback() {
        let stats: HashMap<String, u64> = [("primary".into(), 5u64), ("backup".into(), 3u64)]
            .into_iter()
            .collect();
        let sender = PriorityEmailSender::new(
            vec![
                ok_route(
                    "primary",
                    0,
                    Some(SenderQuota {
                        count: 5,
                        range: QuotaRange::Daily,
                    }),
                ),
                ok_route(
                    "backup",
                    1,
                    Some(SenderQuota {
                        count: 3,
                        range: QuotaRange::Weekly,
                    }),
                ),
            ],
            FakeSenderRepository { stats },
        );
        let result = sender.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "primary");
    }

    #[tokio::test]
    async fn under_quota_sender_is_used_normally() {
        let stats: HashMap<String, u64> = [("primary".into(), 5u64)].into_iter().collect();
        let sender = PriorityEmailSender::new(
            vec![ok_route(
                "primary",
                0,
                Some(SenderQuota {
                    count: 10,
                    range: QuotaRange::Daily,
                }),
            )],
            FakeSenderRepository { stats },
        );
        let result = sender.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "primary");
    }

    #[tokio::test]
    async fn repo_error_treats_all_as_eligible() {
        let sender = PriorityEmailSender::new(
            vec![ok_route(
                "primary",
                0,
                Some(SenderQuota {
                    count: 1,
                    range: QuotaRange::Daily,
                }),
            )],
            ErrorSenderRepository,
        );
        let result = sender.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "primary");
    }
}
