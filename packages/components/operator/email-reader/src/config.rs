use std::sync::LazyLock;

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Zeroize, ZeroizeOnDrop)]
pub struct ImapConfig {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub credentials: ImapCredentials,
}

#[derive(Debug, Clone, Zeroize, ZeroizeOnDrop)]
pub struct AgentMailRestApiConfig {
    pub api_key: String,
    pub inbox_id: String,
    pub base_url: String,
}

#[derive(Debug, Clone, Zeroize, ZeroizeOnDrop)]
pub enum ImapCredentials {
    Plain { username: String, password: String },
}

#[derive(Default)]
pub struct DebugSettings {
    pub print_imap_greeting: bool,
    pub print_imap_capabilities: bool,
}

pub static DEBUG: LazyLock<DebugSettings> = LazyLock::new(|| {
    let mut settings = DebugSettings::default();

    if let Ok(val) = get_env_var_bool("WAVS_ENV_IMAP_DEBUG_GREETING") {
        settings.print_imap_greeting = val;
    }
    if let Ok(val) = get_env_var_bool("WAVS_ENV_IMAP_DEBUG_CAPABILITIES") {
        settings.print_imap_capabilities = val;
    }

    settings
});

impl std::fmt::Display for ImapConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ImapConfig {
            host, port, tls, ..
        } = self;
        write!(f, "IMAP {}:{} (TLS: {})", host, port, tls)
    }
}

impl ImapConfig {
    pub fn new() -> AppResult<Self> {
        let credential_kind = get_env_var("WAVS_ENV_MAIL_CREDENTIAL_KIND")?.to_lowercase();

        let credentials = match credential_kind.as_str() {
            "plain-imap" => {
                let username = get_env_var("WAVS_ENV_IMAP_USERNAME")?;
                let password = get_env_var("WAVS_ENV_IMAP_PASSWORD")?;
                ImapCredentials::Plain { username, password }
            }
            _ => unreachable!(),
        };

        let host = get_env_var("WAVS_ENV_IMAP_HOST")?;
        let port = get_env_var("WAVS_ENV_IMAP_PORT")?;
        let tls = get_env_var("WAVS_ENV_IMAP_TLS")?;

        let port: u16 = port.parse().map_err(|_| AppError::InvalidEnv {
            key: "WAVS_ENV_IMAP_PORT",
            reason: "Not a valid u16",
        })?;

        let tls = match tls.to_lowercase().as_str() {
            "true" => true,
            "false" => false,
            _ => {
                return Err(AppError::InvalidEnv {
                    key: "WAVS_ENV_IMAP_TLS",
                    reason: "Not a valid boolean",
                })
            }
        };

        Ok(Self {
            host: host.to_string(),
            port,
            tls,
            credentials,
        })
    }
}

impl AgentMailRestApiConfig {
    pub fn new() -> AppResult<Self> {
        let api_key = get_env_var("WAVS_ENV_AGENTMAIL_API_KEY")?;
        let inbox_id = get_env_var("WAVS_ENV_AGENTMAIL_INBOX_ID")?;
        let base_url = get_env_var("WAVS_ENV_AGENTMAIL_BASE_URL")
            .unwrap_or_else(|_| "https://api.agentmail.to/v0".to_string());
        Ok(Self {
            api_key,
            inbox_id,
            base_url,
        })
    }
}

pub fn get_env_var(key: &str) -> AppResult<String> {
    let value = std::env::var(key).unwrap_or_default();

    // clean up extra quotes from env vars if present (docker env var weirdness)
    let value = value.trim_matches('"');

    if value.is_empty() {
        return Err(AppError::MissingEnv {
            key: key.to_string(),
        });
    }

    Ok(value.to_string())
}

pub fn get_env_var_bool(key: &str) -> AppResult<bool> {
    get_env_var(key).map(|x| x.to_lowercase() == "true" || x == "1")
}
