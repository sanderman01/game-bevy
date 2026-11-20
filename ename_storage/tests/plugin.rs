//! The plugin installs a backend as the `UserStorage` resource, and `StorageTask` polls its
//! futures from a system.

use bevy::{app::TaskPoolPlugin, prelude::*};
use ename_storage::{
    BlobName, Key, MemoryStorage, StoragePlugin, StorageTask, SyncStatus, UserStorage,
};
use std::time::{Duration, Instant};

const META: BlobName = BlobName::from_static("meta");

fn app(plugin: StoragePlugin) -> App {
    let mut app = App::new();
    app.add_plugins((TaskPoolPlugin::default(), plugin));
    app.update();
    app
}

/// Polls like a system would, once per frame, until the task is done.
fn finish<T: Send + 'static>(app: &mut App, mut task: StorageTask<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(output) = task.poll() {
            assert!(!task.is_pending());
            assert!(task.poll().is_none(), "the output comes once");
            return output;
        }
        assert!(Instant::now() < deadline, "the task did not finish");
        app.update();
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn installs_the_given_backend() {
    let mut app = app(StoragePlugin::with_backend(MemoryStorage::new()));
    let storage = app.world().resource::<UserStorage>().clone();
    assert_eq!(storage.status(), SyncStatus::Volatile);

    let key = Key::new("difficulty/custom").unwrap();
    let mut txn = storage.begin();
    txn.write(&key, &META, b"preset".as_slice());
    finish(&mut app, StorageTask::spawn(txn.commit())).unwrap();
    let read = finish(&mut app, StorageTask::spawn(storage.read(&key, &META)));
    assert_eq!(read.unwrap(), Some(b"preset".to_vec()));
}

#[test]
fn local_without_a_persistent_directory_is_memory() {
    let app = app(StoragePlugin::local(None));
    let storage = app.world().resource::<UserStorage>();
    assert_eq!(storage.status(), SyncStatus::Volatile);
}

#[test]
fn local_with_a_persistent_directory_is_the_filesystem() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app(StoragePlugin::local(Some(root.path())));
    let storage = app.world().resource::<UserStorage>().clone();
    finish(&mut app, StorageTask::spawn(storage.ready())).unwrap();
    assert_eq!(storage.status(), SyncStatus::Synced);
    assert!(
        root.path()
            .join(ename_storage::STORAGE_DIR)
            .join("lock")
            .exists()
    );
}
