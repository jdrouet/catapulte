use crate::service::server::{Configuration, Server};

#[derive(clap::Parser)]
pub(crate) struct Action {
    /// Path to the configuration toml file, default to /etc/catapulte/catapulte.toml.
    #[clap(
        short,
        long,
        default_value = "/etc/catapulte/catapulte.toml",
        env = "CATAPULTE_CONFIG"
    )]
    pub config_path: String,
}

impl Action {
    fn configuration(&self) -> Configuration {
        Configuration::from_path(&self.config_path)
    }

    pub(crate) async fn execute(self) {
        let config = self.configuration();
        Server::from_config(config).run().await
    }
}
