use crate::port::email_sender::OutboundEmail;

pub trait EmailTransport: Send + Sync + 'static {
    /// # Errors
    ///
    /// Returns an error when the underlying transport fails to deliver the email.
    fn deliver<'a>(
        &'a self,
        email: &'a OutboundEmail,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send + 'a;
}
