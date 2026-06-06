use std::future::Future;

use crate::port::health::HealthCheck;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    NotReady,
}

pub trait CheckReadinessUseCase: Send + Sync {
    fn check_readiness(&self) -> impl Future<Output = Readiness> + Send;
}

pub struct CheckReadinessService<H> {
    health: H,
}

impl<H> CheckReadinessService<H> {
    #[must_use]
    pub fn new(health: H) -> Self {
        Self { health }
    }
}

impl<H: HealthCheck> CheckReadinessUseCase for CheckReadinessService<H> {
    async fn check_readiness(&self) -> Readiness {
        match self.health.check().await {
            Ok(()) => Readiness::Ready,
            Err(error) => {
                tracing::warn!(error = ?error, "readiness check failed");
                Readiness::NotReady
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckReadinessService, CheckReadinessUseCase, Readiness};
    use crate::port::health::{HealthCheck, HealthCheckError};

    struct AlwaysOk;

    impl HealthCheck for AlwaysOk {
        async fn check(&self) -> Result<(), HealthCheckError> {
            Ok(())
        }
    }

    struct AlwaysFail;

    impl HealthCheck for AlwaysFail {
        async fn check(&self) -> Result<(), HealthCheckError> {
            Err(HealthCheckError::Unavailable {
                source: anyhow::anyhow!("boom"),
            })
        }
    }

    #[tokio::test]
    async fn healthy_dependency_reports_ready() {
        let svc = CheckReadinessService::new(AlwaysOk);
        assert_eq!(svc.check_readiness().await, Readiness::Ready);
    }

    #[tokio::test]
    async fn unhealthy_dependency_reports_not_ready() {
        let svc = CheckReadinessService::new(AlwaysFail);
        assert_eq!(svc.check_readiness().await, Readiness::NotReady);
    }
}
