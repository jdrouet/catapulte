use thiserror::Error;

use crate::port::event_repository::{
    EventRecord, EventRepository, EventRepositoryError, ListEventsParams,
};

#[derive(Debug, Error)]
pub enum ListEventsError {
    #[error(transparent)]
    Repository(#[from] EventRepositoryError),
}

pub trait ListEventsUseCase: Send + Sync + 'static {
    fn execute(
        &self,
        params: ListEventsParams,
    ) -> impl std::future::Future<Output = Result<Vec<EventRecord>, ListEventsError>> + Send;
}

pub struct ListEventsService<R> {
    repo: R,
}

impl<R> ListEventsService<R> {
    pub fn new(repo: R) -> Self {
        Self { repo }
    }

    async fn execute_inner(
        &self,
        params: ListEventsParams,
    ) -> Result<Vec<EventRecord>, ListEventsError>
    where
        R: EventRepository,
    {
        Ok(self.repo.list_events(params).await?)
    }
}

impl<R: EventRepository + Send + Sync + 'static> ListEventsUseCase for ListEventsService<R> {
    fn execute(
        &self,
        params: ListEventsParams,
    ) -> impl std::future::Future<Output = Result<Vec<EventRecord>, ListEventsError>> + Send {
        self.execute_inner(params)
    }
}
