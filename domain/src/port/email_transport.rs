use crate::port::email_sender::OutboundEmail;

pub trait EmailTransport: Send + Sync {
    fn deliver<'a>(
        &'a self,
        email: &'a OutboundEmail,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send + 'a;
}
