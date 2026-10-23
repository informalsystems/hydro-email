//! Fetch DKIM public keys over DNS-over-HTTPS and validate their format and
//! strength. Keep missing/invalid keys distinct from temporary lookup failures.

use std::time::Duration;

use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use cfdkim::{dns::Lookup, DKIMError};
use futures::future::BoxFuture;
use rsa::{
    pkcs1::DecodeRsaPublicKey, pkcs8::DecodePublicKey, traits::PublicKeyParts, RsaPublicKey,
};
use simple_dns::{rdata::RData, Name, Packet, PacketFlag, Question, QCLASS, QTYPE, RCODE, TYPE};
use wstd::http::{Body, BodyExt, Client, Request};

pub(super) struct DkimDns;

// Based on cfdkim's WASI DNS-over-HTTPS resolver, with response checks, timeouts,
// a response-size limit, and TXT chunk joining added here.
// https://github.com/Lay3rLabs/dkim-wasi/blob/326485409a0a7fe407c2892fb66bdbfe2a243088/src/dns.rs#L46-L130
impl Lookup for DkimDns {
    fn lookup_txt<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Vec<String>, DKIMError>> {
        Box::pin(async move {
            // cfdkim supplies the selector's DNS name. Encode a TXT query in DNS
            // wire format and send it over HTTPS, which is available under WASI.
            let mut packet = Packet::new_query(0x1234);
            packet.set_flags(PacketFlag::RECURSION_DESIRED);
            packet.questions.push(Question::new(
                Name::new(name)
                    .map_err(|_| DKIMError::SignatureSyntaxError("invalid DNS name".into()))?,
                QTYPE::TYPE(TYPE::TXT),
                QCLASS::CLASS(simple_dns::CLASS::IN),
                false,
            ));
            let bytes = packet
                .build_bytes_vec_compressed()
                .map_err(|_| unavailable())?;
            let request = Request::get(format!(
                "https://cloudflare-dns.com/dns-query?dns={}",
                URL_SAFE_NO_PAD.encode(bytes)
            ))
            .header("accept", "application/dns-message")
            .body(Body::empty())
            .map_err(|_| unavailable())?;
            // Bound network waits so an unavailable DNS service does not hold
            // this poll indefinitely while connecting or waiting for bytes.
            let mut client = Client::new();
            client.set_connect_timeout(Duration::from_secs(5));
            client.set_first_byte_timeout(Duration::from_secs(10));
            client.set_between_bytes_timeout(Duration::from_secs(5));
            let response = client.send(request).await.map_err(|_| unavailable())?;
            if !response.status().is_success() {
                return Err(unavailable());
            }
            // Read incrementally and cap the response at the maximum DNS message
            // size instead of accepting an unbounded HTTP response body.
            let mut body = response.into_body().into_boxed_body();
            let mut bytes = Vec::new();
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame.map_err(|_| unavailable())?.into_data() {
                    if bytes.len() + data.len() > 65_535 {
                        return Err(unavailable());
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            parse_response(&bytes)
        })
    }
}

fn unavailable() -> DKIMError {
    // Use a fixed error rather than exposing transport details in reader errors.
    DKIMError::KeyUnavailable("DNS lookup unavailable".into())
}

fn parse_response(bytes: &[u8]) -> Result<Vec<String>, DKIMError> {
    let response = Packet::parse(bytes).map_err(|_| unavailable())?;
    // Incomplete or unexpected responses cannot establish that a key is absent.
    if response.id() != 0x1234
        || !response.has_flags(PacketFlag::RESPONSE)
        || response.has_flags(PacketFlag::TRUNCATION)
    {
        return Err(unavailable());
    }
    // NXDOMAIN establishes that the key name does not exist. SERVFAIL and other
    // DNS errors may recover, so they must not permanently reject the email.
    match response.rcode() {
        RCODE::NameError => return Err(DKIMError::NoKeyForSignature),
        RCODE::NoError => (),
        _ => return Err(unavailable()),
    }
    // One TXT record can contain several string chunks: join those chunks into
    // one DKIM key, while preserving separate records for the ambiguity check.
    response
        .answers
        .into_iter()
        .filter_map(|answer| match answer.rdata {
            RData::TXT(record) => {
                Some(String::try_from(record).map_err(|_| DKIMError::KeySyntaxError))
            }
            _ => None,
        })
        .collect()
}

// Follows cfdkim's retrieve_public_key parsing, adding our key policy and treating
// malformed keys as permanent failures rather than KeyUnavailable.
// https://github.com/Lay3rLabs/dkim-wasi/blob/326485409a0a7fe407c2892fb66bdbfe2a243088/src/public_key.rs#L12-L85
pub(super) fn validate_key_records(records: &[String]) -> Result<(), DKIMError> {
    // A selector must resolve to exactly one key record; choosing among multiple
    // records would make authentication ambiguous.
    let [record] = records else {
        return Err(if records.is_empty() {
            DKIMError::NoKeyForSignature
        } else {
            DKIMError::KeySyntaxError
        });
    };
    // Require the whole record to parse and each tag to occur once, so our
    // policy checks and cfdkim cannot interpret the same key differently.
    let (rest, tags) = cfdkim::parse_tag_list(record).map_err(|_| DKIMError::KeySyntaxError)?;
    let mut names = std::collections::HashSet::new();
    if !rest.trim().is_empty() || tags.iter().any(|tag| !names.insert(&tag.name)) {
        return Err(DKIMError::KeySyntaxError);
    }
    let tag = |name| {
        tags.iter()
            .find(|tag| tag.name == name)
            .map(|tag| tag.value.as_str())
    };
    // v= identifies the DKIM key format; h= optionally limits allowed hashes.
    // Our signature policy permits SHA-256 only.
    if tag("v").is_some_and(|v| v != "DKIM1") {
        return Err(DKIMError::KeyIncompatibleVersion);
    }
    if tag("h").is_some_and(|h| !h.split(':').any(|h| h == "sha256")) {
        return Err(DKIMError::InappropriateKeyAlgorithm);
    }
    // p= contains the base64 public key. An empty value explicitly revokes it.
    let key = tag("p")
        .filter(|key| !key.is_empty())
        .ok_or(DKIMError::NoKeyForSignature)?;
    let key = STANDARD
        .decode(key)
        .map_err(|_| DKIMError::KeySyntaxError)?;
    // k= defaults to RSA. Check key encoding and strength here so malformed keys
    // are permanent failures rather than the library's generic unavailable key.
    match tag("k").unwrap_or("rsa") {
        "rsa" => {
            // Accept both common DER encodings and require at least 1024 bits.
            let key = RsaPublicKey::from_public_key_der(&key)
                .or_else(|_| RsaPublicKey::from_pkcs1_der(&key))
                .map_err(|_| DKIMError::KeySyntaxError)?;
            if key.n().bits() < 1024 {
                return Err(DKIMError::InappropriateKeyAlgorithm);
            }
        }
        "ed25519" => {
            // Ed25519 uses a raw 32-byte public key, which must decode to a point.
            let key = key
                .as_slice()
                .try_into()
                .map_err(|_| DKIMError::KeySyntaxError)?;
            ed25519_dalek::VerifyingKey::from_bytes(key).map_err(|_| DKIMError::KeySyntaxError)?;
        }
        _ => return Err(DKIMError::InappropriateKeyAlgorithm),
    }
    Ok(())
}
