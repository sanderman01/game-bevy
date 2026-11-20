//! A conformance suite any backend can run, a third-party one included: reads, listing,
//! transaction atomicity, sync state, and recovery from a commit stopped at each fault point the
//! backend implements.
//!
//! ```ignore
//! struct MyHarness { /* a fresh store per `reset` */ }
//! impl ename_storage::conformance::Harness for MyHarness { /* ... */ }
//!
//! #[test]
//! fn my_backend_conforms() {
//!     ename_storage::conformance::run_all(&mut MyHarness::new());
//! }
//! ```
//!
//! The checks block on the backend's futures with `futures_lite::future::block_on`, so a backend
//! whose futures need a particular runtime runs its harness inside one.

use crate::{
    BlobName, FaultPoint, FaultSwitch, Key, KeyPrefix, StorageError, SyncStatus, UserStorage,
};
use futures_lite::future::block_on;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

/// What the suite needs from a backend under test.
pub trait Harness {
    /// Empties the store, for the next check. Every handle from earlier checks is dropped by then.
    fn reset(&mut self);

    /// Opens the store as it is now. The suite arms `faults` before a commit it means to stop.
    fn open(&mut self, faults: FaultSwitch) -> UserStorage;

    /// Whether the records outlive their handles: dropping every handle and opening again stands
    /// for a restart. False for the in-memory backend, whose fault checks then go on with the
    /// same handle.
    fn persistent(&self) -> bool;

    /// The fault points the backend implements. A backend that is not persistent can only
    /// implement points before the commit point.
    fn fault_points(&self) -> Vec<FaultPoint>;
}

/// One named check.
pub struct Check {
    pub name: &'static str,
    pub run: fn(&mut dyn Harness),
}

/// Every check, in order.
pub const CHECKS: &[Check] = &[
    Check {
        name: "an empty store",
        run: empty_store,
    },
    Check {
        name: "blobs read back",
        run: blobs_read_back,
    },
    Check {
        name: "blobs are replaced and deleted one by one",
        run: blobs_change_one_by_one,
    },
    Check {
        name: "later changes in a transaction win",
        run: later_changes_win,
    },
    Check {
        name: "a transaction spans records",
        run: transaction_spans_records,
    },
    Check {
        name: "listing",
        run: listing,
    },
    Check {
        name: "sync state",
        run: sync_state,
    },
    Check {
        name: "records survive a restart",
        run: survives_restart,
    },
    Check {
        name: "readers see whole commits",
        run: readers_see_whole_commits,
    },
    Check {
        name: "an interrupted commit recovers to all or nothing",
        run: interrupted_commits,
    },
];

/// Runs every check against a fresh store, naming each on stderr so a failure says which.
pub fn run_all(harness: &mut dyn Harness) {
    for check in CHECKS {
        eprintln!("storage conformance: {}", check.name);
        harness.reset();
        (check.run)(harness);
    }
}

const META: BlobName = BlobName::from_static("meta");
const STATE: BlobName = BlobName::from_static("state");
const THUMBNAIL: BlobName = BlobName::from_static("thumbnail");

fn key(key: &str) -> Key {
    Key::new(key).expect("the suite's keys are valid")
}

fn open_ready(harness: &mut dyn Harness) -> UserStorage {
    let storage = harness.open(FaultSwitch::default());
    block_on(storage.ready()).expect("a fresh store becomes ready");
    storage
}

/// Every record with every blob the suite uses.
fn contents(storage: &UserStorage) -> BTreeMap<String, BTreeMap<String, Vec<u8>>> {
    let listing = block_on(storage.list(&KeyPrefix::all(), None)).expect("list");
    listing
        .into_iter()
        .map(|entry| {
            let record = block_on(storage.read_record(&entry.key, &[META, STATE, THUMBNAIL]))
                .expect("read")
                .expect("a listed record exists");
            let blobs = record
                .iter()
                .map(|(name, bytes)| (name.to_string(), bytes.to_vec()))
                .collect();
            (entry.key.to_string(), blobs)
        })
        .collect()
}

/// Records with text blobs, in the shape [`contents`] returns.
fn expected(
    records: &[(&str, &[(&BlobName, &str)])],
) -> BTreeMap<String, BTreeMap<String, Vec<u8>>> {
    records
        .iter()
        .map(|(key, blobs)| {
            let blobs = blobs
                .iter()
                .map(|(name, text)| (name.to_string(), text.as_bytes().to_vec()))
                .collect();
            (key.to_string(), blobs)
        })
        .collect()
}

