use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

/// The structure of the configuration file.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Config {
    pub(crate) amqp_server_address: String,
    pub(crate) prometheus_listen_address: String,
    pub(crate) submarine_http_endpoint: String,
    pub(crate) update_history_count: u32,
    pub(crate) http_server_listen_address: String,
}

impl Config {
    /// Reads a config from a file.
    pub(crate) fn read_from_file<P: AsRef<Path>>(path: P) -> Result<Config> {
        let contents = fs::read(path).context("unable to read file")?;

        let cfg: Config =
            serde_yaml::from_slice(contents.as_slice()).context("unable to parse config")?;

        Ok(cfg)
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            amqp_server_address: "amqp://127.0.0.1:5672/%2f".to_string(),
            prometheus_listen_address: "0.0.0.0:6879".to_string(),
            submarine_http_endpoint: "http://127.0.0.1:3069".to_string(),
            update_history_count: 1000,
            http_server_listen_address: "0.0.0.0:4350".to_string(),
        }
    }
}
