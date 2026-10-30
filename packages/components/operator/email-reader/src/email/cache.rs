use std::{
    collections::{BTreeMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, ensure, Result};
use sha2::{Digest, Sha256};

use crate::{
    config::{ImapConfig, ImapCredentials},
    wasi::keyvalue::store,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MessageId {
    pub uid_validity: u32,
    pub uid: u32,
}

impl MessageId {
    fn key(self) -> String {
        format!("{}/{}", self.uid_validity, self.uid)
    }

    fn parse(key: &str) -> Result<Self> {
        let (validity, uid) = key
            .split_once('/')
            .ok_or_else(|| anyhow!("Invalid email cache key"))?;
        let id = Self {
            uid_validity: validity.parse()?,
            uid: uid.parse()?,
        };
        ensure!(
            id.uid_validity != 0 && id.uid != 0,
            "Invalid email cache identifier"
        );
        Ok(id)
    }
}

pub trait CacheStore {
    fn keys(&self) -> Result<Vec<String>>;
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    fn set(&self, key: &str, value: &[u8]) -> Result<()>;
    fn remember(&self, key: &str) -> Result<()> {
        self.set(key, &[])
    }
    fn remove(&self, key: &str) -> Result<()>;
}

pub struct WavsStore(store::Bucket);

impl CacheStore for WavsStore {
    fn keys(&self) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut cursor = None;
        loop {
            let page = self
                .0
                .list_keys(cursor.as_deref())
                .map_err(|e| anyhow!("Email cache lookup failed: {e:?}"))?;
            keys.extend(page.keys);
            cursor = page.cursor;
            if cursor.is_none() {
                return Ok(keys);
            }
        }
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.0
            .get(key)
            .map_err(|e| anyhow!("Email cache lookup failed: {e:?}"))
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<()> {
        self.0
            .set(key, value)
            .map_err(|e| anyhow!("Email cache write failed: {e:?}"))
    }

    fn remove(&self, key: &str) -> Result<()> {
        self.0
            .delete(key)
            .map_err(|e| anyhow!("Email cache cleanup failed: {e:?}"))
    }
}

pub struct ProcessedEmails<S = WavsStore> {
    store: S,
    // Empty values mean handled; nonempty values hold a temporary retry schedule.
    snapshot: BTreeMap<MessageId, Option<Retry>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Retry {
    // Verification retries; zero means verification has not been deferred.
    attempts: u32,
    first_failure_at: u64,
    retry_after: u64,
    #[serde(default)]
    processing_attempts: u32,
}

const MAX_PROCESSING_ATTEMPTS: u32 = 3;

const MAX_RETRY_AGE_SECONDS: u64 = 3600;
const MAX_RETRY_DELAY_SECONDS: u64 = 300;

fn now_seconds() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn bucket_name(config: &ImapConfig) -> Result<String> {
    let ImapCredentials::Plain { username, .. } = &config.credentials;
    // Separate mailbox accounts without storing credentials or addresses in keys.
    let identity = serde_json::to_vec(&(
        config.host.to_ascii_lowercase(),
        config.port,
        config.tls,
        username,
        "INBOX",
    ))?;
    Ok(format!(
        "email-reader-processed-v1-{}",
        const_hex::encode(Sha256::digest(identity))
    ))
}

impl ProcessedEmails {
    pub fn open(config: &ImapConfig) -> Result<Self> {
        let bucket = store::open(&bucket_name(config)?)
            .map_err(|e| anyhow!("Cannot open email cache: {e:?}"))?;
        Self::load(WavsStore(bucket))
    }
}

impl<S: CacheStore> ProcessedEmails<S> {
    pub(crate) fn load(store: S) -> Result<Self> {
        let mut snapshot = BTreeMap::new();
        for key in store.keys()? {
            // A concurrent execution may remove a key after it was listed.
            if let Some(value) = store.get(&key)? {
                let retry = if value.is_empty() {
                    None
                } else {
                    Some(serde_json::from_slice(&value)?)
                };
                snapshot.insert(MessageId::parse(&key)?, retry);
            }
        }
        Ok(Self { store, snapshot })
    }

    // Call only after a successful, complete UID SEARCH UNSEEN. Limit cleanup to
    // the snapshot taken before the search, preserving entries added meanwhile.
    pub fn next_unread(
        &self,
        uid_validity: u32,
        unread: &HashSet<u32>,
    ) -> Result<Option<MessageId>> {
        self.next_unread_at(uid_validity, unread, now_seconds()?)
    }

    fn next_unread_at(
        &self,
        uid_validity: u32,
        unread: &HashSet<u32>,
        now: u64,
    ) -> Result<Option<MessageId>> {
        ensure!(uid_validity != 0, "Missing IMAP UIDVALIDITY");
        for id in self.snapshot.keys() {
            if id.uid_validity != uid_validity || !unread.contains(&id.uid) {
                self.store.remove(&id.key())?;
            }
        }
        Ok(unread
            .iter()
            .copied()
            .map(|uid| MessageId { uid_validity, uid })
            .filter(|id| match self.snapshot.get(id) {
                None => true,
                Some(Some(retry)) => retry.retry_after <= now,
                Some(None) => false,
            })
            .min())
    }

    // Persist before fetch/parsing so traps and unclassified errors cannot pin
    // the oldest UID on every poll. This is a retry reservation, not completion.
    pub fn begin_processing(&self, id: MessageId) -> Result<bool> {
        self.begin_processing_at(id, now_seconds()?)
    }

    fn begin_processing_at(&self, id: MessageId, now: u64) -> Result<bool> {
        let previous = self.snapshot.get(&id).and_then(Option::as_ref);
        let attempts = previous.map_or(0, |retry| retry.processing_attempts);
        let retry = Retry {
            attempts: previous.map_or(0, |retry| retry.attempts),
            first_failure_at: previous.map_or(now, |retry| retry.first_failure_at),
            retry_after: now.saturating_add(30),
            processing_attempts: attempts.saturating_add(1),
        };
        self.store.set(&id.key(), &serde_json::to_vec(&retry)?)?;
        Ok(attempts < MAX_PROCESSING_ATTEMPTS)
    }

    pub fn remember(&self, id: MessageId) -> Result<()> {
        // Replacing retry metadata with an empty value ends retries after either
        // successful processing or a confirmed permanent rejection.
        self.store.remember(&id.key())
    }

    pub fn defer(&self, id: MessageId) -> Result<()> {
        self.defer_at(id, now_seconds()?)
    }

    pub fn verification_expired(&self, id: MessageId) -> Result<bool> {
        Ok(self.verification_expired_at(id, now_seconds()?))
    }

    fn verification_expired_at(&self, id: MessageId, now: u64) -> bool {
        self.snapshot
            .get(&id)
            .and_then(Option::as_ref)
            .is_some_and(|retry| {
                retry.attempts > 0
                    && now >= retry.first_failure_at.saturating_add(MAX_RETRY_AGE_SECONDS)
            })
    }

    fn defer_at(&self, id: MessageId, now: u64) -> Result<()> {
        let previous = self
            .snapshot
            .get(&id)
            .and_then(Option::as_ref)
            .filter(|retry| retry.attempts > 0);
        let attempts = previous.map_or(1, |retry| retry.attempts.saturating_add(1));
        // Preserve the original failure time across retries; later failures must
        // not extend the one-hour deadline. Operator restarts reset this cache.
        let first_failure_at = previous.map_or(now, |retry| retry.first_failure_at);
        let deadline = first_failure_at.saturating_add(MAX_RETRY_AGE_SECONDS);
        // Retry after 30s, 1m, 2m, etc., capped at 5m and never past expiry.
        // Skip this UID until then, allowing newer mail to proceed while it stays unread.
        let delay = (30_u64 << attempts.saturating_sub(1).min(4)).min(MAX_RETRY_DELAY_SECONDS);
        let retry = Retry {
            attempts,
            first_failure_at,
            retry_after: now.saturating_add(delay).min(deadline),
            processing_attempts: 0,
        };
        self.store.set(&id.key(), &serde_json::to_vec(&retry)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone, Default)]
    struct MemoryStore(Rc<RefCell<BTreeMap<String, Vec<u8>>>>);

    impl CacheStore for MemoryStore {
        fn keys(&self) -> Result<Vec<String>> {
            Ok(self.0.borrow().keys().cloned().collect())
        }
        fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
            Ok(self.0.borrow().get(key).cloned())
        }
        fn set(&self, key: &str, value: &[u8]) -> Result<()> {
            self.0.borrow_mut().insert(key.into(), value.to_vec());
            Ok(())
        }
        fn remove(&self, key: &str) -> Result<()> {
            self.0.borrow_mut().remove(key);
            Ok(())
        }
    }

    fn id(uid_validity: u32, uid: u32) -> MessageId {
        MessageId { uid_validity, uid }
    }
    fn unread(uids: &[u32]) -> HashSet<u32> {
        uids.iter().copied().collect()
    }
    fn config(host: &str, username: &str, password: &str) -> ImapConfig {
        ImapConfig {
            host: host.into(),
            port: 993,
            tls: true,
            credentials: ImapCredentials::Plain {
                username: username.into(),
                password: password.into(),
            },
        }
    }

    #[test]
    fn unfinished_attempts_back_off_allow_newer_mail_and_exhaust_the_budget() -> Result<()> {
        let store = MemoryStore::default();
        let poison = id(1, 10);
        for attempt in 0..MAX_PROCESSING_ATTEMPTS {
            let now = 1000 + u64::from(attempt) * 30;
            // Reload between attempts to model a failed invocation, including a trap.
            let cache = ProcessedEmails::load(store.clone())?;
            assert_eq!(
                cache.next_unread_at(1, &unread(&[10, 11]), now)?,
                Some(poison)
            );
            assert!(cache.begin_processing_at(poison, now)?);
            let next = ProcessedEmails::load(store.clone())?;
            assert_eq!(
                next.next_unread_at(1, &unread(&[10, 11]), now + 1)?,
                Some(id(1, 11))
            );
        }
        let cache = ProcessedEmails::load(store.clone())?;
        assert!(!cache.begin_processing_at(poison, 1090)?);
        // Only a confirmed rejection ends retries permanently.
        cache.remember(poison)?;
        assert_eq!(
            ProcessedEmails::load(store)?.next_unread_at(1, &unread(&[10, 11]), 1120)?,
            Some(id(1, 11))
        );
        Ok(())
    }

    #[test]
    fn classified_verification_outages_do_not_exhaust_the_processing_budget() -> Result<()> {
        let store = MemoryStore::default();
        let message = id(1, 10);
        for attempt in 0..10 {
            let now = 1000 + attempt * 300;
            let cache = ProcessedEmails::load(store.clone())?;
            assert!(cache.begin_processing_at(message, now)?);
            cache.defer_at(message, now)?;
        }
        assert!(!ProcessedEmails::load(store.clone())?.verification_expired_at(message, 4599));
        assert!(ProcessedEmails::load(store)?.verification_expired_at(message, 4600));
        Ok(())
    }

    #[test]
    fn repeated_polls_process_oldest_uncached_message() -> Result<()> {
        let store = MemoryStore::default();
        for expected in [1, 2, 8] {
            let cache = ProcessedEmails::load(store.clone())?;
            let next = cache.next_unread(10, &unread(&[8, 2, 1]))?.unwrap();
            assert_eq!(next, id(10, expected));
            cache.remember(next)?;
        }
        assert_eq!(
            ProcessedEmails::load(store)?.next_unread(10, &unread(&[1, 2, 8]))?,
            None
        );
        Ok(())
    }

    #[test]
    fn cleanup_handles_gaps_in_unread_uids() -> Result<()> {
        let store = MemoryStore::default();
        for uid in [10, 11, 12] {
            store.remember(&id(1, uid).key())?;
        }
        let cache = ProcessedEmails::load(store.clone())?;
        assert_eq!(
            cache.next_unread(1, &unread(&[10, 12, 13]))?,
            Some(id(1, 13))
        );
        assert_eq!(store.keys()?, ["1/10", "1/12"]);
        Ok(())
    }

    #[test]
    fn successful_empty_search_clears_cache() -> Result<()> {
        let store = MemoryStore::default();
        store.remember("1/10")?;
        assert_eq!(
            ProcessedEmails::load(store.clone())?.next_unread(1, &unread(&[]))?,
            None
        );
        assert!(store.keys()?.is_empty());
        Ok(())
    }

    #[test]
    fn new_uid_validity_allows_reused_uids() -> Result<()> {
        let store = MemoryStore::default();
        store.remember("1/10")?;
        assert_eq!(
            ProcessedEmails::load(store.clone())?.next_unread(2, &unread(&[10]))?,
            Some(id(2, 10))
        );
        assert!(store.keys()?.is_empty());
        Ok(())
    }

    #[test]
    fn operators_and_restarts_have_independent_memory() -> Result<()> {
        let operator = MemoryStore::default();
        ProcessedEmails::load(operator.clone())?.remember(id(1, 10))?;
        assert_eq!(
            ProcessedEmails::load(operator.clone())?.next_unread(1, &unread(&[10]))?,
            None
        );
        let other_operator = MemoryStore::default();
        assert_eq!(
            ProcessedEmails::load(other_operator)?.next_unread(1, &unread(&[10]))?,
            Some(id(1, 10))
        );
        // Model a WAVS restart by clearing its host memory between executions.
        operator.0.borrow_mut().clear();
        assert_eq!(
            ProcessedEmails::load(operator)?.next_unread(1, &unread(&[10]))?,
            Some(id(1, 10))
        );
        Ok(())
    }

    #[test]
    fn selection_does_not_cache_unfinished_processing() -> Result<()> {
        let store = MemoryStore::default();
        for _ in 0..2 {
            assert_eq!(
                ProcessedEmails::load(store.clone())?.next_unread(1, &unread(&[10]))?,
                Some(id(1, 10))
            );
        }
        assert!(store.keys()?.is_empty());
        Ok(())
    }

    #[test]
    fn deferred_mail_allows_newer_mail_and_retries_with_capped_backoff() -> Result<()> {
        let store = MemoryStore::default();
        let mut now = 1000;
        for delay in [30, 60, 120, 240, 300, 300] {
            let cache = ProcessedEmails::load(store.clone())?;
            assert_eq!(
                cache.next_unread_at(1, &unread(&[10, 11]), now)?,
                Some(id(1, 10))
            );
            cache.defer_at(id(1, 10), now)?;
            let cache = ProcessedEmails::load(store.clone())?;
            assert_eq!(
                cache.next_unread_at(1, &unread(&[10, 11]), now + delay - 1)?,
                Some(id(1, 11))
            );
            assert_eq!(
                cache.next_unread_at(1, &unread(&[10]), now + delay - 1)?,
                None
            );
            now += delay;
        }
        // Success or a confirmed rejection replaces retry metadata with handled.
        ProcessedEmails::load(store.clone())?.remember(id(1, 10))?;
        assert_eq!(
            ProcessedEmails::load(store)?.next_unread_at(1, &unread(&[10, 11]), now)?,
            Some(id(1, 11))
        );
        Ok(())
    }

    #[test]
    fn verification_expires_one_hour_after_first_failure() -> Result<()> {
        let store = MemoryStore::default();
        let message = id(1, 10);
        let cache = ProcessedEmails::load(store.clone())?;
        assert!(!cache.verification_expired_at(message, 1000));
        cache.defer_at(message, 1000)?;

        // A late retry must preserve the original deadline and become eligible
        // for rejection at that deadline, rather than waiting another five minutes.
        for now in [1030, 1090, 1210, 1450, 4500] {
            ProcessedEmails::load(store.clone())?.defer_at(message, now)?;
        }
        let cache = ProcessedEmails::load(store.clone())?;
        assert!(!cache.verification_expired_at(message, 4599));
        assert_eq!(cache.next_unread_at(1, &unread(&[10]), 4599)?, None);
        assert!(cache.verification_expired_at(message, 4600));
        assert_eq!(
            cache.next_unread_at(1, &unread(&[10]), 4600)?,
            Some(message)
        );
        // Until the backend confirms rejection, expiry survives later polls.
        assert!(ProcessedEmails::load(store.clone())?.verification_expired_at(message, 4700));
        cache.remember(message)?;
        assert_eq!(
            ProcessedEmails::load(store)?.next_unread_at(1, &unread(&[10]), 4700)?,
            None
        );
        Ok(())
    }

    #[test]
    fn retry_entries_are_cleaned_up_and_reset_with_operator_memory() -> Result<()> {
        let store = MemoryStore::default();
        ProcessedEmails::load(store.clone())?.defer_at(id(1, 10), 1000)?;
        // Mail marked read elsewhere no longer needs a retry entry.
        ProcessedEmails::load(store.clone())?.next_unread_at(1, &unread(&[]), 1001)?;
        assert!(store.keys()?.is_empty());

        ProcessedEmails::load(store.clone())?.defer_at(id(1, 10), 1000)?;
        assert_eq!(
            ProcessedEmails::load(store.clone())?.next_unread_at(2, &unread(&[10]), 1001)?,
            Some(id(2, 10))
        );
        assert!(store.keys()?.is_empty());

        ProcessedEmails::load(store.clone())?.defer_at(id(2, 10), 1000)?;
        // A WAVS restart clears local memory, so unread mail is eligible again.
        store.0.borrow_mut().clear();
        assert_eq!(
            ProcessedEmails::load(store)?.next_unread_at(2, &unread(&[10]), 1001)?,
            Some(id(2, 10))
        );
        Ok(())
    }

    #[test]
    fn loading_cache_without_a_successful_search_does_not_remove_entries() -> Result<()> {
        let store = MemoryStore::default();
        store.remember("1/10")?;
        let _cache = ProcessedEmails::load(store.clone())?;
        assert_eq!(store.keys()?, ["1/10"]);
        Ok(())
    }

    #[test]
    fn stale_search_cannot_delete_entries_added_after_snapshot() -> Result<()> {
        let store = MemoryStore::default();
        let cache = ProcessedEmails::load(store.clone())?;
        store.remember("1/10")?;
        cache.next_unread(1, &unread(&[]))?;
        assert_eq!(store.keys()?, ["1/10"]);
        Ok(())
    }

    #[test]
    fn missing_uid_validity_does_not_clear_cache() -> Result<()> {
        let store = MemoryStore::default();
        store.remember("1/10")?;
        assert!(ProcessedEmails::load(store.clone())?
            .next_unread(0, &unread(&[]))
            .is_err());
        assert_eq!(store.keys()?, ["1/10"]);
        Ok(())
    }

    #[test]
    fn mailbox_namespace_excludes_password_and_preserves_account_identity() -> Result<()> {
        let original = bucket_name(&config("IMAP.example", "user@example", "old"))?;
        assert_eq!(
            original,
            bucket_name(&config("imap.example", "user@example", "new"))?
        );
        assert_ne!(
            original,
            bucket_name(&config("other.example", "user@example", "old"))?
        );
        assert_ne!(
            original,
            bucket_name(&config("imap.example", "User@example", "old"))?
        );
        let mut other_port = config("imap.example", "user@example", "old");
        other_port.port = 143;
        assert_ne!(original, bucket_name(&other_port)?);
        Ok(())
    }
}
