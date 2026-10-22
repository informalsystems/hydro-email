use thiserror::Error;

use crate::email::imap::connection::ImapConnectionError;

pub type AppResult<T> = std::result::Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    InvalidEmail(#[from] crate::email::parser::InvalidEmail),

    #[error("{0:?}")]
    ImapConnection(#[from] ImapConnectionError),

    #[error("{0:?}")]
    ImapClient(#[from] imap::Error),

    #[error("Missing env var for {key}")]
    MissingEnv { key: String },

    #[error("Invalid env var for {key}: {reason}")]
    InvalidEnv {
        key: &'static str,
        reason: &'static str,
    },

    #[error("Failed to fetch email, uid: {0}")]
    FailedToFetchEmail(u32),

    #[error("IMAP server did not provide UIDVALIDITY")]
    MissingUidValidity,

    #[error("Email cache: {0:#}")]
    EmailCache(anyhow::Error),

    #[error("{0:?}")]
    AnyMessageParse(anyhow::Error),

    #[error("{0:?}")]
    MessageParse(#[from] mailparse::MailParseError),

    #[error("Authorization: {0:?}")]
    Auth(anyhow::Error),

    #[error("DKIM: {0:?}")]
    Dkim(#[from] cfdkim::DKIMError),

    #[error("DKIM Result: {0}")]
    DkimResult(String),

    #[error("Cannot extract domain: {0}")]
    CannotExtractDomain(String),

    #[error("slog: {0:?}")]
    Slog(#[from] sloggers::Error),

    #[error("Not implemented: {0}")]
    NotImplemented(&'static str),
}
