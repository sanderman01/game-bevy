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
        name: "commits apply in call order",
        run: commits_apply_in_call_order,
    },
    Check {
        name: "a commit runs whatever happens to its future",
        run: dropped_commit_still_lands,
    },
    Check {
        name: "an interrupted commit recovers to all or nothing",
        run: interrupted_commits,
    },
    Check {
        name: "an interrupted recovery recovers to the new state",
        run: interrupted_recoveries,
    },
    Check {
        name: "an io error in a commit leaves all or nothing, and the store working",
        run: io_errors_in_commits,
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
    let old = old_state();
    let new = new_state();
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
            seed(&storage);

            faults.arm(point, occurrence);
            let result = block_on(the_commit(&storage).commit());
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

type Contents = BTreeMap<String, BTreeMap<String, Vec<u8>>>;

/// The records the fault checks start from.
fn seed(storage: &UserStorage) {
    commit(storage, |txn| {
        txn.write(&key("a"), &META, b"a1".as_slice())
            .write(&key("a"), &STATE, b"s1".as_slice())
            .write(&key("b"), &META, b"b1".as_slice())
            .write(&key("c"), &META, b"c1".as_slice());
    });
}

fn old_state() -> Contents {
    expected(&[
        ("a", &[(&META, "a1"), (&STATE, "s1")]),
        ("b", &[(&META, "b1")]),
        ("c", &[(&META, "c1")]),
    ])
}

fn new_state() -> Contents {
    expected(&[
        ("a", &[(&META, "a2"), (&STATE, "s1")]),
        ("d", &[(&META, "d2")]),
    ])
}

/// The commit the fault checks stop: replaces `a` keeping a blob, removes `b`, removes `c` by
/// deleting its only blob, adds `d`.
fn the_commit(storage: &UserStorage) -> crate::Transaction {
    let mut txn = storage.begin();
    txn.write(&key("a"), &META, b"a2".as_slice())
        .delete(&key("b"))
        .delete_blob(&key("c"), &META)
        .write(&key("d"), &META, b"d2".as_slice());
    txn
}

/// Fires commits without awaiting them, then awaits them in reverse: the order of the calls
/// decides, not the order of the awaits.
fn commits_apply_in_call_order(harness: &mut dyn Harness) {
    let storage = open_ready(harness);
    let commits: Vec<_> = (0..20u32)
        .map(|n| {
            let mut txn = storage.begin();
            txn.write(&key("order"), &META, n.to_le_bytes().to_vec())
                .write(&key(&format!("seq/{n}")), &META, n.to_le_bytes().to_vec());
            txn.commit()
        })
        .collect();
    for commit in commits.into_iter().rev() {
        block_on(commit).expect("commit");
    }
    assert_eq!(
        block_on(storage.read(&key("order"), &META)).unwrap(),
        Some(19u32.to_le_bytes().to_vec()),
        "the last call applies last"
    );
    let seq = block_on(storage.list(&KeyPrefix::new("seq").unwrap(), None)).unwrap();
    assert_eq!(seq.len(), 20);
}

/// "Save and quit": commits whose futures are dropped at once still land, and `idle` waits for
/// them.
fn dropped_commit_still_lands(harness: &mut dyn Harness) {
    let storage = open_ready(harness);
    for n in 0..5u8 {
        let mut txn = storage.begin();
        txn.write(&key("quit"), &META, vec![n]);
        drop(txn.commit());
    }
    block_on(storage.idle());
    assert_eq!(
        block_on(storage.read(&key("quit"), &META)).unwrap(),
        Some(vec![4])
    );
    if harness.persistent() {
        let mut txn = storage.begin();
        txn.write(&key("quit"), &META, vec![5]);
        drop(txn.commit());
        drop(storage);
        let storage = open_ready(harness);
        assert_eq!(
            block_on(storage.read(&key("quit"), &META)).unwrap(),
            Some(vec![5]),
            "dropping the last handle finishes the commits it was given"
        );
    }
}

/// Resets, seeds, and stops [`the_commit`] at `point` #`occurrence` as a crash. False when the
/// commit never got there, and so completed.
fn crash_commit(harness: &mut dyn Harness, point: FaultPoint, occurrence: u32) -> bool {
    harness.reset();
    let faults = FaultSwitch::default();
    let storage = harness.open(faults.clone());
    block_on(storage.ready()).expect("ready");
    seed(&storage);
    faults.arm(point, occurrence);
    let result = block_on(the_commit(&storage).commit());
    faults.disarm();
    match result {
        Ok(()) => false,
        Err(StorageError::Fault(stopped)) => {
            assert_eq!(stopped, point);
            true
        }
        Err(other) => panic!("{point} #{occurrence}: unexpected error {other}"),
    }
}

/// Stops a commit past its commit point, then stops the recovery that finishes it at each of its
/// own points, then recovers fully: always the whole new state.
fn interrupted_recoveries(harness: &mut dyn Harness) {
    if !harness.persistent() {
        return;
    }
    let points = harness.fault_points();
    // A stop after the journal is removed leaves recovery nothing to apply.
    let commit_points: Vec<_> = points
        .iter()
        .copied()
        .filter(|point| point.is_committed() && *point != FaultPoint::JournalRemoved)
        .collect();
    let recovery_points: Vec<_> = points
        .iter()
        .copied()
        .filter(|point| {
            matches!(
                point,
                FaultPoint::OldMovedAside
                    | FaultPoint::NewMovedIn
                    | FaultPoint::Applied
                    | FaultPoint::JournalRemoved
            )
        })
        .collect();
    let new = new_state();
    for &first in &commit_points {
        for first_occurrence in 0..4 {
            if !crash_commit(harness, first, first_occurrence) {
                break;
            }
            for &second in &recovery_points {
                for second_occurrence in 0..4 {
                    assert!(crash_commit(harness, first, first_occurrence));
                    let faults = FaultSwitch::default();
                    faults.arm(second, second_occurrence);
                    let storage = harness.open(faults.clone());
                    // Fails when recovery stopped, as a crashed process would.
                    let _ = block_on(storage.ready());
                    let reached = !faults.is_armed();
                    faults.disarm();
                    drop(storage);
                    let storage = open_ready(harness);
                    assert_eq!(
                        contents(&storage),
                        new,
                        "after {first} #{first_occurrence}, then {second} #{second_occurrence} \
                         in recovery"
                    );
                    if !reached {
                        break;
                    }
                }
            }
        }
    }
}

/// An io error at each point: before the commit point the commit fails and changes nothing; after
/// it the commit is committed, reports `Ok`, and is visible to the next read. Either way the store
/// keeps working, and the next commit lands after it.
fn io_errors_in_commits(harness: &mut dyn Harness) {
    let old = old_state();
    let new = new_state();
    for point in harness.fault_points() {
        harness.reset();
        let faults = FaultSwitch::default();
        let storage = harness.open(faults.clone());
        block_on(storage.ready()).expect("ready");
        seed(&storage);
        faults.arm_io_error(point, 0);
        let result = block_on(the_commit(&storage).commit());
        let reached = !faults.is_armed();
        faults.disarm();
        let want = if !reached || point.is_committed() {
            assert!(result.is_ok(), "an io error at {point}: {result:?}");
            &new
        } else {
            assert!(
                matches!(result, Err(StorageError::Io { .. })),
                "an io error at {point}: {result:?}"
            );
            &old
        };
        assert_eq!(&contents(&storage), want, "after an io error at {point}");

        commit(&storage, |txn| {
            txn.write(&key("a"), &META, b"a3".as_slice());
        });
        let mut want = want.clone();
        want.get_mut("a")
            .expect("both states have a")
            .insert("meta".to_owned(), b"a3".to_vec());
        assert_eq!(contents(&storage), want, "the next commit after {point}");
        if harness.persistent() {
            drop(storage);
            let storage = open_ready(harness);
            assert_eq!(
                contents(&storage),
                want,
                "after a restart, io error at {point}"
            );
        }
    }
}
