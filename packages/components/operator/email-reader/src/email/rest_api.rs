use crate::{
    config::AgentMailRestApiConfig,
    email::ReadEmail,
    error::{AppError, AppResult},
};

// TODO: implement against AgentMail's REST API (https://docs.agentmail.to).
// Needs a cursor/dedup strategy (e.g. last-seen message id or timestamp)
// since AgentMail's message list/search endpoints have no confirmed
// "unread" flag equivalent to Gmail's UNREAD label. `config.base_url` is
// already wired up (defaults to https://api.agentmail.to/v0) for when this
// is implemented.
pub async fn read_next_email_rest_api(
    _config: AgentMailRestApiConfig,
) -> AppResult<Option<ReadEmail>> {
    Err(AppError::NotImplemented(
        "agentmail-rest-api credential kind is a placeholder; use plain-imap for now",
    ))
}
