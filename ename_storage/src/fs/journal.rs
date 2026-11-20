//! The write-ahead journal: staging a commit, the commit point, applying it, and recovering one
//! that a crash interrupted. See `docs/user-storage.md` for the sequence and why each step is safe
//! to repeat.

use super::io::{
    err, is_dir, link_or_copy, remove_dir_all, remove_file, rename, sync_dir, write_synced,
};
use crate::{BlobName, Changes, FaultPoint, FaultSwitch, Key, StorageError};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

const RECORDS: &str = "records";
const STAGING: &str = "staging";
const TRASH: &str = "trash";
const JOURNAL: &str = "journal";
const JOURNAL_TMP: &str = "journal.tmp";
const LOCK: &str = "lock";
const HEADER: &str = "ename_storage journal 1";
const END: &str = "end";

/// The directories and files of one store.
#[derive(Clone, Debug)]
pub(super) struct Layout {
    root: PathBuf,
}

impl Layout {
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn records(&self) -> PathBuf {
        self.root.join(RECORDS)
    }

    /// A record's directory: the key's segments joined with `.`, which no segment contains, so
    /// every record is a sibling and no record nests inside another.
    pub(super) fn record(&self, key: &Key) -> PathBuf {
        self.records()
            .join(key.segments().collect::<Vec<_>>().join("."))
    }

    fn staging(&self) -> PathBuf {
        self.root.join(STAGING)
    }

    fn staged_txn(&self, txn: u64) -> PathBuf {
        self.staging().join(txn.to_string())
    }

    fn staged(&self, txn: u64, index: usize) -> PathBuf {
        self.staged_txn(txn).join(index.to_string())
    }

    fn trash(&self) -> PathBuf {
        self.root.join(TRASH)
    }

    fn trashed(&self, txn: u64, index: usize) -> PathBuf {
        self.trash().join(format!("{txn}-{index}"))
    }

    fn journal(&self) -> PathBuf {
        self.root.join(JOURNAL)
    }

    fn journal_tmp(&self) -> PathBuf {
        self.root.join(JOURNAL_TMP)
    }
}

/// The key a record directory's name stands for; `None` for a name no key maps to.
pub(super) fn key_of_dir(name: &str) -> Option<Key> {
    Key::new(name.replace('.', "/")).ok()
}

/// The blob names in a record directory. Anything else in it (`.DS_Store`, `desktop.ini`) is not
/// a blob and is ignored.
pub(super) fn blobs_in(dir: &Path) -> Result<Vec<BlobName>, StorageError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(err("list", dir)(error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(err("list", dir))?;
        let is_file = entry.file_type().is_ok_and(|kind| kind.is_file());
        if let Some(name) = entry.file_name().to_str()
            && is_file
            && let Ok(name) = BlobName::new(name)
        {
            names.push(name);
        }
    }
    Ok(names)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    /// Swap in `staging/<txn>/<index>`.
    Replace,
    /// Move the record aside.
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    index: usize,
    action: Action,
    key: Key,
}

/// A committed transaction: which records to replace from staging and which to remove.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Journal {
    txn: u64,
    entries: Vec<Entry>,
}

impl Journal {
    /// One line per entry between a header and an end marker, so a cut-off file never parses.
    fn to_text(&self) -> String {
        let mut text = format!("{HEADER}\ntxn {}\n", self.txn);
        for entry in &self.entries {
            let action = match entry.action {
                Action::Replace => "replace",
                Action::Remove => "remove",
            };
            text.push_str(&format!("{action} {} {}\n", entry.index, entry.key));
        }
        text.push_str(END);
        text.push('\n');
        text
    }

    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let txn = lines.next()?.strip_prefix("txn ")?.parse().ok()?;
        let mut entries = Vec::new();
        while let Some(line) = lines.next() {
            if line == END {
                return lines.next().is_none().then_some(Self { txn, entries });
            }
            let mut words = line.split(' ');
            let action = match words.next()? {
                "replace" => Action::Replace,
                "remove" => Action::Remove,
                _ => return None,
            };
            let index = words.next()?.parse().ok()?;
            let key = Key::new(words.next()?).ok()?;
            if words.next().is_some() {
                return None;
            }
            entries.push(Entry { index, action, key });
        }
        None
    }
}

