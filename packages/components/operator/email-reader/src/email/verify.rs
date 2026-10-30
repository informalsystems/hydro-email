//! Authenticate the sender and the headers used to build an action. Invalid mail
//! is rejected permanently; unavailable verification services leave it retryable.

mod dns;

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use cfdkim::{dns::Lookup, DKIMError};
use futures::future::BoxFuture;
use sloggers::{null::NullLoggerBuilder, Build};

use crate::{
    email::parser::{EmailMessage, InvalidEmail},
    error::{AppError, AppResult},
};

const MAX_ALIGNED_SIGNATURES: usize = 3;

pub async fn verify_email(email: &EmailMessage) -> AppResult<()> {
    verify_with_resolver(email, Arc::new(dns::DkimDns)).await
}

async fn verify_with_resolver(email: &EmailMessage, lookup: Arc<dyn Lookup>) -> AppResult<()> {
    // Use the From mailbox as the author. A passing signature must belong to
    // this exact domain, rather than an unrelated domain that also signed mail.
    let from_domain = email.sender_domain()?.to_ascii_lowercase();
    let parsed = email.dkim_view()?;
    // Parse the From value into addresses and require exactly one individual
    // mailbox. Address groups and lists of multiple senders are not accepted.
    let addresses = mailparse::addrparse(&email.original_sender)
        .map_err(|_| InvalidEmail::AuthenticationFailed)?;
    let [mailparse::MailAddr::Single(author)] = addresses.as_slice() else {
        return Err(InvalidEmail::AuthenticationFailed.into());
    };
    // Inspect only the mailbox address, excluding its optional display name:
    //   "Alice Smith" <alice@example.com> -> not flagged
    //   "alice smith"@example.com         -> flagged
    // Whitespace, quotes, or escape characters indicate an address that needs
    // the stricter canonicalization check below. This flag alone rejects nothing.
    let quoted_address = author
        .addr
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'"' | b'\\'));

    // These are the inputs that identify the sender, instruction, and event.
    // Match the event-salt helper's fallback to Date when Message-ID is absent.
    let identity_header = if email.message_id().is_some() {
        "Message-ID"
    } else {
        "Date"
    };
    let required = ["From", "Subject", identity_header];
    // Reject duplicate headers: our parser reads the first occurrence, while
    // DKIM selects occurrences starting from the last. With two Subjects, DKIM
    // could verify a signed "deposit" while we read an unsigned withdrawal.
    // Requiring exactly one of each header ensures we use the signed values.
    for name in required {
        if parsed
            .headers
            .iter()
            .filter(|h| h.get_key_ref().eq_ignore_ascii_case(name))
            .count()
            != 1
        {
            return Err(InvalidEmail::AuthenticationFailed.into());
        }
    }

    // Inspect signature metadata first. Cryptographic verification below checks
    // that at least one signature satisfying our policy actually passes.
    let mut aligned = 0;
    for header in parsed
        .headers
        .iter()
        .filter(|h| h.get_key_ref().eq_ignore_ascii_case("DKIM-Signature"))
    {
        let raw = String::from_utf8_lossy(header.get_value_raw());
        let Ok((rest, tags)) = cfdkim::parse_tag_list(&raw) else {
            // An unparseable signature cannot authenticate the email. Continue
            // looking for another signature that can.
            continue;
        };
        // cfdkim retains the last duplicate tag. Reject ambiguous tag lists before
        // using any tag to decide which signature is allowed to authenticate us.
        let mut names = HashSet::new();
        if !rest.trim().is_empty() || tags.iter().any(|tag| !names.insert(tag.name.as_str())) {
            return Err(InvalidEmail::AuthenticationFailed.into());
        }
        let tag = |name| {
            tags.iter()
                .find(|tag| tag.name == name)
                .map(|tag| tag.value.as_str())
        };
        // d= identifies the signing domain. Only the From domain may authenticate
        // this sender; signatures from other domains do not satisfy our policy.
        if !tag("d").is_some_and(|domain| domain.eq_ignore_ascii_case(&from_domain)) {
            continue;
        }
        aligned += 1;
        if aligned > MAX_ALIGNED_SIGNATURES {
            return Err(InvalidEmail::TooManySignatures.into());
        }
        // Reject the entire email if its mailbox address contains whitespace,
        // quotes, or backslashes AND this signature uses relaxed header rules.
        // For example, these could be mailboxes belonging to different users:
        //   "alice  smith"@example.com  (two spaces)
        //   "alice smith"@example.com   (one space)
        // Relaxed DKIM treats those spaces identically: an attacker could change
        // the address between them while keeping the signature valid. We cannot
        // safely identify the sender in that case, so we reject the email.
        // Ordinary alice@example.com and the display name in
        // "Alice Smith" <alice@example.com> do not trigger this restriction.
        // c= selects header/body rules; the part before '/' controls headers.
        // If c= is absent, DKIM defaults to simple/simple.
        if quoted_address
            && tag("c").unwrap_or("simple/simple").split('/').next() == Some("relaxed")
        {
            return Err(InvalidEmail::AuthenticationFailed.into());
        }

        // h= must include From, Subject, and Message-ID (or Date): an unsigned
        // value could be changed without breaking the signature.
        // a= must be rsa-sha256 or ed25519-sha256.
        // Check every matching signature because the library doesn't identify
        // which one passed; that pass must never come from a signature that
        // leaves our instruction or event identity unsigned.
        let covered = tag("h").is_some_and(|headers| {
            required.iter().all(|name| {
                headers
                    .split(':')
                    .any(|signed| signed.trim().eq_ignore_ascii_case(name))
            })
        });
        if !covered || !matches!(tag("a"), Some("rsa-sha256" | "ed25519-sha256")) {
            return Err(InvalidEmail::AuthenticationFailed.into());
        }
    }
    if aligned == 0 {
        return Err(InvalidEmail::AuthenticationFailed.into());
    }

    // Validate DNS keys before the library uses them, and remember lookup
    // outages across all signatures so they are not mistaken for invalid mail.
    let resolver = Arc::new(CheckedLookup {
        inner: lookup,
        temporary_failure: AtomicBool::new(false),
    });
    let result = cfdkim::verify_email_with_resolver(
        &NullLoggerBuilder.build()?,
        &from_domain,
        &parsed,
        resolver.clone(),
    )
    .await;
    // A verified signature is sufficient even if another lookup failed. Without
    // a pass, any temporary failure means we must retry rather than reject.
    match result {
        Ok(result)
            if result.summary() == "pass"
                && result.domain_used().eq_ignore_ascii_case(&from_domain) =>
        {
            Ok(())
        }
        Ok(result) => {
            if resolver.temporary_failure.load(Ordering::Relaxed)
                || result.error().as_ref().is_some_and(temporary_error)
            {
                Err(AppError::VerificationUnavailable)
            } else {
                Err(InvalidEmail::AuthenticationFailed.into())
            }
        }
        Err(error)
            if resolver.temporary_failure.load(Ordering::Relaxed) || temporary_error(&error) =>
        {
            Err(AppError::VerificationUnavailable)
        }
        Err(_) => Err(InvalidEmail::AuthenticationFailed.into()),
    }
}

