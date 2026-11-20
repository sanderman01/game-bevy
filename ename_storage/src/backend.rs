//! The trait a backend implements, and the values that cross it.

use crate::{BlobName, FaultPoint, Key, KeyPrefix};
use std::{collections::BTreeMap, fmt, future::Future, io, pin::Pin, sync::Arc};

/// A boxed future the caller can poll from a task. `Send` except on wasm, where there is one
/// thread and a browser backend's futures are not.
#[cfg(not(target_arch = "wasm32"))]
pub type BoxedFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A boxed future the caller can poll from a task. `Send` except on wasm, where there is one
/// thread and a browser backend's futures are not.
#[cfg(target_arch = "wasm32")]
pub type BoxedFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

/// A store of records: the filesystem, memory, or a third-party backend.
///
/// Consumers reach it as `dyn` through [`crate::UserStorage`], never by its type. The backend owns
/// any platform context, such as the signed-in Steam or console user; no method carries a user.
///
/// Every method returns at once and does its work in the future, which never blocks the thread
/// that polls it. The futures are `'static`, so a caller can spawn them.
///
/// What a backend guarantees:
/// - A read sees one committed version of the record: every blob it returns comes from the same
///   commit.
/// - [`StorageBackend::commit`] applies all of its [`Changes`] or none of them, across records,
///   also when the process dies part way. `Ok` means the changes are committed and every read that
///   starts afterwards sees them.
/// - A record exists while it has at least one blob.
/// - [`StorageBackend::ready`] resolves once the backend has done what it can to be in sync;
///   [`StorageBackend::status`] then says how that went. Operations called earlier wait for it.
///
/// `ename_storage::conformance` checks these for any backend.
pub trait StorageBackend: Send + Sync + 'static {
    fn status(&self) -> SyncStatus;

    fn ready(&self) -> BoxedFuture<Result<(), StorageError>>;

    /// Every record whose key matches `prefix`, ordered by key, each with `blob` read from the same
    /// commit as the listing when one is asked for and the record has it.
    fn list(
        &self,
        prefix: KeyPrefix,
        blob: Option<BlobName>,
    ) -> BoxedFuture<Result<Vec<Listing>, StorageError>>;

    /// The asked-for blobs of one record, all from one commit. `None` when the record does not
    /// exist; a blob the record lacks is absent from the [`Record`].
    fn read(
        &self,
        key: Key,
        blobs: Vec<BlobName>,
    ) -> BoxedFuture<Result<Option<Record>, StorageError>>;

    /// Applies all of `changes` or none of them.
    fn commit(&self, changes: Changes) -> BoxedFuture<Result<(), StorageError>>;
}

/// Where a backend stands with the place it keeps records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyncStatus {
    /// Starting up, recovering, or reconciling with a remote. Operations wait.
    Syncing,
    /// Durable and in sync.
    Synced,
    /// Working from a local copy; the remote cannot be reached.
    Offline,
    /// The local and remote copies disagree and the backend cannot settle it alone.
    Conflict,
    /// Works, but nothing outlives the process: the in-memory backend.
    Volatile,
    /// Cannot be used: another process holds the store, or it could not be opened or recovered.
    /// [`StorageBackend::ready`] says why.
    Unavailable,
}

impl fmt::Display for SyncStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Syncing => "syncing",
            Self::Synced => "synced",
            Self::Offline => "offline",
            Self::Conflict => "conflict",
            Self::Volatile => "volatile",
            Self::Unavailable => "unavailable",
        })
    }
}

/// Why a storage operation failed.
#[derive(Clone, Debug, thiserror::Error)]
pub enum StorageError {
    /// Another process, or another open handle in this one, is using the store.
    #[error("the store {0} is in use by another process")]
    Locked(String),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: Arc<io::Error>,
    },
    /// The store holds something the backend cannot read. It leaves it untouched.
    #[error("the store is damaged: {0}")]
    Corrupt(String),
    #[error("the store is unavailable: {0}")]
    Unavailable(String),
    /// A commit stopped at an armed fault point, as if the process had died there.
    #[error("stopped at fault point {0}")]
    Fault(FaultPoint),
}

impl StorageError {
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source: Arc::new(source),
        }
    }
}

/// One record of a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub key: Key,
    /// The blob the listing asked for, when the record has it.
    pub blob: Option<Vec<u8>>,
}

/// Blobs read from one record, all from one commit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    blobs: BTreeMap<BlobName, Vec<u8>>,
}

impl Record {
    pub fn get(&self, blob: &BlobName) -> Option<&[u8]> {
        self.blobs.get(blob).map(Vec::as_slice)
    }

    pub fn take(&mut self, blob: &BlobName) -> Option<Vec<u8>> {
        self.blobs.remove(blob)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&BlobName, &[u8])> {
        self.blobs
            .iter()
            .map(|(name, bytes)| (name, bytes.as_slice()))
    }

    pub fn len(&self) -> usize {
        self.blobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }
}

impl FromIterator<(BlobName, Vec<u8>)> for Record {
    fn from_iter<I: IntoIterator<Item = (BlobName, Vec<u8>)>>(iter: I) -> Self {
        Self {
            blobs: iter.into_iter().collect(),
        }
    }
}

/// What a transaction changes, per record, in key order. Built by [`crate::Transaction`]; a
/// backend reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    records: BTreeMap<Key, RecordChange>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Key, &RecordChange)> {
        self.records.iter()
    }

    pub(crate) fn write(&mut self, key: &Key, blob: &BlobName, bytes: Vec<u8>) {
        self.record(key).blobs.insert(blob.clone(), Some(bytes));
    }

    pub(crate) fn delete_blob(&mut self, key: &Key, blob: &BlobName) {
        self.record(key).blobs.insert(blob.clone(), None);
    }

    pub(crate) fn delete(&mut self, key: &Key) {
        self.records.insert(
            key.clone(),
            RecordChange {
                clears: true,
                blobs: BTreeMap::new(),
            },
        );
    }

    fn record(&mut self, key: &Key) -> &mut RecordChange {
        self.records.entry(key.clone()).or_default()
    }
}

/// The change to one record: optionally drop every blob first, then write or delete single blobs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordChange {
    clears: bool,
    blobs: BTreeMap<BlobName, Option<Vec<u8>>>,
}

impl RecordChange {
    /// Whether the record's existing blobs are dropped before [`RecordChange::blobs`] apply.
    pub fn clears(&self) -> bool {
        self.clears
    }

    /// Blobs to write (`Some`) or delete (`None`).
    pub fn blobs(&self) -> impl Iterator<Item = (&BlobName, Option<&[u8]>)> {
        self.blobs
            .iter()
            .map(|(name, bytes)| (name, bytes.as_deref()))
    }

    /// Whether an existing blob survives this change unchanged.
    pub fn keeps(&self, existing: &BlobName) -> bool {
        !self.clears && !self.blobs.contains_key(existing)
    }

    /// The record's blobs after this change, from its blobs before.
    pub fn apply(&self, mut blobs: BTreeMap<BlobName, Vec<u8>>) -> BTreeMap<BlobName, Vec<u8>> {
        if self.clears {
            blobs.clear();
        }
        for (name, bytes) in &self.blobs {
            match bytes {
                Some(bytes) => blobs.insert(name.clone(), bytes.clone()),
                None => blobs.remove(name),
            };
        }
        blobs
    }
}
