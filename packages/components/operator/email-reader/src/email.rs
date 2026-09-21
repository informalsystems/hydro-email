pub mod imap;
mod parser;
pub mod rest_api;
pub mod verify;

use futures::StreamExt;
use thiserror::Error;

use crate::{
    config::{get_env_var, AgentMailRestApiConfig, ImapConfig, DEBUG},
    email::{imap::read_next_email_imap, parser::EmailMessage, rest_api::read_next_email_rest_api},
    error::{AppError, AppResult},
};

pub async fn read_next_email() -> AppResult<Option<EmailMessage>> {
    let credential_kind = get_env_var("WAVS_ENV_MAIL_CREDENTIAL_KIND")?.to_lowercase();

    match credential_kind.as_str() {
        "plain-imap" => read_next_email_imap(ImapConfig::new()?).await,
        "agentmail-rest-api" => read_next_email_rest_api(AgentMailRestApiConfig::new()?).await,
        _ => Err(AppError::InvalidEnv {
            key: "WAVS_ENV_MAIL_CREDENTIAL_KIND",
            reason: "Not a valid credential kind (expected 'plain-imap' or 'agentmail-rest-api')",
        }),
    }
}
