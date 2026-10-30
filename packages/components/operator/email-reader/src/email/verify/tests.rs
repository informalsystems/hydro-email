use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use cfdkim::{canonicalization::Type, DkimPrivateKey, SignerBuilder};
use ed25519_dalek::SigningKey;

const RAW: &str = "From: Alice <Alice@example.com>\r\nSubject: deposit\r\nMessage-ID: <test@example.com>\r\nDate: Thu, 22 Oct 2026 12:00:00 +0000\r\n\r\nTest body\r\n";
const HEADERS: &[&str] = &["From", "Subject", "Message-ID", "Date"];

fn signature(raw: &str, domain: &str, selector: &str, headers: &[&str]) -> String {
    // Deterministic key used only for these tests, with no network or real mail.
    let logger = NullLoggerBuilder.build().unwrap();
    SignerBuilder::new()
        .with_signed_headers(headers)
        .unwrap()
        .with_private_key(DkimPrivateKey::Ed25519(SigningKey::from_bytes(&[42; 32])))
        .with_selector(selector)
        .with_signing_domain(domain)
        .with_logger(&logger)
        .with_header_canonicalization(Type::Relaxed)
        .build()
        .unwrap()
        .sign(&mailparse::parse_mail(raw.as_bytes()).unwrap())
        .unwrap()
}

fn signed(raw: &str) -> String {
    format!(
        "{}\r\n{raw}",
        signature(raw, "example.com", "test", HEADERS)
    )
}

struct TestDns;
impl Lookup for TestDns {
    fn lookup_txt<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Vec<String>, DKIMError>> {
        Box::pin(async move {
            if name.starts_with("down.") {
                return Err(DKIMError::Http("test outage".into()));
            }
            if name.starts_with("unavailable.") {
                return Err(DKIMError::KeyUnavailable("test lookup outage".into()));
            }
            if name.starts_with("missing.") {
                return Err(DKIMError::NoKeyForSignature);
            }
            assert_eq!(name.to_ascii_lowercase(), "test._domainkey.example.com");
            let key = SigningKey::from_bytes(&[42; 32]).verifying_key();
            Ok(vec![format!(
                "v=DKIM1; k=ed25519; p={}",
                STANDARD.encode(key.to_bytes())
            )])
        })
    }
}

fn verify(raw: &str) -> AppResult<()> {
    let email = EmailMessage::parse_rest_api(raw.as_bytes()).unwrap();
    futures::executor::block_on(verify_with_resolver(&email, Arc::new(TestDns)))
}

fn assert_rejected(raw: &str) {
    assert!(matches!(verify(raw), Err(AppError::InvalidEmail(_))));
}

#[test]
fn accepts_real_signature_and_rejects_unsigned_wrong_domain_or_tampered_mail() {
    let valid = signed(RAW);
    verify(&valid).unwrap();
    let quoted = RAW.replace("Alice <Alice@example.com>", "\"a  b\"@example.com");
    assert_rejected(&signed(&quoted));
    assert_rejected(RAW);
    assert_rejected(&format!(
        "{}\r\n{RAW}",
        signature(RAW, "attacker.example", "test", HEADERS)
    ));
    for tampered in [
        valid.replace("Alice@example.com", "victim@example.com"),
        valid.replace(
            "Subject: deposit",
            "Subject: withdraw destination uatom 100",
        ),
        valid.replace("<test@example.com>", "<new@example.com>"),
        valid.replace("Test body", "Different body"),
    ] {
        assert_rejected(&tampered);
    }
}

#[test]
fn requires_signed_instruction_and_identity_without_ambiguous_headers() {
    for headers in [
        &["From"][..],
        &["From", "Subject"][..],
        &["From", "Message-ID"][..],
    ] {
        assert_rejected(&format!(
            "{}\r\n{RAW}",
            signature(RAW, "example.com", "test", headers)
        ));
    }
    let valid = signed(RAW);
    assert_rejected(&valid.replace("a=ed25519-sha256", "a=rsa-sha1"));
    assert_rejected(&format!("Subject: withdraw destination uatom 1\r\n{valid}"));
    assert_rejected(&format!("Message-ID: <injected@example.com>\r\n{valid}"));
    // A passing weak signature cannot bypass the policy on another aligned signature.
    assert_rejected(&format!(
        "{}\r\n{valid}",
        signature(RAW, "example.com", "test", &["From"])
    ));
    let fallback = RAW.replace("Message-ID: <test@example.com>\r\n", "");
    verify(&signed(&fallback)).unwrap();
    assert_rejected(&format!(
        "{}\r\n{fallback}",
        signature(&fallback, "example.com", "test", &["From", "Subject"])
    ));
}

