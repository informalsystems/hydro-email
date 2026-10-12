pub mod auth;
pub mod connection;

use crate::{
    config::{ImapConfig, DEBUG},
    email::{
        imap::{auth::auth_session, connection::ImapConnection},
        parser::EmailMessage,
    },
    error::{AppError, AppResult},
};

pub async fn read_next_email_imap(config: ImapConfig) -> AppResult<Option<EmailMessage>> {
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

    if mailbox.exists == 0 {
        return Ok(None);
    }

    let uids = session.uid_search("UNSEEN")?;
    if uids.is_empty() {
        return Ok(None);
    }

    let latest_uid = *uids.iter().max().unwrap();

    let fetches = session.uid_fetch(latest_uid.to_string(), "(ENVELOPE BODY[])")?;

    let fetch = fetches
        .iter()
        .next()
        .ok_or(AppError::FailedToFetchEmail(latest_uid))?;

    let message = EmailMessage::parse_imap(&fetch).map_err(AppError::AnyMessageParse)?;

    // AgentMail doesn't set \Seen flag on a BODY[] fetch, so mark it as \Seen to avoid processing the same email again
    session.uid_store(latest_uid.to_string(), "+FLAGS (\\Seen)")?;

    Ok(Some(message))
}
