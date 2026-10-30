pub mod auth;
pub mod connection;

use crate::{
    config::{ImapConfig, DEBUG},
    email::{
        cache::ProcessedEmails,
        imap::{auth::auth_session, connection::ImapConnection},
        parser::{EmailMessage, InvalidEmail, MAX_MESSAGE_BYTES},
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

    let allowed = cache.begin_processing(id).map_err(AppError::EmailCache)?;
    let rejection = if cache
        .verification_expired(id)
        .map_err(AppError::EmailCache)?
    {
        Some(InvalidEmail::VerificationExpired)
    } else if !allowed {
        Some(InvalidEmail::ProcessingFailed)
    } else {
        None
    };
    if let Some(reason) = rejection {
        return Ok(Some(ReadEmail {
            message: Err(reason),
            cache,
            id,
        }));
    }

    let message = fetch_message(&mut session, id.uid)?;
    Ok(Some(ReadEmail { message, cache, id }))
}

fn fetch_message<T: std::io::Read + std::io::Write>(
    session: &mut imap::Session<T>,
    uid: u32,
) -> AppResult<Result<EmailMessage, InvalidEmail>> {
    // Request at most limit + 1 bytes. The extra byte distinguishes an oversized
    // message from one exactly at the limit without downloading the entire body.
    let query = format!("(BODY.PEEK[]<0.{}>)", MAX_MESSAGE_BYTES + 1);
    let fetches = session.uid_fetch(uid.to_string(), query)?;

    let fetch = fetches
        .iter()
        .next()
        .ok_or(AppError::FailedToFetchEmail(uid))?;

    // A missing FETCH body is a mailbox failure, not proof of an invalid email.
    let body = fetch.body().ok_or(AppError::FailedToFetchEmail(uid))?;
    Ok(EmailMessage::parse_imap(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        io::{Cursor, Read, Write},
        rc::Rc,
    };

    #[derive(Debug)]
    struct Wire {
        input: Cursor<Vec<u8>>,
        output: Rc<RefCell<Vec<u8>>>,
    }
    impl Read for Wire {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buf)
        }
    }
    impl Write for Wire {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn partial_fetch_rejects_oversize_and_reads_next_instruction_without_truncation() {
        let valid =
            b"From: alice@example.com\r\nSubject: deposit\r\nMessage-ID: <next>\r\n\r\nbody";
        let mut response = b"a1 OK logged in\r\n".to_vec();
        response.extend_from_slice(
            format!(
                "* 1 FETCH (UID 10 BODY[]<0> {{{}}}\r\n",
                MAX_MESSAGE_BYTES + 1
            )
            .as_bytes(),
        );
        response.resize(response.len() + MAX_MESSAGE_BYTES + 1, b'x');
        response.extend_from_slice(b")\r\na2 OK fetched\r\n");
        response.extend_from_slice(
            format!("* 2 FETCH (UID 11 BODY[]<0> {{{}}}\r\n", valid.len()).as_bytes(),
        );
        response.extend_from_slice(valid);
        response.extend_from_slice(b")\r\na3 OK fetched\r\n");
        let output = Rc::new(RefCell::new(Vec::new()));
        let wire = Wire {
            input: Cursor::new(response),
            output: output.clone(),
        };
        let mut session = imap::Client::new(wire).login("user", "password").unwrap();
        assert_eq!(
            fetch_message(&mut session, 10).unwrap().unwrap_err(),
            InvalidEmail::MessageTooLarge
        );
        let email = fetch_message(&mut session, 11).unwrap().unwrap();
        email.validate_subject().unwrap();
        assert_eq!(email.raw_bytes, valid);
        let commands = String::from_utf8(output.borrow().clone()).unwrap();
        assert!(commands.contains(&format!(
            "UID FETCH 10 (BODY.PEEK[]<0.{}>)",
            MAX_MESSAGE_BYTES + 1
        )));
        assert!(commands.contains(&format!(
            "UID FETCH 11 (BODY.PEEK[]<0.{}>)",
            MAX_MESSAGE_BYTES + 1
        )));
    }
}
