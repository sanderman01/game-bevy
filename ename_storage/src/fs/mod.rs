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
use async_lock::{RwLock, RwLockReadGuard, RwLockWriteGuardArc};
use journal::{Layout, Opened};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

/// Records in directories under one root, which this backend owns alone: the lock keeps a second
/// process, or a second handle in this one, off it.
///
/// One worker thread per store opens it, recovers an interrupted commit, and then runs commits one
/// at a time in the order `commit` was called. A commit is queued when `commit` is called and runs
/// whatever happens to its future. Reads and listings run on `blocking`'s thread pool. No future
/// blocks the thread that polls it.
///
/// Dropping the last handle waits for the queued commits, then releases the store.
///
/// Implements every [`FaultPoint`].
#[derive(Clone)]
pub struct FsStorage {
    handle: Arc<Handle>,
}

/// What the handles share; dropping it stops the worker.
struct Handle {
    shared: Arc<Shared>,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// What the worker and the readers share.
struct Shared {
    layout: Layout,
    /// Readers share it; the renames of a commit take it alone, so a reader in this process never
    /// sees a commit half applied.
    store: Arc<RwLock<Store>>,
    status: Mutex<SyncStatus>,
    next_txn: AtomicU64,
    faults: FaultSwitch,
    /// Names in `records/` already warned about, so a listing warns once per run.
    warned: Mutex<HashSet<String>>,
}

enum Store {
    /// Only ever seen under the write lock the worker holds while it opens the store.
    Opening,
    Open {
        _opened: Opened,
        /// A commit passed its commit point but was not applied (an io error, or a journal flush
        /// that failed). The next operation applies the journal again before it reads.
        pending: bool,
    },
    /// `Locked` is tried again by the next operation; anything else stays.
    Failed(StorageError),
}

enum Job {
    Commit {
        changes: Changes,
        reply: async_channel::Sender<Result<(), StorageError>>,
    },
    Idle {
        reply: async_channel::Sender<()>,
    },
}

impl FsStorage {
    pub const FAULT_POINTS: [FaultPoint; 7] = FaultPoint::ALL;

    /// Opens the store at `root`, creating it if needed.
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self::open_with_faults(root, FaultSwitch::default())
    }

    /// As [`FsStorage::open`], with a switch the conformance suite arms to stop a commit or the
    /// recovery when the store opens.
    pub fn open_with_faults(root: impl Into<PathBuf>, faults: FaultSwitch) -> Self {
        let store = Arc::new(RwLock::new(Store::Opening));
        // Taken here, so no reader gets in before the worker has opened the store.
        let opening = store.try_write_arc().expect("a new lock is free");
        let shared = Arc::new(Shared {
            layout: Layout::new(root.into()),
            store,
            status: Mutex::new(SyncStatus::Syncing),
            next_txn: AtomicU64::new(0),
            faults,
            warned: Mutex::default(),
        });
        let (jobs, queue) = mpsc::channel();
        let worker = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("user-storage".into())
                .spawn(move || run_worker(&shared, opening, &queue))
                .expect("could not start the user storage thread")
        };
        Self {
            handle: Arc::new(Handle {
                shared,
                jobs: Mutex::new(Some(jobs)),
                worker: Mutex::new(Some(worker)),
            }),
        }
    }

    pub fn root(&self) -> &Path {
        self.handle.shared.layout.root()
    }
}

/// Opens the store, then runs jobs in order until every handle is gone.
fn run_worker(
    shared: &Shared,
    mut opening: RwLockWriteGuardArc<Store>,
    queue: &mpsc::Receiver<Job>,
) {
    *opening = shared.open_store();
    drop(opening);
    for job in queue {
        match job {
            Job::Commit { changes, reply } => {
                // The caller may have dropped its future; the commit has run either way.
                let _ = reply.try_send(shared.commit(&changes));
            }
            Job::Idle { reply } => {
                let _ = reply.try_send(());
            }
        }
    }
}

impl Handle {
    /// Queues a job; false once the worker has stopped.
    fn send(&self, job: Job) -> bool {
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|jobs| jobs.send(job).is_ok())
    }
}

