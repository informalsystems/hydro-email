use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use async_trait::async_trait;

use crate::{
    action_store::ActionStore,
    mailbox_store::{AcknowledgmentResult, MailboxMessage, UpdateAcknowledgment},
};

#[derive(Clone)]
pub struct ImapMailbox {
    host: String,
    port: u16,
    tls: bool,
    username: String,
    password: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcknowledgmentError {
    Unavailable,
    UidValidityChanged,
}

#[async_trait]
pub trait Mailbox: Send + Sync {
    async fn acknowledge(&self, message: &MailboxMessage) -> Result<(), AcknowledgmentError>;
}

impl ImapMailbox {
    // Use the same receiving account as the operators, without duplicating credentials.
    pub fn from_env() -> Result<Self> {
        fn required(key: &str) -> Result<String> {
            std::env::var(key)
                .ok()
                .map(|value| value.trim_matches('"').to_owned())
                .filter(|value| !value.is_empty())
                .with_context(|| format!("{key} is required for mailbox acknowledgment"))
        }
        ensure!(
            required("WAVS_ENV_MAIL_CREDENTIAL_KIND")? == "plain-imap",
            "Mailbox acknowledgment requires plain-imap"
        );
        Ok(Self {
            host: required("WAVS_ENV_IMAP_HOST")?,
            port: required("WAVS_ENV_IMAP_PORT")?.parse()?,
            tls: required("WAVS_ENV_IMAP_TLS")?.parse()?,
            username: required("WAVS_ENV_IMAP_USERNAME")?,
            password: required("WAVS_ENV_IMAP_PASSWORD")?,
        })
    }

    fn acknowledge_blocking(
        &self,
        folder: &str,
        uid_validity: i64,
        uid: i64,
    ) -> Result<(), AcknowledgmentError> {
        // Never persist or log IMAP errors: servers can echo authentication data.
        let unavailable = |_| AcknowledgmentError::Unavailable;
        let addresses = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(unavailable)?;
        let mut socket = None;
        for address in addresses {
            if let Ok(stream) = TcpStream::connect_timeout(&address, Duration::from_secs(5)) {
                socket = Some(stream);
                break;
            }
        }
        let socket = socket.ok_or(AcknowledgmentError::Unavailable)?;
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(unavailable)?;
        socket
            .set_write_timeout(Some(Duration::from_secs(10)))
            .map_err(unavailable)?;
        trait Stream: Read + Write + Send {}
        impl<T: Read + Write + Send> Stream for T {}
        let stream: Box<dyn Stream> = if self.tls {
            let connector =
                native_tls::TlsConnector::new().map_err(|_| AcknowledgmentError::Unavailable)?;
            Box::new(
                connector
                    .connect(&self.host, socket)
                    .map_err(|_| AcknowledgmentError::Unavailable)?,
            )
        } else {
            Box::new(socket)
        };
        let mut client = imap::Client::new(stream);
        client
            .read_greeting()
            .map_err(|_| AcknowledgmentError::Unavailable)?;
        let mut session = client
            .login(&self.username, &self.password)
            .map_err(|_| AcknowledgmentError::Unavailable)?;
        let mailbox = session
            .select(folder)
            .map_err(|_| AcknowledgmentError::Unavailable)?;
        let current_validity = mailbox
            .uid_validity
            .filter(|value| *value != 0)
            .ok_or(AcknowledgmentError::Unavailable)?;
        if i64::from(current_validity) != uid_validity {
            return Err(AcknowledgmentError::UidValidityChanged);
        }
        // STORE on a removed UID is a successful no-op. Both removal and an
        // already-seen message require no further acknowledgment.
        session
            .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Seen)")
            .map_err(|_| AcknowledgmentError::Unavailable)?;
        let _ = session.logout();
        Ok(())
    }
}

#[async_trait]
impl Mailbox for ImapMailbox {
    async fn acknowledge(&self, message: &MailboxMessage) -> Result<(), AcknowledgmentError> {
        let mailbox = self.clone();
        let folder = message.folder.clone();
        let uid_validity = message.uid_validity;
        let uid = message.uid;
        tokio::task::spawn_blocking(move || {
            mailbox.acknowledge_blocking(&folder, uid_validity, uid)
        })
        .await
        .unwrap_or(Err(AcknowledgmentError::Unavailable))
    }
}

