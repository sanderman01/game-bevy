//! The filesystem backend: one directory per record, made atomic across records by a write-ahead
//! journal. Desktop only.

mod io;
mod journal;
#[cfg(test)]
mod tests;

use crate::{
    BlobName, BoxedFuture, Changes, FaultPoint, FaultSwitch, Key, KeyPrefix, Listing, Record,
    StorageBackend, StorageError, SyncStatus,
};
use async_lock::{RwLock, RwLockReadGuard};
use journal::Layout;
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

/// Records in directories under one root, which this backend owns alone: the lock keeps a second
/// process, or a second handle in this one, off it.
///
/// Opening returns at once; taking the lock and recovering an interrupted commit run on a blocking
/// thread, and every operation waits for them. Each operation runs its file io on `blocking`'s
/// thread pool, so its future never blocks the thread that polls it, and a dropped future never
/// stops a commit half way.
///
/// Implements every [`FaultPoint`].
#[derive(Clone)]
pub struct FsStorage {
    inner: Arc<Inner>,
}

struct Inner {
    layout: Layout,
    /// Readers share it; applying a commit takes it alone, so a reader in this process never sees
    /// a commit half applied.
    store: Arc<RwLock<Store>>,
    /// Serialises commits, so staging reads records no other commit is changing.
    commits: Mutex<()>,
    status: Mutex<SyncStatus>,
    next_txn: AtomicU64,
    faults: FaultSwitch,
}

enum Store {
    /// Only ever seen under the write lock the opening task holds.
    Opening,
    Open {
        _lock: File,
        /// A commit passed its commit point but applying it failed. The next operation applies
        /// it again before it reads anything.
        pending: bool,
    },
    Failed(StorageError),
}

impl FsStorage {
    pub const FAULT_POINTS: [FaultPoint; 7] = FaultPoint::ALL;

    /// Opens the store at `root`, creating it if needed.
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self::open_with_faults(root, FaultSwitch::default())
    }

    /// As [`FsStorage::open`], with a switch the conformance suite arms to stop a commit.
    pub fn open_with_faults(root: impl Into<PathBuf>, faults: FaultSwitch) -> Self {
        let store = Arc::new(RwLock::new(Store::Opening));
        let opening = store.try_write_arc().expect("a new lock is free");
        let inner = Arc::new(Inner {
            layout: Layout::new(root.into()),
            store,
            commits: Mutex::new(()),
            status: Mutex::new(SyncStatus::Syncing),
            next_txn: AtomicU64::new(0),
            faults,
        });
        let task = inner.clone();
        // Detached: dropping the task could cancel the open before it ran, and the store would
        // never open.
        blocking::unblock(move || {
            let mut store = opening;
            *store = match journal::open(&task.layout) {
                Ok(lock) => {
                    task.set_status(SyncStatus::Synced);
                    Store::Open {
                        _lock: lock,
                        pending: false,
                    }
                }
                Err(error) => {
                    let root = task.layout.root().display();
                    // A second game or editor on the same data directory is expected in
                    // development; anything else is a real failure.
                    if matches!(error, StorageError::Locked(_)) {
                        tracing::warn!("user storage at {root}: {error}");
                    } else {
                        tracing::error!("user storage at {root}: {error}");
                    }
                    task.set_status(SyncStatus::Unavailable);
                    Store::Failed(error)
                }
            };
        })
        .detach();
        Self { inner }
    }

    pub fn root(&self) -> &Path {
        self.inner.layout.root()
    }
}

