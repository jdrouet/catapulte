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

use crate::queue::QueueAdapter;
use crate::storage::StorageAdapter;

pub(crate) type ProcessService = ProcessQueuedEmailService<
    TemplateResolverAdapter,
    MiniJinjaInterpolator,
    MjmlRenderer,
    SmtpSender,
>;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) submit_email: Arc<SubmitEmailService<StorageAdapter, QueueAdapter, StorageAdapter>>,
    pub(crate) process_queued_email: Arc<ProcessService>,
    pub(crate) storage: StorageAdapter,
    pub(crate) queue: QueueAdapter,
}

impl HttpServerState for AppState {
    fn submit_email(&self) -> &impl SubmitEmailUseCase {
        self.submit_email.as_ref()
    }

    fn event_repository(&self) -> &impl catapulte_domain::port::event_repository::EventRepository {
        &self.storage
    }

    fn email_repository(&self) -> &impl catapulte_domain::port::email_repository::EmailRepository {
        &self.storage
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
        &self.storage
    }
}
