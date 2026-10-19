use std::{env, net::SocketAddr};

use anyhow::{ensure, Context, Result};

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub listen: SocketAddr,
    pub api_key: String,
    pub chain_key: String,
    pub registry_address: String,
    pub registrations_per_hour: i64,
    pub registrations_per_day: i64,
    pub provisioning_enabled: bool,
    pub proxy_code_id: u64,
    pub service_handler_address: String,
    pub control_centers: Vec<String>,
    pub signing_mnemonic: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let provisioning_enabled = value("BACKEND_PROVISIONING_ENABLED", "false").parse()?;
        let config = Self {
            database_url: required("BACKEND_DATABASE_URL")?,
            listen: value("BACKEND_LISTEN", "127.0.0.1:8080").parse()?,
            api_key: required("BACKEND_API_KEY")?,
            chain_key: required("BACKEND_CHAIN_KEY")?,
            registry_address: required("BACKEND_USER_REGISTRY_ADDRESS")?,
            registrations_per_hour: value("BACKEND_REGISTRATIONS_PER_HOUR", "10").parse()?,
            registrations_per_day: value("BACKEND_REGISTRATIONS_PER_DAY", "50").parse()?,
            provisioning_enabled,
            proxy_code_id: value("BACKEND_PROXY_CODE_ID", "0").parse()?,
            service_handler_address: required("BACKEND_SERVICE_HANDLER_ADDRESS")?,
            control_centers: value("BACKEND_CONTROL_CENTER_ADDRESSES", "")
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            signing_mnemonic: value("BACKEND_SIGNING_MNEMONIC", ""),
        };
        ensure!(
            config.api_key.len() >= 32,
            "BACKEND_API_KEY must contain at least 32 characters"
        );
        ensure!(
            config.registrations_per_hour > 0 && config.registrations_per_day > 0,
            "Registration quotas must be positive"
        );
        if provisioning_enabled {
            ensure!(
                config.proxy_code_id > 0,
                "BACKEND_PROXY_CODE_ID is required for provisioning"
            );
            ensure!(
                !config.control_centers.is_empty(),
                "BACKEND_CONTROL_CENTER_ADDRESSES is required for provisioning"
            );
            ensure!(
                !config.signing_mnemonic.is_empty(),
                "BACKEND_SIGNING_MNEMONIC is required for provisioning"
            );
        }
        Ok(config)
    }
}

fn value(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn required(key: &str) -> Result<String> {
    env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .with_context(|| format!("{key} is required"))
}