/// Roots open in this process. The OS lock alone does not keep a second handle in the same
/// process off a store everywhere: where `flock` is emulated with locks a process shares (NFS),
/// the second would succeed, and dropping it would release the first one's lock.
static OPEN_ROOTS: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// A root claimed in [`OPEN_ROOTS`] until dropped.
struct RootClaim(PathBuf);

impl RootClaim {
    fn take(root: &Path) -> Result<Self, StorageError> {
        let canonical = fs::canonicalize(root).map_err(err("resolve", root))?;
        let mut roots = OPEN_ROOTS.lock().unwrap_or_else(PoisonError::into_inner);
        if !roots.insert(canonical.clone()) {
            return Err(StorageError::Locked(format!(
                "{} is already open in this process",
                root.display()
            )));
        }
        Ok(Self(canonical))
    }
}

impl Drop for RootClaim {
    fn drop(&mut self) {
        OPEN_ROOTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// An open store's hold on its root: the OS lock, then the claim in this process. Dropping it
/// releases both.
pub(super) struct Opened {
    _lock: File,
    _claim: RootClaim,
}

/// Opens a store: claims it, takes the lock, finishes a committed transaction, discards anything
/// uncommitted.
///
/// The lock is the OS's (`flock` on Unix, `LockFileEx` on Windows), held by the open file, so it
/// goes away with the process however it ends; a lock file left behind by a crash is not stale.
pub(super) fn open(layout: &Layout, faults: &FaultSwitch) -> Result<Opened, StorageError> {
    let root = layout.root();
    fs::create_dir_all(root).map_err(err("create", root))?;
    let claim = RootClaim::take(root)?;
    let lock_path = root.join(LOCK);
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(err("open", &lock_path))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => {
            return Err(StorageError::Locked(format!(
                "{} is open in another process",
                root.display()
            )));
        }
        Err(fs::TryLockError::Error(error)) => return Err(err("lock", &lock_path)(error)),
    }
    for dir in [layout.records(), layout.staging(), layout.trash()] {
        fs::create_dir_all(&dir).map_err(err("create", &dir))?;
    }
    sync_dir(root)?;
    recover(layout, faults)?;
    Ok(Opened {
        _lock: lock,
        _claim: claim,
    })
}

/// Applies a journal found on disk, then clears out staging and trash. Every step is safe to
/// repeat, so a crash during recovery is recovered the same way.
///
/// No record is renamed until the root has been flushed, so the journal is durable before any
/// of its changes: a commit whose journal flush failed must not be applied until one succeeds.
pub(super) fn recover(layout: &Layout, faults: &FaultSwitch) -> Result<(), StorageError> {
    // Never renamed into place, so never committed.
    remove_file(&layout.journal_tmp())?;
    let path = layout.journal();
    match fs::read_to_string(&path) {
        Ok(text) => {
            let journal = Journal::parse(&text).ok_or_else(|| {
                StorageError::Corrupt(format!(
                    "{} cannot be read; the store is left as it is",
                    path.display()
                ))
            })?;
            tracing::info!(
                "user storage: finishing a committed transaction ({} records)",
                journal.entries.len()
            );
            sync_dir(layout.root())?;
            apply_renames(layout, &journal, faults)?;
            finish(layout, &journal, faults)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(err("read", &path)(error)),
    }
    for dir in [layout.staging(), layout.trash()] {
        clear(&dir);
    }
    Ok(())
}

/// Removes everything inside `dir`. Leftovers only cost space and are tried again on the next
/// start, so a failure is a warning.
fn clear(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let removed = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            remove_dir_all(&path)
        } else {
            remove_file(&path)
        };
        if let Err(error) = removed {
            tracing::warn!("user storage: could not clean up: {error}");
        }
    }
}