fn commit(storage: &UserStorage, build: impl FnOnce(&mut crate::Transaction)) {
    let mut txn = storage.begin();
    build(&mut txn);
    block_on(txn.commit()).expect("commit");
}

fn empty_store(harness: &mut dyn Harness) {
    let storage = open_ready(harness);
    assert!(
        block_on(storage.list(&KeyPrefix::all(), Some(&META)))
            .unwrap()
            .is_empty()
    );
    assert_eq!(block_on(storage.read(&key("a"), &META)).unwrap(), None);
    assert_eq!(
        block_on(storage.read_record(&key("a"), &[META])).unwrap(),
        None
    );
    block_on(storage.begin().commit()).expect("an empty transaction commits");
    assert!(contents(&storage).is_empty());
}

fn blobs_read_back(harness: &mut dyn Harness) {
    let storage = open_ready(harness);
    let big: Vec<u8> = (0..1 << 20).map(|i: u32| (i % 251) as u8).collect();
    commit(&storage, |txn| {
        txn.write(&key("saves/s/1"), &META, b"meta".as_slice())
            .write(&key("saves/s/1"), &STATE, big.clone())
            .write(&key("saves/s/1"), &THUMBNAIL, Vec::new());
    });
    let record = block_on(storage.read_record(&key("saves/s/1"), &[META, STATE, THUMBNAIL]))
        .unwrap()
        .expect("written record exists");
    assert_eq!(record.get(&META), Some(b"meta".as_slice()));
    assert_eq!(record.get(&STATE), Some(big.as_slice()));
    assert_eq!(
        record.get(&THUMBNAIL),
        Some(b"".as_slice()),
        "an empty blob is a blob"
    );
    assert_eq!(
        block_on(storage.read(&key("saves/s/1"), &META)).unwrap(),
        Some(b"meta".to_vec())
    );
    let other = BlobName::from_static("other");
    assert_eq!(
        block_on(storage.read(&key("saves/s/1"), &other)).unwrap(),
        None
    );
    let partial = block_on(storage.read_record(&key("saves/s/1"), &[META, other]))
        .unwrap()
        .unwrap();
    assert_eq!(partial.len(), 1, "a missing blob is absent from the record");
}

fn blobs_change_one_by_one(harness: &mut dyn Harness) {
    let storage = open_ready(harness);
    commit(&storage, |txn| {
        txn.write(&key("a"), &META, b"m1".as_slice())
            .write(&key("a"), &STATE, b"s1".as_slice());
    });
    commit(&storage, |txn| {
        txn.write(&key("a"), &META, b"m2".as_slice());
    });
    assert_eq!(
        contents(&storage),
        expected(&[("a", &[(&META, "m2"), (&STATE, "s1")])]),
        "a write keeps the other blobs"
    );
    commit(&storage, |txn| {
        txn.delete_blob(&key("a"), &META);
    });
    assert_eq!(contents(&storage), expected(&[("a", &[(&STATE, "s1")])]));
    commit(&storage, |txn| {
        txn.delete_blob(&key("a"), &STATE);
    });
    assert!(
        contents(&storage).is_empty(),
        "a record without blobs does not exist"
    );
    assert_eq!(
        block_on(storage.read_record(&key("a"), &[META])).unwrap(),
        None
    );

    commit(&storage, |txn| {
        txn.write(&key("b"), &META, b"m".as_slice())
            .write(&key("b"), &STATE, b"s".as_slice());
    });
    commit(&storage, |txn| {
        txn.delete(&key("b"));
    });
    assert!(contents(&storage).is_empty(), "delete removes the record");
    commit(&storage, |txn| {
        txn.delete(&key("missing"))
            .delete_blob(&key("missing2"), &META);
    });
    assert!(contents(&storage).is_empty(), "deleting nothing is fine");
}

fn later_changes_win(harness: &mut dyn Harness) {
    let storage = open_ready(harness);
    commit(&storage, |txn| {
        txn.write(&key("a"), &META, b"old".as_slice())
            .write(&key("a"), &STATE, b"old".as_slice());
    });
    commit(&storage, |txn| {
        txn.delete(&key("a"))
            .write(&key("a"), &META, b"new".as_slice())
            .write(&key("b"), &META, b"first".as_slice())
            .delete_blob(&key("b"), &META)
            .write(&key("c"), &META, b"first".as_slice())
            .write(&key("c"), &META, b"second".as_slice());
    });
    assert_eq!(
        contents(&storage),
        expected(&[("a", &[(&META, "new")]), ("c", &[(&META, "second")])]),
    );
}