impl Inner {
    fn set_status(&self, status: SyncStatus) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }

    /// Waits for the store to open and for a pending commit to be applied, then holds it open
    /// for reading.
    fn open_for_reading(&self) -> Result<RwLockReadGuard<'_, Store>, StorageError> {
        loop {
            let store = self.store.read_blocking();
            let pending = match &*store {
                Store::Open { pending, .. } => *pending,
                Store::Failed(error) => return Err(error.clone()),
                Store::Opening => {
                    return Err(StorageError::Unavailable("the store is not open".into()));
                }
            };
            if !pending {
                return Ok(store);
            }
            drop(store);
            let mut store = self.store.write_blocking();
            if let Store::Open { pending, .. } = &mut *store
                && *pending
            {
                journal::recover(&self.layout)?;
                *pending = false;
                self.set_status(SyncStatus::Synced);
            }
        }
    }

    fn list(
        &self,
        prefix: &KeyPrefix,
        blob: Option<&BlobName>,
    ) -> Result<Vec<Listing>, StorageError> {
        let _store = self.open_for_reading()?;
        let records = self.layout.records();
        let entries = fs::read_dir(&records).map_err(io::err("list", &records))?;
        let mut listing = Vec::new();
        for entry in entries {
            let entry = entry.map_err(io::err("list", &records))?;
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Some(key) = entry.file_name().to_str().and_then(journal::key_of_dir) else {
                tracing::warn!(
                    "user storage: ignoring {}, which is not a record",
                    entry.path().display()
                );
                continue;
            };
            if !prefix.matches(&key) {
                continue;
            }
            let blob = match blob {
                Some(name) => read_blob(&entry.path().join(name.as_str()))?,
                None => None,
            };
            listing.push(Listing { key, blob });
        }
        listing.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(listing)
    }

    fn read(&self, key: &Key, blobs: &[BlobName]) -> Result<Option<Record>, StorageError> {
        let _store = self.open_for_reading()?;
        let dir = self.layout.record(key);
        if !io::is_dir(&dir) {
            return Ok(None);
        }
        let mut record = Vec::new();
        for name in blobs {
            if let Some(bytes) = read_blob(&dir.join(name.as_str()))? {
                record.push((name.clone(), bytes));
            }
        }
        Ok(Some(record.into_iter().collect()))
    }

    fn commit(&self, changes: &Changes) -> Result<(), StorageError> {
        let _serial = self.commits.lock().unwrap_or_else(PoisonError::into_inner);
        let store = self.open_for_reading()?;
        if changes.is_empty() {
            return Ok(());
        }
        let txn = self.next_txn.fetch_add(1, Ordering::Relaxed);
        let staged = journal::stage(&self.layout, txn, changes, &self.faults);
        drop(store);
        let journal = match staged {
            Ok(Some(journal)) => journal,
            Ok(None) => return Ok(()),
            Err(error @ StorageError::Fault(_)) => return Err(self.die(error)),
            Err(error) => return Err(error),
        };

        let mut store = self.store.write_blocking();
        match journal::apply(&self.layout, &journal, &self.faults) {
            Ok(()) => Ok(()),
            Err(error @ StorageError::Fault(_)) => {
                drop(store);
                Err(self.die(error))
            }
            // Committed: the journal is durable and is applied again before anything else reads.
            Err(error) => {
                tracing::warn!(
                    "user storage: committed, but applying it failed; retrying on next use: {error}"
                );
                if let Store::Open { pending, .. } = &mut *store {
                    *pending = true;
                }
                self.set_status(SyncStatus::Syncing);
                Ok(())
            }
        }
    }

    /// After a fault: behaves as a dead process. The lock is released with the open store, and
    /// every later call fails until the store is opened again.
    fn die(&self, error: StorageError) -> StorageError {
        *self.store.write_blocking() = Store::Failed(StorageError::Unavailable(format!(
            "{error}; open the store again"
        )));
        self.set_status(SyncStatus::Unavailable);
        error
    }
}

fn read_blob(path: &Path) -> Result<Option<Vec<u8>>, StorageError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io::err("read", path)(error)),
    }
}

impl StorageBackend for FsStorage {
    fn status(&self) -> SyncStatus {
        *self
            .inner
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn ready(&self) -> BoxedFuture<Result<(), StorageError>> {
        let inner = self.inner.clone();
        Box::pin(blocking::unblock(move || {
            inner.open_for_reading().map(|_| ())
        }))
    }

    fn list(
        &self,
        prefix: KeyPrefix,
        blob: Option<BlobName>,
    ) -> BoxedFuture<Result<Vec<Listing>, StorageError>> {
        let inner = self.inner.clone();
        Box::pin(blocking::unblock(move || {
            inner.list(&prefix, blob.as_ref())
        }))
    }

    fn read(
        &self,
        key: Key,
        blobs: Vec<BlobName>,
    ) -> BoxedFuture<Result<Option<Record>, StorageError>> {
        let inner = self.inner.clone();
        Box::pin(blocking::unblock(move || inner.read(&key, &blobs)))
    }

    fn commit(&self, changes: Changes) -> BoxedFuture<Result<(), StorageError>> {
        let inner = self.inner.clone();
        Box::pin(blocking::unblock(move || inner.commit(&changes)))
    }
}
