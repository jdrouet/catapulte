use anyhow::Context;
use catapulte_outbound_attachment_fetcher::fetcher::{
    HttpAttachmentFetcher, HttpAttachmentFetcherConfig,
};

/// Builds an `HttpAttachmentFetcher` from environment variables.
///
/// # Errors
///
/// Returns an error when env var parsing or client construction fails.
pub fn from_env() -> anyhow::Result<HttpAttachmentFetcher> {
    HttpAttachmentFetcherConfig::from_env("CATAPULTE_ATTACHMENT_FETCHER")
        .context("loading attachment fetcher config")?
        .build()
        .context("building attachment fetcher")
}
