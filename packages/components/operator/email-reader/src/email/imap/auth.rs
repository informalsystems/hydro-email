use imap::{Client, Session};

use crate::{
    config::{ImapConfig, ImapCredentials},
    email::imap::connection::ImapConnection,
    error::{AppError, AppResult},
};

pub async fn auth_session(
    client: Client<ImapConnection>,
    config: &ImapConfig,
) -> AppResult<Session<ImapConnection>> {
    match &config.credentials {
        ImapCredentials::Plain { username, password } => {
            println!("Getting email for {username}");

            client
                .login(username, password)
                .map_err(|(e, _)| AppError::Auth(e.into()))
        }
    }
}
