pub mod auth;
pub mod connection;

use crate::{
    config::{ImapConfig, DEBUG},
    email::{
        cache::ProcessedEmails,
        imap::{auth::auth_session, connection::ImapConnection},
        parser::EmailMessage,
        ReadEmail,
    },
    error::{AppError, AppResult},
};

pub async fn read_next_email_imap(config: ImapConfig) -> AppResult<Option<ReadEmail>> {
    // Snapshot before the mailbox search so cleanup cannot erase newer entries.
    let cache = ProcessedEmails::open(&config).map_err(AppError::EmailCache)?;
    let connection = ImapConnection::new(&config).await?;
    println!("Successfully connected to {config}");

    let mut client = imap::Client::new(connection);

    let greeting = {
        let s = client.read_greeting()?;
        let s = String::from_utf8_lossy(&s);
        s.trim_end_matches(['\r', '\n']).to_string()
    };

    if DEBUG.print_imap_greeting {
        println!("Imap greeting: {greeting}");
    }

    let mut session = auth_session(client, &config).await?;

    if DEBUG.print_imap_capabilities {
        for capability in session.capabilities()?.iter() {
            println!("Server capability: {:?}", capability);
        }
    }

    let mailbox = session.select("INBOX")?;

    let uid_validity = mailbox
        .uid_validity
        .filter(|value| *value != 0)
        .ok_or(AppError::MissingUidValidity)?;
    let uids = session.uid_search("UNSEEN")?;
    let Some(id) = cache
        .next_unread(uid_validity, &uids)
        .map_err(AppError::EmailCache)?
    else {
        return Ok(None);
    };

    let fetches = session.uid_fetch(id.uid.to_string(), "(ENVELOPE BODY.PEEK[])")?;

    let fetch = fetches
        .iter()
        .next()
        .ok_or(AppError::FailedToFetchEmail(id.uid))?;

    let message = EmailMessage::parse_imap(&fetch).map_err(AppError::AnyMessageParse)?;

    Ok(Some(ReadEmail { message, cache, id }))
}