/// A transaction past its commit point.
pub(super) struct Committed {
    pub(super) journal: Journal,
    /// Whether the journal is known to be durable. Until it is, no record may be renamed: a power
    /// cut could keep the renames and lose the journal.
    pub(super) durable: Result<(), StorageError>,
}

/// Stages the new version of every changed record under `staging/<txn>`, makes it durable, and
/// writes the journal: the rename of `journal.tmp` to `journal` is the commit point. `None` when
/// the changes change nothing on disk.
///
/// An error is from before the commit point: the staging is removed and nothing is committed. A
/// fault leaves everything as it is, as a crash would.
pub(super) fn stage(
    layout: &Layout,
    txn: u64,
    changes: &Changes,
    faults: &FaultSwitch,
) -> Result<Option<Committed>, StorageError> {
    let dir = layout.staged_txn(txn);
    let staged = stage_records(layout, txn, changes).and_then(|journal| {
        if journal.is_some() {
            faults.check(FaultPoint::Staged)?;
        }
        Ok(journal)
    });
    let journal = match staged {
        Ok(Some(journal)) => journal,
        Ok(None) => {
            remove_dir_all(&dir)?;
            return Ok(None);
        }
        Err(error @ StorageError::Fault(_)) => return Err(error),
        Err(error) => {
            discard(&dir);
            return Err(error);
        }
    };

    let tmp = layout.journal_tmp();
    let written = remove_file(&tmp)
        .and_then(|()| write_synced(&tmp, journal.to_text().as_bytes()))
        .and_then(|()| faults.check(FaultPoint::JournalWritten))
        .and_then(|()| rename(&tmp, &layout.journal()));
    match written {
        Ok(()) => {}
        Err(error @ StorageError::Fault(_)) => return Err(error),
        Err(error) => {
            discard(&tmp);
            discard(&dir);
            return Err(error);
        }
    }
    let durable = sync_dir(layout.root()).and_then(|()| faults.check(FaultPoint::Committed));
    Ok(Some(Committed { journal, durable }))
}

fn stage_records(
    layout: &Layout,
    txn: u64,
    changes: &Changes,
) -> Result<Option<Journal>, StorageError> {
    let dir = layout.staged_txn(txn);
    // A leftover from a run whose cleanup failed.
    remove_dir_all(&dir)?;
    fs::create_dir(&dir).map_err(err("create", &dir))?;
    let mut entries = Vec::new();
    for (index, (key, change)) in changes.iter().enumerate() {
        let current = layout.record(key);
        let staged = layout.staged(txn, index);
        fs::create_dir(&staged).map_err(err("create", &staged))?;
        let mut blobs = 0;
        for name in blobs_in(&current)? {
            if change.keeps(&name) {
                link_or_copy(&current.join(name.as_str()), &staged.join(name.as_str()))?;
                blobs += 1;
            }
        }
        for (name, bytes) in change.blobs() {
            if let Some(bytes) = bytes {
                write_synced(&staged.join(name.as_str()), bytes)?;
                blobs += 1;
            }
        }
        let action = if blobs > 0 {
            sync_dir(&staged)?;
            Action::Replace
        } else {
            fs::remove_dir(&staged).map_err(err("remove", &staged))?;
            if !is_dir(&current) {
                continue;
            }
            Action::Remove
        };
        entries.push(Entry {
            index,
            action,
            key: key.clone(),
        });
    }
    if entries.is_empty() {
        return Ok(None);
    }
    sync_dir(&dir)?;
    sync_dir(&layout.staging())?;
    Ok(Some(Journal { txn, entries }))
}

fn discard(path: &Path) {
    let removed = if is_dir(path) {
        remove_dir_all(path)
    } else {
        remove_file(path)
    };
    if let Err(error) = removed {
        tracing::warn!("user storage: could not discard an uncommitted transaction: {error}");
    }
}