#[test]
fn author_identity_comes_only_from_one_from_header() {
    let forwarded = format!(
        "Resent-From: victim@example.com\r\nSender: victim@example.com\r\n{}",
        signed(RAW)
    );
    verify(&forwarded).unwrap();
    let email = EmailMessage::parse_rest_api(forwarded.as_bytes()).unwrap();
    assert_eq!(email.original_sender, "Alice <Alice@example.com>");
    assert!(matches!(
        EmailMessage::parse_rest_api(
            format!("From: victim@example.com\r\n{}", signed(RAW)).as_bytes()
        ),
        Err(InvalidEmail::SenderAmbiguous)
    ));
    assert!(matches!(
        EmailMessage::parse_rest_api(b"Sender: victim@example.com\r\nSubject: deposit\r\n\r\n"),
        Err(InvalidEmail::SenderMissing)
    ));
}

#[test]
fn lookup_outages_remain_retryable_even_if_a_later_signature_has_no_key() {
    let down = signature(RAW, "example.com", "down", HEADERS);
    let missing = signature(RAW, "example.com", "missing", HEADERS);
    for raw in [
        format!(
            "{}\r\n{RAW}",
            signature(RAW, "example.com", "unavailable", HEADERS)
        ),
        format!("{down}\r\n{RAW}"),
        format!("{down}\r\n{missing}\r\n{RAW}"),
    ] {
        assert!(matches!(
            verify(&raw),
            Err(AppError::VerificationUnavailable)
        ));
    }
    // Any passing policy-compliant signature is enough, despite another key's outage.
    verify(&format!("{down}\r\n{}", signed(RAW))).unwrap();
    assert_rejected(&format!("{missing}\r\n{RAW}"));
}

#[test]
fn library_key_errors_are_not_assumed_to_be_lookup_outages() {
    assert!(!temporary_error(&DKIMError::KeyUnavailable(
        "malformed public key".into()
    )));
    assert!(temporary_error(&DKIMError::Http("outage".into())));
}

#[test]
fn relaxed_header_whitespace_cannot_create_a_new_event_id() {
    let valid = signed(RAW);
    let changed = valid
        .replace("Subject: deposit", "sUbJeCt:\t deposit  ")
        .replace(
            "Alice <Alice@example.com>",
            "Alice\t \r\n <Alice@example.com>",
        )
        .replace(
            "Message-ID: <test@example.com>",
            "Message-ID:\t<test@example.com>\t",
        );
    verify(&valid).unwrap();
    verify(&changed).unwrap();
    let salt = |raw: &str| {
        EmailMessage::parse_rest_api(raw.as_bytes())
            .unwrap()
            .event_id_salt()
            .unwrap()
    };
    assert_eq!(salt(&valid), salt(&changed));
    assert_ne!(
        salt(&valid),
        salt(&signed(
            &RAW.replace("<test@example.com>", "<another@example.com>")
        ))
    );
}

#[test]
fn opaque_multipart_body_remains_authenticated_and_commands_are_unchanged() {
    for subject in [
        "deposit",
        "withdraw recipient uatom 42",
        "withdraw_receipt recipient receipt 1",
    ] {
        let raw = RAW.replace("Subject: deposit", &format!("Subject: {subject}"))
            .replace("\r\n\r\nTest body\r\n", "\r\nContent-Type: multipart/mixed; boundary=outer\r\n\r\n--outer\r\nContent-Type: text/plain\r\n\r\nTest body\r\n--outer--\r\n");
        let signed = signed(&raw);
        verify(&signed).unwrap();
        let email = EmailMessage::parse_imap(signed.as_bytes()).unwrap();
        email.validate_subject().unwrap();
        assert_eq!(email.subject.as_deref(), Some(subject));
        // MIME syntax is ignored for commands, but its exact bytes remain signed.
        assert_rejected(&signed.replace("--outer--", "--changed--"));
        assert_rejected(&signed.replace("Test body", "changed body"));
    }
}