impl Drop for Handle {
    /// Closes the queue and waits for the worker to finish the commits in it, so the store is
    /// released when the last handle is gone. The worker never holds a handle, so this never runs
    /// on it.
    fn drop(&mut self) {
        drop(
            self.jobs
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        let worker = self
            .worker
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(worker) = worker
            && worker.join().is_err()
        {
            tracing::error!("user storage: the storage thread panicked");
        }
    }
}

impl Shared {
    fn set_status(&self, status: SyncStatus) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }

    /// Opens the store and reports how that went. A fault in recovery stands for a crash: the
    /// store stays closed until it is opened again.
    fn open_store(&self) -> Store {
        match journal::open(&self.layout, &self.faults) {
            Ok(opened) => {
                self.set_status(SyncStatus::Synced);
                Store::Open {
                    _opened: opened,
                    pending: false,
                }
            }
            Err(error @ StorageError::Fault(_)) => self.dead(&error),
            Err(error) => {
                let root = self.layout.root().display();
                // A second game or editor on the same data directory is expected in
                // development; anything else is a real failure.
                if matches!(error, StorageError::Locked(_)) {
                    tracing::warn!("user storage at {root}: {error}");
                } else {
                    tracing::error!("user storage at {root}: {error}");
                }
                self.set_status(SyncStatus::Unavailable);
                Store::Failed(error)
            }
        }
    }

    /// The store after a fault: a dead process. Its lock is released with the open store, and
    /// every later call fails until the store is opened again.
    fn dead(&self, error: &StorageError) -> Store {
        self.set_status(SyncStatus::Unavailable);
        Store::Failed(StorageError::Unavailable(format!(
            "{error}; open the store again"
        )))
    }

