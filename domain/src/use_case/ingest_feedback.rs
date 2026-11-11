use std::collections::HashSet;

use thiserror::Error;

use crate::entity::email::EmailId;
use crate::entity::lifecycle_event::LifecycleEvent;
use crate::entity::return_path::BounceConfig;
use crate::entity::suppression::{Suppression, SuppressionReason, suppression_key};
use crate::port::clock::Clock;
use crate::port::email_lookup::{EmailLookup, EmailLookupError};
use crate::port::event_publisher::{EventPublisher, EventPublisherError};
use crate::port::suppression_repository::{SuppressionRepository, SuppressionRepositoryError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReportKind {
    Bounce {
        hard: bool,
        diagnostic_code: Option<String>,
    },
    Complaint,
}

/// What the report says about the signed return path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReturnPathEvidence {
    /// No bounce address appears in the report.
    Absent,
    /// A bounce address appears but cannot be read as `bounces+{uuid}-{tag}`.
    Malformed,
    Signed {
        id: EmailId,
        tag: String,
    },
}

/// One recipient a report speaks about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportItem {
    pub kind: ReportKind,
    pub recipient: String,
}

/// A parsed bounce or complaint. Every field comes from untrusted mail.
#[derive(Clone, Debug)]
pub struct FeedbackReport {
    /// One item per recipient block (a DSN can report several recipients).
    pub items: Vec<ReportItem>,
    pub return_path: ReturnPathEvidence,
    /// Email id taken from the `Message-ID` of the embedded original; never authentication.
    pub original_message_id: Option<EmailId>,
}

