//! The in-memory backend, for targets with no persistent directory (the browser today) and for
//! tests. Nothing outlives the process, and it says so: its status is always `Volatile`.

use crate::{
    BlobName, BoxedFuture, Changes, FaultPoint, FaultSwitch, Key, KeyPrefix, Listing, Record,
    StorageBackend, StorageError, SyncStatus,
};
use std::{
    collections::BTreeMap,
    future,
    sync::{Arc, PoisonError, RwLock},
};

type Records = BTreeMap<Key, BTreeMap<BlobName, Vec<u8>>>;

/// Records in a map behind one lock, so every read and listing sees whole commits. Each operation
/// completes before its future is returned.
///
/// Implements [`FaultPoint::Staged`], the only point its commit has.
#[derive(Clone, Default)]
pub struct MemoryStorage {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    records: RwLock<Records>,
    faults: FaultSwitch,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_faults(faults: FaultSwitch) -> Self {
        Self {
            inner: Arc::new(Inner {
                records: RwLock::default(),
                faults,
            }),
        }
    }

    pub const FAULT_POINTS: [FaultPoint; 1] = [FaultPoint::Staged];

    fn commit_now(&self, changes: &Changes) -> Result<(), StorageError> {
        let mut records = self
            .inner
            .records
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let staged: Vec<(Key, BTreeMap<BlobName, Vec<u8>>)> = changes
            .iter()
            .map(|(key, change)| {
                let before = records.get(key).cloned().unwrap_or_default();
                (key.clone(), change.apply(before))
            })
            .collect();
        self.inner.faults.check(FaultPoint::Staged)?;
        for (key, blobs) in staged {
            if blobs.is_empty() {
                records.remove(&key);
            } else {
                records.insert(key, blobs);
            }
        }
        Ok(())
    }
}

impl StorageBackend for MemoryStorage {
    fn status(&self) -> SyncStatus {
        SyncStatus::Volatile
    }

    fn ready(&self) -> BoxedFuture<Result<(), StorageError>> {
        Box::pin(future::ready(Ok::<_, StorageError>(())))
    }

    fn list(
        &self,
        prefix: KeyPrefix,
        blob: Option<BlobName>,
    ) -> BoxedFuture<Result<Vec<Listing>, StorageError>> {
        let records = self
            .inner
            .records
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let listing: Vec<Listing> = records
            .iter()
            .filter(|(key, _)| prefix.matches(key))
            .map(|(key, blobs)| Listing {
                key: key.clone(),
                blob: blob.as_ref().and_then(|name| blobs.get(name).cloned()),
            })
            .collect();
        Box::pin(future::ready(Ok::<_, StorageError>(listing)))
    }

    fn read(
        &self,
        key: Key,
        blobs: Vec<BlobName>,
    ) -> BoxedFuture<Result<Option<Record>, StorageError>> {
        let records = self
            .inner
            .records
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let record: Option<Record> = records.get(&key).map(|stored| {
            blobs
                .into_iter()
                .filter_map(|name| stored.get(&name).cloned().map(|bytes| (name, bytes)))
                .collect()
        });
        Box::pin(future::ready(Ok::<_, StorageError>(record)))
    }

    fn commit(&self, changes: Changes) -> BoxedFuture<Result<(), StorageError>> {
        Box::pin(future::ready(self.commit_now(&changes)))
    }
}
