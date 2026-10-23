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
    #[error("email_sender_ambiguous")]
    SenderAmbiguous,
    #[error("email_authentication_failed")]
    AuthenticationFailed,
    #[error("email_verification_expired")]
    VerificationExpired,
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
    // 1) Author from the single From header; alternate sender headers are not trusted.
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
    pub fn parse_imap(body_bytes: &[u8]) -> Result<Self, InvalidEmail> {
        parse_any(body_bytes)
    }

    pub fn parse_rest_api(body_bytes: &[u8]) -> Result<Self, InvalidEmail> {
        parse_any(body_bytes)
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
        let identity = if self.message_id().is_some() {
            "message-id"
        } else if self.date().is_some() {
            "date"
        } else {
            return Err(InvalidEmail::IdentityMissing);
        };
        if self.subject.is_none() {
            return Err(InvalidEmail::SubjectMissing);
        }
        let parsed = parse_mail(&self.raw_bytes).map_err(|_| InvalidEmail::ParseFailed)?;
        let mut hasher = Sha256::new();
        for name in [identity, "from", "subject"] {
            let header = parsed
                .headers
                .iter()
                .find(|h| h.get_key_ref().eq_ignore_ascii_case(name))
                .ok_or(InvalidEmail::AuthenticationFailed)?;
            // RFC 6376 relaxed header canonicalization permits these whitespace
            // changes without invalidating a signature. They must not change ID.
            hasher.update(name);
            hasher.update(":");
            hasher.update(canonical_header_value(header.get_value_raw()));
            hasher.update("\r\n");
        }

        Ok(hasher.finalize().to_vec())
    }
}

// Matches cfdkim's private canonicalize_header_value_relaxed helper: remove
// CRLF, trim spaces/tabs, and collapse runs of spaces/tabs to a single space.
// Reference at the dependency commit currently recorded in Cargo.lock:
// https://github.com/Lay3rLabs/dkim-wasi/blob/326485409a0a7fe407c2892fb66bdbfe2a243088/src/canonicalization.rs#L104-L131
fn canonical_header_value(value: &[u8]) -> Vec<u8> {
    let mut value = value.to_vec();

    // Replace tabs with spaces (cfdkim's private bytes::replace helper).
    for byte in &mut value {
        if *byte == b'\t' {
            *byte = b' ';
        }
    }

    // Remove CRLF to unfold headers (its private bytes::replace_slice helper).
    let mut i = 0;
    while i + 2 <= value.len() {
        if value[i..].starts_with(b"\r\n") {
            value.drain(i..i + 2);
        } else {
            i += 1;
        }
    }

    // Trim trailing and leading spaces.
    while value.ends_with(b" ") {
        value.remove(value.len() - 1);
    }
    while value.starts_with(b" ") {
        value.remove(0);
    }

    // Collapse consecutive spaces into one, preserving all other bytes.
    let mut previous = false;
    value.retain(|c| {
        if *c == b' ' {
            if previous {
                false
            } else {
                previous = true;
                true
            }
        } else {
            previous = false;
            true
        }
    });

    value
}

fn parse_any(body_bytes: &[u8]) -> Result<EmailMessage, InvalidEmail> {
    let msg = parse_mail(body_bytes).map_err(|_| InvalidEmail::ParseFailed)?;

    let subject = msg
        .headers
        .iter()
        .find(|h| h.get_key().eq_ignore_ascii_case("Subject"))
        .map(|h| h.get_value());

    // DKIM authenticates From, not Sender, Resent-From, or the IMAP envelope.
    let mut senders = msg
        .headers
        .iter()
        .filter(|h| h.get_key_ref().eq_ignore_ascii_case("From"));
    let original_sender = senders
        .next()
        .map(|h| h.get_value())
        .filter(|sender| !sender.trim().is_empty())
        .ok_or(InvalidEmail::SenderMissing)?;
    if senders.next().is_some() {
        return Err(InvalidEmail::SenderAmbiguous);
    }

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