pub struct MailboxWorker {
    pub store: ActionStore,
    pub mailbox: Arc<dyn Mailbox>,
}

impl MailboxWorker {
    pub async fn run(&self) -> Result<()> {
        loop {
            self.tick().await?;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    pub async fn tick(&self) -> Result<()> {
        for message in self.store.pending_acknowledgments(25).await? {
            let result = match self.mailbox.acknowledge(&message).await {
                Ok(()) => AcknowledgmentResult::Acknowledged,
                Err(AcknowledgmentError::UidValidityChanged) => AcknowledgmentResult::Obsolete,
                Err(AcknowledgmentError::Unavailable) => AcknowledgmentResult::Retry {
                    error_code: "mailbox_unavailable".into(),
                    retry_after_seconds: (5 * 2u32.pow(message.attempt_count.clamp(0, 10) as u32))
                        .min(3600),
                },
            };
            // Concurrent workers may repeat the idempotent IMAP flag update.
            // Store locking prevents a late failure from overwriting success.
            if let UpdateAcknowledgment::Updated(saved) = self
                .store
                .report_acknowledgment(message.action_id, message.id, result)
                .await?
            {
                tracing::info!(
                    action_id = saved.action_id,
                    message_id = saved.id,
                    acknowledgment_status = %saved.acknowledgment_status,
                    error_code = ?saved.last_error_code,
                    "Mailbox acknowledgment attempted"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        thread,
    };

    fn server(validity: u32, reject_store: bool) -> (ImapMailbox, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket.write_all(b"* OK test mailbox\r\n").unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut commands = Vec::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let tag = line.split_whitespace().next().unwrap();
                let reply = if line.contains(" LOGIN ") {
                    format!("{tag} OK login\r\n")
                } else if line.contains(" SELECT ") {
                    format!("* 1 EXISTS\r\n* OK [UIDVALIDITY {validity}] stable\r\n{tag} OK selected\r\n")
                } else if line.contains(" UID STORE ") {
                    if reject_store {
                        format!("{tag} NO unavailable\r\n")
                    } else {
                        format!("{tag} OK stored\r\n")
                    }
                } else if line.contains(" LOGOUT") {
                    format!("* BYE closing\r\n{tag} OK logout\r\n")
                } else {
                    panic!("Unexpected IMAP command");
                };
                let logout = line.contains(" LOGOUT");
                commands.push(line);
                socket.write_all(reply.as_bytes()).unwrap();
                if logout {
                    break;
                }
            }
            commands
        });
        (
            ImapMailbox {
                host: "127.0.0.1".into(),
                port,
                tls: false,
                username: "test-user".into(),
                password: "test-password".into(),
            },
            handle,
        )
    }

    #[test]
    fn stores_seen_by_uid_and_repeated_acknowledgment_is_safe() {
        for _ in 0..2 {
            let (mailbox, server) = server(100, false);
            assert_eq!(mailbox.acknowledge_blocking("INBOX", 100, 10), Ok(()));
            let commands = server.join().unwrap();
            assert!(commands
                .iter()
                .any(|line| line.contains("SELECT \"INBOX\"")));
            assert!(commands
                .iter()
                .any(|line| line.contains("UID STORE 10 +FLAGS.SILENT (\\Seen)")));
            assert!(!commands
                .iter()
                .any(|line| line.contains("FETCH") || line.contains("EXPUNGE")));
        }
    }

    #[test]
    fn changed_uid_validity_never_marks_a_reused_uid_read() {
        let (mailbox, server) = server(101, false);
        assert_eq!(
            mailbox.acknowledge_blocking("INBOX", 100, 10),
            Err(AcknowledgmentError::UidValidityChanged)
        );
        assert!(!server
            .join()
            .unwrap()
            .iter()
            .any(|line| line.contains("STORE")));
    }

    #[test]
    fn imap_rejection_is_reported_without_raw_server_errors() {
        let (mailbox, server) = server(100, true);
        assert_eq!(
            mailbox.acknowledge_blocking("INBOX", 100, 10),
            Err(AcknowledgmentError::Unavailable)
        );
        server.join().unwrap();
    }
}