/// Swaps in the staged records and moves the removed ones aside. Only renames: the caller holds
/// the write lock for this part alone, and [`finish`] makes it durable afterwards.
///
/// Safe to repeat from the top after a stop anywhere. A staged directory that still exists has
/// not been moved in, so the record directory beside it, if any, is the old version and goes to
/// the trash first; one that is gone was moved in already. A record directory is never renamed
/// onto an existing one, which Windows refuses, so between the two renames the record is briefly
/// absent on disk; the write lock hides that in this process, and the journal finishes it after a
/// crash.
pub(super) fn apply_renames(
    layout: &Layout,
    journal: &Journal,
    faults: &FaultSwitch,
) -> Result<(), StorageError> {
    let txn = journal.txn;
    for entry in &journal.entries {
        let record = layout.record(&entry.key);
        let trashed = layout.trashed(txn, entry.index);
        match entry.action {
            Action::Replace => {
                let staged = layout.staged(txn, entry.index);
                if !is_dir(&staged) {
                    continue;
                }
                if is_dir(&record) {
                    remove_dir_all(&trashed)?;
                    rename(&record, &trashed)?;
                    faults.check(FaultPoint::OldMovedAside)?;
                }
                rename(&staged, &record)?;
                faults.check(FaultPoint::NewMovedIn)?;
            }
            Action::Remove => {
                if is_dir(&record) {
                    remove_dir_all(&trashed)?;
                    rename(&record, &trashed)?;
                    faults.check(FaultPoint::OldMovedAside)?;
                }
            }
        }
    }
    Ok(())
}

/// Makes the renames durable, removes the journal, and cleans up. Until the journal is removed, a
/// stop here is recovered by applying the journal again.
pub(super) fn finish(
    layout: &Layout,
    journal: &Journal,
    faults: &FaultSwitch,
) -> Result<(), StorageError> {
    let txn = journal.txn;
    sync_dir(&layout.records())?;
    sync_dir(&layout.trash())?;
    if is_dir(&layout.staged_txn(txn)) {
        sync_dir(&layout.staged_txn(txn))?;
    }
    faults.check(FaultPoint::Applied)?;

    remove_file(&layout.journal())?;
    sync_dir(layout.root())?;
    faults.check(FaultPoint::JournalRemoved)?;

    let mut leftovers = vec![layout.staged_txn(txn)];
    leftovers.extend(
        journal
            .entries
            .iter()
            .map(|entry| layout.trashed(txn, entry.index)),
    );
    for path in leftovers {
        if let Err(error) = remove_dir_all(&path) {
            tracing::warn!("user storage: could not clean up: {error}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal() -> Journal {
        Journal {
            txn: 7,
            entries: vec![
                Entry {
                    index: 0,
                    action: Action::Replace,
                    key: Key::new("saves/a/1").unwrap(),
                },
                Entry {
                    index: 2,
                    action: Action::Remove,
                    key: Key::new("difficulty/hard").unwrap(),
                },
            ],
        }
    }

    #[test]
    fn journal_round_trips() {
        let journal = journal();
        assert_eq!(Journal::parse(&journal.to_text()), Some(journal));
    }

    #[test]
    fn a_cut_off_journal_does_not_parse() {
        let text = journal().to_text();
        for cut in 0..text.len() - 1 {
            assert_eq!(Journal::parse(&text[..cut]), None, "cut at {cut}");
        }
    }

    #[test]
    fn junk_after_the_end_does_not_parse() {
        let text = format!("{}junk\n", journal().to_text());
        assert_eq!(Journal::parse(&text), None);
    }

    #[test]
    fn record_directories_are_flat_and_map_back() {
        let layout = Layout::new(PathBuf::from("root"));
        let key = Key::new("saves/s-1/a_2").unwrap();
        let dir = layout.record(&key);
        assert_eq!(dir, Path::new("root").join("records").join("saves.s-1.a_2"));
        let name = dir.file_name().unwrap().to_str().unwrap();
        assert_eq!(key_of_dir(name), Some(key));
        assert_eq!(key_of_dir(".DS_Store"), None);
        assert_eq!(key_of_dir("Saves.a"), None);
    }
}