fn transaction_spans_records(harness: &mut dyn Harness) {
    let storage = open_ready(harness);
    commit(&storage, |txn| {
        txn.write(&key("saves/s/old"), &META, b"old".as_slice());
    });
    // Rotating an autosave: the new one and the removal of the oldest in one commit.
    commit(&storage, |txn| {
        txn.write(&key("saves/s/new"), &META, b"new".as_slice())
            .write(&key("saves/s/new"), &STATE, b"state".as_slice())
            .delete(&key("saves/s/old"))
            .write(&key("difficulty/custom"), &META, b"d".as_slice());
    });
    assert_eq!(
        contents(&storage),
        expected(&[
            ("difficulty/custom", &[(&META, "d")]),
            ("saves/s/new", &[(&META, "new"), (&STATE, "state")]),
        ]),
    );
    let mut dropped = storage.begin();
    dropped.write(&key("never"), &META, b"x".as_slice());
    drop(dropped);
    assert_eq!(block_on(storage.read(&key("never"), &META)).unwrap(), None);
}

fn listing(harness: &mut dyn Harness) {
    let storage = open_ready(harness);
    commit(&storage, |txn| {
        txn.write(&key("saves/b/2"), &META, b"b2".as_slice())
            .write(&key("saves/a/1"), &META, b"a1".as_slice())
            .write(&key("saves/a/1"), &STATE, b"s".as_slice())
            .write(&key("saves/a/1/x"), &META, b"nested".as_slice())
            .write(&key("saves/a-b/3"), &STATE, b"no meta".as_slice())
            .write(&key("savesx/1"), &META, b"other".as_slice())
            .write(&key("saves"), &META, b"top".as_slice());
    });
    let list = |prefix: &str, blob: Option<&BlobName>| {
        block_on(storage.list(&KeyPrefix::new(prefix).unwrap(), blob))
            .unwrap()
            .into_iter()
            .map(|entry| (entry.key.to_string(), entry.blob))
            .collect::<Vec<_>>()
    };
    let some = |bytes: &[u8]| Some(bytes.to_vec());
    assert_eq!(
        list("saves", Some(&META)),
        [
            ("saves".to_owned(), some(b"top")),
            ("saves/a/1".to_owned(), some(b"a1")),
            ("saves/a/1/x".to_owned(), some(b"nested")),
            ("saves/a-b/3".to_owned(), None),
            ("saves/b/2".to_owned(), some(b"b2")),
        ],
        "ordered by segments, the chosen blob when present"
    );
    assert_eq!(
        list("saves/a", None),
        [
            ("saves/a/1".to_owned(), None),
            ("saves/a/1/x".to_owned(), None)
        ],
        "a prefix matches whole segments"
    );
    assert_eq!(list("", None).len(), 6);
    assert!(list("nothing", Some(&META)).is_empty());
}

fn sync_state(harness: &mut dyn Harness) {
    let storage = harness.open(FaultSwitch::default());
    // Operations called before `ready` wait for it.
    commit(&storage, |txn| {
        txn.write(&key("a"), &META, b"early".as_slice());
    });
    block_on(storage.ready()).expect("ready");
    block_on(storage.ready()).expect("ready resolves again");
    let status = storage.status();
    assert!(
        !matches!(status, SyncStatus::Syncing | SyncStatus::Unavailable),
        "after ready the status is settled, got {status}"
    );
    if harness.persistent() {
        assert_ne!(status, SyncStatus::Volatile);
    } else {
        assert_eq!(status, SyncStatus::Volatile);
    }
}

fn survives_restart(harness: &mut dyn Harness) {
    if !harness.persistent() {
        return;
    }
    let storage = open_ready(harness);
    commit(&storage, |txn| {
        txn.write(&key("a"), &META, b"kept".as_slice());
    });
    drop(storage);
    let storage = open_ready(harness);
    assert_eq!(contents(&storage), expected(&[("a", &[(&META, "kept")])]));
}