    /// Waits for the store to open, opens it again if another process held it, and applies a
    /// pending commit; then holds it open for reading.
    fn open_for_reading(&self) -> Result<RwLockReadGuard<'_, Store>, StorageError> {
        loop {
            let store = self.store.read_blocking();
            let usable = match &*store {
                Store::Open { pending, .. } => !*pending,
                Store::Failed(StorageError::Locked(_)) => false,
                Store::Failed(error) => return Err(error.clone()),
                Store::Opening => {
                    return Err(StorageError::Unavailable("the store is not open".into()));
                }
            };
            if usable {
                return Ok(store);
            }
            drop(store);
            let mut store = self.store.write_blocking();
            if matches!(&*store, Store::Open { pending: true, .. }) {
                match journal::recover(&self.layout, &self.faults) {
                    Ok(()) => {
                        if let Store::Open { pending, .. } = &mut *store {
                            *pending = false;
                        }
                        self.set_status(SyncStatus::Synced);
                    }
                    Err(error @ StorageError::Fault(_)) => {
                        *store = self.dead(&error);
                        return Err(error);
                    }
                    Err(error) => return Err(error),
                }
            } else if matches!(&*store, Store::Failed(StorageError::Locked(_))) {
                *store = self.open_store();
                if let Store::Failed(error) = &*store {
                    return Err(error.clone());
                }
            }
        }
    }

    /// Whether a record directory holds at least one blob; one that holds none is not a record.
    fn has_blobs(dir: &Path) -> Result<bool, StorageError> {
        Ok(!journal::blobs_in(dir)?.is_empty())
    }

    fn warn_once(&self, name: &str, path: &Path) {
        let mut warned = self.warned.lock().unwrap_or_else(PoisonError::into_inner);
        if warned.insert(name.to_owned()) {
            tracing::warn!(
                "user storage: ignoring {}, which is not a record",
                path.display()
            );
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
            let name = entry.file_name();
            let Some(key) = name.to_str().and_then(journal::key_of_dir) else {
                self.warn_once(&name.to_string_lossy(), &entry.path());
                continue;
            };
            if !prefix.matches(&key) {
                continue;
            }
            let path = entry.path();
            let blob = match blob {
                Some(name) => read_blob(&path.join(name.as_str()))?,
                None => None,
            };
            if blob.is_none() && !Self::has_blobs(&path)? {
                continue;
            }
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
        if record.is_empty() && !Self::has_blobs(&dir)? {
            return Ok(None);
        }
        Ok(Some(record.into_iter().collect()))
    }

    /// Runs on the worker only, so commits never overlap.
    fn commit(&self, changes: &Changes) -> Result<(), StorageError> {
        let store = self.open_for_reading()?;
        if changes.is_empty() {
            return Ok(());
        }
        let txn = self.next_txn.fetch_add(1, Ordering::Relaxed);
        let staged = journal::stage(&self.layout, txn, changes, &self.faults);
        drop(store);
        let committed = match staged {
            Ok(Some(committed)) => committed,
            Ok(None) => return Ok(()),
            Err(error @ StorageError::Fault(_)) => return Err(self.die(error)),
            Err(error) => return Err(error),
        };
        // Committed from here on.
        if let Err(error) = committed.durable {
            return self.leave_pending(error);
        }
        let renamed = {
            let _store = self.store.write_blocking();
            journal::apply_renames(&self.layout, &committed.journal, &self.faults)
        };
        // The flushes run outside the write lock, so readers do not wait for them; readers may
        // see the new records before they are durable. The next commit waits, being on this
        // thread.
        match renamed.and_then(|()| journal::finish(&self.layout, &committed.journal, &self.faults))
        {
            Ok(()) => Ok(()),
            Err(error) => self.leave_pending(error),
        }
    }

    /// Past the commit point. A fault stands for a crash. Any other error leaves the journal to be
    /// applied again, from a flush of the root, before the next read; the commit is committed, so
    /// it reports `Ok`.
    fn leave_pending(&self, error: StorageError) -> Result<(), StorageError> {
        if matches!(error, StorageError::Fault(_)) {
            return Err(self.die(error));
        }
        tracing::warn!("user storage: committed, not yet applied; retrying on next use: {error}");
        if let Store::Open { pending, .. } = &mut *self.store.write_blocking() {
            *pending = true;
        }
        self.set_status(SyncStatus::Syncing);
        Ok(())
    }

    fn die(&self, error: StorageError) -> StorageError {
        *self.store.write_blocking() = self.dead(&error);
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

fn stopped() -> StorageError {
    StorageError::Unavailable("the storage thread has stopped".into())
}

impl StorageBackend for FsStorage {
    fn status(&self) -> SyncStatus {
        *self
            .handle
            .shared
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn ready(&self) -> BoxedFuture<Result<(), StorageError>> {
        let shared = self.handle.shared.clone();
        Box::pin(blocking::unblock(move || {
            shared.open_for_reading().map(|_| ())
        }))
    }

    fn list(
        &self,
        prefix: KeyPrefix,
        blob: Option<BlobName>,
    ) -> BoxedFuture<Result<Vec<Listing>, StorageError>> {
        let shared = self.handle.shared.clone();
        Box::pin(blocking::unblock(move || {
            shared.list(&prefix, blob.as_ref())
        }))
    }

    fn read(
        &self,
        key: Key,
        blobs: Vec<BlobName>,
    ) -> BoxedFuture<Result<Option<Record>, StorageError>> {
        let shared = self.handle.shared.clone();
        Box::pin(blocking::unblock(move || shared.read(&key, &blobs)))
    }

    fn commit(&self, changes: Changes) -> BoxedFuture<Result<(), StorageError>> {
        let (reply, result) = async_channel::bounded(1);
        let queued = self.handle.send(Job::Commit { changes, reply });
        Box::pin(async move {
            if !queued {
                return Err(stopped());
            }
            result.recv().await.unwrap_or_else(|_| Err(stopped()))
        })
    }

    fn idle(&self) -> BoxedFuture<()> {
        let (reply, done) = async_channel::bounded(1);
        let queued = self.handle.send(Job::Idle { reply });
        Box::pin(async move {
            if queued {
                // An error means the worker stopped, which is idle too.
                let _ = done.recv().await;
            }
        })
    }
}
