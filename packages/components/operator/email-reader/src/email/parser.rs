use imap::types::Fetch;
use mailparse::*;
use sha2::{Digest, Sha256};
use std::borrow::Cow;

use crate::error::AppResult;

#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq, Eq)]
pub enum InvalidEmail {
    #[error("email_parse_failed")]
    ParseFailed,
    #[error("email_sender_missing")]
    SenderMissing,
    #[error("email_sender_domain_invalid")]
    SenderDomainInvalid,
    #[error("email_subject_missing")]
    SubjectMissing,
    #[error("email_subject_invalid")]
    SubjectInvalid,
    #[error("email_identity_missing")]
    IdentityMissing,
}

impl InvalidEmail {
    pub fn code(self) -> String {
        self.to_string()
    }
}

pub struct EmailMessage {
    // 1) "Original sender" best-effort (From / Resent-From / Sender / envelope)
    pub original_sender: String,
    // 2) All DKIM-Signature header values (there can be multiple)
    pub dkim_signatures: Vec<String>,
    // 3) Subject (decoded)
    pub subject: Option<String>,
    // 4) Body (best-effort: prefer text/plain part; fall back to full text)
    pub body_text: Option<String>,
    // 5) Raw email bytes (can be parsed on demand)
    pub raw_bytes: Vec<u8>,
    // 6) All headers as key-value pairs
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for EmailMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailMessage")
            .field("original_sender", &self.original_sender)
            .field("dkim_signatures", &self.dkim_signatures)
            .field("subject", &self.subject)
            .field(
                "body_text",
                &self
                    .body_text
                    .as_ref()
                    .map(|s| Cow::Owned(format!("{}...", s.chars().take(30).collect::<String>())))
                    .unwrap_or(Cow::Borrowed("None")),
            )
            .field("raw_bytes_len", &self.raw_bytes.len())
            .field("headers_count", &self.headers.len())
            .finish()
    }
}

impl EmailMessage {
    pub fn parse_imap(f: &Fetch, body_bytes: &[u8]) -> Result<Self, InvalidEmail> {
        parse_any(body_bytes, Some(f))
    }

    pub fn parse_rest_api(body_bytes: &[u8]) -> Result<Self, InvalidEmail> {
        parse_any(body_bytes, None)
    }

    /// Parse the raw email bytes and return a ParsedMail
    pub fn get_parsed(&self) -> AppResult<ParsedMail> {
        Ok(parse_mail(&self.raw_bytes)?)
    }

    /// Try to extract the Message-ID header value
    pub fn message_id(&self) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("Message-ID"))
            .map(|(_, v)| v.as_str())
    }

    /// Try to extract the Date header value
    pub fn date(&self) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("Date"))
            .map(|(_, v)| v.as_str())
    }

    pub fn sender_domain(&self) -> Result<String, InvalidEmail> {
        let addresses =
            addrparse(&self.original_sender).map_err(|_| InvalidEmail::SenderDomainInvalid)?;
        let [MailAddr::Single(sender)] = addresses.as_slice() else {
            return Err(InvalidEmail::SenderDomainInvalid);
        };
        let (_, domain) = sender
            .addr
            .split_once('@')
            .filter(|(local, domain)| !local.is_empty() && !domain.is_empty())
            .ok_or(InvalidEmail::SenderDomainInvalid)?;
        Ok(domain.to_owned())
    }

    pub fn validate_subject(&self) -> Result<(), InvalidEmail> {
        let subject = self
            .subject
            .as_deref()
            .ok_or(InvalidEmail::SubjectMissing)?;
        app_contract_api::proxy::ProxyExecuteMsg::from_email_subject(subject)
            .map(|_| ())
            .map_err(|_| InvalidEmail::SubjectInvalid)
    }

    pub fn event_id_salt(&self) -> Result<Vec<u8>, InvalidEmail> {
        let unique_id = self
            .message_id()
            .or_else(|| self.date())
            .ok_or(InvalidEmail::IdentityMissing)?;

        let subject = self.subject.as_ref().ok_or(InvalidEmail::SubjectMissing)?;

        let mut hasher = Sha256::new();

        hasher.update(unique_id);
        hasher.update(&self.original_sender);
        hasher.update(subject);

        Ok(hasher.finalize().to_vec())
    }
}

