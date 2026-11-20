use super::*;
use crate::UserStorage;
use futures_lite::future::block_on;

const META: BlobName = BlobName::from_static("meta");

fn key(key: &str) -> Key {
    Key::new(key).unwrap()
}

fn open(root: &Path) -> UserStorage {
    let storage = UserStorage::new(FsStorage::open(root));
    block_on(storage.ready()).unwrap();
    storage
}

fn write(storage: &UserStorage, key_name: &str, bytes: &[u8]) {
    let mut txn = storage.begin();
    txn.write(&key(key_name), &META, bytes);
    block_on(txn.commit()).unwrap();
}

#[test]
fn records_are_flat_directories_of_blob_files() {
    let root = tempfile::tempdir().unwrap();
    let storage = open(root.path());
    write(&storage, "saves/s1/a", b"x");
    let blob = root.path().join("records").join("saves.s1.a").join("meta");
    assert_eq!(fs::read(blob).unwrap(), b"x");
    assert!(!root.path().join("journal").exists());
    assert_eq!(
        fs::read_dir(root.path().join("staging")).unwrap().count(),
        0
    );
    assert_eq!(fs::read_dir(root.path().join("trash")).unwrap().count(), 0);
}

#[test]
fn a_second_handle_is_locked_out_until_the_first_is_dropped() {
    let root = tempfile::tempdir().unwrap();
    let first = open(root.path());
    let second = UserStorage::new(FsStorage::open(root.path()));
    assert!(matches!(
        block_on(second.ready()),
        Err(StorageError::Locked(_))
    ));
    assert_eq!(second.status(), SyncStatus::Unavailable);
    assert!(block_on(second.list(&KeyPrefix::all(), None)).is_err());
    drop(first);
    let third = open(root.path());
    assert_eq!(third.status(), SyncStatus::Synced);
}

#[test]
fn uncommitted_staging_is_discarded_on_open() {
    let root = tempfile::tempdir().unwrap();
    drop(open(root.path()));
    let staged = root.path().join("staging").join("0").join("0");
    fs::create_dir_all(&staged).unwrap();
    fs::write(staged.join("meta"), b"never committed").unwrap();
    fs::write(root.path().join("journal.tmp"), b"half").unwrap();
    fs::create_dir_all(root.path().join("trash").join("0-0")).unwrap();

    let storage = open(root.path());
    assert!(
        block_on(storage.list(&KeyPrefix::all(), None))
            .unwrap()
            .is_empty()
    );
    assert!(!root.path().join("journal.tmp").exists());
    assert_eq!(
        fs::read_dir(root.path().join("staging")).unwrap().count(),
        0
    );
    assert_eq!(fs::read_dir(root.path().join("trash")).unwrap().count(), 0);
}

#[test]
fn a_damaged_journal_leaves_the_store_untouched() {
    let root = tempfile::tempdir().unwrap();
    {
        let storage = open(root.path());
        write(&storage, "a", b"1");
    }
    let staged = root.path().join("staging").join("0").join("0");
    fs::create_dir_all(&staged).unwrap();
    fs::write(
        root.path().join("journal"),
        b"ename_storage journal 1\ntxn 0\n",
    )
    .unwrap();

    let storage = UserStorage::new(FsStorage::open(root.path()));
    assert!(matches!(
        block_on(storage.ready()),
        Err(StorageError::Corrupt(_))
    ));
    assert_eq!(storage.status(), SyncStatus::Unavailable);
    assert!(root.path().join("journal").exists());
    assert!(staged.exists());
    assert!(root.path().join("records").join("a").join("meta").exists());
}

#[test]
fn listing_skips_what_is_not_a_record() {
    let root = tempfile::tempdir().unwrap();
    let storage = open(root.path());
    write(&storage, "a", b"1");
    let records = root.path().join("records");
    fs::create_dir(records.join("Not A Key")).unwrap();
    fs::write(records.join("stray-file"), b"").unwrap();
    fs::write(records.join("a").join(".DS_Store"), b"").unwrap();

    let listing = block_on(storage.list(&KeyPrefix::all(), Some(&META))).unwrap();
    assert_eq!(listing.len(), 1);
    assert_eq!(listing[0].key, key("a"));

    // A file that is not a blob is not carried into the record's next version.
    write(&storage, "a", b"2");
    assert!(!records.join("a").join(".DS_Store").exists());
}

#[test]
fn unchanged_blobs_carry_over_to_the_new_version() {
    let root = tempfile::tempdir().unwrap();
    let storage = open(root.path());
    let state = BlobName::from_static("state");
    let mut txn = storage.begin();
    txn.write(&key("a"), &META, b"m1".as_slice())
        .write(&key("a"), &state, vec![7; 4096]);
    block_on(txn.commit()).unwrap();
    write(&storage, "a", b"m2");
    let record = block_on(storage.read_record(&key("a"), &[META, state.clone()]))
        .unwrap()
        .unwrap();
    assert_eq!(record.get(&META), Some(b"m2".as_slice()));
    assert_eq!(record.get(&state), Some(vec![7; 4096].as_slice()));
}
