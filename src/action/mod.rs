mod openapi;
mod serve;

#[derive(clap::Subcommand)]
pub(crate) enum Action {
    /// Run the Tekitoi server
    Serve(serve::Action),
    /// Prints the open api schema
    OpenApi(openapi::Action),
}

impl Action {
    pub(crate) async fn execute(self) {
        match self {
            Self::Serve(inner) => inner.execute().await,
            Self::OpenApi(inner) => inner.execute(),
        }
    }
}
