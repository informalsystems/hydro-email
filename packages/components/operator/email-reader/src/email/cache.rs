use std::collections::{BTreeSet, HashSet};

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
    fn remember(&self, key: &str) -> Result<()>;
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

    fn remember(&self, key: &str) -> Result<()> {
        self.0
            .set(key, &[])
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
    snapshot: BTreeSet<MessageId>,
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
        let snapshot = store
            .keys()?
            .iter()
            .map(|key| MessageId::parse(key))
            .collect::<Result<_>>()?;
        Ok(Self { store, snapshot })
    }

    // Call only after a successful, complete UID SEARCH UNSEEN. Limit cleanup to
    // the snapshot taken before the search, preserving entries added meanwhile.
    pub fn next_unread(
        &self,
        uid_validity: u32,
        unread: &HashSet<u32>,
    ) -> Result<Option<MessageId>> {
        ensure!(uid_validity != 0, "Missing IMAP UIDVALIDITY");
        for id in &self.snapshot {
            if id.uid_validity != uid_validity || !unread.contains(&id.uid) {
                self.store.remove(&id.key())?;
            }
        }
        Ok(unread
            .iter()
            .copied()
            .map(|uid| MessageId { uid_validity, uid })
            .filter(|id| !self.snapshot.contains(id))
            .min())
    }

    pub fn remember(&self, id: MessageId) -> Result<()> {
        self.store.remember(&id.key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone, Default)]
    struct MemoryStore(Rc<RefCell<BTreeSet<String>>>);

    impl CacheStore for MemoryStore {
        fn keys(&self) -> Result<Vec<String>> {
            Ok(self.0.borrow().iter().cloned().collect())
        }
        fn remember(&self, key: &str) -> Result<()> {
            self.0.borrow_mut().insert(key.into());
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
