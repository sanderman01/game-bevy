//! The handle consumers hold, and the transaction they write through.

use crate::{
    BlobName, BoxedFuture, Changes, Key, KeyPrefix, Listing, Record, StorageBackend, StorageError,
    SyncStatus,
};
use std::{fmt, sync::Arc};

/// The app's user storage: a cheap, cloneable handle to whichever backend the app installed.
/// With the `bevy` feature it is the resource [`crate::StoragePlugin`] inserts.
///
/// Every method returns a `'static` future; poll it from a task ([`crate::StorageTask`] in a Bevy
/// app), never by blocking a frame.
#[derive(Clone)]
#[cfg_attr(feature = "bevy", derive(bevy::prelude::Resource))]
pub struct UserStorage(Arc<dyn StorageBackend>);

impl UserStorage {
    pub fn new(backend: impl StorageBackend) -> Self {
        Self(Arc::new(backend))
    }

    pub fn from_arc(backend: Arc<dyn StorageBackend>) -> Self {
        Self(backend)
    }

    pub fn backend(&self) -> &dyn StorageBackend {
        &*self.0
    }

    pub fn status(&self) -> SyncStatus {
        self.0.status()
    }

    /// Resolves once the backend has done what it can to be in sync; [`UserStorage::status`] then
    /// says how that went. A save list or "continue" waits on it.
    pub fn ready(&self) -> BoxedFuture<Result<(), StorageError>> {
        self.0.ready()
    }

    /// Resolves once every commit called before it has finished. [`crate::StoragePlugin`] waits
    /// for it when the app exits.
    pub fn idle(&self) -> BoxedFuture<()> {
        self.0.idle()
    }

    /// Every record under `prefix`, ordered by key, each with `blob` when given and present, so a
    /// save list is one call: `list(&saves, Some(&meta))`.
    pub fn list(
        &self,
        prefix: &KeyPrefix,
        blob: Option<&BlobName>,
    ) -> BoxedFuture<Result<Vec<Listing>, StorageError>> {
        self.0.list(prefix.clone(), blob.cloned())
    }

    /// One blob; `None` when the record or the blob does not exist.
    pub fn read(
        &self,
        key: &Key,
        blob: &BlobName,
    ) -> BoxedFuture<Result<Option<Vec<u8>>, StorageError>> {
        let read = self.0.read(key.clone(), vec![blob.clone()]);
        let blob = blob.clone();
        Box::pin(async move {
            Ok::<_, StorageError>(read.await?.and_then(|mut record| record.take(&blob)))
        })
    }

    /// Several blobs of one record, all from the same commit. `None` when the record does not
    /// exist.
    pub fn read_record(
        &self,
        key: &Key,
        blobs: &[BlobName],
    ) -> BoxedFuture<Result<Option<Record>, StorageError>> {
        self.0.read(key.clone(), blobs.to_vec())
    }

    /// Starts a transaction. Nothing reaches the backend until [`Transaction::commit`]; dropping
    /// it uncommitted discards it.
    pub fn begin(&self) -> Transaction {
        Transaction {
            storage: self.clone(),
            changes: Changes::default(),
        }
    }
}

impl fmt::Debug for UserStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserStorage")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

/// Blob writes and deletes across one or more records, committed all or nothing.
///
/// Later calls win over earlier ones for the same blob. [`Transaction::delete`] drops the whole
/// record, so a write after it starts the record afresh. A record left with no blobs is removed.
#[must_use = "a transaction does nothing until it is committed"]
pub struct Transaction {
    storage: UserStorage,
    changes: Changes,
}

impl Transaction {
    pub fn write(&mut self, key: &Key, blob: &BlobName, bytes: impl Into<Vec<u8>>) -> &mut Self {
        self.changes.write(key, blob, bytes.into());
        self
    }

    pub fn delete_blob(&mut self, key: &Key, blob: &BlobName) -> &mut Self {
        self.changes.delete_blob(key, blob);
        self
    }

    /// Removes the record and every blob in it.
    pub fn delete(&mut self, key: &Key) -> &mut Self {
        self.changes.delete(key);
        self
    }

    pub fn changes(&self) -> &Changes {
        &self.changes
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Commits after every transaction committed before it. The commit starts now and runs even if
    /// the returned future is dropped; the future only reports the outcome.
    ///
    /// `Ok` once every change is committed. On an error none is, except for a
    /// [`StorageError::Fault`] past the commit point, which stands for a crash: the store then
    /// recovers to the new state when it is opened again.
    pub fn commit(self) -> BoxedFuture<Result<(), StorageError>> {
        self.storage.0.commit(self.changes)
    }
}

impl fmt::Debug for Transaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transaction")
            .field("changes", &self.changes)
            .finish_non_exhaustive()
    }
}
