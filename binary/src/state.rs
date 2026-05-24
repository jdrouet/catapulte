use std::sync::Arc;

use catapulte_domain::port::email_queue::EmailQueue;
use catapulte_domain::port::event_publisher::EventPublisher;
use catapulte_domain::use_case::process_queued_email::{
    ProcessQueuedEmailService, ProcessQueuedEmailUseCase,
};
use catapulte_domain::use_case::submit_email::{SubmitEmailService, SubmitEmailUseCase};
use catapulte_inbound_http::HttpServerState;
use catapulte_inbound_worker::worker::WorkerState;
use catapulte_outbound_interpolator::interpolator::MiniJinjaInterpolator;
use catapulte_outbound_mjml::renderer::MjmlRenderer;
use catapulte_outbound_resolver::resolver::TemplateResolverAdapter;
use catapulte_outbound_smtp::sender::SmtpSender;
use catapulte_outbound_sqlite::SqliteAdapter;

use crate::queue::QueueAdapter;

pub(crate) type ProcessService = ProcessQueuedEmailService<
    TemplateResolverAdapter,
    MiniJinjaInterpolator,
    MjmlRenderer,
    SmtpSender,
>;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) submit_email: Arc<SubmitEmailService<SqliteAdapter, QueueAdapter>>,
    pub(crate) process_queued_email: Arc<ProcessService>,
    pub(crate) sqlite: SqliteAdapter,
    pub(crate) queue: QueueAdapter,
}

impl HttpServerState for AppState {
    fn submit_email(&self) -> &impl SubmitEmailUseCase {
        self.submit_email.as_ref()
    }
}

impl WorkerState for AppState {
    fn process_queued_email(&self) -> &impl ProcessQueuedEmailUseCase {
        self.process_queued_email.as_ref()
    }

    fn email_queue(&self) -> &impl EmailQueue {
        &self.queue
    }

    fn event_publisher(&self) -> &impl EventPublisher {
        &self.sqlite
    }
}
