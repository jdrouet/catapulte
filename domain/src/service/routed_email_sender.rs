use std::collections::HashMap;

use thiserror::Error;

use crate::entity::sender::{SenderName, SenderQuota};
use crate::port::clock::{Clock, SystemClock};
use crate::port::email_sender::{EmailSender, OutboundEmail, SendError};
use crate::port::email_transport::{EmailTransport, TransportErrorKind};
use crate::port::sender_usage::{SenderStats, SenderUsage};

#[derive(Debug, Error)]
pub enum RoutedEmailSenderError {
    #[error("routes must not be empty")]
    EmptyRoutes,
}

pub struct NoopSenderUsage;

impl SenderUsage for NoopSenderUsage {
    async fn get_stats(
        &self,
        names: &[SenderName],
        _since_ms: i64,
    ) -> Result<Vec<SenderStats>, crate::port::sender_usage::SenderUsageError> {
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

fn normalize_domain(s: &str) -> String {
    s.trim_end_matches('.').to_ascii_lowercase()
}

pub struct SenderRoute<T> {
    pub name: SenderName,
    pub priority: u8,
    pub quota: Option<SenderQuota>,
    pub match_sender_domain: Option<String>,
    pub transport: T,
}

pub struct RoutedEmailSender<T, U = NoopSenderUsage, C = SystemClock> {
    routes: Vec<SenderRoute<T>>,
    usage: U,
    clock: C,
}

impl<T, U, C> RoutedEmailSender<T, U, C> {
    /// # Errors
    ///
    /// Returns `RoutedEmailSenderError::EmptyRoutes` if `routes` is empty.
    pub fn new(
        mut routes: Vec<SenderRoute<T>>,
        usage: U,
        clock: C,
    ) -> Result<Self, RoutedEmailSenderError> {
        if routes.is_empty() {
            return Err(RoutedEmailSenderError::EmptyRoutes);
        }
        for route in &mut routes {
            if let Some(d) = &route.match_sender_domain {
                route.match_sender_domain = Some(normalize_domain(d));
            }
        }
        routes.sort_by_key(|r| r.priority);
        Ok(Self {
            routes,
            usage,
            clock,
        })
    }
}

impl<T, U, C> EmailSender for RoutedEmailSender<T, U, C>
where
    T: EmailTransport,
    U: SenderUsage,
    C: Clock,
{
    /// Sends `email` through the highest-priority eligible sender.
    ///
    /// Candidates are ordered: domain-matched routes first (by priority), then
    /// catch-all routes (by priority). First pass: senders whose quota is
    /// exhausted are skipped. If the usage port is unavailable the error is
    /// logged and every sender is treated as eligible (fail-open). Second pass:
    /// if every sender was over-quota in the first pass, the quota check is
    /// bypassed so delivery still succeeds.
    ///
    /// # Errors
    ///
    /// Returns `SendError::NoMatchingRoute` when no route matches the sender
    /// domain and there are no catch-all routes. Returns `SendError::Send` when
    /// all attempted senders fail to deliver.
    #[allow(clippy::too_many_lines)]
    async fn send(&self, email: OutboundEmail) -> Result<SenderName, SendError> {
        let sender_domain = crate::entity::email::bare_address(&email.sender)
            .rsplit_once('@')
            .map(|(_, domain)| normalize_domain(domain))
            .unwrap_or_default();

        // Build ordered candidate list: domain-matched first, then catch-alls.
        // self.routes is already sorted by priority from construction.
        // match_sender_domain values are pre-normalized in RoutedEmailSender::new.
        let candidates: Vec<&SenderRoute<T>> = self
            .routes
            .iter()
            .filter(|r| {
                r.match_sender_domain
                    .as_ref()
                    .is_some_and(|d| d == &sender_domain)
            })
            .chain(
                self.routes
                    .iter()
                    .filter(|r| r.match_sender_domain.is_none()),
            )
            .collect();

        if candidates.is_empty() {
            return Err(SendError::NoMatchingRoute { sender_domain });
        }

        let now_ms = self.clock.now_ms();

        let mut since_ms_to_names: HashMap<i64, Vec<SenderName>> = HashMap::new();
        for route in &candidates {
            if let Some(quota) = &route.quota {
                let since_ms = quota.range.since_ms(now_ms);
                since_ms_to_names
                    .entry(since_ms)
                    .or_default()
                    .push(route.name.clone());
            }
        }

        let mut sent_map: HashMap<SenderName, u64> = HashMap::new();
        for (since_ms, names) in &since_ms_to_names {
            match self.usage.get_stats(names, *since_ms).await {
                Ok(stats) => {
                    for stat in stats {
                        sent_map.insert(stat.name, stat.sent_in_range);
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "sender usage port unavailable; quota checks skipped, all senders treated as eligible"
                    );
                }
            }
        }

        let eligible: Vec<&SenderRoute<T>> = candidates
            .iter()
            .copied()
            .filter(|route| {
                route
                    .quota
                    .as_ref()
                    .is_none_or(|q| sent_map.get(&route.name).copied().unwrap_or(0) < q.count)
            })
            .collect();

        if let Some(result) = Self::try_routes(&eligible, &email).await {
            return result;
        }

        // Every route was over quota: bypass the quota check so delivery still succeeds.
        Self::try_routes(&candidates, &email)
            .await
            .unwrap_or_else(|| unreachable!("non-empty candidates guarantee an attempt"))
    }
}

impl<T, U, C> RoutedEmailSender<T, U, C>
where
    T: EmailTransport,
{
    /// Tries `routes` in order. A message-level permanent failure stops immediately, since
    /// no other route can deliver it; any other failure tries the next route and, once routes
    /// are exhausted, stays retryable. Returns `None` when `routes` is empty.
    async fn try_routes(
        routes: &[&SenderRoute<T>],
        email: &OutboundEmail,
    ) -> Option<Result<SenderName, SendError>> {
        use tracing::Instrument as _;

        let mut last: Option<(SenderName, anyhow::Error)> = None;
        for route in routes {
            let deliver_span = tracing::info_span!(
                "smtp.deliver",
                sender = route.name.as_str(),
                outcome = tracing::field::Empty,
            );
            let result = route
                .transport
                .deliver(email)
                .instrument(deliver_span.clone())
                .await;
            match result {
                Ok(()) => {
                    deliver_span.record("outcome", "ok");
                    return Some(Ok(route.name.clone()));
                }
                Err(err) => {
                    deliver_span.record("outcome", "error");
                    if err.kind == TransportErrorKind::PermanentMessage {
                        return Some(Err(SendError::Send {
                            sender_name: route.name.clone(),
                            permanent: true,
                            source: err.source,
                        }));
                    }
                    last = Some((route.name.clone(), err.source));
                }
            }
        }
        last.map(|(sender_name, source)| {
            Err(SendError::Send {
                sender_name,
                permanent: false,
                source,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{NoopSenderUsage, RoutedEmailSender, SenderRoute};
    use crate::entity::body::{Plain, RenderedBody};
    use crate::entity::sender::{QuotaRange, SenderName, SenderQuota};
    use crate::port::clock::SystemClock;
    use crate::port::email_sender::{EmailSender, OutboundEmail};
    use crate::port::email_transport::{EmailTransport, TransportError, TransportErrorKind};
    use crate::port::sender_usage::{SenderStats, SenderUsage, SenderUsageError};

    enum FakeTransport {
        Ok,
        Fail,
        /// Fails with `kind`, counting calls.
        FailWith(TransportErrorKind, Arc<AtomicUsize>),
        /// Succeeds, counting calls.
        OkCounting(Arc<AtomicUsize>),
    }

    impl EmailTransport for FakeTransport {
        async fn deliver<'a>(&'a self, _email: &'a OutboundEmail) -> Result<(), TransportError> {
            let fail = |kind| TransportError {
                kind,
                source: anyhow::anyhow!("simulated failure"),
            };
            match self {
                Self::Ok => Ok(()),
                Self::Fail => Err(fail(TransportErrorKind::Transient)),
                Self::FailWith(kind, calls) => {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(fail(*kind))
                }
                Self::OkCounting(calls) => {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
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
            match_sender_domain: None,
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
            match_sender_domain: None,
            transport: FakeTransport::Fail,
        }
    }

    fn ok_route_with_domain(name: &str, priority: u8, domain: &str) -> SenderRoute<FakeTransport> {
        SenderRoute {
            name: SenderName::new(name),
            priority,
            quota: None,
            match_sender_domain: Some(domain.to_owned()),
            transport: FakeTransport::Ok,
        }
    }

    fn fail_route_with_domain(
        name: &str,
        priority: u8,
        domain: &str,
    ) -> SenderRoute<FakeTransport> {
        SenderRoute {
            name: SenderName::new(name),
            priority,
            quota: None,
            match_sender_domain: Some(domain.to_owned()),
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
            attachments: vec![],
            reply_to: vec![],
            headers: vec![],
        }
    }

    struct FakeSenderUsage {
        stats: HashMap<String, u64>,
    }

    impl SenderUsage for FakeSenderUsage {
        async fn get_stats(
            &self,
            names: &[SenderName],
            _since_ms: i64,
        ) -> Result<Vec<SenderStats>, SenderUsageError> {
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

    struct ErrorSenderUsage;

    impl SenderUsage for ErrorSenderUsage {
        async fn get_stats(
            &self,
            _names: &[SenderName],
            _since_ms: i64,
        ) -> Result<Vec<SenderStats>, SenderUsageError> {
            Err(SenderUsageError::Storage {
                source: anyhow::anyhow!("db error"),
            })
        }
    }

    #[test]
    fn new_with_empty_routes_returns_error() {
        let result = RoutedEmailSender::new(
            Vec::<SenderRoute<FakeTransport>>::new(),
            NoopSenderUsage,
            SystemClock,
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn first_sender_fails_second_succeeds() {
        let sender = RoutedEmailSender::new(
            vec![fail_route("first", 0, None), ok_route("second", 1, None)],
            NoopSenderUsage,
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "second");
    }

    #[tokio::test]
    async fn all_senders_fail_returns_last_error() {
        let sender = RoutedEmailSender::new(
            vec![fail_route("first", 0, None), fail_route("second", 1, None)],
            NoopSenderUsage,
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email()).await;
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().sender_name().unwrap().as_str(),
            "second"
        );
    }

    #[tokio::test]
    async fn first_sender_succeeds_returns_immediately() {
        let sender = RoutedEmailSender::new(
            vec![ok_route("first", 0, None), ok_route("second", 1, None)],
            NoopSenderUsage,
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "first");
    }

    #[tokio::test]
    async fn over_quota_sender_is_skipped_for_eligible_one() {
        let stats: HashMap<String, u64> = [("primary".into(), 1u64)].into_iter().collect();
        let sender = RoutedEmailSender::new(
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
            FakeSenderUsage { stats },
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "backup");
    }

    #[tokio::test]
    async fn all_over_quota_still_delivers_via_fallback() {
        let stats: HashMap<String, u64> = [("primary".into(), 5u64), ("backup".into(), 3u64)]
            .into_iter()
            .collect();
        let sender = RoutedEmailSender::new(
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
            FakeSenderUsage { stats },
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "primary");
    }

    #[tokio::test]
    async fn under_quota_sender_is_used_normally() {
        let stats: HashMap<String, u64> = [("primary".into(), 5u64)].into_iter().collect();
        let sender = RoutedEmailSender::new(
            vec![ok_route(
                "primary",
                0,
                Some(SenderQuota {
                    count: 10,
                    range: QuotaRange::Daily,
                }),
            )],
            FakeSenderUsage { stats },
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "primary");
    }

    #[tokio::test]
    async fn repo_error_treats_all_as_eligible() {
        let sender = RoutedEmailSender::new(
            vec![ok_route(
                "primary",
                0,
                Some(SenderQuota {
                    count: 1,
                    range: QuotaRange::Daily,
                }),
            )],
            ErrorSenderUsage,
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email()).await;
        assert_eq!(result.unwrap().as_str(), "primary");
    }

    fn make_email_from(sender: &str) -> OutboundEmail {
        OutboundEmail {
            sender: sender.to_owned(),
            subject: None,
            recipients: vec![],
            body: RenderedBody::new(
                Plain::try_new(None, Some("<p>hi</p>".into())).expect("valid body"),
            ),
            attachments: vec![],
            reply_to: vec![],
            headers: vec![],
        }
    }

    #[tokio::test]
    async fn domain_matched_route_picked_over_catch_all() {
        let sender = RoutedEmailSender::new(
            vec![
                ok_route_with_domain("transactional", 1, "acme.com"),
                ok_route("catchall", 2, None),
            ],
            NoopSenderUsage,
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email_from("foo@acme.com")).await;
        assert_eq!(result.unwrap().as_str(), "transactional");
    }

    #[tokio::test]
    async fn display_name_sender_matches_its_domain_route() {
        let sender = RoutedEmailSender::new(
            vec![
                ok_route_with_domain("transactional", 1, "acme.com"),
                ok_route("catchall", 2, None),
            ],
            NoopSenderUsage,
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email_from("Acme <foo@acme.com>")).await;
        assert_eq!(result.unwrap().as_str(), "transactional");
    }

    #[tokio::test]
    async fn mismatching_sender_uses_catch_all() {
        let sender = RoutedEmailSender::new(
            vec![
                ok_route_with_domain("transactional", 1, "acme.com"),
                ok_route("catchall", 2, None),
            ],
            NoopSenderUsage,
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email_from("foo@other.com")).await;
        assert_eq!(result.unwrap().as_str(), "catchall");
    }

    #[tokio::test]
    async fn domain_match_is_case_insensitive() {
        let sender = RoutedEmailSender::new(
            vec![
                ok_route_with_domain("transactional", 1, "Acme.COM"),
                ok_route("catchall", 2, None),
            ],
            NoopSenderUsage,
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email_from("foo@acme.com")).await;
        assert_eq!(result.unwrap().as_str(), "transactional");
    }

    #[tokio::test]
    async fn no_matching_or_catch_all_returns_no_matching_route() {
        use crate::port::email_sender::SendError;
        let sender = RoutedEmailSender::new(
            vec![ok_route_with_domain("transactional", 1, "acme.com")],
            NoopSenderUsage,
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email_from("foo@other.com")).await;
        assert!(
            matches!(result, Err(SendError::NoMatchingRoute { .. })),
            "expected NoMatchingRoute, got {result:?}"
        );
    }

    #[tokio::test]
    async fn matched_route_failure_falls_back_to_catch_all() {
        let sender = RoutedEmailSender::new(
            vec![
                fail_route_with_domain("transactional", 1, "acme.com"),
                ok_route("catchall", 2, None),
            ],
            NoopSenderUsage,
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email_from("foo@acme.com")).await;
        assert_eq!(result.unwrap().as_str(), "catchall");
    }

    #[tokio::test]
    async fn multiple_matched_routes_respect_priority() {
        let sender = RoutedEmailSender::new(
            vec![
                ok_route_with_domain("high", 1, "acme.com"),
                ok_route_with_domain("low", 2, "acme.com"),
            ],
            NoopSenderUsage,
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email_from("foo@acme.com")).await;
        assert_eq!(result.unwrap().as_str(), "high");
    }

    #[tokio::test]
    async fn domain_match_strips_trailing_dot() {
        let sender = RoutedEmailSender::new(
            vec![
                ok_route_with_domain("transactional", 1, "acme.com"),
                ok_route("catchall", 2, None),
            ],
            NoopSenderUsage,
            SystemClock,
        )
        .unwrap();
        let result = sender.send(make_email_from("foo@acme.com.")).await;
        assert_eq!(result.unwrap().as_str(), "transactional");
    }

    #[test]
    fn is_transient_send_is_true() {
        use crate::port::email_sender::SendError;
        let err = SendError::Send {
            sender_name: SenderName::new("x"),
            permanent: false,
            source: anyhow::anyhow!("oops"),
        };
        assert!(err.is_transient());
    }

    #[test]
    fn is_transient_permanent_send_is_false() {
        use crate::port::email_sender::SendError;
        let err = SendError::Send {
            sender_name: SenderName::new("x"),
            permanent: true,
            source: anyhow::anyhow!("550 5.1.1"),
        };
        assert!(!err.is_transient());
        assert_eq!(err.to_string(), "email send failed: 550 5.1.1");
    }

    fn counted_route(
        name: &str,
        priority: u8,
        transport: FakeTransport,
    ) -> SenderRoute<FakeTransport> {
        SenderRoute {
            name: SenderName::new(name),
            priority,
            quota: None,
            match_sender_domain: None,
            transport,
        }
    }

    fn router(routes: Vec<SenderRoute<FakeTransport>>) -> RoutedEmailSender<FakeTransport> {
        RoutedEmailSender::new(routes, NoopSenderUsage, SystemClock).unwrap()
    }

    fn send_err(
        result: Result<SenderName, crate::port::email_sender::SendError>,
    ) -> (String, bool) {
        match result.unwrap_err() {
            crate::port::email_sender::SendError::Send {
                sender_name,
                permanent,
                ..
            } => (sender_name.as_str().to_owned(), permanent),
            other @ crate::port::email_sender::SendError::NoMatchingRoute { .. } => {
                panic!("unexpected {other}")
            }
        }
    }

    #[tokio::test]
    async fn message_level_failure_does_not_try_next_route() {
        let second = Arc::new(AtomicUsize::new(0));
        let sender = router(vec![
            counted_route(
                "first",
                1,
                FakeTransport::FailWith(
                    TransportErrorKind::PermanentMessage,
                    Arc::new(AtomicUsize::new(0)),
                ),
            ),
            counted_route("second", 2, FakeTransport::OkCounting(second.clone())),
        ]);

        let (name, permanent) = send_err(sender.send(make_email()).await);

        assert_eq!((name.as_str(), permanent), ("first", true));
        assert_eq!(second.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn route_level_failure_falls_back_to_next_route() {
        let sender = router(vec![
            counted_route(
                "first",
                1,
                FakeTransport::FailWith(
                    TransportErrorKind::PermanentRoute,
                    Arc::new(AtomicUsize::new(0)),
                ),
            ),
            counted_route("second", 2, FakeTransport::Ok),
        ]);

        assert_eq!(sender.send(make_email()).await.unwrap().as_str(), "second");
    }

    #[tokio::test]
    async fn transient_failure_falls_back_and_stays_transient() {
        let sender = router(vec![
            counted_route("first", 1, FakeTransport::Fail),
            counted_route("second", 2, FakeTransport::Fail),
        ]);

        let (name, permanent) = send_err(sender.send(make_email()).await);

        assert_eq!((name.as_str(), permanent), ("second", false));
    }

    fn route_level_failing(name: &str, priority: u8) -> SenderRoute<FakeTransport> {
        counted_route(
            name,
            priority,
            FakeTransport::FailWith(
                TransportErrorKind::PermanentRoute,
                Arc::new(AtomicUsize::new(0)),
            ),
        )
    }

    #[tokio::test]
    async fn all_route_level_failures_stay_transient() {
        let sender = router(vec![
            route_level_failing("first", 1),
            route_level_failing("second", 2),
        ]);

        let (_, permanent) = send_err(sender.send(make_email()).await);

        assert!(!permanent);
    }

    #[tokio::test]
    async fn size_rejection_on_first_route_falls_back_to_second() {
        // A 5.3.4 size rejection is classified as route-level, so the next route is tried.
        let second = Arc::new(AtomicUsize::new(0));
        let sender = router(vec![
            route_level_failing("first", 1),
            counted_route("second", 2, FakeTransport::OkCounting(second.clone())),
        ]);

        assert_eq!(sender.send(make_email()).await.unwrap().as_str(), "second");
        assert_eq!(second.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn route_level_failure_with_over_quota_backup_stays_transient() {
        let backup = SenderRoute {
            quota: Some(SenderQuota {
                count: 1,
                range: QuotaRange::Daily,
            }),
            ..counted_route("backup", 2, FakeTransport::Ok)
        };
        let usage = FakeSenderUsage {
            stats: HashMap::from([("backup".to_owned(), 5)]),
        };
        let sender = RoutedEmailSender::new(
            vec![route_level_failing("first", 1), backup],
            usage,
            SystemClock,
        )
        .unwrap();

        let (name, permanent) = send_err(sender.send(make_email()).await);

        assert_eq!((name.as_str(), permanent), ("first", false));
    }

    #[tokio::test]
    async fn mixed_permanent_and_transient_failures_stay_transient() {
        let sender = router(vec![
            counted_route(
                "first",
                1,
                FakeTransport::FailWith(
                    TransportErrorKind::PermanentRoute,
                    Arc::new(AtomicUsize::new(0)),
                ),
            ),
            counted_route("second", 2, FakeTransport::Fail),
        ]);

        let (_, permanent) = send_err(sender.send(make_email()).await);

        assert!(!permanent);
    }

    #[test]
    fn is_transient_no_matching_route_is_false() {
        use crate::port::email_sender::SendError;
        let err = SendError::NoMatchingRoute {
            sender_domain: "example.com".to_owned(),
        };
        assert!(!err.is_transient());
    }
}
