use crate::config::Config as RootConfig;
use std::sync::Arc;

pub struct Config(pub Arc<RootConfig>);

impl Config {
    pub fn to_bind(&self) -> String {
        format!("{}:{}", self.0.server_address, self.0.server_port)
    }
}
