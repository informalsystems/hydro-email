pub mod cache;
pub mod imap;
pub(crate) mod parser;
pub mod rest_api;
pub mod verify;

use crate::{
    config::{get_env_var, AgentMailRestApiConfig, ImapConfig},
    email::{imap::read_next_email_imap, parser::EmailMessage, rest_api::read_next_email_rest_api},
    error::{AppError, AppResult},
};

pub struct ReadEmail {
    pub message: Result<EmailMessage, parser::InvalidEmail>,
    pub cache: cache::ProcessedEmails,
    pub id: cache::MessageId,
}

impl ReadEmail {
    pub fn remember(&self) -> AppResult<()> {
        self.cache.remember(self.id).map_err(AppError::EmailCache)
    }
}

pub async fn read_next_email() -> AppResult<Option<ReadEmail>> {
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