fn parse_any(body_bytes: &[u8], f: Option<&Fetch>) -> Result<EmailMessage, InvalidEmail> {
    let msg = parse_mail(body_bytes).map_err(|_| InvalidEmail::ParseFailed)?;

    let subject = msg
        .headers
        .iter()
        .find(|h| h.get_key().eq_ignore_ascii_case("Subject"))
        .map(|h| h.get_value());

    // best effort original sender extraction
    let original_sender = msg
        .headers
        .iter()
        .find(|h| {
            h.get_key().eq_ignore_ascii_case("Resent-From")
                || h.get_key().eq_ignore_ascii_case("From")
                || h.get_key().eq_ignore_ascii_case("Sender")
        })
        .map(|h| h.get_value())
        .or_else(|| {
            f.and_then(|f| {
                f.envelope()
                    .and_then(|env| {
                        env.from.as_ref().map(|addrs| {
                            addrs
                                .first()
                                .map(|a| a.mailbox.map(|s| String::from_utf8(s.to_vec()).ok()))
                        })
                    })
                    .flatten()
                    .flatten()
                    .flatten()
            })
        })
        .filter(|sender| !sender.trim().is_empty())
        .ok_or(InvalidEmail::SenderMissing)?;

    // there can be multiple signatures
    let dkim_signatures = msg
        .headers
        .iter()
        .filter(|h| h.get_key().eq_ignore_ascii_case("DKIM-Signature"))
        .map(|h| h.get_value())
        .collect::<Vec<_>>();

    // prefer text/plain part; fall back to html text
    let body_text = msg.get_body().ok();

    // collect all headers as key-value pairs
    let headers = msg
        .headers
        .iter()
        .map(|h| (h.get_key().to_string(), h.get_value()))
        .collect();

    Ok(EmailMessage {
        subject: subject.map(|s| s.to_string()),
        original_sender: original_sender.to_string(),
        dkim_signatures: dkim_signatures.into_iter().map(|s| s.to_string()).collect(),
        body_text: body_text.map(|s| s.to_string()),
        raw_bytes: body_bytes.to_vec(),
        headers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_content_and_missing_identity_have_stable_rejection_codes() {
        for (raw, expected) in [
            (" broken header", InvalidEmail::ParseFailed),
            ("Subject: deposit\r\n\r\nbody", InvalidEmail::SenderMissing),
            (
                "From: alice@example.com\r\nSubject: deposit\r\n\r\n",
                InvalidEmail::IdentityMissing,
            ),
            (
                "From: alice@example.com\r\nMessage-ID: <1>\r\n\r\n",
                InvalidEmail::SubjectMissing,
            ),
        ] {
            let result = EmailMessage::parse_rest_api(raw.as_bytes())
                .and_then(|email| email.event_id_salt());
            assert_eq!(result.unwrap_err(), expected);
        }
        let invalid = EmailMessage::parse_rest_api(b"From: alice\r\n\r\n").unwrap();
        assert_eq!(
            invalid.sender_domain().unwrap_err(),
            InvalidEmail::SenderDomainInvalid
        );
    }

    #[test]
    fn subject_validation_rejects_invalid_instructions_and_keeps_date_fallback() {
        for (subject, valid) in [
            ("deposit", true),
            ("", false),
            ("unknown instruction", false),
        ] {
            let raw = format!("From: alice@example.com\r\nSubject: {subject}\r\nDate: Thu, 22 Oct 2026 12:00:00 +0000\r\n\r\nbody");
            let email = EmailMessage::parse_rest_api(raw.as_bytes()).unwrap();
            assert_eq!(email.event_id_salt().unwrap().len(), 32);
            if valid {
                email.validate_subject().unwrap();
            } else {
                assert_eq!(
                    email.validate_subject().unwrap_err(),
                    InvalidEmail::SubjectInvalid
                );
            }
        }
    }
}