// Transport and internal errors mean verification could not finish, not that the
// email is invalid. Leave it unread for retry: services may recover, and internal
// faults may need an operator fix. Recovery is not guaranteed.
// KeyUnavailable is excluded here: cfdkim also uses it for malformed keys.
// Only CheckedLookup can classify it as a lookup outage, before key validation.
// Don't use DKIMError::status(): the fork misclassifies transport errors and may
// panic for signing/builder errors (unexpected on this verification path).
fn temporary_error(error: &DKIMError) -> bool {
    matches!(
        error,
        DKIMError::Http(_)
            | DKIMError::SimpleDns(_)
            | DKIMError::UnknownInternalError(_)
            | DKIMError::BuilderError(_)
            | DKIMError::FailedToSign(_)
    )
}

// Wrap either the production DNS resolver or a test resolver with the same key
// policy. The flag records an outage; it does not contain email or DNS details.
struct CheckedLookup {
    inner: Arc<dyn Lookup>,
    temporary_failure: AtomicBool,
}

impl Lookup for CheckedLookup {
    fn lookup_txt<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Vec<String>, DKIMError>> {
        Box::pin(async move {
            let result = self.inner.lookup_txt(name).await;
            if result.as_ref().err().is_some_and(|error| {
                // Our DNS resolver uses KeyUnavailable for failed lookups.
                // At this boundary no key has been returned or decoded yet;
                // the same variant from cfdkim later is not assumed temporary.
                temporary_error(error) || matches!(error, DKIMError::KeyUnavailable(_))
            }) {
                // cfdkim keeps only its final signature error; retain outages from
                // earlier signatures so a later failure cannot cause rejection.
                self.temporary_failure.store(true, Ordering::Relaxed);
            }
            let records = result?;
            dns::validate_key_records(&records)?;
            Ok(records)
        })
    }
}

#[cfg(test)]
mod tests;