#[derive(Debug, Error)]
pub enum IngestFeedbackError {
    #[error("the return path tag is not valid")]
    InvalidReturnPath,
    #[error("the report cannot be tied to an email")]
    Uncorrelated,
    #[error("the email is unknown")]
    UnknownEmail,
    #[error("the reported recipient is not a recipient of the email")]
    RecipientNotOnEmail,
    #[error(transparent)]
    Lookup(#[from] EmailLookupError),
    #[error(transparent)]
    Suppression(#[from] SuppressionRepositoryError),
    #[error(transparent)]
    Publish(#[from] EventPublisherError),
}

impl IngestFeedbackError {
    /// True when the report itself is refused, as opposed to an infrastructure failure.
    #[must_use]
    pub fn is_rejection(&self) -> bool {
        matches!(
            self,
            Self::InvalidReturnPath
                | Self::Uncorrelated
                | Self::UnknownEmail
                | Self::RecipientNotOnEmail
        )
    }
}

pub trait IngestFeedbackUseCase: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns a rejection (see [`IngestFeedbackError::is_rejection`]) when the report cannot
    /// be trusted or matched, and another error when storage or publishing fails.
    fn execute(
        &self,
        report: FeedbackReport,
    ) -> impl std::future::Future<Output = Result<(), IngestFeedbackError>> + Send;
}

pub struct IngestFeedbackService<L, P, S, C> {
    lookup: L,
    publisher: P,
    suppressions: S,
    clock: C,
    bounce: Option<BounceConfig>,
}

impl<L, P, S, C> IngestFeedbackService<L, P, S, C> {
    /// Without a bounce configuration no signed return path can verify, so only
    /// `Message-ID` complaints are accepted.
    pub fn new(
        lookup: L,
        publisher: P,
        suppressions: S,
        clock: C,
        bounce: Option<BounceConfig>,
    ) -> Self {
        Self {
            lookup,
            publisher,
            suppressions,
            clock,
            bounce,
        }
    }
}

impl<L, P, S, C> IngestFeedbackService<L, P, S, C>
where
    L: EmailLookup,
    P: EventPublisher,
    S: SuppressionRepository,
    C: Clock,
{
    async fn ingest(&self, report: FeedbackReport) -> Result<(), IngestFeedbackError> {
        let (id, verified) = match &report.return_path {
            ReturnPathEvidence::Malformed => return Err(IngestFeedbackError::InvalidReturnPath),
            ReturnPathEvidence::Signed { id, tag } => {
                if !self.bounce.as_ref().is_some_and(|b| b.verify(*id, tag)) {
                    return Err(IngestFeedbackError::InvalidReturnPath);
                }
                (*id, true)
            }
            ReturnPathEvidence::Absent => {
                let all_complaints = report
                    .items
                    .iter()
                    .all(|item| item.kind == ReportKind::Complaint);
                match report.original_message_id {
                    Some(id) if all_complaints => (id, false),
                    _ => return Err(IngestFeedbackError::Uncorrelated),
                }
            }
        };

        let summary = self
            .lookup
            .find(id)
            .await?
            .ok_or(IngestFeedbackError::UnknownEmail)?;
        let on_email: HashSet<String> = summary
            .recipients
            .iter()
            .map(|(_, address)| suppression_key(address))
            .collect();
        // Counted as stored: entries, not distinct addresses. The stored list can only be a
        // superset of what was sent, so a miscount skips a suppression and never adds one.
        let single_recipient = summary.recipients.len() == 1;
        // One item per recipient, so repeated blocks cannot amplify events.
        let mut seen = HashSet::new();
        let items: Vec<ReportItem> = report
            .items
            .into_iter()
            .filter(|item| {
                let key = suppression_key(&item.recipient);
                on_email.contains(&key) && seen.insert(key)
            })
            .collect();
        if items.is_empty() {
            return Err(IngestFeedbackError::RecipientNotOnEmail);
        }

        // The signed tag binds the email, not a recipient: on a multi-recipient email a
        // co-recipient could name someone else, so only a single-recipient email suppresses.
        let may_suppress = verified && single_recipient;
        for item in items {
            let recipient = suppression_key(&item.recipient);
            let (reason, event) = match item.kind {
                ReportKind::Bounce {
                    hard,
                    diagnostic_code,
                } => (
                    (hard && may_suppress).then_some(SuppressionReason::Bounce),
                    LifecycleEvent::Bounced {
                        id,
                        hard,
                        recipient: recipient.clone(),
                        diagnostic_code,
                        context: summary.context.clone(),
                    },
                ),
                ReportKind::Complaint => (
                    may_suppress.then_some(SuppressionReason::Complaint),
                    LifecycleEvent::Complained {
                        id,
                        recipient: recipient.clone(),
                        verified,
                        context: summary.context.clone(),
                    },
                ),
            };
            if let Some(reason) = reason {
                self.suppressions
                    .add(&[Suppression {
                        address: recipient,
                        reason,
                        created_at_ms: self.clock.now_ms(),
                        source_email_id: Some(id),
                    }])
                    .await?;
            }
            self.publisher.publish(&event).await?;
        }
        Ok(())
    }
}

impl<L, P, S, C> IngestFeedbackUseCase for IngestFeedbackService<L, P, S, C>
where
    L: EmailLookup,
    P: EventPublisher,
    S: SuppressionRepository,
    C: Clock,
{
    fn execute(
        &self,
        report: FeedbackReport,
    ) -> impl std::future::Future<Output = Result<(), IngestFeedbackError>> + Send {
        self.ingest(report)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::entity::email::RecipientKind;
    use crate::entity::lifecycle_event::EventContext;
    use crate::port::email_lookup::EmailSummary;
    use crate::port::suppression_repository::{ListSuppressionsParams, NoopSuppressionList};

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn bounce_config() -> BounceConfig {
        BounceConfig::new("bounces.example.com", SECRET).unwrap()
    }

    struct FixedLookup(Option<EmailSummary>);

    impl EmailLookup for FixedLookup {
        async fn find(&self, _: EmailId) -> Result<Option<EmailSummary>, EmailLookupError> {
            Ok(self.0.clone())
        }
    }

    #[derive(Clone, Default)]
    struct Recorder {
        events: Arc<Mutex<Vec<LifecycleEvent>>>,
        added: Arc<Mutex<Vec<Suppression>>>,
    }

    impl EventPublisher for Recorder {
        async fn publish(&self, event: &LifecycleEvent) -> Result<(), EventPublisherError> {
            self.events.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    impl SuppressionRepository for Recorder {
        async fn list(
            &self,
            _: ListSuppressionsParams,
        ) -> Result<Vec<Suppression>, SuppressionRepositoryError> {
            Ok(vec![])
        }

        async fn add(&self, entries: &[Suppression]) -> Result<(), SuppressionRepositoryError> {
            self.added.lock().unwrap().extend_from_slice(entries);
            Ok(())
        }

        async fn delete(&self, _: &str) -> Result<bool, SuppressionRepositoryError> {
            Ok(false)
        }

        async fn find_suppressed(
            &self,
            _: &[String],
        ) -> Result<HashSet<String>, SuppressionRepositoryError> {
            Ok(HashSet::new())
        }
    }

    struct FixedClock;

    impl Clock for FixedClock {
        fn now_ms(&self) -> i64 {
            42
        }
    }

    fn summary(recipients: &[&str]) -> EmailSummary {
        EmailSummary {
            recipients: recipients
                .iter()
                .map(|a| (RecipientKind::To, (*a).to_owned()))
                .collect(),
            context: EventContext {
                tags: vec!["welcome".to_owned()],
                ..EventContext::default()
            },
        }
    }

    fn service(
        recipients: Option<&[&str]>,
        recorder: &Recorder,
    ) -> IngestFeedbackService<FixedLookup, Recorder, Recorder, FixedClock> {
        IngestFeedbackService::new(
            FixedLookup(recipients.map(summary)),
            recorder.clone(),
            recorder.clone(),
            FixedClock,
            Some(bounce_config()),
        )
    }

    fn signed(id: EmailId) -> ReturnPathEvidence {
        ReturnPathEvidence::Signed {
            id,
            tag: bounce_config().sign(id),
        }
    }

    fn report(
        kind: ReportKind,
        recipient: &str,
        return_path: ReturnPathEvidence,
    ) -> FeedbackReport {
        FeedbackReport {
            items: vec![ReportItem {
                kind,
                recipient: recipient.to_owned(),
            }],
            return_path,
            original_message_id: None,
        }
    }

    fn hard() -> ReportKind {
        ReportKind::Bounce {
            hard: true,
            diagnostic_code: Some("550 5.1.1 no such user".to_owned()),
        }
    }

    #[tokio::test]
    async fn signed_hard_bounce_on_a_single_recipient_email_suppresses() {
        let recorder = Recorder::default();
        let id = EmailId::default();

        service(Some(&["a@x.com"]), &recorder)
            .ingest(report(hard(), "A@X.com", signed(id)))
            .await
            .unwrap();

        let added = recorder.added.lock().unwrap();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].address, "a@x.com");
        assert_eq!(added[0].reason, SuppressionReason::Bounce);
        assert_eq!(added[0].source_email_id, Some(id));
        let events = recorder.events.lock().unwrap();
        assert!(matches!(
            &events[0],
            LifecycleEvent::Bounced { hard: true, context, .. } if context.tags == ["welcome"]
        ));
    }

    #[tokio::test]
    async fn multi_recipient_emails_only_get_an_event() {
        let recorder = Recorder::default();

        service(Some(&["a@x.com", "b@x.com"]), &recorder)
            .ingest(report(hard(), "a@x.com", signed(EmailId::default())))
            .await
            .unwrap();

        assert!(recorder.added.lock().unwrap().is_empty());
        assert_eq!(recorder.events.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn soft_bounces_only_get_an_event() {
        let recorder = Recorder::default();
        let soft = ReportKind::Bounce {
            hard: false,
            diagnostic_code: None,
        };

        service(Some(&["a@x.com"]), &recorder)
            .ingest(report(soft, "a@x.com", signed(EmailId::default())))
            .await
            .unwrap();

        assert!(recorder.added.lock().unwrap().is_empty());
        assert_eq!(recorder.events.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rejections_publish_nothing_and_suppress_nothing() {
        let id = EmailId::default();
        let bad_tag = ReturnPathEvidence::Signed {
            id,
            tag: "aaaaaaaaaaaaaaaa".to_owned(),
        };
        let cases = [
            (Some(&["a@x.com"][..]), report(hard(), "a@x.com", bad_tag)),
            (
                Some(&["a@x.com"][..]),
                report(hard(), "a@x.com", ReturnPathEvidence::Malformed),
            ),
            (
                Some(&["a@x.com"][..]),
                report(hard(), "a@x.com", ReturnPathEvidence::Absent),
            ),
            (None, report(hard(), "a@x.com", signed(id))),
            (
                Some(&["a@x.com"][..]),
                report(hard(), "victim@x.com", signed(id)),
            ),
        ];
        for (recipients, case) in cases {
            let recorder = Recorder::default();
            let err = service(recipients, &recorder)
                .ingest(case)
                .await
                .unwrap_err();
            assert!(err.is_rejection(), "{err}");
            assert!(recorder.events.lock().unwrap().is_empty());
            assert!(recorder.added.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn message_id_only_complaint_is_an_unverified_event_and_never_suppresses() {
        let recorder = Recorder::default();
        let mut complaint = report(ReportKind::Complaint, "a@x.com", ReturnPathEvidence::Absent);
        complaint.original_message_id = Some(EmailId::default());

        service(Some(&["a@x.com"]), &recorder)
            .ingest(complaint)
            .await
            .unwrap();

        assert!(recorder.added.lock().unwrap().is_empty());
        assert!(matches!(
            &recorder.events.lock().unwrap()[0],
            LifecycleEvent::Complained {
                verified: false,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn verified_complaint_suppresses_and_a_bounce_never_trusts_the_message_id() {
        let recorder = Recorder::default();
        let id = EmailId::default();
        service(Some(&["a@x.com"]), &recorder)
            .ingest(report(ReportKind::Complaint, "a@x.com", signed(id)))
            .await
            .unwrap();
        assert_eq!(
            recorder.added.lock().unwrap()[0].reason,
            SuppressionReason::Complaint
        );

        let mut dsn = report(hard(), "a@x.com", ReturnPathEvidence::Absent);
        dsn.original_message_id = Some(id);
        let err = service(Some(&["a@x.com"]), &Recorder::default())
            .ingest(dsn)
            .await
            .unwrap_err();
        assert!(matches!(err, IngestFeedbackError::Uncorrelated));
    }

    #[tokio::test]
    async fn without_a_bounce_config_no_tag_verifies() {
        let service = IngestFeedbackService::new(
            FixedLookup(Some(summary(&["a@x.com"]))),
            Recorder::default(),
            NoopSuppressionList,
            FixedClock,
            None,
        );

        let err = service
            .ingest(report(hard(), "a@x.com", signed(EmailId::default())))
            .await
            .unwrap_err();

        assert!(matches!(err, IngestFeedbackError::InvalidReturnPath));
    }

    #[tokio::test]
    async fn stored_entries_are_counted_as_they_are_not_deduplicated() {
        let recorder = Recorder::default();

        service(Some(&["Alice@example.org", "alice@example.org"]), &recorder)
            .ingest(report(
                hard(),
                "alice@example.org",
                signed(EmailId::default()),
            ))
            .await
            .unwrap();

        assert!(recorder.added.lock().unwrap().is_empty());
        assert_eq!(recorder.events.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn every_matching_block_gets_an_event_and_foreign_ones_are_skipped() {
        let recorder = Recorder::default();
        let mut multi = report(hard(), "a@x.com", signed(EmailId::default()));
        multi.items.push(ReportItem {
            kind: hard(),
            recipient: "B@x.com".to_owned(),
        });
        multi.items.push(ReportItem {
            kind: hard(),
            recipient: "stranger@x.com".to_owned(),
        });

        service(Some(&["a@x.com", "b@x.com"]), &recorder)
            .ingest(multi)
            .await
            .unwrap();

        let events = recorder.events.lock().unwrap();
        let named: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                LifecycleEvent::Bounced { recipient, .. } => Some(recipient.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(named, ["a@x.com", "b@x.com"]);
        assert!(recorder.added.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn repeated_blocks_for_one_recipient_produce_one_event() {
        let recorder = Recorder::default();
        let mut repeated = report(hard(), "a@x.com", signed(EmailId::default()));
        for _ in 0..3 {
            repeated.items.push(ReportItem {
                kind: hard(),
                recipient: "A@x.com".to_owned(),
            });
        }

        service(Some(&["a@x.com"]), &recorder)
            .ingest(repeated)
            .await
            .unwrap();

        assert_eq!(recorder.events.lock().unwrap().len(), 1);
        assert_eq!(recorder.added.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_report_whose_blocks_all_name_strangers_is_rejected() {
        let recorder = Recorder::default();

        let err = service(Some(&["a@x.com"]), &recorder)
            .ingest(report(hard(), "stranger@x.com", signed(EmailId::default())))
            .await
            .unwrap_err();

        assert!(matches!(err, IngestFeedbackError::RecipientNotOnEmail));
    }
}