/// A writer commits both blobs of a record with the same counter while readers read both, and
/// list across two records written in one commit.
fn readers_see_whole_commits(harness: &mut dyn Harness) {
    let storage = open_ready(harness);
    let write = |n: u32| {
        let bytes = n.to_le_bytes().to_vec();
        commit(&storage, |txn| {
            txn.write(&key("r"), &META, bytes.clone())
                .write(&key("r"), &STATE, bytes.clone())
                .write(&key("pair/1"), &META, bytes.clone())
                .write(&key("pair/2"), &META, bytes.clone());
        });
    };
    write(0);
    let done = Arc::new(AtomicBool::new(false));
    thread::scope(|scope| {
        let reader = {
            let storage = storage.clone();
            let done = done.clone();
            scope.spawn(move || {
                let mut reads = 0;
                while !done.load(Ordering::Relaxed) || reads == 0 {
                    let record = block_on(storage.read_record(&key("r"), &[META, STATE]))
                        .unwrap()
                        .unwrap();
                    assert_eq!(record.get(&META), record.get(&STATE), "a torn record");
                    let pair: Vec<_> =
                        block_on(storage.list(&KeyPrefix::new("pair").unwrap(), Some(&META)))
                            .unwrap()
                            .into_iter()
                            .map(|entry| entry.blob)
                            .collect();
                    assert_eq!(pair.len(), 2, "a record missing mid-commit");
                    assert_eq!(pair[0], pair[1], "a listing across commits");
                    reads += 1;
                }
            })
        };
        for n in 1..=20 {
            write(n);
        }
        done.store(true, Ordering::Relaxed);
        reader.join().expect("reader");
    });
}

/// Seeds three records, then stops a commit that changes all of them and adds a fourth at every
/// occurrence of every fault point, and checks that recovery gives the whole old or the whole new
/// state.
fn interrupted_commits(harness: &mut dyn Harness) {
    let old = expected(&[
        ("a", &[(&META, "a1"), (&STATE, "s1")]),
        ("b", &[(&META, "b1")]),
        ("c", &[(&META, "c1")]),
    ]);
    let new = expected(&[
        ("a", &[(&META, "a2"), (&STATE, "s1")]),
        ("d", &[(&META, "d2")]),
    ]);
    for point in harness.fault_points() {
        assert!(
            harness.persistent() || !point.is_committed(),
            "a backend that is not persistent cannot recover from {point}"
        );
        // Each point occurs at most once per record of the commit.
        for occurrence in 0..4 {
            harness.reset();
            let faults = FaultSwitch::default();
            let storage = harness.open(faults.clone());
            block_on(storage.ready()).expect("ready");
            commit(&storage, |txn| {
                txn.write(&key("a"), &META, b"a1".as_slice())
                    .write(&key("a"), &STATE, b"s1".as_slice())
                    .write(&key("b"), &META, b"b1".as_slice())
                    .write(&key("c"), &META, b"c1".as_slice());
            });

            faults.arm(point, occurrence);
            let mut txn = storage.begin();
            txn.write(&key("a"), &META, b"a2".as_slice())
                .delete(&key("b"))
                .delete_blob(&key("c"), &META)
                .write(&key("d"), &META, b"d2".as_slice());
            let result = block_on(txn.commit());
            faults.disarm();
            match result {
                Ok(()) => {
                    assert_eq!(contents(&storage), new, "{point} #{occurrence} not reached");
                    break;
                }
                Err(StorageError::Fault(stopped)) => assert_eq!(stopped, point),
                Err(other) => panic!("{point} #{occurrence}: unexpected error {other}"),
            }

            let storage = if harness.persistent() {
                drop(storage);
                harness.open(FaultSwitch::default())
            } else {
                storage
            };
            block_on(storage.ready()).unwrap_or_else(|err| {
                panic!("{point} #{occurrence}: the store does not recover: {err}")
            });
            let want = if point.is_committed() { &new } else { &old };
            assert_eq!(&contents(&storage), want, "after {point} #{occurrence}");

            commit(&storage, |txn| {
                txn.write(&key("e"), &META, b"after".as_slice());
            });
            assert_eq!(
                block_on(storage.read(&key("e"), &META)).unwrap(),
                Some(b"after".to_vec()),
                "the store works after recovering from {point} #{occurrence}"
            );
            if harness.persistent() {
                drop(storage);
                let storage = open_ready(harness);
                let mut want = want.clone();
                want.insert(
                    "e".to_owned(),
                    BTreeMap::from([("meta".to_owned(), b"after".to_vec())]),
                );
                assert_eq!(
                    contents(&storage),
                    want,
                    "stable across another restart after {point} #{occurrence}"
                );
            }
        }
    }
}
